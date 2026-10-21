use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::{Error, Result};

pub const USER_AGENT: &str = concat!(
    "geode/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/littlesquirt1/geode)"
);

/// How long one request may take, and how long it may spend reaching the host.
/// Named rather than written where they are used because a joined server's
/// pack is downloaded through a client of its own (`share::pack`), and the
/// answer to "how patient is Geode?" should not depend on which one asked.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_max_idle_per_host(16)
        .build()
        .expect("http client")
}

// ---------------------------------------------------------------------------
// Progress reporting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub id: String,
    pub stage: String,
    pub detail: String,
    /// Bytes (or items) completed and expected. `total == 0` means indeterminate.
    pub current: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// Something the user has to be told that is neither progress nor a failure.
///
/// A job's outcome already travels back to whoever called the command, in
/// whatever shape that command returns — a list of problems, an install
/// report, a filename. This is for a fact that has to be said *the same way*
/// from several of those shapes at once, which threading it through all of
/// them would mean writing three times.
///
/// One sender today: [`crate::install::blocked`] says, in one sentence written
/// in one place, that a CurseForge author's copy of a file is not
/// distributable so Geode installed that author's own Modrinth release
/// instead. It is said from a pack import, a shared pack's reference and an
/// ordinary install, which report in three different shapes and none of which
/// should be able to forget it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub title: String,
    pub body: String,
}

/// A throttled progress channel tied to one long-running job (an install, an
/// import, a Java download). The frontend keys off `id`.
///
/// The sink is optional so the install pipeline can run headlessly — from a
/// test, a CLI, or the container daemon — without a window attached. It is a
/// trait rather than an `AppHandle` for the same reason: none of this logic
/// has anything to do with windows.
#[derive(Clone)]
pub struct Progress {
    sink: Option<crate::events::EventSink>,
    id: String,
    stage: Arc<Mutex<String>>,
    current: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    last_emit: Arc<Mutex<Instant>>,
    cancelled: Arc<AtomicBool>,
}

impl Progress {
    pub fn new(sink: crate::events::EventSink, id: impl Into<String>) -> Self {
        Self::with_sink(Some(sink), id)
    }

    /// A progress handle that reports nowhere. For headless callers.
    pub fn silent(id: impl Into<String>) -> Self {
        Self::with_sink(None, id)
    }

    fn with_sink(sink: Option<crate::events::EventSink>, id: impl Into<String>) -> Self {
        Self {
            sink,
            id: id.into(),
            stage: Arc::new(Mutex::new(String::new())),
            current: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
            last_emit: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1))),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    pub fn check_cancelled(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Begin a new stage, resetting the counters.
    pub async fn stage(&self, name: &str, total: u64) {
        *self.stage.lock().await = name.to_string();
        self.current.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.emit(name.to_string(), false, None).await;
    }

    pub async fn advance(&self, by: u64, detail: &str) {
        self.current.fetch_add(by, Ordering::Relaxed);
        // Throttle to ~20 fps; a 3000-file asset download would otherwise
        // flood the IPC bridge and stall the webview.
        let mut last = self.last_emit.lock().await;
        if last.elapsed() >= Duration::from_millis(50) {
            *last = Instant::now();
            drop(last);
            self.emit(detail.to_string(), false, None).await;
        }
    }

    pub async fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    pub async fn finish(&self) {
        self.current
            .store(self.total.load(Ordering::Relaxed), Ordering::Relaxed);
        self.emit("Done".into(), true, None).await;
    }

    pub async fn fail(&self, err: &str) {
        self.emit("Failed".into(), true, Some(err.to_string()))
            .await;
    }

    /// Tell the user a [`Notice`]. Not throttled and not tied to a stage: it
    /// is said once per thing that happened, and a job that said one still
    /// finishes or fails as it would have.
    pub fn notice(&self, title: impl Into<String>, body: impl Into<String>) {
        crate::events::emit(
            &self.sink,
            "geode://notice",
            &Notice {
                title: title.into(),
                body: body.into(),
            },
        );
    }

    async fn emit(&self, detail: String, done: bool, error: Option<String>) {
        if self.sink.is_none() {
            return;
        }
        let ev = ProgressEvent {
            id: self.id.clone(),
            stage: self.stage.lock().await.clone(),
            detail,
            current: self.current.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            done,
            error,
        };
        crate::events::emit(&self.sink, "geode://progress", &ev);
    }
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Download {
    pub url: String,
    /// Somewhere else the same bytes are, tried when `url` will not answer.
    ///
    /// A pack index may list several hosts for one file, and a mirror listed
    /// second is there for the day the first is down. Only one, because that
    /// is what the format's readers honour and what the hashes make safe: the
    /// bytes are checked against the same pair whichever host served them.
    pub mirror: Option<String>,
    pub dest: PathBuf,
    pub sha1: Option<String>,
    /// Some publishers checksum with SHA-256 instead — PaperMC does — and a
    /// hash we cannot check is a hash that protects nothing.
    pub sha256: Option<String>,
    pub size: Option<u64>,
    /// Human-readable name shown in the progress detail line.
    pub label: String,
}

impl Download {
    pub fn new(url: impl Into<String>, dest: PathBuf) -> Self {
        let url = url.into();
        let label = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| url.clone());
        Self {
            url,
            mirror: None,
            dest,
            sha1: None,
            sha256: None,
            size: None,
            label,
        }
    }

    pub fn mirror(mut self, url: Option<String>) -> Self {
        self.mirror = url;
        self
    }

    pub fn sha1(mut self, hash: Option<String>) -> Self {
        self.sha1 = hash;
        self
    }

    pub fn sha256(mut self, hash: Option<String>) -> Self {
        self.sha256 = hash;
        self
    }

    pub fn size(mut self, size: Option<u64>) -> Self {
        self.size = size;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

pub async fn sha1_file(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let mut h = Sha1::new();
    h.update(&bytes);
    Ok(hex::encode(h.finalize()))
}

/// Modrinth's pack index requires this one specifically, and the launchers
/// that read a `.mrpack` require it without a fallback — though the pack
/// writer now takes it from `digests`. What keeps this here is that it reads
/// the file whole in one go, so it is the independent answer the streaming
/// version is checked against, and that check is the only thing standing
/// between a chunk-boundary bug and a pack whose index disagrees with its
/// own bytes.
pub async fn sha512_file(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let mut h = sha2::Sha512::new();
    h.update(&bytes);
    Ok(hex::encode(h.finalize()))
}

pub async fn sha256_file(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex::encode(h.finalize()))
}

/// Everything a pack writer or a platform lookup can want to know about one
/// file's bytes. Both SHA hashes because the launchers that read a `.mrpack`
/// disagree about which one is optional, and the murmur2 because that is the
/// only name CurseForge knows a file by. The murmur2 is `None` when the caller
/// did not ask for one, so a fingerprint nobody computed can never be read as
/// one that came out zero.
#[derive(Debug, Clone)]
pub struct Digests {
    pub size: u64,
    pub sha1: String,
    pub sha512: String,
    pub murmur2: Option<u32>,
}

/// Everything one file's bytes can say about themselves, read in 1 MiB chunks
/// rather than whole: a pack export runs this over every file in an instance,
/// and a resource pack can be hundreds of megabytes.
///
/// The size and both SHA hashes come out of one pass. `want_murmur2` costs a
/// second pass over the same open file, and that second pass is forced by the
/// algorithm: CurseForge's murmur2 seeds its state with the number of bytes it
/// is about to hash, so that count has to be known before any mixing starts.
/// Xoring the length in at the end instead does not give the same answer —
/// multiplication mod 2^32 does not distribute over xor, and `b"abcd"` comes
/// out 792981580 that way instead of 3376380438. Prism reads its own stream
/// twice for this reason too; the second pass comes from the page cache.
///
/// So it is asked for rather than always taken. The argument for computing it
/// unconditionally was that it costs a few cycles per byte beside the I/O,
/// which holds for a single interleaved pass and not for the second read the
/// paragraph above forces: a 300-jar export would read every jar twice to
/// fingerprint files for a platform the pack cannot even name. One `if` around
/// a pass that is already separate is cheaper than that, and cheaper than the
/// two interleaved code paths the alternative was weighed against.
pub async fn digests(path: &Path, want_murmur2: bool) -> Result<Digests> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || digests_blocking(&owned, want_murmur2))
        .await
        .map_err(|e| Error::msg(format!("hashing a file panicked: {e}")))?
}

fn digests_blocking(path: &Path, want_murmur2: bool) -> Result<Digests> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];

    let mut sha1 = Sha1::new();
    let mut sha512 = sha2::Sha512::new();
    let mut size = 0u64;
    let mut hashable = 0u64;
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        sha1.update(chunk);
        sha512.update(chunk);
        size += read as u64;
        if want_murmur2 {
            hashable += chunk.iter().filter(|b| !murmur2::is_skipped(**b)).count() as u64;
        }
    }

    let mut murmur = None;
    if want_murmur2 {
        // The count is kept to 32 bits because that is the width of the value
        // CurseForge matches against. A file with more than 4 GiB of hashable
        // bytes therefore gets a fingerprint no platform will recognise, which
        // costs a miss — the file travels as bytes — and nothing worse.
        file.seek(SeekFrom::Start(0))?;
        let mut acc = murmur2::Accumulator::new(hashable as u32);
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            acc.update(&buf[..read]);
        }
        murmur = Some(acc.finish());
    }

    Ok(Digests {
        size,
        sha1: hex::encode(sha1.finalize()),
        sha512: hex::encode(sha512.finalize()),
        murmur2: murmur,
    })
}

/// CurseForge identifies a file by MurmurHash2, 32-bit, over its bytes with
/// every tab, newline, carriage return and space dropped, seeded with 1 xor
/// the number of bytes that survive that filter. Transcribed from Prism's
/// `libraries/murmur2/src/MurmurHash2.cpp` and the filter in its
/// `launcher/modplatform/helpers/HashUtils.cpp`.
///
/// Nothing in here is called `fingerprint`: in `providers::curseforge` that
/// word already means an API key rendered safe to display.
pub mod murmur2 {
    const M: u32 = 0x5bd1_e995;
    const R: u32 = 24;

    /// The four bytes CurseForge removes before hashing.
    pub fn is_skipped(b: u8) -> bool {
        matches!(b, 0x09 | 0x0a | 0x0d | 0x20)
    }

    /// The hash of a slice already in hand.
    pub fn hash(bytes: &[u8]) -> u32 {
        let hashable = bytes.iter().filter(|b| !is_skipped(**b)).count() as u32;
        let mut acc = Accumulator::new(hashable);
        acc.update(bytes);
        acc.finish()
    }

    /// Fed raw bytes in pieces of any size; it filters them itself and keeps
    /// the four-byte block it is part way through. The length it is built with
    /// is the post-filter count, and seeding is all the length is needed for:
    /// whether a block or the tail gets mixed falls out of how many bytes have
    /// actually arrived.
    pub struct Accumulator {
        h: u32,
        block: [u8; 4],
        filled: usize,
    }

    impl Accumulator {
        pub fn new(hashable_len: u32) -> Self {
            Self {
                h: 1 ^ hashable_len,
                block: [0; 4],
                filled: 0,
            }
        }

        pub fn update(&mut self, bytes: &[u8]) {
            for &b in bytes.iter().filter(|b| !is_skipped(**b)) {
                self.block[self.filled] = b;
                self.filled += 1;
                if self.filled == 4 {
                    self.mix_block();
                    self.filled = 0;
                }
            }
        }

        fn mix_block(&mut self) {
            let mut k = u32::from_le_bytes(self.block).wrapping_mul(M);
            k ^= k >> R;
            k = k.wrapping_mul(M);
            self.h = self.h.wrapping_mul(M) ^ k;
        }

        /// Mixes in whatever is left of a block, then the shuffles that spread
        /// those last bytes through the whole word.
        pub fn finish(self) -> u32 {
            let mut h = self.h;
            if self.filled == 3 {
                h ^= u32::from(self.block[2]) << 16;
            }
            if self.filled >= 2 {
                h ^= u32::from(self.block[1]) << 8;
            }
            if self.filled >= 1 {
                h ^= u32::from(self.block[0]);
                h = h.wrapping_mul(M);
            }
            h ^= h >> 13;
            h = h.wrapping_mul(M);
            h ^= h >> 15;
            h
        }
    }
}

/// True when the file already on disk matches what we were going to download.
/// When we have a hash we trust only the hash; otherwise we fall back to size,
/// and with neither we accept any existing file that is not empty — a
/// zero-byte file is a write that did not finish, not a download that did.
async fn already_valid(d: &Download) -> bool {
    let Ok(meta) = tokio::fs::metadata(&d.dest).await else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    if let Some(expect) = &d.sha256 {
        return matches!(sha256_file(&d.dest).await, Ok(actual) if actual.eq_ignore_ascii_case(expect));
    }
    if let Some(expect) = &d.sha1 {
        return matches!(sha1_file(&d.dest).await, Ok(actual) if actual.eq_ignore_ascii_case(expect));
    }
    if let Some(size) = d.size {
        return meta.len() == size;
    }
    meta.len() > 0
}

/// Download one file, verifying its hash. Writes to a `.part` sibling and
/// renames on success so an interrupted run never leaves a truncated jar that
/// looks complete on the next launch.
pub async fn fetch_one(client: &reqwest::Client, d: &Download) -> Result<()> {
    if already_valid(d).await {
        return Ok(());
    }
    if let Some(parent) = d.dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut last_err: Option<Error> = None;
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(400 * (1 << attempt))).await;
        }
        // The mirror within the round rather than after all of them: a host
        // that is simply gone refuses at once, and waiting out three rounds of
        // backoff before trying the other one is a minute of nothing per file.
        for url in [Some(&d.url), d.mirror.as_ref()].into_iter().flatten() {
            match try_fetch(client, d, url).await {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }
    }
    Err(last_err.unwrap_or_else(|| Error::msg("download failed")))
}

async fn try_fetch(client: &reqwest::Client, d: &Download, url: &str) -> Result<()> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(Error::msg(format!(
            "{} returned HTTP {}",
            d.label,
            resp.status()
        )));
    }

    let tmp = d.dest.with_extension(format!(
        "{}.part",
        d.dest.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    let mut file = tokio::fs::File::create(&tmp).await?;
    // Only hash what we were given something to compare against.
    let mut sha1 = d.sha1.as_ref().map(|_| Sha1::new());
    let mut sha256 = d.sha256.as_ref().map(|_| Sha256::new());
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if let Some(h) = &mut sha1 {
            h.update(&chunk);
        }
        if let Some(h) = &mut sha256 {
            h.update(&chunk);
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    let checked = [
        (d.sha256.as_ref(), sha256.map(|h| hex::encode(h.finalize()))),
        (d.sha1.as_ref(), sha1.map(|h| hex::encode(h.finalize()))),
    ];
    for (expect, actual) in checked {
        let (Some(expect), Some(actual)) = (expect, actual) else {
            continue;
        };
        if !actual.eq_ignore_ascii_case(expect) {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(Error::Checksum {
                file: d.label.clone(),
                expected: expect.clone(),
                actual,
            });
        }
    }

    tokio::fs::rename(&tmp, &d.dest).await?;
    Ok(())
}

/// Run a batch of downloads with bounded concurrency, reporting per-file
/// progress. Counts files rather than bytes: asset objects are tiny and
/// numerous, so a file count is both cheaper to track and more legible.
pub async fn fetch_all(
    client: &reqwest::Client,
    downloads: Vec<Download>,
    progress: &Progress,
    concurrency: usize,
) -> Result<()> {
    let total = downloads.len() as u64;
    progress.set_total(total).await;
    if downloads.is_empty() {
        return Ok(());
    }

    let mut iter = downloads.into_iter();
    let mut inflight = FuturesUnordered::new();

    for _ in 0..concurrency.min(total as usize) {
        if let Some(d) = iter.next() {
            inflight.push(spawn_one(client.clone(), d));
        }
    }

    while let Some(res) = inflight.next().await {
        let label = res?;
        progress.advance(1, &label).await;
        progress.check_cancelled()?;
        if let Some(d) = iter.next() {
            inflight.push(spawn_one(client.clone(), d));
        }
    }
    Ok(())
}

async fn spawn_one(client: reqwest::Client, d: Download) -> Result<String> {
    fetch_one(&client, &d).await?;
    Ok(d.label)
}

pub async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(Error::msg(format!("{url} returned HTTP {status}")));
    }
    serde_json::from_str(&text).map_err(|e| Error::json(url, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    /// Bytes a test can ask for again and expect to the last one, so the
    /// murmur2 values below can come from an implementation outside Rust.
    fn pseudo_random(len: usize) -> Vec<u8> {
        let mut state = 1u64;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    /// The values Prism's own murmur2 produces, covering every tail length and
    /// a whole block. They pin the constants, the little-endian block reads,
    /// the tail switch and the final mixes: get any one of those wrong and
    /// every CurseForge lookup becomes a silent miss.
    #[test]
    fn murmur2_matches_prisms_own_values() {
        for (bytes, want) in [
            (&b""[..], 1540447798u32),
            (&b"a"[..], 626045324),
            (&b"ab"[..], 1692487918),
            (&b"abc"[..], 1621425345),
            (&b"abcd"[..], 3376380438),
            (&b"abcde"[..], 3469237630),
            (&b"abcdefgh"[..], 1080400154),
        ] {
            assert_eq!(murmur2::hash(bytes), want, "{bytes:?}");
        }
    }

    /// The filter runs first and the length term counts what survives it, so
    /// bytes that differ only by whitespace hash identically. This is the
    /// failure that looks like "CurseForge never recognises anything" rather
    /// than like a bug.
    #[test]
    fn whitespace_is_stripped_before_the_length_is_taken() {
        assert_eq!(murmur2::hash(b"a\tb\nc\rd e"), 3469237630);
        assert_eq!(murmur2::hash(b"abcde"), 3469237630);
        assert_eq!(murmur2::hash(b"   "), 1540447798);
        assert_eq!(murmur2::hash(b"\t\n\r "), 1540447798);
        assert_eq!(murmur2::hash(b""), 1540447798);
    }

    /// The streaming rewrite must not have moved a digest: a pack whose index
    /// hashes disagree with its bytes is one every reader refuses.
    #[tokio::test]
    async fn digests_agree_with_the_single_hash_helpers() {
        let scratch = Scratch::new("digests");
        tokio::fs::create_dir_all(&scratch.paths.root)
            .await
            .unwrap();
        let path = scratch.paths.root.join("blob.bin");
        tokio::fs::write(&path, pseudo_random(5000)).await.unwrap();

        let d = digests(&path, true).await.unwrap();
        assert_eq!(d.sha1, sha1_file(&path).await.unwrap());
        assert_eq!(d.sha512, sha512_file(&path).await.unwrap());
        assert_eq!(d.size, tokio::fs::metadata(&path).await.unwrap().len());
    }

    /// The murmur2 needs a whole second read of the file, so a caller that is
    /// not going to ask CurseForge about anything must not pay for one. An
    /// `.mrpack` export is exactly that caller, and it runs over every file in
    /// the instance.
    #[tokio::test]
    async fn a_caller_that_wants_no_murmur2_is_not_given_one() {
        let scratch = Scratch::new("digests-unasked");
        tokio::fs::create_dir_all(&scratch.paths.root)
            .await
            .unwrap();
        let path = scratch.paths.root.join("blob.bin");
        let bytes = pseudo_random(5000);
        tokio::fs::write(&path, &bytes).await.unwrap();

        let asked = digests(&path, true).await.unwrap();
        let unasked = digests(&path, false).await.unwrap();

        assert_eq!(asked.murmur2, Some(murmur2::hash(&bytes)));
        assert_eq!(unasked.murmur2, None);
        // Everything the first pass produces is there either way.
        assert_eq!(unasked.sha1, asked.sha1);
        assert_eq!(unasked.sha512, asked.sha512);
        assert_eq!(unasked.size, asked.size);
    }

    /// A file of exactly one chunk and one of a chunk plus three bytes, so the
    /// three accumulators are checked across a boundary at once — and the
    /// murmur2 against values from a separate implementation of the same
    /// algorithm, not just against this module's own slice form.
    #[tokio::test]
    async fn digests_streams_across_chunk_boundaries() {
        let scratch = Scratch::new("digests-chunks");
        tokio::fs::create_dir_all(&scratch.paths.root)
            .await
            .unwrap();

        for (len, murmur) in [(1 << 20, 1336806114u32), ((1 << 20) + 3, 3025778575)] {
            let path = scratch.paths.root.join(format!("blob-{len}.bin"));
            let bytes = pseudo_random(len);
            tokio::fs::write(&path, &bytes).await.unwrap();

            let d = digests(&path, true).await.unwrap();
            assert_eq!(d.size, len as u64, "size at {len}");
            assert_eq!(d.sha1, sha1_file(&path).await.unwrap(), "sha1 at {len}");
            assert_eq!(
                d.sha512,
                sha512_file(&path).await.unwrap(),
                "sha512 at {len}"
            );
            assert_eq!(d.murmur2, Some(murmur), "murmur2 at {len}");
            assert_eq!(
                d.murmur2,
                Some(murmur2::hash(&bytes)),
                "murmur2 in one go at {len}"
            );
        }
    }
}

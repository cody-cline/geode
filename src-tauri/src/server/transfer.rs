//! Moving or copying a whole server from one machine to another.
//!
//! The piece `daemon/mod.rs` was written around and deliberately left out. A
//! server lives in one folder, and this sends that folder: `server.json`, the
//! whole of `run/` — the world, `server.properties`, `eula.txt`, Minecraft's
//! own player files, mods, plugins and config — `media/`, and `backups/`.
//!
//! # The order the decisions were made in
//!
//! **Nothing is deleted until the other side is proved complete.** A move is a
//! copy that is verified and only then takes the source away. Verified *by
//! content*: the sender hashes every file as it sends it and writes the list of
//! digests as the **last** entry in the bundle, so a stream that stopped part
//! way has no list at all and can never be committed. The receiver hashes what
//! it writes, compares the two lists both ways, and only then renames its
//! staging directory into place. A byte count would have passed a world that
//! arrived corrupt; this does not.
//!
//! **A running server is not transferable.** Refused on both sides, by the
//! route and the command rather than here, the way `delete` and `restore` are
//! refused — a world being written underneath a reader is a world that arrives
//! torn, and the digest of a file that changed mid-read is a digest of bytes
//! that were never a file. [`write_bundle`] refuses outright if a file's length
//! moves while it is being read, which is the same accident arriving through
//! the back door.
//!
//! **A failure leaves both sides consistent.** The source is untouched until
//! the end. The destination assembles in `transfers/<id>/`, which is *outside*
//! `servers/` on purpose: `server::list` reads every directory under
//! `servers/`, and a half-arrived server there would show in the app as a real
//! one the moment its `server.json` landed. Anything that goes wrong — a
//! refusal, a dropped connection, a cancel, a digest that did not match —
//! removes that directory, so the destination is absent rather than plausible.
//! A folder left in `transfers/` is a transfer whose process died; nothing
//! reads it, the next attempt at that server clears it, and [`sweep`] clears
//! the lot as a host starts — so one interrupted by a restart is not left on
//! the disk for good.
//!
//! **An id clash is refused, never merged or renamed.** Unpacking a second
//! `survival` over the first would interleave two worlds; renaming it to
//! `survival-2` would leave two servers with the same name on the same machine
//! and no way to tell which is which. So the receiver refuses by name and says
//! what to do about it, before it has written a byte.
//!
//! **Bytes, not references — and that is not a reversal.** The pack format
//! carries provider content by project and version, and
//! docs/sharing-and-hosting.md says why. A transfer is not a share: it is one
//! person's own files moving between two of their own machines, with nothing
//! redistributed to anybody. Re-fetching a server's mods on the host would need
//! the host to hold that person's CurseForge key, and would quietly lose any
//! mod whose version has since been taken down. A move that loses mods is not a
//! move.
//!
//! # What happens to the sharing
//!
//! A server's sharing is its password, the address its links were built from,
//! and the baseline the next settings push is measured against. The rule is:
//! **a copy carries the server; a move carries the server and its sharing.**
//!
//! * **A copy arrives with no sharing at all.** Two machines serving the same
//!   pack under the same password is two divergent servers that players cannot
//!   tell apart, and a password leaked on one would then be a password leaked
//!   on both. It could not even work: the certificate players pin belongs to
//!   the *machine* (`paths::share_dir`), so the copy would present a different
//!   one. The admin shares the copy themselves, and gets a fresh password for
//!   it.
//! * **A move carries the password, stopped.** Players hold links built from
//!   it, and an admin who had to replace it on every move would cut off
//!   everybody each time. It arrives as `share.stopped.json` — the shape
//!   `share.rs` already has for "has a password, is not serving" — because the
//!   new machine has a different certificate and may not have the port open,
//!   and a server that began serving itself the moment it landed would be
//!   serving something no existing link can verify. The receiver never accepts
//!   a `share.json`, so that is a property of this code rather than of the
//!   sender's good manners.
//! * **A move carries `share.link.json` too**, which is what makes the
//!   destination's Share tab say, by itself and in words it already has, that
//!   the links handed out point at an address that has moved on. That is
//!   exactly true after a move, and it is the one thing the admin has to act on.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use futures::{Stream, StreamExt};
use hyper::body::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::error::{Context, Error, Result};
use crate::net::Progress;
use crate::paths::Paths;

/// The bundle format. Refused rather than guessed at if it is not this: a
/// newer Geode may carry something this one would drop on the floor.
pub const FORMAT: u32 = 1;

/// The digest list, and the last entry in every bundle.
///
/// Last, not first: a bundle that stopped part way then has no manifest, so a
/// truncated transfer cannot be committed however complete it looks. It also
/// means one read of each file rather than two.
pub const MANIFEST_NAME: &str = "geode-transfer.json";

/// The header an uploader uses to say how much is coming, so a host can refuse
/// in one round trip instead of after 64 GiB. Advisory: what actually bounds a
/// transfer is [`MAX_BUNDLE_BYTES`], counted as the bytes land.
pub const DECLARED_BYTES: &str = "x-geode-transfer-bytes";

/// The most a bundle may be.
///
/// Not a capacity plan. A world grows without limit, and a staging directory is
/// the largest thing anybody holding a host's token can make it allocate — so
/// there is a number past which it refuses rather than filling the disk it also
/// runs servers from. A host serves one transfer at a time (`daemon::api`), so
/// this is the whole of what one can cost.
pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// How much is handed over at a time between the blocking archive and the
/// socket. Big enough that a gigabyte is a few thousand messages rather than a
/// hundred thousand; small enough to be nothing in memory.
const CHUNK: usize = 256 * 1024;

/// What a transfer always carries, by the name at the top of `servers/<id>/`.
///
/// The sender's list of what to walk and the receiver's list of what to accept,
/// which is why it is one list. `logs/` and `crash-reports/` are in `run/` and
/// travel with it: unlike a backup, a transfer is the same server afterwards,
/// and its history is part of it.
const CARRIED: &[&str] = &["server.json", "run", "media", "backups"];

/// What only a move carries: the server's sharing. See the module header for
/// each one, and why a `share.json` is not among them.
const MOVED: &[&str] = &["share.stopped.json", "share.link.json", "published"];

/// What a transfer does to the server it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Leave the original where it is. The arriving server is not shared.
    Copy,
    /// Take the original away, once the arrival has been proved complete.
    Move,
}

/// How big a transfer would be, before it starts.
///
/// Backups are counted apart because they are usually most of it and the person
/// deciding should see that before committing a domestic uplink to it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measure {
    pub files: u64,
    pub bytes: u64,
    pub backup_files: u64,
    pub backup_bytes: u64,
}

/// A server that arrived and was proved whole.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Arrived {
    pub id: String,
    pub files: u64,
    pub bytes: u64,
}

/// One file, as the manifest names it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: u32,
    /// Which server this is, so a bundle cannot be committed under a name it
    /// was not made for.
    id: String,
    files: Vec<Entry>,
}

// ---------------------------------------------------------------------------
// Working out what to send
// ---------------------------------------------------------------------------

/// A file to send: where it is, the name it travels under, and what the
/// filesystem said about it when the plan was made.
struct Planned {
    from: PathBuf,
    name: String,
    size: u64,
    /// When it was last written, as far as this filesystem will say.
    ///
    /// Read so that [`changed`] can hold a second reading of the server
    /// against this one: a file rewritten in place at the same length is
    /// otherwise a digest of bytes that were never one file.
    modified: Option<std::time::SystemTime>,
}

/// Everything a bundle will hold, worked out from metadata alone.
#[derive(Default)]
struct Plan {
    /// Folders, so an empty one survives the trip — `run/client-mods/` is
    /// deliberately a folder no loader reads, and it is still part of the
    /// server.
    dirs: Vec<String>,
    files: Vec<Planned>,
    measure: Measure,
    /// Every folder walked, by the path the filesystem resolves it to.
    ///
    /// Links are followed, so without this a server that holds a link to one
    /// of its own folders would be walked round in a circle forever.
    seen: std::collections::HashSet<PathBuf>,
}

impl Plan {
    fn push(&mut self, from: PathBuf, name: String, meta: &std::fs::Metadata, backups: bool) {
        self.measure.files += 1;
        self.measure.bytes += meta.len();
        if backups {
            self.measure.backup_files += 1;
            self.measure.backup_bytes += meta.len();
        }
        self.files.push(Planned {
            from,
            name,
            size: meta.len(),
            modified: meta.modified().ok(),
        });
    }
}

/// Walk the server's folder and say exactly what a bundle of it would hold.
///
/// Metadata only, so it is quick enough to answer the question the UI asks
/// before anybody commits to the transfer.
fn plan(root: &Path, mode: Mode) -> Result<Plan> {
    let mut plan = Plan::default();
    let tops: Vec<&str> = match mode {
        Mode::Copy => CARRIED.to_vec(),
        Mode::Move => CARRIED.iter().chain(MOVED.iter()).copied().collect(),
    };

    for top in tops {
        // A move carries the password whichever of the two files holds it, and
        // always under the stopped name: see the module header.
        let from = if top == "share.stopped.json" {
            let live = root.join("share.json");
            if live.is_file() {
                live
            } else {
                root.join(top)
            }
        } else {
            root.join(top)
        };

        // Two readings, because they answer two different questions. Whether
        // there is anything here at all is asked without following a link: a
        // server with no backups yet is not an error, and a link whose target
        // has gone is. What travels is then what the link points at — an
        // admin who keeps the world on a second disk has one here, and
        // everything in the server travels.
        if std::fs::symlink_metadata(&from).is_err() {
            continue;
        }
        let meta = std::fs::metadata(&from).map_err(|e| {
            Error::msg(format!(
                "'{top}' in this server cannot be read ({e}), so this server cannot be \
                 transferred — if it is a link, point it at the folder it should be, or remove it"
            ))
        })?;
        if meta.is_dir() {
            walk(&from, top, top == "backups", &mut plan)?;
        } else if meta.is_file() {
            plan.push(from, top.to_string(), &meta, false);
        } else {
            return Err(neither_a_file_nor_a_folder(top));
        }
    }
    Ok(plan)
}

/// Something in the server that a bundle cannot carry.
///
/// A socket, a device, a pipe. Nothing could be sent for it and nothing on the
/// other machine could hold it — and the alternative to refusing is a bundle
/// that quietly lacks it, which the receiver would then verify against a
/// manifest that does not name it either and a move would take as leave to
/// delete the original.
fn neither_a_file_nor_a_folder(name: &str) -> Error {
    Error::msg(format!(
        "'{name}' in this server is neither a file nor a folder, so this server cannot be \
         transferred — move it out of the server's folder and try again"
    ))
}

/// One folder and everything under it, named relative to the server.
///
/// Sorted, so bundling the same server twice produces the same bundle. Links
/// are followed, so what travels is what they point at: a world kept on a
/// second disk is still the server's world, and a bundle without it is a
/// bundle a move would delete the only copy of the world for. Anything that is
/// then still neither a file nor a folder is refused rather than dropped.
fn walk(dir: &Path, name: &str, backups: bool, plan: &mut Plan) -> Result<()> {
    // Following a link means a server can hold one of its own folders, which
    // without this is walked round in a circle until the stack runs out.
    // Compared by the resolved path, which is the only thing two names for one
    // folder have in common.
    let real = dir
        .canonicalize()
        .map_err(|e| Error::msg(format!("could not read {}: {e}", dir.display())))?;
    if !plan.seen.insert(real) {
        return Err(Error::msg(format!(
            "'{name}' is a link to a folder this server already holds, so this server cannot be \
             transferred — what is inside it would be sent twice or without end"
        )));
    }

    plan.dirs.push(name.to_string());
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)
        .map_err(|e| Error::msg(format!("could not read {}: {e}", dir.display())))?
        .flatten()
        .collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let Some(part) = entry.file_name().to_str().map(str::to_string) else {
            return Err(Error::msg(format!(
                "'{}' has a name that is not text, so this server cannot be transferred — \
                 rename it and try again",
                entry.path().display()
            )));
        };
        let child = format!("{name}/{part}");
        // Followed, for the reason at the top of this function. A link whose
        // target has gone fails here rather than being passed over.
        let meta = std::fs::metadata(entry.path())
            .map_err(|e| Error::msg(format!("could not read {}: {e}", entry.path().display())))?;
        if meta.is_dir() {
            walk(&entry.path(), &child, backups, plan)?;
        } else if meta.is_file() {
            plan.push(entry.path(), child, &meta, backups);
        } else {
            return Err(neither_a_file_nor_a_folder(&child));
        }
    }
    Ok(())
}

/// How big a transfer of this server would be.
pub async fn measure(paths: &Paths, id: &str, mode: Mode) -> Result<Measure> {
    let root = paths.server(id);
    tokio::task::spawn_blocking(move || plan(&root, mode).map(|plan| plan.measure))
        .await
        .map_err(|e| Error::msg(format!("reading the server's files failed: {e}")))?
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

/// The bundle, as bytes to put on a socket.
///
/// The archive is written by a blocking task — `tar` and the hashing are both
/// synchronous — which hands each chunk over as it is made. Nothing is held in
/// memory but the chunk in flight, and nothing is staged on disk: a server is
/// as big as its world, and a sender that wrote the whole bundle out first
/// would need room for a second copy of it.
///
/// A failure part way arrives as an `Err` in the stream, which fails the
/// request carrying it. The receiver then has a stream that ended with no
/// manifest, which it refuses.
pub fn bundle(
    paths: &Paths,
    id: &str,
    mode: Mode,
    progress: Progress,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    let (tx, rx) = mpsc::channel::<std::io::Result<Bytes>>(4);
    let root = paths.server(id);
    let detail = id.to_string();
    let id = id.to_string();
    let cancelled = progress.cancel_flag();

    let failed = tx.clone();
    tokio::task::spawn_blocking(move || {
        let out = std::io::BufWriter::with_capacity(CHUNK, Chunks { tx });
        if let Err(e) = write_bundle(&root, &id, mode, out, &cancelled) {
            // Best effort: the other end may already have hung up, which is
            // how a cancel on the receiving side reaches this task at all.
            let _ = failed.blocking_send(Err(std::io::Error::other(e.to_string())));
        }
    });

    futures::stream::unfold((rx, progress, detail), |(mut rx, progress, detail)| async move {
        let chunk = rx.recv().await?;
        if let Ok(bytes) = &chunk {
            progress.advance(bytes.len() as u64, &detail).await;
        }
        Some((chunk, (rx, progress, detail)))
    })
}

/// A `Write` that hands each buffer to whoever is reading the stream.
///
/// `blocking_send` is correct here and only here: this runs on a blocking task,
/// so what it blocks is one of those threads and never a runtime worker. A
/// send that fails means the reader has gone, which is what a cancelled
/// download looks like from in here.
struct Chunks {
    tx: mpsc::Sender<std::io::Result<Bytes>>,
}

impl Write for Chunks {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tx
            .blocking_send(Ok(Bytes::copy_from_slice(buf)))
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the other end stopped reading the transfer",
                )
            })?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Write the whole bundle: the folders, then the files with their digests
/// collected, then the manifest.
///
/// Mode and mtime are not carried. A transfer carries the server's data, not
/// another machine's filesystem bookkeeping, and nothing in Minecraft or in
/// Geode reads either off these files.
fn write_bundle<W: Write>(
    root: &Path,
    id: &str,
    mode: Mode,
    out: W,
    cancelled: &AtomicBool,
) -> Result<()> {
    let plan = plan(root, mode)?;
    let mut archive = tar::Builder::new(out);

    for dir in &plan.dirs {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_mtime(0);
        header.set_size(0);
        archive.append_data(&mut header, dir, std::io::empty())?;
    }

    let mut files = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let source = std::fs::File::open(&file.from)
            .map_err(|e| Error::msg(format!("could not read {}: {e}", file.from.display())))?;
        let mut hashing = Hashing {
            inner: source,
            hasher: Sha256::new(),
            read: 0,
        };
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_size(file.size);
        archive.append_data(&mut header, &file.name, &mut hashing)?;

        // The header said how long this file was a moment ago, and the archive
        // is padded to that. A file that moved underneath us has made a bundle
        // whose contents do not match its own frame, so it is refused here
        // rather than becoming a digest mismatch on another machine.
        if hashing.read != file.size {
            return Err(Error::msg(format!(
                "{} changed while it was being sent, so nothing was transferred — \
                 something is still writing to this server",
                file.name
            )));
        }
        files.push(Entry {
            path: file.name.clone(),
            size: hashing.read,
            sha256: hex::encode(hashing.hasher.finalize()),
        });
    }

    // The plan was a snapshot of the folder as it was before a byte of it had
    // been read, and the manifest only ever describes that snapshot: a file
    // written after it was taken is neither sent nor missed, and one rewritten
    // in place at the same length passes the check above and arrives as a
    // digest of bytes that were never one file. Both verify cleanly on the
    // other machine, and a move then deletes the original. "Not while it runs"
    // cannot cover this on its own — it is a question asked once, and a
    // transfer takes an hour — so the folder is read again here and what was
    // sent has to still be what is there.
    let after = self::plan(root, mode)?; // `self::`: the binding above has this name.
    if let Some(name) = changed(&plan, &after) {
        return Err(Error::msg(format!(
            "this server was written to while it was being sent, so nothing was transferred — \
             {name} is not as it was when the transfer began"
        )));
    }

    let manifest = serde_json::to_vec(&Manifest {
        version: FORMAT,
        id: id.to_string(),
        files,
    })?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_size(manifest.len() as u64);
    archive.append_data(&mut header, MANIFEST_NAME, &manifest[..])?;

    let mut out = archive.into_inner()?;
    out.flush()?;
    Ok(())
}

/// The first thing two readings of one server disagree about, if any.
///
/// By name, length and modification time, in the sorted order a plan walks
/// them. A file whose content was replaced at the same length in the same
/// instant the filesystem's clock last named is the one change this cannot
/// see; everything a server or a restored backup does is slower than that.
fn changed(before: &Plan, after: &Plan) -> Option<String> {
    for (was, now) in before.files.iter().zip(&after.files) {
        if was.name != now.name || was.size != now.size || was.modified != now.modified {
            return Some(now.name.clone());
        }
    }
    let common = before.files.len().min(after.files.len());
    if before.files.len() != after.files.len() {
        let longer = if before.files.len() > common {
            &before.files
        } else {
            &after.files
        };
        return Some(longer[common].name.clone());
    }
    if before.dirs != after.dirs {
        let gone = before.dirs.iter().find(|dir| !after.dirs.contains(dir));
        let new = after.dirs.iter().find(|dir| !before.dirs.contains(dir));
        return gone.or(new).cloned();
    }
    None
}

/// A reader that hashes and counts what passes through it, so a file is read
/// once whether it is being sent or being measured.
struct Hashing<R> {
    inner: R,
    hasher: Sha256,
    read: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.hasher.update(&buf[..read]);
        self.read += read as u64;
        Ok(read)
    }
}

// ---------------------------------------------------------------------------
// Receiving
// ---------------------------------------------------------------------------

/// The staging directories being filled right now, by this process.
///
/// Keyed by the directory rather than by the id, because two `GEODE_HOME`s are
/// two different machines as far as a transfer is concerned.
static ARRIVING: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// One transfer into one place at a time, held until that transfer is over.
///
/// A host takes one upload at a time (`daemon::api`) but the app has no such
/// permit, and either way nothing stopped two transfers of the *same* server
/// starting: [`receive`] clears the staging directory before it writes to it,
/// so the second would delete the first's files from under it — and the first,
/// whose digests are its own record of what it wrote rather than a reading of
/// the disk, would verify them and rename whatever the second had written so
/// far into place as a server proved complete. Both sides then delete the
/// original. So this is a claim, in the shape
/// [`crate::server::run::ServerRegistry::claim_start`] already has, and the
/// second transfer is refused before anything has been touched.
struct Arriving(PathBuf);

impl Arriving {
    fn take(staging: &Path) -> Option<Self> {
        let mut arriving = ARRIVING.lock().ok()?;
        if !arriving.insert(staging.to_path_buf()) {
            return None;
        }
        Some(Self(staging.to_path_buf()))
    }
}

impl Drop for Arriving {
    fn drop(&mut self) {
        if let Ok(mut arriving) = ARRIVING.lock() {
            arriving.remove(&self.0);
        }
    }
}

/// Take a bundle, prove it whole, and put the server in place.
///
/// Every failure leaves the destination with nothing: the staging directory
/// goes, and `servers/<id>` is never touched until the rename that is the last
/// thing to happen.
pub async fn receive<S, E>(
    paths: &Paths,
    id: &str,
    stream: S,
    declared: u64,
    progress: &Progress,
) -> Result<Arrived>
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Send + Unpin,
    E: std::fmt::Display,
{
    let staging = paths.transfer(id);
    let destination = paths.server(id);
    // An id off a network path, or out of a webview. Both are re-parsed before
    // they reach here; this is what makes that not matter.
    if !staging.starts_with(paths.transfers())
        || staging == paths.transfers()
        || !destination.starts_with(paths.servers())
        || destination == paths.servers()
    {
        return Err(Error::msg("that is not a server name"));
    }
    if tokio::fs::metadata(&destination).await.is_ok() {
        return Err(clash(id));
    }
    if declared > MAX_BUNDLE_BYTES {
        return Err(too_large());
    }

    // Before the staging directory is touched: see [`Arriving`].
    let Some(_place) = Arriving::take(&staging) else {
        return Err(Error::msg(format!(
            "'{id}' is already arriving here — wait for that transfer to finish, or cancel it, \
             before starting another"
        )));
    };

    // A transfer whose process died part way. Nothing reads it — it is outside
    // `servers/` — so there is nothing to do with it but clear it.
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging).await?;

    match land(paths, id, &staging, stream, declared, progress).await {
        Ok(arrived) => Ok(arrived),
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            Err(e)
        }
    }
}

/// Clear every staging directory. For a process starting up, and nothing else.
///
/// [`receive`] clears the one directory it is about to use, which covers a
/// second attempt at the same server and nothing else: a host restarted by its
/// compose file or by `systemctl` mid-upload drops the request where it stands
/// (`daemon::serve` cuts what it is serving), and that id's staging directory
/// — up to [`MAX_BUNDLE_BYTES`] of it — is then read by nothing and removed by
/// nothing. A few restarts over a few different servers fill the disk the host
/// also runs its servers from, which is the exact thing the ceiling and the
/// one-at-a-time permit were there to bound.
///
/// Called before anything can be arriving, because it does not ask: a sweep
/// during a transfer would delete that transfer's own files.
pub async fn sweep(paths: &Paths) {
    // Best effort. A host that cannot clear this can do nothing about it, and
    // refusing to start would be refusing to run servers because a dead
    // transfer is in the way.
    let _ = tokio::fs::remove_dir_all(paths.transfers()).await;
}

fn clash(id: &str) -> Error {
    Error::msg(format!(
        "a server called '{id}' is already there — rename it or delete it first, \
         so a transfer can never arrive over the top of one"
    ))
}

/// A bundle that stopped part way, or was never a bundle.
///
/// One sentence for both, because from inside the unpacking they are the same
/// thing: the archive ran out before it said it had finished. `tar`'s own words
/// go in brackets — they are a diagnosis, not the answer.
fn cut_short(e: std::io::Error) -> Error {
    Error::msg(format!(
        "the transfer ended before it was complete, so nothing was kept ({e})"
    ))
}

fn too_large() -> Error {
    Error::msg(format!(
        "that server is larger than a transfer may be ({} GB)",
        MAX_BUNDLE_BYTES / (1024 * 1024 * 1024)
    ))
}

/// Unpack, verify, promote. Anything it returns `Err` for is cleaned up by
/// [`receive`], which is why nothing in here undoes its own work.
async fn land<S, E>(
    paths: &Paths,
    id: &str,
    staging: &Path,
    mut stream: S,
    declared: u64,
    progress: &Progress,
) -> Result<Arrived>
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Send + Unpin,
    E: std::fmt::Display,
{
    let (tx, rx) = mpsc::channel::<Bytes>(4);
    let dest = staging.to_path_buf();
    let unpacking = tokio::task::spawn_blocking(move || unpack(Incoming::new(rx), &dest));

    progress.stage("Receiving the server", declared).await;
    let fed = async {
        let mut received = 0u64;
        while let Some(chunk) = crate::share::join::unless_cancelled(progress, stream.next()).await?
        {
            let chunk = chunk
                .map_err(|e| Error::msg(format!("the transfer was interrupted: {e}")))?;
            let size = chunk.len() as u64;
            received += size;
            if received > MAX_BUNDLE_BYTES {
                return Err(too_large());
            }
            if tx.send(chunk).await.is_err() {
                // The unpacker has stopped, and its own error says why. Stop
                // reading rather than reporting a broken channel over the top
                // of it.
                break;
            }
            progress.advance(size, id).await;
        }
        Ok(())
    }
    .await;
    // Before the join: the unpacker is blocked on the next chunk until the
    // sender is gone, and `fed` owns the only other handle to it.
    drop(tx);

    let landed = unpacking
        .await
        .map_err(|e| Error::msg(format!("unpacking the transfer failed: {e}")))?;
    // What went wrong with the stream explains an unpack that merely ran out
    // of bytes, so it is reported first.
    fed?;
    let landed = landed?;

    let Some(manifest) = landed.manifest else {
        return Err(Error::msg(
            "the transfer ended before it was complete, so nothing was kept — \
             the list of files a sender writes last never arrived",
        ));
    };
    let arrived = verify(id, &manifest, &landed.files)?;

    // The record says which server this is, and every route reaches it by the
    // folder's name. A bundle whose `server.json` disagrees would land a server
    // that cannot be started, stopped or deleted.
    let bytes = tokio::fs::read(staging.join("server.json"))
        .await
        .map_err(|_| Error::msg("what arrived holds no server.json, so it is not a server"))?;
    let record: crate::server::Server = serde_json::from_slice(&bytes)
        .map_err(|e| Error::json("the transferred server.json", e))?;
    if record.id != id {
        return Err(Error::msg(format!(
            "that transfer holds the server '{}', not '{id}'",
            record.id
        )));
    }

    // Checked again: the clash check at the start of a transfer is minutes old
    // by now, and a server created in the meantime must not be renamed over.
    if tokio::fs::metadata(paths.server(id)).await.is_ok() {
        return Err(clash(id));
    }
    tokio::fs::create_dir_all(paths.servers()).await?;
    tokio::fs::rename(staging, paths.server(id)).await?;
    // The rename is the moment the server exists here, and the answer that
    // goes back from it is what authorises deleting the other copy — so the
    // name has to survive a power cut as much as the bytes under it do.
    let servers = paths.servers();
    tokio::task::spawn_blocking(move || sync_dir(&servers))
        .await
        .map_err(|e| Error::msg(format!("saving the arrived server failed: {e}")))??;
    Ok(arrived)
}

/// Everything that landed, by the name it landed under.
#[derive(Default)]
struct Landed {
    /// Path to the size and digest of what was actually written.
    files: std::collections::BTreeMap<String, (u64, String)>,
    /// Every folder something was written into, so each one's own list of
    /// names can be made durable once rather than once per file. See
    /// [`sync_dir`].
    dirs: std::collections::BTreeSet<PathBuf>,
    manifest: Option<Manifest>,
}

/// Read the bundle into `dest`, hashing every file as it is written.
///
/// Hashed on the way in rather than read back afterwards: the digest then
/// covers the bytes that went to the disk, and a second pass over a 10 GB world
/// is an hour nobody has.
///
/// Every file is synced before this returns, and so is every folder's own list
/// of names. What this function says happened is what [`verify`] proves the
/// transfer by, and that proof is the whole authority for deleting the only
/// other copy of the server — so it has to mean the disk rather than the page
/// cache. A host that answers 201 and then loses power must still have the
/// world when it comes back. `server::write_atomic` pays the same price for a
/// far smaller stake.
fn unpack<R: Read>(input: R, dest: &Path) -> Result<Landed> {
    let mut archive = tar::Archive::new(input);
    let mut landed = Landed::default();

    for entry in archive.entries().map_err(cut_short)? {
        let mut entry = entry.map_err(cut_short)?;
        let path = entry.path().map_err(cut_short)?.to_path_buf();
        let name = path.to_string_lossy().replace('\\', "/");

        if landed.manifest.is_some() {
            return Err(Error::msg(format!(
                "this transfer holds '{name}' after the list of files that should have ended it, \
                 so it was not kept"
            )));
        }
        if name == MANIFEST_NAME {
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(cut_short)?;
            landed.manifest =
                Some(serde_json::from_str(&text).map_err(|e| Error::json(MANIFEST_NAME, e))?);
            continue;
        }

        // This is an archive Geode did not create — it arrived from another
        // machine — so the same rule applies as to any other.
        if !crate::paths::is_safe_relative(&path) || !carried(&path) {
            return Err(Error::msg(format!(
                "this transfer holds '{name}', which is not part of a server, so it was not kept"
            )));
        }
        let target = dest.join(&path);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&target).ctx(format!("making {name}"))?;
            landed.dirs.insert(target);
            continue;
        }
        if !kind.is_file() {
            return Err(Error::msg(format!(
                "this transfer holds '{name}', which is not a file or a folder, so it was not kept"
            )));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).ctx(format!("making a place for {name}"))?;
            landed.dirs.insert(parent.to_path_buf());
        }

        let mut file = std::fs::File::create(&target).ctx(format!("writing {name}"))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut size = 0u64;
        loop {
            let read = entry.read(&mut buf).map_err(cut_short)?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
            file.write_all(&buf[..read]).ctx(format!("writing {name}"))?;
            size += read as u64;
        }
        // `sync_all` rather than `flush`, for the reason above this function.
        file.sync_all().ctx(format!("writing {name}"))?;
        landed
            .files
            .insert(name, (size, hex::encode(hasher.finalize())));
    }

    // The files are on the disk; this is what makes their names so. Last, and
    // in one pass, so a folder with a thousand region files in it costs one
    // sync rather than a thousand.
    for dir in &landed.dirs {
        sync_dir(dir)?;
    }
    Ok(landed)
}

/// Make a folder's own list of names durable, so a file that was synced can
/// still be found after the power goes.
///
/// Unix only. Windows will not open a directory as a file and `std` offers no
/// equivalent there, so what a transfer onto Windows can promise is each
/// file's own `sync_all` and nothing about the names — which is worth saying
/// plainly rather than papering over, like everything else about the two
/// platforms nobody here has run.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .ctx(format!("saving {}", dir.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Whether a path in an arriving bundle is part of a server at all.
///
/// The top of it only. What is below is the server's own business — a mod
/// writes whatever it likes into `config/` — and [`crate::paths::is_safe_relative`]
/// is what keeps all of it inside the folder being built.
fn carried(path: &Path) -> bool {
    path.components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .is_some_and(|top| CARRIED.contains(&top) || MOVED.contains(&top))
}

/// Hold what arrived against what the sender said it was sending.
///
/// Both ways. A file the manifest names and the bundle did not carry is a
/// truncated transfer; a file the bundle carried and the manifest does not name
/// is a bundle nobody can vouch for, and it would be sitting in the server's
/// folder afterwards.
fn verify(
    id: &str,
    manifest: &Manifest,
    landed: &std::collections::BTreeMap<String, (u64, String)>,
) -> Result<Arrived> {
    if manifest.version != FORMAT {
        return Err(Error::msg(format!(
            "this transfer was made by a different version of Geode (format {}, this one speaks {FORMAT}) — \
             update both and try again",
            manifest.version
        )));
    }
    if manifest.id != id {
        return Err(Error::msg(format!(
            "that transfer holds the server '{}', not '{id}'",
            manifest.id
        )));
    }

    let mut bytes = 0u64;
    for want in &manifest.files {
        let Some((size, sha256)) = landed.get(&want.path) else {
            return Err(Error::msg(format!(
                "{} never arrived, so nothing was kept — the transfer was cut short",
                want.path
            )));
        };
        if *size != want.size || *sha256 != want.sha256 {
            return Err(Error::msg(format!(
                "{} arrived holding something other than what was sent, so nothing was kept — \
                 the transfer was corrupted on the way",
                want.path
            )));
        }
        bytes += want.size;
    }
    for name in landed.keys() {
        if !manifest.files.iter().any(|want| &want.path == name) {
            return Err(Error::msg(format!(
                "{name} arrived without being listed, so nothing was kept"
            )));
        }
    }

    Ok(Arrived {
        id: id.to_string(),
        files: manifest.files.len() as u64,
        bytes,
    })
}

/// A `Read` over chunks arriving from a socket.
///
/// The mirror of [`Chunks`]: `tar` reads synchronously, the body arrives
/// asynchronously, and this is the one place the two meet. `blocking_recv` is
/// on a blocking task for the same reason the send is.
struct Incoming {
    rx: mpsc::Receiver<Bytes>,
    current: Bytes,
}

impl Incoming {
    fn new(rx: mpsc::Receiver<Bytes>) -> Self {
        Self {
            rx,
            current: Bytes::new(),
        }
    }
}

impl Read for Incoming {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.current.is_empty() {
            match self.rx.blocking_recv() {
                Some(next) => self.current = next,
                // Whoever was feeding this has stopped. End of stream: `tar`
                // reports a short archive, and a bundle with no manifest is
                // refused.
                None => return Ok(0),
            }
        }
        let take = self.current.len().min(buf.len());
        buf[..take].copy_from_slice(&self.current.split_to(take));
        Ok(take)
    }
}

#[cfg(test)]
mod tests {
    //! Two scratch roots, one per machine: a server is bundled out of one and
    //! received into the other, through the same streams the network carries.
    //! Nothing here opens a socket.

    use super::*;
    use crate::paths::Scratch;
    use crate::server::{Server, ServerKind};

    /// A server with one file in each of the four things a transfer carries,
    /// plus an empty folder that is part of the server too.
    async fn a_server(paths: &Paths, id: &str) -> Server {
        let mut server = Server::new(format!("Server {id}"), ServerKind::Paper, "1.21.1".into());
        server.id = id.to_string();
        let run = paths.server_run(id);
        for dir in [
            run.join("world"),
            run.join("plugins"),
            run.join("client-mods"),
            paths.server_media(id),
            paths.server_backups(id),
        ] {
            tokio::fs::create_dir_all(&dir).await.unwrap();
        }
        tokio::fs::write(run.join("world/level.dat"), b"a world").await.unwrap();
        tokio::fs::write(run.join("server.properties"), b"level-name=world\n").await.unwrap();
        tokio::fs::write(run.join("plugins/thing.jar"), vec![7u8; 4096]).await.unwrap();
        tokio::fs::write(paths.server_media(id).join("icon.png"), b"an icon").await.unwrap();
        tokio::fs::write(
            paths.server_backups(id).join("2026-01-01T00-00-00.zip"),
            vec![1u8; 2048],
        )
        .await
        .unwrap();
        server.save(paths).await.unwrap();
        server
    }

    /// Every chunk a bundle of `id` is made of.
    async fn bundled(paths: &Paths, id: &str, mode: Mode) -> Vec<std::io::Result<Bytes>> {
        bundle(paths, id, mode, Progress::silent("t"))
            .collect::<Vec<_>>()
            .await
    }

    fn joined(chunks: &[std::io::Result<Bytes>]) -> Vec<u8> {
        let mut whole = Vec::new();
        for chunk in chunks {
            whole.extend_from_slice(chunk.as_ref().expect("the bundle was written"));
        }
        whole
    }

    fn as_stream(bytes: Vec<u8>) -> impl Stream<Item = std::io::Result<Bytes>> + Send + Unpin {
        Box::pin(futures::stream::iter(vec![Ok(Bytes::from(bytes))]))
    }

    async fn received(paths: &Paths, id: &str, bytes: Vec<u8>) -> Result<Arrived> {
        receive(paths, id, as_stream(bytes), 0, &Progress::silent("t")).await
    }

    /// `src/lib/types.ts` spells these two words by hand, and a command whose
    /// argument will not deserialise fails at the IPC boundary with nothing to
    /// read. Pinned here because nothing else holds the two files together.
    #[test]
    fn a_mode_is_the_word_the_frontend_sends() {
        assert_eq!(serde_json::to_string(&Mode::Copy).unwrap(), "\"copy\"");
        assert_eq!(serde_json::to_string(&Mode::Move).unwrap(), "\"move\"");
        assert_eq!(
            serde_json::from_str::<Mode>("\"move\"").unwrap(),
            Mode::Move
        );
        let types = include_str!("../../../src/lib/types.ts");
        assert!(
            types.contains(r#"export type TransferMode = "copy" | "move";"#),
            "types.ts no longer spells the two words this enum serialises to"
        );
    }

    #[tokio::test]
    async fn a_copy_arrives_file_for_file_and_leaves_the_original_alone() {
        let here = Scratch::new("transfer-copy-from");
        let there = Scratch::new("transfer-copy-to");
        a_server(&here.paths, "survival").await;

        let bundle = joined(&bundled(&here.paths, "survival", Mode::Copy).await);
        let arrived = received(&there.paths, "survival", bundle).await.unwrap();

        assert_eq!(arrived.files, 6, "server.json, four files and a backup");
        for file in [
            "server.json",
            "run/world/level.dat",
            "run/server.properties",
            "run/plugins/thing.jar",
            "media/icon.png",
            "backups/2026-01-01T00-00-00.zip",
        ] {
            let sent = tokio::fs::read(here.paths.server("survival").join(file)).await.unwrap();
            let got = tokio::fs::read(there.paths.server("survival").join(file)).await.unwrap();
            assert_eq!(sent, got, "{file} did not arrive intact");
        }
        // An empty folder is part of the server: `run/client-mods/` is
        // deliberately a folder no loader reads, and it must still be there.
        assert!(there.paths.server_run("survival").join("client-mods").is_dir());
        // And the original is exactly where it was: a copy takes nothing away.
        assert!(here.paths.server_run("survival").join("world/level.dat").exists());
    }

    /// A world kept somewhere else on the machine is still the server's world.
    ///
    /// An admin with a big disk for worlds has a link in the server's folder,
    /// and a bundle that quietly leaves out what it points at is a bundle a
    /// move deletes the only copy of the world for — the host then generating
    /// a brand new one on its first start, and the app showing no server at
    /// all. So links are followed and what they point at travels.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_world_kept_on_another_disk_travels_with_the_server() {
        let here = Scratch::new("transfer-link-from");
        let there = Scratch::new("transfer-link-to");
        let disk = Scratch::new("transfer-link-disk");
        a_server(&here.paths, "survival").await;

        // The world on another disk, and one of Minecraft's own player files
        // kept outside the server too.
        let world = disk.paths.root.join("survival-world");
        tokio::fs::create_dir_all(world.join("region")).await.unwrap();
        tokio::fs::write(world.join("region/r.0.0.mca"), vec![9u8; 512]).await.unwrap();
        let run = here.paths.server_run("survival");
        tokio::fs::remove_dir_all(run.join("world")).await.unwrap();
        std::os::unix::fs::symlink(&world, run.join("world")).unwrap();
        let ops = disk.paths.root.join("ops.json");
        tokio::fs::write(&ops, b"[]").await.unwrap();
        std::os::unix::fs::symlink(&ops, run.join("ops.json")).unwrap();

        let measured = measure(&here.paths, "survival", Mode::Copy).await.unwrap();
        let bundle = joined(&bundled(&here.paths, "survival", Mode::Copy).await);
        let arrived = received(&there.paths, "survival", bundle).await.unwrap();

        assert_eq!(arrived.files, measured.files, "the dialog's figure is what arrived");
        assert_eq!(
            tokio::fs::read(there.paths.server_run("survival").join("world/region/r.0.0.mca"))
                .await
                .unwrap(),
            vec![9u8; 512],
            "the world the link pointed at did not travel"
        );
        assert_eq!(
            tokio::fs::read(there.paths.server_run("survival").join("ops.json")).await.unwrap(),
            b"[]",
            "a linked file did not travel either"
        );
    }

    /// A server that holds one of its own folders by another name. Links are
    /// followed, so without a guard this walk never ends.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_server_holding_a_link_to_one_of_its_own_folders_is_refused() {
        let here = Scratch::new("transfer-loop");
        a_server(&here.paths, "survival").await;
        let run = here.paths.server_run("survival");
        std::os::unix::fs::symlink(&run, run.join("world/back")).unwrap();

        let refused = measure(&here.paths, "survival", Mode::Copy)
            .await
            .expect_err("a folder that holds itself");
        assert!(refused.to_string().contains("without end"), "{refused}");
    }

    /// Two transfers of one server at once, which is an Escape key and a
    /// second click away. Without a claim the second clears the first's
    /// staging directory, and the first — whose digests are its own record of
    /// what it wrote rather than a reading of the disk — then verifies them
    /// and commits what the second has written so far as a proved server.
    #[tokio::test]
    async fn a_second_transfer_of_one_server_is_refused_rather_than_written_over() {
        let here = Scratch::new("transfer-twice-from");
        let there = Scratch::new("transfer-twice-to");
        a_server(&here.paths, "survival").await;
        let bundle = joined(&bundled(&here.paths, "survival", Mode::Copy).await);

        // A transfer in flight, holding the one thing it holds: its staging
        // directory, with the part of the world it has written so far in it.
        let staging = there.paths.transfer("survival");
        tokio::fs::create_dir_all(&staging).await.unwrap();
        tokio::fs::write(staging.join("part-of-a-world"), b"half").await.unwrap();
        let held = Arriving::take(&staging).expect("nothing else holds it");

        let refused = received(&there.paths, "survival", bundle.clone())
            .await
            .expect_err("one transfer into one place at a time");
        assert!(refused.to_string().contains("already arriving"), "{refused}");
        assert!(
            staging.join("part-of-a-world").exists(),
            "the second transfer cleared the staging directory of the first"
        );
        assert!(!there.paths.server("survival").exists());

        // And once the first is over, the next one is taken.
        drop(held);
        received(&there.paths, "survival", bundle).await.unwrap();
        assert!(there.paths.server_run("survival").join("world/level.dat").exists());
    }

    /// A file the server wrote after the plan was taken. The manifest never
    /// names it, so the receiver verifies a bundle that is missing it and a
    /// move deletes the only copy that has it.
    #[tokio::test]
    async fn a_file_written_after_the_plan_refuses_the_whole_bundle() {
        let here = Scratch::new("transfer-wrote-during");
        a_server(&here.paths, "survival").await;
        let region = here.paths.server_run("survival").join("world/r.3.-2.mca");

        let refused = write_bundle(
            &here.paths.server("survival"),
            "survival",
            Mode::Copy,
            WroteDuring {
                during: Some(|| std::fs::write(&region, b"a region").unwrap()),
            },
            &AtomicBool::new(false),
        )
        .expect_err("a region file appeared while the server was being sent");
        assert!(refused.to_string().contains("r.3.-2.mca"), "{refused}");
        assert!(
            refused.to_string().contains("written to while it was being sent"),
            "it should say what happened: {refused}"
        );
    }

    /// The same accident in the shape the per-file length check cannot see: a
    /// region file rewritten in place at the same sector count, which is what
    /// a running server does to one.
    #[tokio::test]
    async fn a_file_rewritten_in_place_at_the_same_length_is_caught_too() {
        let here = Scratch::new("transfer-rewritten");
        a_server(&here.paths, "survival").await;
        let jar = here.paths.server_run("survival").join("plugins/thing.jar");

        let refused = write_bundle(
            &here.paths.server("survival"),
            "survival",
            Mode::Copy,
            WroteDuring {
                during: Some(|| {
                    std::fs::write(&jar, vec![8u8; 4096]).unwrap();
                    // Said rather than left to the filesystem's own clock, so
                    // this holds on a filesystem whose timestamps are coarse.
                    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
                    std::fs::File::options()
                        .write(true)
                        .open(&jar)
                        .unwrap()
                        .set_modified(later)
                        .unwrap();
                }),
            },
            &AtomicBool::new(false),
        )
        .expect_err("the file was replaced while the server was being sent");
        assert!(refused.to_string().contains("thing.jar"), "{refused}");
    }

    /// A sink that writes to the server the first time the bundle writes
    /// anything, which is a server started on the host ten minutes into a
    /// forty-minute pull. `is_running` was asked once, before any of this.
    struct WroteDuring<F: FnOnce()> {
        during: Option<F>,
    }

    impl<F: FnOnce()> Write for WroteDuring<F> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Some(during) = self.during.take() {
                during();
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// What proves a transfer complete is the whole authority for deleting the
    /// only other copy of the server, so "complete" has to mean the disk and
    /// not the page cache: a host that answers 201 and loses power ninety
    /// seconds later must still have the world.
    ///
    /// Held by reading this file, because an `fsync` is invisible from a test
    /// — nothing short of cutting the power observes one — and in the same
    /// spirit as `a_mode_is_the_word_the_frontend_sends` above: the only place
    /// the rule can be held is where it would be deleted from.
    #[test]
    fn what_arrives_is_on_the_disk_before_it_is_called_complete() {
        let code = include_str!("transfer.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the code above these tests");
        assert!(
            code.contains("file.sync_all()"),
            "unpack no longer syncs the files it writes, so a verified arrival is only a \
             page-cache arrival"
        );
        assert!(
            !code.contains("file.flush()"),
            "a flush is not what makes an arriving file durable"
        );
        // Between the rename and what `land` returns, rather than anywhere
        // after it: the rest of this file mentions `sync_dir` for other
        // reasons, and an assertion that cannot fail is not one.
        let promoted = code
            .split("tokio::fs::rename(staging")
            .nth(1)
            .and_then(|rest| rest.split("Ok(arrived)").next())
            .expect("the rename that promotes the staging directory");
        assert!(
            promoted.contains("sync_dir"),
            "the rename that makes the arrived server exist is not itself made durable"
        );
    }

    /// A host restarted mid-upload drops the request where it stands, and the
    /// staging directory it was filling is then read by nothing and removed by
    /// nothing — so a few restarts fill the disk the host runs its servers
    /// from, which is the thing the ceiling was there to bound.
    #[tokio::test]
    async fn staging_left_by_a_restart_is_swept_when_a_host_starts() {
        let there = Scratch::new("transfer-sweep");
        let abandoned = there.paths.transfer("survival");
        tokio::fs::create_dir_all(abandoned.join("run/world")).await.unwrap();
        tokio::fs::write(abandoned.join("run/world/level.dat"), vec![0u8; 1024])
            .await
            .unwrap();
        a_server(&there.paths, "creative").await;

        sweep(&there.paths).await;

        assert!(
            !abandoned.exists(),
            "an upload interrupted by a restart is still taking up the disk"
        );
        assert!(
            there.paths.server_run("creative").join("world/level.dat").exists(),
            "a sweep must touch nothing but the staging directories"
        );
    }

    #[tokio::test]
    async fn a_copy_carries_no_sharing_and_a_move_carries_it_stopped() {
        let here = Scratch::new("transfer-share-from");
        let copy = Scratch::new("transfer-share-copy");
        let moved = Scratch::new("transfer-share-move");
        a_server(&here.paths, "survival").await;
        crate::server::share::start(&here.paths, "survival").await.unwrap();
        crate::server::share::save_link_address(&here.paths, "survival", "box.example:25566")
            .await
            .unwrap();
        let password = crate::server::share::load(&here.paths, "survival")
            .await
            .unwrap()
            .expect("it is shared")
            .code;

        let bundle = joined(&bundled(&here.paths, "survival", Mode::Copy).await);
        received(&copy.paths, "survival", bundle).await.unwrap();
        assert!(
            crate::server::share::load(&copy.paths, "survival").await.unwrap().is_none(),
            "a copy must arrive with no password at all — two machines serving one pack \
             under one password is two servers players cannot tell apart"
        );

        let bundle = joined(&bundled(&here.paths, "survival", Mode::Move).await);
        received(&moved.paths, "survival", bundle).await.unwrap();
        let share = crate::server::share::load(&moved.paths, "survival")
            .await
            .unwrap()
            .expect("a move carries the password");
        assert_eq!(share.code, password, "the links players hold still name it");
        assert!(!share.shared, "a moved server must not arrive serving");
        assert!(
            !crate::server::share::path(&moved.paths, "survival").exists(),
            "it is the stopped file that arrives, never share.json"
        );
        assert_eq!(
            crate::server::share::link_address(&moved.paths, "survival").await.as_deref(),
            Some("box.example:25566"),
            "the address the links were built from travels, so the Share tab can say it has moved on"
        );
    }

    #[tokio::test]
    async fn a_transfer_onto_a_server_that_is_already_there_is_refused() {
        let here = Scratch::new("transfer-clash-from");
        let there = Scratch::new("transfer-clash-to");
        a_server(&here.paths, "survival").await;
        a_server(&there.paths, "survival").await;
        tokio::fs::write(there.paths.server_run("survival").join("world/level.dat"), b"theirs")
            .await
            .unwrap();

        let bundle = joined(&bundled(&here.paths, "survival", Mode::Copy).await);
        let refused = received(&there.paths, "survival", bundle)
            .await
            .expect_err("the id is taken");
        assert!(
            refused.to_string().contains("already there"),
            "the refusal should say what to do about it: {refused}"
        );
        // Refused without writing anything, and without touching what was there.
        assert!(!there.paths.transfer("survival").exists());
        assert_eq!(
            tokio::fs::read(there.paths.server_run("survival").join("world/level.dat")).await.unwrap(),
            b"theirs"
        );
    }

    #[tokio::test]
    async fn a_file_that_arrives_changed_is_caught_and_nothing_is_kept() {
        let here = Scratch::new("transfer-digest-from");
        let there = Scratch::new("transfer-digest-to");
        a_server(&here.paths, "survival").await;

        // One byte of the world, flipped on the wire. The bundle is otherwise
        // whole — every file is there and so is the manifest — so only the
        // content disagrees, which is exactly what a byte count waves through.
        let mut bundle = joined(&bundled(&here.paths, "survival", Mode::Copy).await);
        let at = find(&bundle, b"a world").expect("the world is in the bundle");
        bundle[at] = b'A';

        let refused = received(&there.paths, "survival", bundle)
            .await
            .expect_err("the digest does not match");
        assert!(
            refused.to_string().contains("level.dat"),
            "it should name the file that did not match: {refused}"
        );
        assert!(!there.paths.server("survival").exists(), "no half server is left behind");
        assert!(!there.paths.transfer("survival").exists(), "and no staging either");
    }

    /// A bundle that is a complete, well-formed archive and simply carries no
    /// list of files. Nothing vouches for what arrived, so nothing is kept.
    ///
    /// The other half of the test below it, and a separate one on purpose: a
    /// bundle cut off mid-file is caught by the archive itself running out,
    /// and this is the case where it does not — which is the whole reason the
    /// manifest is the last entry rather than the first.
    #[tokio::test]
    async fn a_bundle_with_no_list_of_files_is_refused_even_when_it_is_whole() {
        let here = Scratch::new("transfer-nolist-from");
        let there = Scratch::new("transfer-nolist-to");
        a_server(&here.paths, "survival").await;

        let whole = joined(&bundled(&here.paths, "survival", Mode::Copy).await);
        let refused = received(&there.paths, "survival", without_the_manifest(&whole))
            .await
            .expect_err("nothing vouches for what arrived");
        assert!(
            refused.to_string().contains("before it was complete"),
            "{refused}"
        );
        assert!(!there.paths.server("survival").exists());
        assert!(!there.paths.transfer("survival").exists());
        assert!(here.paths.server_run("survival").join("world/level.dat").exists());
    }

    /// Re-pack a bundle without its manifest, leaving a perfectly good archive.
    fn without_the_manifest(whole: &[u8]) -> Vec<u8> {
        let mut out = tar::Builder::new(Vec::new());
        let mut archive = tar::Archive::new(whole);
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_path_buf();
            if path.to_string_lossy() == MANIFEST_NAME {
                continue;
            }
            let kind = entry.header().entry_type();
            let mut body = Vec::new();
            entry.read_to_end(&mut body).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_size(body.len() as u64);
            out.append_data(&mut header, &path, &body[..]).unwrap();
        }
        out.into_inner().unwrap()
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[tokio::test]
    async fn a_transfer_cut_short_leaves_nothing_to_commit() {
        let here = Scratch::new("transfer-cut-from");
        let there = Scratch::new("transfer-cut-to");
        a_server(&here.paths, "survival").await;

        // Three quarters of it: files, and no manifest.
        let whole = joined(&bundled(&here.paths, "survival", Mode::Copy).await);
        let cut = whole[..whole.len() * 3 / 4].to_vec();

        let refused = received(&there.paths, "survival", cut)
            .await
            .expect_err("a bundle with no manifest proves nothing");
        assert!(
            refused.to_string().contains("before it was complete"),
            "it should say the transfer did not finish: {refused}"
        );
        assert!(!there.paths.server("survival").exists());
        assert!(!there.paths.transfer("survival").exists());
        // And the source is untouched, which is the whole point of the order.
        assert!(here.paths.server_run("survival").join("world/level.dat").exists());
    }

    #[tokio::test]
    async fn a_bundle_cannot_write_outside_the_server_it_is_building() {
        let there = Scratch::new("transfer-escape");
        tokio::fs::create_dir_all(there.paths.servers()).await.unwrap();

        for name in ["../escaped.json", "launch/evil.json", "share.json"] {
            let mut out = tar::Builder::new(Vec::new());
            let body = b"no";
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_size(body.len() as u64);
            // The name goes into the header by hand: `tar` refuses to *write*
            // a path holding `..`, and a bundle arriving off another machine
            // was not necessarily written by `tar`. What is being proved here
            // is this module's guard, not that crate's.
            let bytes = name.as_bytes();
            header.as_gnu_mut().unwrap().name[..bytes.len()].copy_from_slice(bytes);
            header.set_cksum();
            out.append(&header, &body[..]).unwrap();

            let refused = received(&there.paths, "survival", out.into_inner().unwrap())
                .await
                .expect_err("nothing outside a server may be written");
            assert!(
                refused.to_string().contains("not part of a server"),
                "{name} should be refused as not part of a server: {refused}"
            );
            assert!(!there.paths.root.join("escaped.json").exists());
            assert!(!there.paths.server("survival").exists());
        }
    }

    #[tokio::test]
    async fn the_measurement_says_how_much_of_it_is_backups() {
        let here = Scratch::new("transfer-measure-from");
        let there = Scratch::new("transfer-measure-to");
        a_server(&here.paths, "survival").await;

        let measured = measure(&here.paths, "survival", Mode::Copy).await.unwrap();
        assert_eq!(measured.files, 6);
        assert_eq!(measured.backup_files, 1);
        assert_eq!(measured.backup_bytes, 2048);
        assert!(
            measured.bytes > measured.backup_bytes,
            "the total covers more than the backups"
        );

        // And what it said matches what a transfer actually carried, so the
        // figure the person decides on is the figure they get.
        let bundle = joined(&bundled(&here.paths, "survival", Mode::Copy).await);
        let arrived = received(&there.paths, "survival", bundle).await.unwrap();
        assert_eq!(arrived.files, measured.files);
        assert_eq!(arrived.bytes, measured.bytes);
    }

    #[tokio::test]
    async fn a_bundle_past_the_ceiling_is_refused_rather_than_written() {
        let there = Scratch::new("transfer-ceiling");
        tokio::fs::create_dir_all(there.paths.servers()).await.unwrap();

        let refused = receive(
            &there.paths,
            "survival",
            as_stream(Vec::new()),
            MAX_BUNDLE_BYTES + 1,
            &Progress::silent("t"),
        )
        .await
        .expect_err("more than a host will take");
        assert!(
            refused.to_string().contains("larger than a transfer may be"),
            "{refused}"
        );
        assert!(
            !there.paths.transfer("survival").exists(),
            "nothing should have been created to hold it"
        );
    }

    #[tokio::test]
    async fn a_cancelled_bundle_stops_sending_and_never_writes_a_manifest() {
        let here = Scratch::new("transfer-cancel-from");
        let there = Scratch::new("transfer-cancel-to");
        a_server(&here.paths, "survival").await;

        let progress = Progress::silent("t");
        progress.cancel_flag().store(true, Ordering::Relaxed);
        let chunks = bundle(&here.paths, "survival", Mode::Copy, progress)
            .collect::<Vec<_>>()
            .await;
        assert!(
            chunks.iter().any(|chunk| chunk.is_err()),
            "a cancelled bundle must fail rather than quietly end"
        );

        let sent: Vec<u8> = chunks
            .iter()
            .filter_map(|chunk| chunk.as_ref().ok())
            .flat_map(|bytes| bytes.to_vec())
            .collect();
        let refused = received(&there.paths, "survival", sent)
            .await
            .expect_err("what was sent before the cancel is not a server");
        assert!(!there.paths.server("survival").exists(), "{refused}");
        assert!(!there.paths.transfer("survival").exists());
        assert!(here.paths.server_run("survival").join("world/level.dat").exists());
    }
}

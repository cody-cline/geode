//! The recipient's side, held against packs built to hurt it.
//!
//! Every other test of the sharing code is written from the sender's side: an
//! admin's Geode builds a pack, and the assertions are about what it carried.
//! This file starts from the other end. Nothing here was written by
//! `export_server` or by `export` — each archive is assembled byte by byte in
//! the test, with one thing wrong with it on purpose, and the question is only
//! ever what `share::pack::import` and `share::pack::update` do when they are
//! handed it.
//!
//! That matters because the recipient's side is the side with no author. A
//! `geode://join` pack comes from a machine the player was given an address
//! for and nothing else; a `geode://pack?url=…` archive comes from wherever
//! its sharer put it; a `.mrpack` comes from wherever somebody downloaded it.
//! `share/host.rs`'s own tests cover the listener strangers reach.
//! `live_share_join.rs` covers a *well-built* pack arriving over the real
//! wire. Neither asks what happens when the archive itself is the attack.
//!
//! Every test is `#[ignore]`d: each writes megabytes to a scratch
//! `GEODE_HOME`, several bind a real port on loopback, and one assembles a
//! hundred and twenty thousand zip entries.
//!
//! ```sh
//! cargo test --test live_pack_hostile -- --ignored --nocapture
//!
//! # the crowded-archive one on its own; it is the slow test here
//! cargo test --test live_pack_hostile a_pack_may_list -- --ignored --nocapture
//! ```
//!
//! **No real API is reached.** Every download a test needs is served from a
//! `TcpListener` this file binds on `127.0.0.1`, and the client the imports
//! are given ([`only_loopback`]) sends anything that is not loopback to a dead
//! port — so the pass that asks Modrinth what arrived cannot leave the
//! machine, and a test that passed because an address was unreachable rather
//! than because a guard fired would say so.
//!
//! **Four tests pin a hole rather than a guard.** They are named `…_today`
//! and each carries a comment saying what the assertion should become when the
//! hole is closed. A test asserting the behaviour somebody wants would fail on
//! every run until then, which is a broken suite rather than a finding; a test
//! asserting what the code does now fails the day it changes, which is the
//! notice that is actually wanted. Two more — the zstd entry and the entry
//! named like a folder — pin a *wording* that is wrong rather than a
//! behaviour, and say so in place rather than in their names.
//!
//! What a green run does **not** prove:
//!
//! - **Windows or macOS.** A reserved device name, a trailing dot and a
//!   case-insensitive collision are all ordinary file names on the platform
//!   this runs on, so [`odd_file_names_still_land_inside_the_instance`] proves
//!   containment and nothing about what those names do on the two platforms
//!   where they mean something.
//! - **The arrival.** Nothing here speaks TLS or goes through `share::join`.
//!   Each archive is placed on disk where a download would have left it. What
//!   it takes to *get* a hostile archive that far — a hostile host, or a
//!   `geode://pack` link whose SHA-256 its author chose — is
//!   `live_share_join.rs`'s and `share::fetch`'s.
//! - **A CurseForge reference resolving.** `share::pack::import` and
//!   `share::pack::update` hardcode the real CurseForge API as the base they
//!   resolve against, so no listener here can answer a reference and nothing in
//!   this file reaches one. Everything a note's `source` can be made to do
//!   short of a lookup is already pinned offline — see the notes at the foot of
//!   this file.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use geode_lib::instance;
use geode_lib::meta::Loader;
use geode_lib::net::Progress;
use geode_lib::paths::Paths;
use geode_lib::share::manifest::Kind;
use geode_lib::share::pack;

// ---------------------------------------------------------------- the machine

/// A `GEODE_HOME` of its own under the system temporary directory, removed
/// when the test ends however it ends.
///
/// `paths::Scratch` is the crate's own version of this and is `#[cfg(test)]`,
/// so it exists for unit tests and not for anything under `tests/`. Copied
/// here for the same reason `live_share_host.rs` copies it, with the same
/// guard: these tests write files that a missing path check would place
/// *outside* the directory under test, so the one thing that must never be
/// true is that the directory under test is the user's own.
struct Scratch {
    paths: Paths,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("geode-live-hostile-{tag}-{}", uuid::Uuid::new_v4()));
        let real = Paths::new().root;
        assert!(
            !root.starts_with(&real),
            "the scratch home {} is inside the real one at {}",
            root.display(),
            real.display()
        );
        std::fs::create_dir_all(&root).unwrap();
        Self {
            paths: Paths { root },
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.paths.root);
    }
}

/// A client that can reach loopback and nothing else.
///
/// Built from the same knobs as `net::client` — which is what
/// `commands::import_modpack` hands the importer — rather than from
/// `net::client` itself, because what is added is a proxy: every address that
/// is not loopback goes to port 9, which discards. So the listeners below are
/// reached directly and the pass that asks Modrinth about what arrived
/// (`install::content::name_what_arrived`, which an import runs on success)
/// cannot leave the machine.
///
/// Not a weakened client. A real one would reach the loopback addresses these
/// packs name too; that is the whole of what
/// [`only_a_server_pack_is_held_to_modrinth_so_a_link_pack_reaches_any_host_today`]
/// is about.
fn only_loopback() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(geode_lib::net::USER_AGENT)
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5))
        .proxy(reqwest::Proxy::custom(|url| {
            let loopback = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
            if loopback {
                None
            } else {
                Some("http://127.0.0.1:9".to_string())
            }
        }))
        .build()
        .expect("http client")
}

/// A fresh instance on disk, the way `commands::install_joined` makes one.
async fn fresh(paths: &Paths, name: &str) -> instance::Instance {
    instance::create(
        paths,
        name.to_string(),
        "1.21.1".into(),
        Loader::Fabric,
        "0.16.9".into(),
    )
    .await
    .expect("the instance is created")
}

/// Every regular file under `dir`, as paths relative to it.
fn files_under(dir: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // `symlink_metadata`, so a link is a leaf rather than something to
            // walk through — the point of several tests below is where a link
            // points.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(dir) {
                found.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    found
}

fn sha1_of(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

// ----------------------------------------------------------- the archives

/// One entry to put in an archive, as hostile as it needs to be.
struct Entry {
    name: String,
    body: Vec<u8>,
    /// Set to a unix symlink mode (`0o120777`) to make the entry *claim* to be
    /// a link — which is a thing a zip can say and a thing a real archiver
    /// writes.
    mode: Option<u32>,
}

impl Entry {
    fn at(name: &str, body: impl Into<Vec<u8>>) -> Self {
        Entry {
            name: name.to_string(),
            body: body.into(),
            mode: None,
        }
    }

    fn saying_it_is_a_symlink(mut self) -> Self {
        self.mode = Some(0o120777);
        self
    }
}

/// Write an archive holding exactly these entries, in exactly this order,
/// duplicates and all.
///
/// Stored rather than deflated: nothing here is about compression, and a
/// stored entry is one whose bytes can be found and patched afterwards — see
/// [`pretend_an_entry_was_written_with_zstd`].
fn write_archive(dest: &Path, entries: &[Entry]) {
    let file = std::fs::File::create(dest).expect("the archive is created");
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    for entry in entries {
        let mut options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        if let Some(mode) = entry.mode {
            options = options.unix_permissions(mode);
        }
        zip.start_file(entry.name.clone(), options).unwrap();
        zip.write_all(&entry.body).unwrap();
    }
    zip.finish().unwrap();
}

/// A `modrinth.index.json`, with whatever files are given.
fn index(files: Vec<serde_json::Value>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "1.0.0",
        "name": "Hostile",
        "files": files,
        "dependencies": { "minecraft": "1.21.1", "fabric-loader": "0.16.9" },
    }))
    .unwrap()
}

/// One index entry: where it lands, where it is fetched from, and what it is
/// claimed to hash to and to weigh.
fn indexed(
    path: &str,
    downloads: Vec<String>,
    sha1: Option<&str>,
    size: Option<u64>,
) -> serde_json::Value {
    let mut hashes = serde_json::Map::new();
    if let Some(sha1) = sha1 {
        hashes.insert("sha1".into(), sha1.into());
    }
    let mut file = serde_json::Map::new();
    file.insert("path".into(), path.into());
    file.insert("hashes".into(), hashes.into());
    file.insert("downloads".into(), downloads.into());
    if let Some(size) = size {
        file.insert("fileSize".into(), size.into());
    }
    file.into()
}

/// A Geode manifest, written as JSON rather than through `Manifest` so a test
/// can say things the struct's own constructors never would.
fn manifest(kind: &str, extra: serde_json::Value) -> Vec<u8> {
    let mut out = serde_json::json!({
        "geodeFormat": 1,
        "kind": kind,
        "profile": { "name": "Hostile" },
    });
    let (Some(out_map), Some(extra)) = (out.as_object_mut(), extra.as_object()) else {
        unreachable!("both are objects");
    };
    for (key, value) in extra {
        out_map.insert(key.clone(), value.clone());
    }
    serde_json::to_vec(&out).unwrap()
}

/// `geode/manifest.json`, which is where the Geode half lives inside a pack.
const MANIFEST_PATH: &str = "geode/manifest.json";

/// How many times a name appears in an archive's raw bytes.
///
/// Two per entry in a well-formed zip — once in the local file header and once
/// in the central directory record — so four says the name is in the archive
/// twice. Counted on the bytes rather than through a reader on purpose: every
/// reader here is one of the things under test.
fn times_named(archive: &Path, name: &str) -> usize {
    let bytes = std::fs::read(archive).unwrap();
    let needle = name.as_bytes();
    (0..bytes.len().saturating_sub(needle.len()))
        .filter(|i| &bytes[*i..*i + needle.len()] == needle)
        .count()
}

/// Rename one entry in a finished archive, in both the local header and the
/// central directory record, to a name of exactly the same length.
///
/// The way to put the *same* name in an archive twice. `ZipWriter` refuses a
/// duplicate filename outright — `InvalidArchive("Duplicate filename")` — so a
/// pack with two `geode/manifest.json` entries cannot be written through it,
/// and a zip holding one is a thing other archivers write and the format
/// permits. Writing both entries under names that differ in one character and
/// then patching that character leaves everything else — sizes, CRCs, offsets,
/// the name length field — exactly as the writer laid it out.
///
/// Both names must be the same length, and `from` must appear nowhere in any
/// entry's bytes, which the callers below arrange.
fn rename_entry_to(archive: &Path, from: &str, to: &str) {
    assert_eq!(from.len(), to.len(), "a rename must not move any offset");
    let mut bytes = std::fs::read(archive).unwrap();
    let (needle, replacement) = (from.as_bytes(), to.as_bytes());
    let mut renamed = 0;
    for i in 0..bytes.len().saturating_sub(needle.len()) {
        if &bytes[i..i + needle.len()] == needle {
            bytes[i..i + needle.len()].copy_from_slice(replacement);
            renamed += 1;
        }
    }
    assert_eq!(
        renamed, 2,
        "expected one local header and one central record naming {from}, renamed {renamed}"
    );
    std::fs::write(archive, bytes).unwrap();
}

/// Rewrite one entry's compression method to zstd (93), which this build of
/// `zip` is not compiled to open.
///
/// The archive is written with that entry *stored*, then the two bytes naming
/// the method are patched in both the local header and the central directory
/// record. That is far more reliable than hand-assembling a zip: everything
/// else about the archive is whatever the `zip` crate writes, so the only
/// thing wrong with it is the one thing the test is about.
///
/// A local file header is 30 fixed bytes and then the name, with the method at
/// offset 8; a central directory record is 46 and then the name, with the
/// method at offset 10. Both are found by looking backwards from each
/// occurrence of the name for the record signature, and the count is asserted
/// so a `zip` release that lays either out differently fails here rather than
/// making the test pass for the wrong reason.
fn pretend_an_entry_was_written_with_zstd(archive: &Path, name: &str) {
    const ZSTD: [u8; 2] = [93, 0];
    let mut bytes = std::fs::read(archive).unwrap();
    let needle = name.as_bytes();
    let mut patched = 0;
    for i in 0..bytes.len().saturating_sub(needle.len()) {
        if &bytes[i..i + needle.len()] != needle {
            continue;
        }
        if i >= 30 && &bytes[i - 30..i - 26] == b"PK\x03\x04" {
            bytes[i - 30 + 8..i - 30 + 10].copy_from_slice(&ZSTD);
            patched += 1;
        } else if i >= 46 && &bytes[i - 46..i - 42] == b"PK\x01\x02" {
            bytes[i - 46 + 10..i - 46 + 12].copy_from_slice(&ZSTD);
            patched += 1;
        }
    }
    assert_eq!(
        patched, 2,
        "expected one local header and one central record naming {name}, patched {patched}"
    );
    std::fs::write(archive, bytes).unwrap();
}

// ---------------------------------------------------------------- the listener

/// A listener that answers only what it was handed, and remembers every path
/// it was asked for.
///
/// Plain HTTP on loopback, written by hand rather than through hyper: what the
/// tests need of it is one GET at a time and a truthful record of which
/// requests arrived, and the record is the assertion in half the tests here —
/// a guard that fires *before* a download is a guard that leaves no line in
/// it.
struct Serving {
    port: u16,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Serving {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Every path asked for, sorted and deduplicated. A failed download is
    /// retried three times, so the raw log counts the same path over and over.
    fn asked_for(&self) -> BTreeSet<String> {
        self.asked.lock().unwrap().iter().cloned().collect()
    }
}

/// Start a listener answering these `(path, status, body)` triples. Anything
/// else is a 404 — and is still recorded, which is what makes "the listener
/// was never asked" an assertion rather than an absence.
async fn serving(answers: Vec<(&str, u16, Vec<u8>)>) -> Serving {
    let answers: Arc<Vec<(String, u16, Vec<u8>)>> = Arc::new(
        answers
            .into_iter()
            .map(|(path, status, body)| (path.to_string(), status, body))
            .collect(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let asked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let answers = answers.clone();
            let log = log.clone();
            tokio::spawn(async move {
                // One request per connection: every reply says
                // `Connection: close` and the socket is dropped after it.
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte).await {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                    if head.len() > 8192 {
                        return;
                    }
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let target = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                log.lock().unwrap().push(target.clone());
                let (status, body) = answers
                    .iter()
                    .find(|(path, _, _)| *path == target)
                    .map(|(_, status, body)| (*status, body.clone()))
                    .unwrap_or((404, b"no".to_vec()));
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.flush().await;
            });
        }
    });
    Serving { port, asked }
}

// ------------------------------------------------------------------- the tests

/// Nothing a pack names lands outside the instance, whichever half of the pack
/// names it — and the guard fires before the download rather than after it.
///
/// Four shapes in one archive, because they are four expressions of one rule
/// and a pack can carry all four at once: an index entry climbing out with
/// `../`, an index entry naming an absolute path, an override entry climbing
/// out of its own tree, and an override entry naming an absolute path. The
/// absolute ones are pointed *inside the scratch home* on purpose. Aimed at
/// `/etc` they would prove nothing — the test has no permission there, so the
/// assertion would hold with every guard removed. Aimed somewhere this process
/// can write, a missing guard really does create the file. The test does not
/// take that on trust: it writes each of the four destinations itself first,
/// finds it, and removes it again, so a run where the escapes were impossible
/// rather than refused fails at that control instead of passing silently.
///
/// The listener is the other half of the proof. Every escaping index entry is
/// pointed at a URL of its own, and the assertion is that only the honest
/// file's URL was ever requested: `is_safe_relative` is checked while the
/// download list is built (`instance/modpack.rs`), so a pack naming `../` gets
/// no request at all, and a test that only looked at the filesystem could not
/// tell that from a download that failed.
///
/// Not covered by the offline suite. `a_world_that_climbs_out_of_its_folder_does_not_get_out`
/// (instance/modpack.rs) is the same `is_safe_relative` call with a world's
/// empty tree prefix, `a_file_name_that_climbs_out_of_the_instance_installs_nothing`
/// (tests/curseforge_fingerprints.rs) is the CurseForge arm, and
/// `an_unsafe_reference_is_reported_rather_than_skipped` (share/pack.rs) is the
/// Geode half's reference list. The Modrinth index arm has no test anywhere,
/// and no test anywhere checks that no request was made.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a loopback port and writes a scratch GEODE_HOME"]
async fn nothing_a_pack_names_lands_outside_the_instance() {
    let scratch = Scratch::new("escape");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let good = b"a real jar's bytes".to_vec();
    let host = serving(vec![
        ("/good.jar", 200, good.clone()),
        ("/climbing.jar", 200, b"climbed out".to_vec()),
        ("/absolute.jar", 200, b"wrote to an absolute path".to_vec()),
    ])
    .await;

    // Inside the scratch home and outside the instance: somewhere this process
    // really can write, so a missing guard leaves a file behind.
    let absolute_target = paths.root.join("absolute-escape.jar");
    let archive = paths.root.join("escape.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at(
                "modrinth.index.json",
                index(vec![
                    indexed(
                        "mods/good.jar",
                        vec![host.url("/good.jar")],
                        Some(&sha1_of(&good)),
                        Some(good.len() as u64),
                    ),
                    indexed(
                        "../../../index-climbed.jar",
                        vec![host.url("/climbing.jar")],
                        None,
                        None,
                    ),
                    indexed(
                        &absolute_target.to_string_lossy(),
                        vec![host.url("/absolute.jar")],
                        None,
                        None,
                    ),
                ]),
            ),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
            Entry::at("overrides/config/honest.toml", "kept = true"),
            Entry::at("overrides/../../../overrides-climbed.txt", "climbed out"),
            Entry::at(
                &format!("overrides/{}", paths.root.join("overrides-absolute.txt").display()),
                "wrote to an absolute path",
            ),
        ],
    );

    let mut inst = fresh(paths, "Escape").await;
    let game_dir = paths.game_dir(&inst.id);

    // A positive control, run before the import, so that every "nothing
    // escaped" assertion below is a statement about the guard rather than
    // about the filesystem.
    //
    // Each destination is built the way the extractor would build it: the two
    // climbing ones by joining `../../../…` onto the game directory, the two
    // absolute ones by joining an absolute path onto it, which in Rust
    // discards the left side entirely. Each is created, found, and removed
    // again. Aimed at a path this process could not write — `/etc` being the
    // obvious one — the sweep would pass with `is_safe_relative` deleted from
    // instance/modpack.rs, which is the way this kind of test proves nothing.
    for dest in [
        game_dir.join("../../../index-climbed.jar"),
        game_dir.join("../../../overrides-climbed.txt"),
        game_dir.join(&absolute_target),
        game_dir.join(paths.root.join("overrides-absolute.txt")),
    ] {
        std::fs::write(&dest, b"a control, not the pack's").unwrap_or_else(|e| {
            panic!(
                "this test cannot write {}, so it could not tell a guard from a \
                 permission: {e}",
                dest.display()
            )
        });
        assert!(dest.exists(), "{} was not created", dest.display());
        std::fs::remove_file(&dest).unwrap();
    }

    let missing = pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-escape"),
    )
    .await
    .expect("the honest half of the pack still imports");

    let landed = files_under(&game_dir);
    assert!(
        landed.contains("mods/good.jar"),
        "the honest file did not land: {landed:?}"
    );
    assert!(
        landed.contains("config/honest.toml"),
        "the honest override did not land: {landed:?}"
    );

    // Nothing anywhere in the scratch home outside the instance's own
    // directory. Stated as a whole-tree sweep rather than as four `exists()`
    // calls, so a guard that merely moved the escape somewhere else is caught
    // too.
    let everywhere = files_under(&paths.root);
    let outside: Vec<&String> = everywhere
        .iter()
        .filter(|rel| {
            let path = paths.root.join(rel);
            !path.starts_with(paths.instances()) && path != archive
        })
        .collect();
    assert!(
        outside.is_empty(),
        "a pack wrote outside the instances directory: {outside:?}"
    );
    for climbed in [
        "index-climbed.jar",
        "overrides-climbed.txt",
        "absolute-escape.jar",
        "overrides-absolute.txt",
    ] {
        assert!(
            !everywhere.iter().any(|rel| rel.ends_with(climbed)),
            "{climbed} exists somewhere under the scratch home: {everywhere:?}"
        );
    }

    // And the escaping entries were never requested, so the refusal happened
    // before the network rather than after it.
    assert_eq!(
        host.asked_for(),
        BTreeSet::from(["/good.jar".to_string()]),
        "an escaping index entry was fetched"
    );

    // Silently dropped rather than reported, which is worth pinning because it
    // is the opposite of what the Geode half does with the same shape
    // (`an_unsafe_reference_is_reported_rather_than_skipped`). If the index arm
    // ever gains a line of its own, this assertion is the one to change.
    assert_eq!(
        missing,
        Vec::<String>::new(),
        "the index arm has started reporting what it drops"
    );
}

/// A zip entry that claims to be a symlink arrives as a file holding the path
/// it claimed to point at, and never as a link.
///
/// This is the half of the symlink question a pack controls. An archive can
/// say an entry is a link — `unix_permissions(0o120777)` is exactly what `zip`
/// and `tar` write for one — and if the extractor honoured it, a pack could
/// put a link at `config/` or at `mods/x.jar` and have the *next* write go
/// wherever it pointed. `extract_trees` writes every entry's bytes with
/// `File::create`, so what lands is a regular file whose contents are the
/// target string, which is inert.
///
/// Asserted through `symlink_metadata`, which is the only call that can tell
/// the two apart: `metadata` on a link to a real file reports the file.
///
/// Nothing in the offline suite feeds a symlink entry to an import —
/// `grep -rn symlink src-tauri/src/instance/modpack.rs` finds nothing, and the
/// symlink tests in `share/pack.rs` are all on the admin's *read* side (what a
/// pack may be built from), not on where its bytes land.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a scratch GEODE_HOME"]
async fn a_symlink_entry_arrives_as_a_file_holding_its_target() {
    let scratch = Scratch::new("symentry");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    // Inside the scratch home, for the reason the escape test gives: a link
    // honoured at `/etc/passwd` would fail to be written anyway.
    let secret = paths.root.join("not-the-packs-business.txt");
    std::fs::write(&secret, b"the player's own bytes").unwrap();

    let archive = paths.root.join("symentry.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at("modrinth.index.json", index(vec![])),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
            Entry::at("overrides/config/pointer.toml", secret.to_string_lossy().as_bytes())
                .saying_it_is_a_symlink(),
        ],
    );

    let mut inst = fresh(paths, "Symlink Entry").await;
    pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-symentry"),
    )
    .await
    .expect("a pack full of link entries still imports; they are just files");

    let game_dir = paths.game_dir(&inst.id);
    let pointer = game_dir.join("config/pointer.toml");
    let meta = std::fs::symlink_metadata(&pointer).expect("the entry landed");
    assert!(
        !meta.file_type().is_symlink(),
        "a pack's entry became a real symlink at {}",
        pointer.display()
    );
    assert!(meta.is_file(), "{} is not a regular file", pointer.display());
    assert_eq!(
        std::fs::read(&pointer).unwrap(),
        secret.to_string_lossy().as_bytes(),
        "the target path should have landed as the file's own contents"
    );

    // The player's file is untouched, and the entry written "through" the link
    // entry landed inside the instance.
    assert_eq!(
        std::fs::read(&secret).unwrap(),
        b"the player's own bytes",
        "the pack wrote through its own link entry"
    );
    // Nothing else of the pack's landed anywhere but inside the instance.
    let everywhere = files_under(&paths.root);
    let outside: Vec<&String> = everywhere
        .iter()
        .filter(|rel| {
            let path = paths.root.join(rel);
            !path.starts_with(paths.instances()) && path != archive && path != secret
        })
        .collect();
    assert!(outside.is_empty(), "a link entry wrote outside: {outside:?}");
}

/// **A hole, pinned as it is.** A settings file the server sends is written
/// *through* a symlink the player made in their own game directory, so the
/// bytes land outside the instance.
///
/// Symlinking `config/` or a file under it to somewhere shared — another
/// launcher's instance, a dotfiles repo, a synced folder — is ordinary
/// Minecraft practice, and Geode hands the player the folder to do it in
/// (`InstanceView`'s reveal button). `extract_trees` checks the archive
/// *entry's* path with `is_safe_relative` and then opens the destination with
/// `File::create`, which follows an existing link and truncates its target;
/// nothing in `instance/modpack.rs` calls `symlink_metadata` at all. With
/// `Policy::Enforce` — which is what a path nobody chose a behaviour for gets
/// — the sender's copy is written on every update, so this is not a one-off.
///
/// Contrast with `settings::splice_file`, which *does* call `symlink_metadata`
/// and refuses, pinned offline by `a_setting_is_not_spliced_through_a_link_and_is_reported`
/// (share/pack.rs). That test cannot reach this code: its path is kept in
/// sync, so the extraction skips the entry entirely. The two writers disagree
/// and only one of them is checked.
///
/// **When this is fixed** the assertion to keep is the one marked below: the
/// outside file should still hold the player's own bytes, and the server's copy
/// should either land on a fresh file or be reported as one that could not be
/// written. Flip it then; do not delete it.
///
/// Unix only, because making the link needs `std::os::unix::fs::symlink` and
/// the whole file has to go on compiling on the two platforms CI builds and
/// nobody runs. The same hole exists on Windows — `File::create` follows a
/// directory junction and a developer-mode symlink alike — but a test for it
/// would need `std::os::windows::fs`, and writing one against a platform
/// nobody here can run it on is how a test that proves nothing gets written.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a scratch GEODE_HOME"]
async fn a_config_file_is_written_through_a_symlink_the_player_made_today() {
    let scratch = Scratch::new("symdisk");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let shared = paths.root.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let outside = shared.join("mine.toml");
    std::fs::write(&outside, b"the player's own setting").unwrap();

    let archive = paths.root.join("symdisk.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at("modrinth.index.json", index(vec![])),
            // A server's pack, because that is the case that recurs: a joined
            // instance extracts one on every Play the revision moved.
            Entry::at(MANIFEST_PATH, manifest("server", serde_json::json!({}))),
            Entry::at("overrides/config/mine.toml", "the admin's setting"),
        ],
    );

    let mut inst = fresh(paths, "Through A Link").await;
    let game_dir = paths.game_dir(&inst.id);
    let inside = game_dir.join("config/mine.toml");
    std::os::unix::fs::symlink(&outside, &inside).expect("the player links their own config");

    pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-symdisk"),
    )
    .await
    .expect("the import finishes");

    assert!(
        std::fs::symlink_metadata(&inside)
            .expect("the player's link is still there")
            .file_type()
            .is_symlink(),
        "the link was replaced rather than written through, which is the fix"
    );

    // THE HOLE. When a `symlink_metadata` check lands in `extract_trees`, this
    // becomes `assert_eq!(.., b"the player's own setting")` and the test keeps
    // its name minus the `_today`.
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"the admin's setting",
        "the write no longer goes through the player's link — flip this assertion \
         and rename the test, the hole is closed"
    );
}

/// Odd file names stay inside the instance, and one of them does not stop the
/// rest of the pack landing.
///
/// Four shapes a real archiver will happily write and a real pack never does:
/// a Windows reserved device name, a name differing from another only in case,
/// a trailing dot, and a trailing space.
///
/// **What this proves and what it does not.** On the platform these tests run
/// on, every one of those is an ordinary file name, so what is asserted is
/// containment: each lands under the instance's own folders and nothing lands
/// anywhere else. It says nothing about what `CON.jar` does on Windows, where
/// opening it reaches a device rather than a file, or about which of
/// `Evil.jar` and `evil.jar` survives on a case-insensitive filesystem. Those
/// are claims about two platforms nobody has ever run this app on
/// (`CLAUDE.md`), and a test here cannot make them. The reason to have this
/// one anyway is the other half: a name the extractor cannot handle must not
/// take the rest of the pack down with it, and that *is* checked here, by
/// asserting the honest file landed too.
///
/// And each odd name is asserted to have landed under its own name holding its
/// own bytes, which is the part that can fail. Containment alone could not:
/// none of these names escapes anything, so an extractor that silently dropped
/// all six would have passed a test that only swept for files outside the
/// instance.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a scratch GEODE_HOME"]
async fn odd_file_names_still_land_inside_the_instance() {
    let scratch = Scratch::new("oddnames");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let archive = paths.root.join("oddnames.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at("modrinth.index.json", index(vec![])),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
            Entry::at("overrides/mods/honest.jar", "honest"),
            Entry::at("overrides/mods/CON.jar", "a device name on Windows"),
            Entry::at("overrides/mods/nul.jar", "another one"),
            Entry::at("overrides/mods/Evil.jar", "upper"),
            Entry::at("overrides/mods/evil.jar", "lower"),
            Entry::at("overrides/config/trailing.", "a trailing dot"),
            Entry::at("overrides/config/trailing ", "a trailing space"),
        ],
    );

    let mut inst = fresh(paths, "Odd Names").await;
    pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-oddnames"),
    )
    .await
    .expect("a pack full of awkward names still imports");

    let game_dir = paths.game_dir(&inst.id);
    let landed = files_under(&game_dir);
    assert!(
        landed.contains("mods/honest.jar"),
        "an awkward name took the honest file down with it: {landed:?}"
    );

    // Every one of them landed, under its own name, holding its own bytes.
    //
    // This is what gives the test something to fail on. Containment and "the
    // honest file survived" both hold on this platform however the extractor
    // treats these names — including if it quietly dropped every one of them —
    // so without this loop there was no change to `extract_trees` that the
    // test would notice. It is also the honest statement of what *is* true
    // here: `extract_trees` sanitises nothing beyond `is_safe_relative`
    // (instance/modpack.rs), so on a filesystem where these are ordinary names
    // they are ordinary files, `Evil.jar` and `evil.jar` are two of them, and
    // a name filter added without a decision about Windows would fail here and
    // have to be argued for.
    for (rel, body) in [
        ("mods/CON.jar", "a device name on Windows"),
        ("mods/nul.jar", "another one"),
        ("mods/Evil.jar", "upper"),
        ("mods/evil.jar", "lower"),
        ("config/trailing.", "a trailing dot"),
        ("config/trailing ", "a trailing space"),
    ] {
        assert!(
            landed.contains(rel),
            "{rel} did not land, so something is filtering names: {landed:?}"
        );
        assert_eq!(
            std::fs::read(game_dir.join(rel)).unwrap(),
            body.as_bytes(),
            "{rel} holds another entry's bytes"
        );
    }

    // Every name that landed is under one of the instance's own folders.
    for rel in &landed {
        let under = rel.split('/').next().unwrap_or_default();
        assert!(
            ["mods", "config", "resourcepacks", "shaderpacks", "saves"].contains(&under),
            "{rel} landed somewhere the pack had no business writing"
        );
    }
    let everywhere = files_under(&paths.root);
    let outside: Vec<&String> = everywhere
        .iter()
        .filter(|rel| {
            let path = paths.root.join(rel);
            !path.starts_with(paths.instances()) && path != archive
        })
        .collect();
    assert!(outside.is_empty(), "a pack wrote outside: {outside:?}");
}

/// Two entries at one path leave one file, and a second Geode manifest cannot
/// be smuggled past the first.
///
/// A zip may hold the same name twice — nothing in the format forbids it, and
/// readers disagree about which one wins. That is a way to show one thing to
/// anybody who looks and do another: a first `geode/manifest.json` saying
/// `kind: server`, which is what a join demands, and a second saying
/// `kind: instance`, which is what unlocks the launcher settings a server pack
/// is never trusted with (`share::pack::apply`'s `personal` gate).
///
/// What is asserted is not which one wins — that is the `zip` crate's to
/// decide — but that **every reader agrees**. `read_manifest` picks one, and
/// the import must have acted on that same one: Java arguments land if and
/// only if the manifest the readers can see says `instance`. A split between
/// them is the bug, because `commands::install_joined` refuses a pack on
/// `read_manifest`'s answer and then imports on the importer's.
///
/// It holds today for a structural reason worth writing down, because it is
/// not a decision anybody here made: `zip` 2 keys its entry list by name, so a
/// duplicate is collapsed as the central directory is parsed and every caller
/// in the process is handed the same single record. The assertions below say
/// that in both directions — the archive names the manifest twice, the reader
/// reports it once — so a `zip` release that started surfacing both would fail
/// here rather than quietly making which-reader-wins a live question.
///
/// Writing the fixture needed a patch, which is itself the thing that makes
/// the shape worth a test: `ZipWriter` refuses a duplicate filename, so no
/// pack Geode builds can be like this, and an archive that is like this came
/// from somewhere else. See [`rename_entry_to`].
///
/// `a_server_pack_never_sets_a_launcher_setting` and
/// `a_server_pack_cannot_set_the_jvm_or_claim_to_be_a_modpack` (both in
/// share/pack.rs) already pin the gate itself, for a pack with one manifest.
/// Neither puts two in the archive.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a scratch GEODE_HOME"]
async fn duplicate_entries_cannot_smuggle_a_second_manifest() {
    let scratch = Scratch::new("dupes");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    const SMUGGLED: &str = "-javaagent:mods/agent.jar";
    /// The second manifest's name before it is patched onto the first's: the
    /// same length, differing in one character that appears in no entry's
    /// bytes.
    const SECOND_MANIFEST: &str = "geode/manifestXjson";
    const SECOND_DUP: &str = "overrides/mods/duq.jar";
    let archive = paths.root.join("dupes.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at("modrinth.index.json", index(vec![])),
            // What anybody looking at the archive's first manifest sees.
            Entry::at(MANIFEST_PATH, manifest("server", serde_json::json!({}))),
            // And the one hiding behind it, written under a name one character
            // away and patched onto the first's below.
            Entry::at(
                SECOND_MANIFEST,
                manifest(
                    "instance",
                    serde_json::json!({ "profile": { "name": "Hostile", "javaArgs": SMUGGLED } }),
                ),
            ),
            Entry::at("overrides/mods/dup.jar", "the first body"),
            Entry::at(SECOND_DUP, "the second body"),
        ],
    );
    rename_entry_to(&archive, SECOND_MANIFEST, MANIFEST_PATH);
    rename_entry_to(&archive, SECOND_DUP, "overrides/mods/dup.jar");
    // The archive on disk really does name the manifest twice — two local
    // headers and two central directory records — so what follows is about the
    // reader and not about a test that failed to build its fixture.
    assert_eq!(
        times_named(&archive, MANIFEST_PATH),
        4,
        "the archive does not really hold the manifest twice, so this proves nothing"
    );
    assert_eq!(times_named(&archive, "overrides/mods/dup.jar"), 4);

    // And the reader collapses each duplicate to one entry, which is the
    // property that makes a split between readers impossible: `zip` 2 keys its
    // entry list by name, so every reader in this process — `read_manifest`,
    // `read_pack`, `arrivals`, `extract_trees` — is looking at the same single
    // record, and the one it is looking at is the last the archive declared.
    {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap())
            .expect("the archive still opens");
        let named: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index_raw(i).unwrap().name().to_string())
            .collect();
        assert_eq!(
            named.iter().filter(|name| *name == MANIFEST_PATH).count(),
            1,
            "this reader has started surfacing both records, so which one each caller gets \
             is now a live question: {named:?}"
        );
    }

    let effective = pack::read_manifest(&archive)
        .expect("the manifest reads")
        .expect("the pack has a Geode half");
    println!("of the two manifests, the readers see kind {:?}", effective.kind);

    let mut inst = fresh(paths, "Duplicates").await;
    pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-dupes"),
    )
    .await
    .expect("a pack with duplicate entries still imports");

    let expected = (effective.kind == Kind::Instance).then(|| SMUGGLED.to_string());
    assert_eq!(
        inst.overrides.java_args, expected,
        "the importer acted on a different manifest from the one `read_manifest` returned \
         (it saw kind {:?})",
        effective.kind
    );

    // One name, one file, whichever body won.
    let game_dir = paths.game_dir(&inst.id);
    let landed = files_under(&game_dir);
    let dupes: Vec<&String> = landed.iter().filter(|rel| rel.contains("dup.jar")).collect();
    assert_eq!(dupes.len(), 1, "duplicate entries left {dupes:?}");
    let body = std::fs::read(game_dir.join("mods/dup.jar")).unwrap();
    assert!(
        body == b"the first body" || body == b"the second body",
        "mods/dup.jar holds neither entry's bytes: {body:?}"
    );
}

/// A server's pack is held to Modrinth at the import, and a pack that is not a
/// server's is held to nothing — **the second half is the hole**.
///
/// One archive, built once, imported twice with nothing changed but the
/// manifest's `kind`, through the same client. That is the whole proof: the
/// difference cannot be the client, the URL, the bytes or the listener.
///
/// As a server's pack it is refused with a sentence naming the file, and the
/// listener is never asked — `check_server_downloads` runs before a byte moves
/// (`share/pack.rs`), and `server_pack_client` would refuse a plain-HTTP
/// address at the builder even if it did not. As an instance's pack —  which
/// is what a `geode://pack?url=…` link carries, and what a `.mrpack` somebody
/// downloaded is — the index's `http://127.0.0.1` URL is requested and
/// whatever came back is written into `mods/`.
///
/// **What is new here and what is not.**
/// `a_server_pack_is_still_held_to_modrinth_only` (share/pack.rs) already runs
/// a server's pack through `import` and asserts this same refusal, so the
/// first half of this test does *not* discover that the gate is wired in —
/// that is covered, and a claim here that it was not would be false. What this
/// half adds is the one thing an offline test cannot say: its client is
/// `offline()`, so it cannot tell a refusal from an address that was simply
/// unreachable. Here the address is a listener that would have answered, and
/// the assertion is that it was never dialled.
///
/// What nothing anywhere pins is the other branch, which is the hole below.
///
/// **On the hole.** `share/pack.rs` states the position for a `.mrpack` the
/// player chose themselves: "names whatever hosts that format allows". The
/// thing that does not fit it is a link pack, which `commands::refuse_java_args`
/// describes as specifically *not* a pack the player went and found. Closing
/// it is a behaviour change, so this test pins what happens today; when the
/// link path stops using the general client, the second half of this test
/// should become a refusal and lose its `_today`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a loopback port and writes a scratch GEODE_HOME"]
async fn only_a_server_pack_is_held_to_modrinth_so_a_link_pack_reaches_any_host_today() {
    let scratch = Scratch::new("hosts");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let payload = b"bytes from an address the pack chose".to_vec();
    let host = serving(vec![("/evil.jar", 200, payload.clone())]).await;
    let entry = indexed(
        "mods/elsewhere.jar",
        vec![host.url("/evil.jar")],
        Some(&sha1_of(&payload)),
        Some(payload.len() as u64),
    );

    let as_a_server = paths.root.join("server.geodepack");
    write_archive(
        &as_a_server,
        &[
            Entry::at("modrinth.index.json", index(vec![entry.clone()])),
            Entry::at(MANIFEST_PATH, manifest("server", serde_json::json!({}))),
        ],
    );
    let as_a_link = paths.root.join("link.geodepack");
    write_archive(
        &as_a_link,
        &[
            Entry::at("modrinth.index.json", index(vec![entry])),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
        ],
    );

    // A server's pack: refused, readably, before the network.
    let mut joined = fresh(paths, "From A Server").await;
    let refusal = pack::import(
        &only_loopback(),
        paths,
        &mut joined,
        &as_a_server,
        None,
        &Progress::silent("hostile-host-server"),
    )
    .await
    .expect_err("a server's pack may not download from an address it chose");
    let said = refusal.to_string();
    assert!(
        said.contains("somewhere other than Modrinth"),
        "the refusal does not say why: {said}"
    );
    assert!(
        said.contains("mods/elsewhere.jar"),
        "the refusal does not name the file: {said}"
    );
    // CLAUDE.md's two registers: a refusal that states a fact is a lowercase
    // fragment with no full stop. (`|| c == 't'` stood here, which is a
    // tautology — `'t'` is lowercase — and let the whole check pass on
    // anything.)
    assert!(
        said.starts_with(char::is_lowercase),
        "a refusal that states a fact is a lowercase fragment: {said}"
    );
    assert!(
        !said.ends_with('.'),
        "a refusal that states a fact carries no full stop: {said}"
    );
    assert!(
        host.asked_for().is_empty(),
        "a server pack's address was dialled before it was refused: {:?}",
        host.asked_for()
    );
    assert!(
        !paths
            .game_dir(&joined.id)
            .join("mods/elsewhere.jar")
            .exists(),
        "a refused server pack still wrote a jar"
    );

    // THE HOLE. The same index, the same client, `kind: instance`: fetched.
    let mut linked = fresh(paths, "From A Link").await;
    pack::import(
        &only_loopback(),
        paths,
        &mut linked,
        &as_a_link,
        None,
        &Progress::silent("hostile-host-link"),
    )
    .await
    .expect("a link pack's index is honoured wherever it points, today");
    assert_eq!(
        host.asked_for(),
        BTreeSet::from(["/evil.jar".to_string()]),
        "the link pack's chosen address was not reached — if a host rule landed on this \
         path, this half of the test should become a refusal"
    );
    assert_eq!(
        std::fs::read(paths.game_dir(&linked.id).join("mods/elsewhere.jar")).unwrap(),
        payload,
        "the bytes from the pack's own address landed in mods/"
    );
}

/// An index entry whose hash does not match the bytes served refuses the
/// import, names the file, and leaves nothing behind.
///
/// The hashes in a `.mrpack`'s index are the only thing tying a URL to the
/// bytes a pack meant. A pack whose index vouches for one jar and whose host
/// answers with another is the shape that matters for a server pack, where
/// the hashes are computed from the admin's own disk while the URL names
/// Modrinth — so a record that has gone stale, or an upstream file that has
/// moved, makes the two disagree.
///
/// What is asserted is the whole refusal, not just that it failed:
/// `net::try_fetch` compares the hash it computed while streaming and returns
/// `Error::Checksum`, which names the file and both hashes; the `.part` file
/// is unlinked, so no half-verified jar is left where the next launch would
/// read it; and the destination does not exist, so nothing is passed off as
/// installed.
///
/// Three attempts with backoff happen first (`net::fetch_one`), so the
/// listener is asked more than once — which is why it logs and why the
/// assertion is on the set of paths rather than the count.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a loopback port and writes a scratch GEODE_HOME"]
async fn bytes_that_do_not_match_the_index_hash_refuse_the_import_and_name_the_file() {
    let scratch = Scratch::new("badhash");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let served = b"what the host actually sends".to_vec();
    let vouched = b"what the index says it is".to_vec();
    let host = serving(vec![("/liar.jar", 200, served.clone())]).await;

    let archive = paths.root.join("badhash.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at(
                "modrinth.index.json",
                index(vec![indexed(
                    "mods/liar.jar",
                    vec![host.url("/liar.jar")],
                    Some(&sha1_of(&vouched)),
                    Some(vouched.len() as u64),
                )]),
            ),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
        ],
    );

    let mut inst = fresh(paths, "Bad Hash").await;
    let refusal = pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-badhash"),
    )
    .await
    .expect_err("bytes that do not match the index must not be installed");

    let said = refusal.to_string();
    assert!(
        said.contains("mods/liar.jar"),
        "the refusal does not name the file: {said}"
    );
    assert!(
        said.contains(&sha1_of(&vouched)),
        "the refusal does not say what was expected: {said}"
    );
    assert!(
        said.contains(&sha1_of(&served)),
        "the refusal does not say what arrived: {said}"
    );

    let game_dir = paths.game_dir(&inst.id);
    assert!(
        !game_dir.join("mods/liar.jar").exists(),
        "the unverified bytes were kept"
    );
    let landed = files_under(&game_dir);
    assert!(
        landed.iter().all(|rel| !rel.contains("liar")),
        "a .part file was left behind: {landed:?}"
    );
    assert_eq!(
        host.asked_for(),
        BTreeSet::from(["/liar.jar".to_string()]),
        "something other than the one file was fetched"
    );
}

/// **A hole, pinned as it is.** An index entry's declared size is not checked
/// against what arrives, so a pack that says a file weighs seven bytes may
/// hand over as many as it likes.
///
/// `net::Download::size` is read in exactly one place — `already_valid`, which
/// decides whether a file *already on disk* can be skipped, and only when no
/// hash was given. `try_fetch` streams the body into a `.part` file with no
/// byte counter and nothing to compare against, so `fileSize` is a hint about
/// files the recipient already has and never a ceiling on the one coming in.
///
/// The index's hashes are the real protection, and an entry carrying one is
/// safe: see
/// [`bytes_that_do_not_match_the_index_hash_refuse_the_import_and_name_the_file`].
/// But `MrFile::hashes` defaults to empty on purpose — "one entry missing the
/// key makes an index unparsable" — so an entry with no `sha1` is a pack this
/// reader accepts, and for that entry nothing anywhere bounds the download.
/// `extract_trees` has a budget for the bytes a pack *unpacks* to
/// (`an_override_bomb_is_stopped_before_it_fills_the_disk`, instance/modpack.rs);
/// the files it *downloads* have none, which is the asymmetry here.
///
/// Kept small on purpose: a few hundred kilobytes against a declared seven
/// bytes makes the point as well as a gigabyte would, and a test that really
/// tried to fill a disk would be a worse test.
///
/// **When this is fixed** this becomes a refusal naming `mods/fat.jar`, and
/// loses its `_today`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a loopback port and writes a scratch GEODE_HOME"]
async fn a_declared_size_is_never_checked_against_what_arrives_today() {
    let scratch = Scratch::new("fatsize");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let fat = vec![b'x'; 512 * 1024];
    let host = serving(vec![("/fat.jar", 200, fat.clone())]).await;

    let archive = paths.root.join("fatsize.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at(
                "modrinth.index.json",
                index(vec![indexed(
                    "mods/fat.jar",
                    vec![host.url("/fat.jar")],
                    // No hash at all, which this reader accepts on purpose.
                    None,
                    Some(7),
                )]),
            ),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
        ],
    );

    let mut inst = fresh(paths, "Fat Size").await;
    pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-fatsize"),
    )
    .await
    .expect("a lying fileSize does not stop an import, today");

    // THE HOLE. A size check would have refused this, or truncated at seven.
    let landed = paths.game_dir(&inst.id).join("mods/fat.jar");
    assert_eq!(
        std::fs::metadata(&landed).unwrap().len(),
        fat.len() as u64,
        "the declared size is now being enforced — make this a refusal naming mods/fat.jar \
         and rename the test, the hole is closed"
    );
}

/// One index entry that cannot be fetched fails the whole import, and the file
/// never reaches the list of things to fetch by hand — while the entries that
/// did land are kept rather than abandoned.
///
/// This is the player-visible half of the fail-fast question. `net::fetch_all`
/// takes the first error and returns, dropping the downloads still in flight
/// and never starting the ones still queued; `instance/modpack.rs` propagates
/// it with a bare `?`, and `share::pack`'s `outcome?` returns before any
/// `UpdateReport` is built. The only soft channel in the Modrinth arm fires
/// for an entry with no `downloads` key at all, so an entry that *has* a URL
/// and cannot be fetched has no soft path: a mod deleted upstream is a failed
/// import rather than a line saying which mod to go and get.
///
/// Both halves are asserted, because the second is the one that is easy to
/// overstate. All four jars that could complete are on disk afterwards —
/// `fetch_one` writes a `.part` sibling and renames, so a download that
/// finished finished — and because the index carries their hashes,
/// `already_valid` skips them on the next attempt. What is lost is the report,
/// not the work.
///
/// `a_failed_update_leaves_ownership_right_and_versions_alone` (share/pack.rs)
/// is the nearest offline test and it gives the pack exactly one index entry,
/// so there is no second one to abandon and nothing to say about what a player
/// is told. Nothing offline exercises a successful index download at all: every
/// update test pre-seeds the bytes so `already_valid` short-circuits.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a loopback port and writes a scratch GEODE_HOME"]
async fn one_entry_that_cannot_be_fetched_fails_the_whole_import_and_reaches_no_report() {
    let scratch = Scratch::new("onedead");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let alive: Vec<Vec<u8>> = (0..4)
        .map(|i| format!("live mod number {i}").into_bytes())
        .collect();
    let mut answers: Vec<(&str, u16, Vec<u8>)> = Vec::new();
    let paths_served = ["/m0.jar", "/m1.jar", "/m2.jar", "/m3.jar"];
    for (path, body) in paths_served.iter().zip(&alive) {
        answers.push((path, 200, body.clone()));
    }
    answers.push(("/gone.jar", 404, b"deleted upstream".to_vec()));
    let host = serving(answers).await;

    let mut files: Vec<serde_json::Value> = Vec::new();
    // The dead one first, so the abort happens with four still to come: the
    // worst case for "the others are abandoned", which is the claim being
    // measured.
    files.push(indexed(
        "mods/gone.jar",
        vec![host.url("/gone.jar")],
        None,
        None,
    ));
    for (i, (path, body)) in paths_served.iter().zip(&alive).enumerate() {
        files.push(indexed(
            &format!("mods/m{i}.jar"),
            vec![host.url(path)],
            Some(&sha1_of(body)),
            Some(body.len() as u64),
        ));
    }

    let archive = paths.root.join("onedead.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at("modrinth.index.json", index(files)),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
        ],
    );

    let mut inst = fresh(paths, "One Dead").await;
    let refusal = pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-onedead"),
    )
    .await
    .expect_err("one unfetchable index entry fails the whole import");

    let said = refusal.to_string();
    assert!(
        said.contains("mods/gone.jar"),
        "the failure does not name the file: {said}"
    );
    assert!(
        said.contains("404"),
        "the failure does not say what the host answered: {said}"
    );

    // The other four are not abandoned: whichever completed are on disk.
    let game_dir = paths.game_dir(&inst.id);
    let landed = files_under(&game_dir);
    let kept = landed.iter().filter(|rel| rel.starts_with("mods/m")).count();
    // All four, not merely one. This is deterministic rather than a race, and
    // the reason is worth writing down because `> 0` stood here and would have
    // passed with three of the four lost — which is exactly the regression
    // this assertion is for. `fetch_all` pushes `min(concurrency, total)`
    // futures at once and this call passes 10 for five entries (net.rs), so
    // every download is in flight before any of them resolves; and the dead
    // one cannot return its error for at least 2.4 seconds, because
    // `fetch_one` makes three attempts with `400 * (1 << attempt)` ms of
    // backoff between them. Four nineteen-byte bodies off a loopback listener
    // land in a thousandth of that.
    assert_eq!(
        kept,
        alive.len(),
        "a download that had completed was thrown away when a later one failed: {landed:?}"
    );
    println!("kept {kept} of {} live entries after the abort", alive.len());
    assert!(
        !game_dir.join("mods/gone.jar").exists(),
        "the entry that 404ed left a file behind"
    );

    // And a second run resumes rather than restarting, because the index
    // carries hashes for what landed.
    let before = host.asked_for().len();
    let _ = pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-onedead-again"),
    )
    .await
    .expect_err("the dead entry is dead on every attempt");
    println!(
        "asked for {} distinct paths over two runs (was {before} after one)",
        host.asked_for().len()
    );
    assert!(
        files_under(&game_dir)
            .iter()
            .filter(|rel| rel.starts_with("mods/m"))
            .count()
            >= kept,
        "the second attempt lost ground"
    );
}

/// **A hole, pinned as it is.** Nothing bounds how many files a pack may
/// declare: a fourteen-megabyte archive lists a hundred and twenty thousand
/// entries and is opened without complaint.
///
/// `open_pack_within`'s budget is `MAX_DIRECTORY_READ`, sixteen mebibytes *of
/// the bytes read while finding the list*, and its own doc says what it does
/// not cover: "the count of files the zip declares". Short names are the whole
/// trick. The one offline test of that budget,
/// `a_pack_listing_too_many_files_is_refused_while_they_are_read`
/// (instance/modpack.rs), uses 270-byte names *precisely so the byte budget
/// trips*, and its helper takes `files: u16`, which cannot express this many.
///
/// **Measured, because the arithmetic is wrong.** An `overrides/mods/NNNNNN.jar`
/// entry costs exactly 71 bytes of central directory in this writer's output,
/// which suggests about 236,000 entries would fit the budget. They do not:
/// walking the count up in five-thousand steps, this build accepts 165,000 and
/// refuses 170,000 — a directory of 11.7 MB against a budget of 16.8 MB, so the
/// reader spends roughly 1.4 bytes of budget per byte of directory. The count
/// used here sits well under that so the test cannot sit on the boundary, and
/// it is still three orders of magnitude past any real pack: the largest packs
/// on either provider list a few hundred files.
///
/// What is asserted is only the opening, which is the cheap and load-bearing
/// part: `preview` and `read_manifest` both go through `open_pack`, and
/// accepting the archive is what hands every later stage a hundred and
/// twenty thousand entries to loop over — `extract_trees` one file at a time,
/// `arrivals` one map entry each, `settle_server_files` a `ContentRecord`
/// each, into an `instance.json` that is serialised on every save and crosses
/// the IPC boundary whole. This test deliberately does **not** run the import:
/// what that costs is the finding, not something a test should sit through.
///
/// **When a count ceiling lands** this becomes a refusal, and the refusal has
/// to read as something a person can act on — the byte-budget one already does
/// ("lists far more files than any real pack").
#[tokio::test(flavor = "multi_thread")]
#[ignore = "assembles a hundred and twenty thousand zip entries; the slow test in this file"]
async fn a_pack_may_list_a_hundred_and_twenty_thousand_files_and_still_be_opened_today() {
    let scratch = Scratch::new("crowded");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    // Short names, so the list stays inside the budget — see the measurement
    // in this test's doc. Past 65,535 entries a zip needs ZIP64 records, which
    // the writer emits by itself; the count assertion below is what says it got
    // them right, since a classic end-of-directory record's count is a u16 and
    // would wrap (a first attempt at this test wrote 230,001 entries and a
    // reader that ignored ZIP64 would have reported 33,969 of them).
    const ENTRIES: u32 = 120_000;
    let archive = paths.root.join("crowded.mrpack");
    let started = std::time::Instant::now();
    {
        let file = std::fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("modrinth.index.json", options).unwrap();
        zip.write_all(&index(vec![])).unwrap();
        for i in 0..ENTRIES {
            // Inside an override tree, so these are entries a real import
            // would write — not junk the reader would skip.
            zip.start_file(format!("overrides/mods/{i:06}.jar"), options)
                .unwrap();
        }
        zip.finish().unwrap();
    }
    let bytes = std::fs::metadata(&archive).unwrap().len();
    println!(
        "wrote {ENTRIES} entries in {:?}, {} MB",
        started.elapsed(),
        bytes / (1024 * 1024)
    );

    // Read back through the plain reader first, so the next assertion is about
    // Geode's guard and not about whether the archive is well formed.
    let count = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap())
        .expect("the archive is a valid zip")
        .len();
    assert_eq!(
        count,
        ENTRIES as usize + 1,
        "the writer did not record every entry, so this proves nothing about the ceiling"
    );

    /// Which of the two refusals this is, because they mean opposite things
    /// and only one of them is good news.
    ///
    /// A count ceiling landing is the hole closing. The *byte* budget tripping
    /// at this count is the test rotting: the measured boundary is between
    /// 165,000 and 170,000 entries, only a little above `ENTRIES`, so a `zip`
    /// release that lays the central directory out differently can move it
    /// under this count — and a bare `expect` would have reported that as the
    /// hole being fixed.
    fn blame(said: &str) -> ! {
        assert!(
            !said.contains("far more files than any real pack"),
            "this is `MAX_DIRECTORY_READ` refusing a {ENTRIES}-entry directory, not a \
             ceiling on the count: the hole is NOT closed, the test has rotted. Re-measure \
             the boundary and lower `ENTRIES`: {said}"
        );
        panic!("a count ceiling has landed — make this a refusal and rename the test: {said}");
    }

    // THE HOLE. `read_manifest` and `preview` are the two readers an import
    // meets first, and both go through `open_pack`.
    if let Err(e) = pack::read_manifest(&archive) {
        blame(&e.to_string());
    }
    let described = match instance::modpack::preview(archive.clone()).await {
        Ok(described) => described,
        Err(e) => blame(&e.to_string()),
    };
    assert_eq!(
        described.file_count, 0,
        "the one number shown before a person says yes counts the index, not the \
         {ENTRIES} files about to be written"
    );
}

/// One entry in a compression this build cannot open fails the whole pack, and
/// the refusal names no file.
///
/// `zip` is compiled with `deflate` and nothing else, so Stored and Deflate are
/// the only methods openable here. `extract_trees` does `zip.by_index(i)?`
/// *before* it asks whether the entry is a directory and before it asks whether
/// the entry belongs to any tree it writes — so an entry that would never have
/// been written at all takes the import down, with the `zip` crate's own static
/// string, which names no entry and gives a person nothing to act on. A repack
/// through an archiver with zstd turned on is enough to cause it.
///
/// Three things are asserted, and the second and third are the finding: the
/// import fails; the message does not name the entry that caused it; and the
/// honest override that had already been written is rolled back, although
/// nothing was wrong with it.
///
/// **Scope.** This is an archive a person supplies — a `.mrpack` they
/// downloaded, a `geode://pack` link, a pack published by a provider. It is
/// *not* reachable for a joined server's pack: that pack is always built by
/// `export_server` through Geode's own writer, which hardcodes Deflated, and
/// this build cannot emit anything else. So the manifest here says `instance`,
/// and a `bring_up_to_date` loop is not part of the claim.
///
/// `a_refused_pack_takes_back_only_what_it_wrote` (instance/modpack.rs) is the
/// nearest offline test; all three of its entries are Deflated and its failure
/// arrives from the inflate budget *after* the entry opened, so it never
/// reaches the `?` that this test is about.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a scratch GEODE_HOME"]
async fn one_entry_in_a_compression_this_build_cannot_open_fails_the_whole_pack() {
    let scratch = Scratch::new("zstd");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let archive = paths.root.join("zstd.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at("modrinth.index.json", index(vec![])),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
            // Written before the bad entry, so it has already landed when the
            // run aborts and the rollback is a real rollback.
            Entry::at("overrides/config/honest.toml", "nothing wrong with me"),
            // Belongs to no tree an import writes. Named distinctively so the
            // "names no file" assertion cannot pass by accident.
            Entry::at("a-note-for-the-reader.txt", "repacked with zstd on"),
        ],
    );
    pretend_an_entry_was_written_with_zstd(&archive, "a-note-for-the-reader.txt");

    // Still a readable zip: the guard under test is in the extraction, so a
    // test whose archive the reader refused outright would prove nothing.
    pack::read_manifest(&archive)
        .expect("the manifest still reads")
        .expect("the pack still has a Geode half");

    let mut inst = fresh(paths, "Zstd").await;
    let refusal = pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-zstd"),
    )
    .await
    .expect_err("an entry this build cannot open fails the import");

    let said = refusal.to_string();
    println!("the whole message a person sees: {said}");
    // That it is *this* failure, identified by the error's own variant rather
    // than by its wording. `zip` is depended on as
    // `default-features = false, features = ["deflate"]` (Cargo.toml), so
    // method 93 is one this build was not compiled to open and it says so with
    // `UnsupportedArchive`. Matching the variant keeps an upstream release
    // that rewords that string from failing this test for a reason that has
    // nothing to do with Geode — while the assertion below, which is the
    // finding, still reads the wording a person is actually shown.
    assert!(
        matches!(
            &refusal,
            geode_lib::error::Error::Zip(zip::result::ZipError::UnsupportedArchive(_))
        ),
        "the failure is not the one this test is about: {said}"
    );
    // THE FINDING. When the extraction learns to skip an entry outside every
    // tree before opening it, this import succeeds and both assertions below
    // should be rewritten — the honest override should land, and an entry
    // *inside* a tree that cannot be read should be named.
    assert!(
        !said.contains("a-note-for-the-reader.txt"),
        "the refusal has started naming the entry — the finding is fixed, rewrite this test: \
         {said}"
    );
    let game_dir = paths.game_dir(&inst.id);
    assert!(
        !game_dir.join("config/honest.toml").exists(),
        "the honest override survived, so the rollback no longer takes it back: {:?}",
        files_under(&game_dir)
    );
}

/// A manifest from a newer Geode is refused by name, and a revision far ahead
/// of anything real is only a string.
///
/// Two claims about the Geode half's own fields, which are the ones no other
/// launcher reads and so the ones nothing else would catch.
///
/// The format refusal is a real guard: `Manifest::readable` compares against
/// `FORMAT`, and a pack claiming more is refused rather than half-read,
/// because "silently ignoring fields it does not know would import a pack that
/// looks right and is not". Asserted through `read_manifest` *and* through
/// `import`, so the check is pinned where it is reached rather than where it is
/// written. `a_pack_from_a_future_format_version_is_refused`
/// (instance/modpack.rs) is the same idea for Modrinth's `formatVersion`, a
/// different field in a different half.
///
/// The revision is the other way round: nothing bounds it. It is a label the
/// recipient keeps so that a server which changed and changed back is noticed,
/// and it is compared for equality and nothing else — so a hostile one cannot
/// make the recipient install anything, and the only thing it can do is be
/// large. Pinned here at a megabyte, which is accepted: the only ceiling in
/// front of it is the eight-megabyte one on the manifest as a whole
/// (`an_oversized_geode_manifest_is_refused`, share/pack.rs), and what it ends
/// up in is `instance.json`, which is re-serialised on every save and crosses
/// the IPC boundary whole. Worth knowing; not worth refusing a pack over.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a scratch GEODE_HOME"]
async fn a_newer_format_is_refused_and_a_revision_far_ahead_is_only_a_string() {
    let scratch = Scratch::new("format");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    // ---- a format from the future
    let future = paths.root.join("future.geodepack");
    write_archive(
        &future,
        &[
            Entry::at("modrinth.index.json", index(vec![])),
            Entry::at(
                MANIFEST_PATH,
                serde_json::to_vec(&serde_json::json!({
                    "geodeFormat": 9_999,
                    "kind": "server",
                    "profile": { "name": "From The Future" },
                }))
                .unwrap(),
            ),
        ],
    );
    let refusal = pack::read_manifest(&future).expect_err("a newer format must be refused");
    let said = refusal.to_string();
    assert!(
        said.contains("newer Geode") && said.contains("update"),
        "the refusal does not tell a person what to do: {said}"
    );

    let mut inst = fresh(paths, "From The Future").await;
    let through_import = pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &future,
        None,
        &Progress::silent("hostile-future"),
    )
    .await
    .expect_err("the import must refuse it too, not only read_manifest");
    assert!(
        through_import.to_string().contains("newer Geode"),
        "the import's refusal is a different one: {through_import}"
    );
    assert!(
        files_under(&paths.game_dir(&inst.id)).is_empty(),
        "a refused pack still wrote something: {:?}",
        files_under(&paths.game_dir(&inst.id))
    );

    // ---- a revision nobody could have reached
    let huge = "9".repeat(1024 * 1024);
    let ahead = paths.root.join("ahead.geodepack");
    write_archive(
        &ahead,
        &[
            Entry::at("modrinth.index.json", index(vec![])),
            Entry::at(
                MANIFEST_PATH,
                manifest("server", serde_json::json!({ "revision": huge })),
            ),
            Entry::at("overrides/config/sent.toml", "the admin's copy"),
        ],
    );

    let mut joined = fresh(paths, "Far Ahead").await;
    // Moved off what the index declares *before* the update, which is what
    // makes the two assertions below mean anything. `fresh` creates at
    // 1.21.1/Fabric 0.16.9 and [`index`] declares exactly that, so asserting
    // those values afterwards held whether `update` assigned them from the
    // index, assigned them from nowhere, or never ran at all.
    joined.mc_version = "1.19.2".into();
    joined.loader = Loader::Forge;
    joined.loader_version = "1.19.2-43.2.0".into();

    let report = pack::update(
        &only_loopback(),
        paths,
        &mut joined,
        &ahead,
        None,
        &Progress::silent("hostile-ahead"),
    )
    .await
    .expect("an absurd revision is still only a revision");
    assert_eq!(
        report.revision.as_deref(),
        Some(huge.as_str()),
        "the revision a pack carried is kept verbatim"
    );
    // The thing a revision must not be able to do: decide what is installed.
    // All three are assigned from the *index's* `dependencies` — `instance
    // .mc_version = described.mc_version` and its two neighbours in
    // `share/pack.rs` — and the instance held none of these values a moment
    // ago, so arriving at them is the index deciding and nothing else.
    assert_eq!(
        joined.mc_version, "1.21.1",
        "the Minecraft version did not come from the index's dependencies"
    );
    assert_eq!(
        joined.loader,
        Loader::Fabric,
        "the loader did not come from the index's dependencies"
    );
    assert_eq!(
        joined.loader_version, "0.16.9",
        "the loader version did not come from the index's dependencies"
    );
    assert!(
        paths
            .game_dir(&joined.id)
            .join("config/sent.toml")
            .is_file(),
        "the pack's own files still landed"
    );
}

/// An entry named like a folder the instance already has fails the whole
/// import, with an error that names nothing and takes the rest of the pack
/// back with it.
///
/// `extract_trees` writes every entry's bytes with `File::create`, which on an
/// existing directory fails with the operating system's own "Is a directory".
/// That error is not a `Landing::Offer` one, so it is not collected into the
/// report — it leaves the closure with `return Err(e.into())`, the run rolls
/// back everything it created, and what reaches the person is an `Io` variant
/// with no path in it at all.
///
/// A pack can say this about any of the five folders `instance::create` makes,
/// and the shape is the one an archiver writes for a *symlinked* directory —
/// which is how a pack would carry `mods -> /somewhere/else` if the extractor
/// honoured link entries. It does not
/// ([`a_symlink_entry_arrives_as_a_file_holding_its_target`]), so what is left
/// is this: not an escape, but a pack that refuses itself and says nothing
/// useful while doing it.
///
/// The claim is deliberately narrow, because the harm is: the attacker's own
/// pack does not import. It is here because the *wording* is the problem and
/// nothing else measures it — `Error::Io`'s `Display` is the `std::io::Error`
/// and the file name is lost, exactly as in
/// [`one_entry_in_a_compression_this_build_cannot_open_fails_the_whole_pack`].
/// The two share one fix: name the entry.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a scratch GEODE_HOME"]
async fn an_entry_named_like_a_folder_the_instance_has_fails_the_whole_import() {
    let scratch = Scratch::new("collide");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let archive = paths.root.join("collide.geodepack");
    write_archive(
        &archive,
        &[
            Entry::at("modrinth.index.json", index(vec![])),
            Entry::at(MANIFEST_PATH, manifest("instance", serde_json::json!({}))),
            Entry::at("overrides/config/honest.toml", "nothing wrong with me"),
            // Written as a link entry, which is the shape a real archiver
            // produces for a symlinked folder — and lands as a plain file
            // entry named `mods`.
            Entry::at("overrides/mods", "/somewhere/else").saying_it_is_a_symlink(),
        ],
    );

    let mut inst = fresh(paths, "Collide").await;
    let refusal = pack::import(
        &only_loopback(),
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("hostile-collide"),
    )
    .await
    .expect_err("an entry named like an existing folder cannot be written");

    let said = refusal.to_string();
    println!("the whole message a person sees: {said}");
    // That it is *this* failure and not some other refusal. Without this the
    // test asserted only `expect_err` and the absence of a substring, so a
    // pack refused for an unrelated reason — a manifest the reader stopped
    // accepting, a tree that stopped being written — would have passed it
    // while proving nothing. `Error::Io`'s own `#[error("filesystem error:
    // {0}")]` is the first half; `std::io::Error`'s `Display` appending
    // `(os error N)` is the second, and it does that on every platform, so
    // this says "the write itself failed at the OS level" without naming
    // Linux's wording for it.
    assert!(
        said.contains("filesystem error"),
        "the refusal is not the failed write this test is about: {said}"
    );
    assert!(
        said.contains("os error"),
        "the refusal did not come from the operating system: {said}"
    );
    // THE FINDING, in two halves.
    assert!(
        !said.contains("mods"),
        "the refusal has started naming the entry, which is the fix — rewrite this test: {said}"
    );
    let game_dir = paths.game_dir(&inst.id);
    assert!(
        std::fs::symlink_metadata(game_dir.join("mods"))
            .expect("the instance still has a mods folder")
            .is_dir(),
        "the pack replaced mods/ with something that is not a directory"
    );
    assert!(
        !game_dir.join("config/honest.toml").exists(),
        "the honest override survived the rollback: {:?}",
        files_under(&game_dir)
    );
}

// ---------------------------------------------------------------- not covered
//
// Shapes the brief asked for that are already pinned offline, named here so
// the next person does not write them twice:
//
// * **A manifest claiming to be a server's when it is not** — and the inverse,
//   which is the dangerous direction. `a_server_pack_never_sets_a_launcher_setting`
//   and `a_server_pack_cannot_set_the_jvm_or_claim_to_be_a_modpack`
//   (src-tauri/src/share/pack.rs) run a pack that sets a Java agent, a heap, a
//   window and a modpack label through the whole import under both kinds, and
//   `an_update_refuses_a_personal_pack` (same file) covers a pack that is not a
//   server's arriving on the update path. The duplicate-manifest trick is the
//   one thing they do not cover, and that is
//   `duplicate_entries_cannot_smuggle_a_second_manifest` above.
// * **A reference path that escapes** —
//   `an_unsafe_reference_is_reported_rather_than_skipped`
//   (src-tauri/src/share/pack.rs) is exactly this, with the reported line
//   asserted. Used above only as a control.
// * **Artwork choosing where it is written, and oversized artwork** —
//   `a_pack_cannot_choose_where_its_artwork_is_written` and
//   `oversized_artwork_is_refused` (src-tauri/src/share/pack.rs), the first of
//   which already tries `geode/media/C:evil.png`.
// * **A listing that would garble a player's server list** —
//   `an_exported_server_pack_carries_the_admins_listing_and_an_instance_pack_never_does`
//   (src-tauri/src/share/pack.rs) imports one and asserts no row is written;
//   `a_listing_round_trips_and_a_name_a_list_cannot_show_is_refused`
//   (src-tauri/src/share/manifest.rs) holds the validator.
// * **A ban naming a path instead of an identity** — `Forbidden::checked` and
//   its tests in src-tauri/src/share/manifest.rs, plus the ban family at the
//   end of src-tauri/src/share/pack.rs.
// * **An oversized index or manifest, and a pack that unpacks to more than it
//   should** — `an_index_too_large_to_be_real_is_refused`,
//   `an_override_bomb_is_stopped_before_it_fills_the_disk`,
//   `a_world_that_inflates_past_its_budget_is_stopped`
//   (src-tauri/src/instance/modpack.rs) and
//   `an_oversized_geode_manifest_is_refused` (src-tauri/src/share/pack.rs).
//
// * **A reference path nested deeper than its content folder** — the shape
//   where `config/mods/victim.jar` resolves as a mod because `kind_for` reads
//   only the last folder, is fetched into a folder no loader reads, and has a
//   record written for it that evicts the player's own record for their own
//   `mods/victim.jar`. This file had a test for it; while it was being written
//   the guard landed in `resolve_references` (a two-component filter beside
//   the `is_safe_relative` check) together with its own offline test,
//   `a_reference_nested_below_a_content_folder_is_refused`
//   (src-tauri/src/share/pack.rs), which runs the whole import and asserts the
//   reported line, that no provider was asked, and that nothing was written
//   under `config/mods/`. That is strictly more than this file could say, so
//   the test here was deleted rather than duplicated. If that guard is ever
//   reverted, that test is the one to restore.
// * **An entry whose name is not UTF-8** — the same sitting also landed the
//   `to_str()` filter in `server_mods`, with
//   `a_jar_whose_name_is_not_text_is_counted_in_nothing_that_sends_it`
//   (src-tauri/src/share/pack.rs). That is the admin's side of the pack rather
//   than the recipient's, so it was never this file's.
//
// And one the brief asked for that no test here can make honestly:
//
// * **A pack whose index entry's hash describes the admin's bytes while its URL
//   names upstream** — the server-pack half of the stale-record problem. A
//   server's pack may download only from `cdn.modrinth.com`, so a listener in
//   this file can never serve one of its index entries, and the only honest
//   version of the test would fetch from the real CDN. What is provable without
//   it is the recipient's response to a hash that does not match, which is
//   `bytes_that_do_not_match_the_index_hash_refuse_the_import_and_name_the_file`
//   above, run on a pack that is not a server's. The admin-side half — that
//   nothing at export time checks the constructed URL against what Modrinth
//   serves — is not a test, it is a change of behaviour.


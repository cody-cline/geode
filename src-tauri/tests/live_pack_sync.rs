//! A server's pack, its admin's changes, and a real player keeping up — over
//! the real network, with real Modrinth mods.
//!
//! `live_share_host` starts the listener and proves a pack leaves the machine.
//! `live_share_join` carries one through the steps `commands::install_joined`
//! takes. Both stop where this file begins: every mod in their fixtures is a
//! hand-made jar that exists on no provider, so it travels as **bytes**, and
//! the client they import with is a dead proxy. That leaves the half of pack
//! syncing that only a real provider can exercise untouched:
//!
//! - A mod a server installed from Modrinth travels as a **reference**, not as
//!   bytes, and the URL that reference becomes is *constructed* —
//!   `export_server` identifies with `Ask::Nobody` on purpose, so
//!   `PlatformRef::url` is always `None` and `write_mrpack` builds
//!   `https://cdn.modrinth.com/data/{project}/versions/{version}/{the name the
//!   platform published}` instead (`instance/modpack.rs`), falling back to the
//!   name on the admin's disk for a record written before that name was kept.
//!   No offline test can fetch one of those URLs; the offline suite pins the
//!   *string* and stops.
//! - No offline test exercises a **successful** index download at all: every
//!   one of them seeds the server's jars into the player's `mods/` first so
//!   `net::already_valid` short-circuits the fetch.
//!
//! So this file installs real mods into a real server, serves the real host,
//! joins with a real player instance, and then walks a sequence of the
//! mutations an admin actually performs — add a mod, edit a config, turn a mod
//! off, delete one, forbid one of the player's own, change nothing — asserting
//! after each one both what the revision did and what the player's instance
//! holds once it has updated.
//!
//! Every test is `#[ignore]`d: each binds a port, speaks real TLS, and talks to
//! api.modrinth.com and cdn.modrinth.com. **Modrinth only** — nothing here
//! needs or reads a CurseForge key.
//!
//! ```sh
//! # one test at a time, so the printed sequence reads as a sequence: seven
//! # tests sharing stdout interleave into something nobody can follow
//! cargo test --test live_pack_sync -- --ignored --nocapture --test-threads=1
//!
//! # somewhere else in the version matrix
//! GEODE_PACK_MC=1.20.1 GEODE_PACK_LOADER_VERSION=0.16.9 \
//!   cargo test --test live_pack_sync -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The knobs, all optional:
//!
//! | variable | default | what it is |
//! |---|---|---|
//! | `GEODE_PACK_MC` | `1.21.1` | the Minecraft version the server runs |
//! | `GEODE_PACK_LOADER_VERSION` | `0.16.9` | the Fabric loader build, which for a Fabric server *is* what a player needs |
//! | `GEODE_PACK_BOTH_MOD` | `lithium` | a Modrinth slug for a mod on both sides, whose published filename carries a `+` |
//! | `GEODE_PACK_CLIENT_MOD` | `sodium` | a Modrinth slug for a client-only mod, installed into `run/client-mods/` |
//! | `GEODE_PACK_EXTRA_MOD` | `fabric-api` | a Modrinth slug added mid-sequence, whose published filename also carries a `+` |
//!
//! Every slug is resolved against the real API and the real published
//! filenames, so a run on another version picks up whatever that version
//! actually has. A slug with no build for the chosen version fails the test
//! rather than being skipped: a sequence that silently lost a mod would assert
//! nothing.
//!
//! What a green run does **not** prove:
//!
//! - **Anybody's screen.** This calls what the commands call, in the order
//!   they call it. `commands.rs` is behind the `desktop` feature, so
//!   `install_joined` and `bring_up_to_date` are reproduced here rather than
//!   invoked, and neither the Share tab nor the join dialog is driven.
//! - **Two networks.** The host is loopback. Provider traffic is real; player
//!   traffic is not.
//! - **CurseForge.** Every reference here is Modrinth's. The asymmetry between
//!   a failed CurseForge *lookup* (soft, lands in `UpdateReport.missing`) and a
//!   failed *download* (hard) is untouched — `update` hardcodes the real
//!   CurseForge API base, so there is nowhere to point a stub, and a real
//!   429 cannot be asked for.
//! - **A race between a build and an admin.** Everything here changes the
//!   server between requests, never during one. A file that is present when
//!   `server_revision` reads it and gone when the walk reaches it is a window
//!   measured in one `read_dir`, and a test that tried to land inside it would
//!   be a coin toss with an assertion attached.
//! - **The 30-second preview deadline.** Whether a real server's description
//!   fits inside it is a question about the admin's disk, and a wall-clock
//!   assertion against a fixed deadline on unknown hardware is a flake, not a
//!   test.
//! - **The host's ceilings.** `MAX_BUILDS`, the connection limits, the
//!   revocation sweep and the five-second preview cache belong to
//!   `share/host.rs`'s own unit tests, which can shrink them. Nothing here
//!   waits one out — and the revision assertions deliberately read
//!   `server_pack_info` directly rather than a preview, so the cache cannot
//!   make them flap.
//! - **That the game starts.** Nothing is launched. "The player holds the
//!   right files" is as far as this goes.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use geode_lib::instance::content::{ContentKind, Origin};
use geode_lib::instance::{self, Instance};
use geode_lib::net::Progress;
use geode_lib::paths::Paths;
use geode_lib::providers::{self, Provider, Version};
use geode_lib::server::share::ServerShare;
use geode_lib::server::{Server, ServerKind};
use geode_lib::share::code::Code;
use geode_lib::share::host::{Host, PackInfo};
use geode_lib::share::identity::Identity;
use geode_lib::share::join;
use geode_lib::share::link::{Address, Link, Pin};
use geode_lib::share::manifest::{Forbidden, Kind, Policy};
use geode_lib::share::pack::{self, Pick};

const DEFAULT_MC: &str = "1.21.1";
/// For a Fabric server the loader build *is* the version a player needs, and
/// `pack::client_loader` refuses a server whose build is empty.
const DEFAULT_LOADER_VERSION: &str = "0.16.9";
const DEFAULT_BOTH_MOD: &str = "lithium";
const DEFAULT_CLIENT_MOD: &str = "sodium";
const DEFAULT_EXTRA_MOD: &str = "fabric-api";

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

fn mc_version() -> String {
    env_or("GEODE_PACK_MC", DEFAULT_MC)
}

fn loader_version() -> String {
    env_or("GEODE_PACK_LOADER_VERSION", DEFAULT_LOADER_VERSION)
}

/// One line of the run, aligned so a human reading `--nocapture` can follow
/// the sequence without counting columns.
fn say(what: &str, detail: impl std::fmt::Display) {
    println!("{what:<12} {detail}");
}

// ---------------------------------------------------------------- the machine

/// A `GEODE_HOME` of its own under the system temporary directory, removed
/// when the test ends however it ends.
///
/// `paths::Scratch` is exactly this and is `#[cfg(test)]`, so it exists for the
/// crate's own unit tests and not for anything under `tests/`. Copied here for
/// the same reason `live_share_join` copies it: a `remove_dir_all` at the end
/// of a test does not run when the test panics, which is precisely when the
/// directory is worth having gone.
struct Scratch {
    paths: Paths,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("geode-live-sync-{tag}-{}", uuid::Uuid::new_v4()));
        // Never the real one, and never inside it. The user's own data
        // directory holds their accounts, their instances and the certificate
        // every link they have handed out is pinned to — and this file
        // generates a certificate, serves it to a socket, and installs mods.
        let real = Paths::new().root;
        assert!(
            !root.starts_with(&real),
            "the scratch home {} is inside the real one at {}",
            root.display(),
            real.display()
        );
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

/// A real jar: a zip carrying a `fabric.mod.json` that `instance::jar::read`
/// understands, so a hand-added mod here is a file the launcher can read
/// rather than random bytes, and the side it declares is a side the pack has
/// to carry.
fn jar(id: &str, name: &str, environment: &str) -> Vec<u8> {
    let declaration = serde_json::json!({
        "schemaVersion": 1,
        "id": id,
        "version": "1.0.0",
        "name": name,
        "environment": environment,
    });
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("fabric.mod.json", options).unwrap();
    zip.write_all(declaration.to_string().as_bytes()).unwrap();
    zip.start_file(format!("{id}/Mod.class"), options).unwrap();
    zip.write_all(&[0xca, 0xfe, 0xba, 0xbe]).unwrap();
    zip.finish().unwrap().into_inner()
}

/// A Fabric server on disk, shared, with the folders a pack is built from.
///
/// Written the way the app writes one: `Server::save` for `server.json`, and
/// `server::share::save` for `share.json`, which is what `share::start` calls
/// once it has decided which code to use.
async fn a_shared_server(paths: &Paths, code: &Code, share: ServerShare) -> Server {
    let mut server = Server::new("Survival".into(), ServerKind::Fabric, mc_version());
    server.id = "survival".into();
    server.build = loader_version();
    let run = paths.server_run(&server.id);
    for folder in ["mods", "client-mods", "client-files", "config"] {
        tokio::fs::create_dir_all(run.join(folder)).await.unwrap();
    }
    server.save(paths).await.unwrap();
    let mut share = share;
    share.code = code.clone();
    geode_lib::server::share::save(paths, &server.id, &share)
        .await
        .unwrap();
    server
}

fn a_share(code: &Code) -> ServerShare {
    ServerShare {
        code: code.clone(),
        include_config: false,
        settings: Default::default(),
        picked: Vec::new(),
        listing: None,
        forbidden: Vec::new(),
        shared: true,
    }
}

/// Install one real Modrinth mod into one of the server's folders, through the
/// same function `commands::install_server_content` reaches: the version is
/// asked of the real API for this server's Minecraft version and loader, and
/// the record written is the one the app would write.
///
/// Answers with the filename on disk and the version it came from, because
/// every assertion below is against the real published names and hashes rather
/// than against anything this file made up.
async fn install_from_modrinth(
    client: &reqwest::Client,
    paths: &Paths,
    server: &mut Server,
    slug: &str,
    kind: ContentKind,
) -> (String, Version) {
    let project = providers::project(client, Provider::Modrinth, slug, None)
        .await
        .unwrap_or_else(|e| panic!("Modrinth would not describe {slug}: {e}"));
    let versions = providers::versions(
        client,
        Provider::Modrinth,
        &project.id,
        Some(&server.mc_version),
        Some("fabric"),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("Modrinth would not list {slug}'s versions: {e}"));
    let version = geode_lib::install::content::pick_best_or_blocked(&versions)
        .unwrap_or_else(|| {
            panic!(
                "{slug} publishes no Fabric build for {} — pick another slug with the env knobs",
                server.mc_version
            )
        })
        .clone();

    let dir = geode_lib::server::content_dir(paths, &server.id, kind).await;
    let report = geode_lib::install::content::install_with_dependencies_into(
        client,
        &dir,
        &mut server.content,
        &version,
        &project,
        kind,
        &server.mc_version,
        "fabric",
        None,
        &Progress::silent("live-sync-install"),
    )
    .await
    .unwrap_or_else(|e| panic!("installing {slug} into the server failed: {e}"));
    // One file, so every later assertion about the pack's contents is about
    // files this test put there. A dependency appearing is a change worth
    // failing on rather than one to absorb silently.
    assert_eq!(
        report.installed.len(),
        1,
        "{slug} brought more than itself: {:?}",
        report.installed
    );
    server.save(paths).await.unwrap();
    let file = report.installed[0].clone();
    say(
        "installed",
        format!(
            "{slug} -> {}/{file}  (project {}, version {})",
            kind.folder(),
            version.project_id,
            version.id
        ),
    );
    (file, version)
}

/// This machine's sharing certificate, which is the fingerprint a player pins.
async fn pin_of(paths: &Paths) -> Pin {
    Identity::load_or_create(paths).await.unwrap().pin()
}

/// A link to this host, built the way `service::local_link_for` builds one:
/// the address read back through the strict parser, because nothing here is
/// special enough to skip it.
fn link_to(port: u16, code: &Code, pin: Pin) -> Link {
    Link {
        address: Address::parse("127.0.0.1", port).expect("an address the parser accepts"),
        code: code.clone(),
        pin: Some(pin),
    }
}

// ------------------------------------------------------------------- the pack

/// Every entry in a `.geodepack`, sorted.
fn entries(pack: &Path) -> Vec<String> {
    let file = std::fs::File::open(pack).expect("the pack was written");
    let zip = zip::ZipArchive::new(file).expect("the pack opens as a zip");
    let mut names: Vec<String> = zip.file_names().map(str::to_string).collect();
    names.sort();
    names
}

/// Every entry a player's files come out of as bytes, with the CRC of those
/// bytes.
///
/// `geode/` is left out: that is the manifest, which carries the revision and
/// the settings half and so changes whenever anything about the share does.
/// `modrinth.index.json` is left out too and compared separately by
/// [`index_facts`], because its `dependencies` is a `HashMap` on the way out —
/// two exports of one unchanged server really do differ in the byte order of
/// those two keys, which cost this file one false failure before it was
/// understood.
fn payload(pack: &Path) -> BTreeMap<String, u32> {
    let file = std::fs::File::open(pack).expect("the pack was written");
    let mut zip = zip::ZipArchive::new(file).expect("the pack opens as a zip");
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i).expect("the entry reads");
        if entry.is_dir()
            || entry.name().starts_with("geode/")
            || entry.name() == "modrinth.index.json"
        {
            continue;
        }
        out.insert(entry.name().to_string(), entry.crc32());
    }
    out
}

/// What the index says, in an order two exports cannot disagree about: the
/// pack's name and version, the loader and game version it declares, and every
/// file it points at with the addresses and hashes beside it.
fn index_facts(pack: &Path) -> String {
    let index: serde_json::Value =
        serde_json::from_slice(&read_entry(pack, "modrinth.index.json"))
            .expect("the index is JSON");
    let dependencies: BTreeMap<String, String> = index["dependencies"]
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut files: Vec<String> = indexed(pack)
        .into_iter()
        .map(|f| format!("{} {:?} {:?} {:?}", f.path, f.downloads, f.sha1, f.size))
        .collect();
    files.sort();
    format!(
        "name={} version={} summary={} deps={dependencies:?}\n{}",
        index["name"],
        index["versionId"],
        index["summary"],
        files.join("\n")
    )
}

/// One file out of a pack, by name.
fn read_entry(pack: &Path, name: &str) -> Vec<u8> {
    let file = std::fs::File::open(pack).expect("the pack was written");
    let mut zip = zip::ZipArchive::new(file).expect("the pack opens as a zip");
    let mut entry = zip
        .by_name(name)
        .unwrap_or_else(|e| panic!("{name} is not in the pack: {e}"));
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("the entry reads");
    bytes
}

/// One entry of the pack's `modrinth.index.json`: the path it lands at, the
/// addresses it names, and the hashes it vouches for.
#[derive(Debug, Clone)]
struct Indexed {
    path: String,
    downloads: Vec<String>,
    sha1: Option<String>,
    size: Option<u64>,
}

/// What a pack says a player should fetch, read back out of the pack itself.
fn indexed(pack: &Path) -> Vec<Indexed> {
    let index: serde_json::Value =
        serde_json::from_slice(&read_entry(pack, "modrinth.index.json"))
            .expect("the index is JSON");
    index["files"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|file| Indexed {
            path: file["path"].as_str().unwrap_or_default().to_string(),
            downloads: file["downloads"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|url| url.as_str().map(str::to_string))
                .collect(),
            sha1: file["hashes"]["sha1"].as_str().map(str::to_string),
            size: file["fileSize"].as_u64(),
        })
        .collect()
}

/// The revision the server would advertise right now, asked of the same
/// function the admin's own screen and the host's description both ask.
///
/// Deliberately not a preview: the host keeps a description for five seconds
/// (`PREVIEW_FRESH`), and a revision assertion that read through that cache
/// would be a clock test wearing an assertion's clothes.
async fn revision_of(paths: &Paths, server: &Server) -> String {
    pack::server_pack_info(paths, server)
        .await
        .unwrap_or_else(|e| panic!("the server would not describe itself: {e}"))
        .revision
}

/// What a player is shown before importing, as the host answers it.
async fn preview_of(link: Link) -> (Link, PackInfo) {
    let preview = join::preview(link)
        .await
        .unwrap_or_else(|e| panic!("the host would not describe its pack: {e}"));
    (preview.link, preview.info)
}

/// Download the pack this host is serving right now.
async fn fetch_pack(paths: &Paths, link: &Link, tag: &str) -> PathBuf {
    let dest = paths.root.join(format!("{tag}.geodepack"));
    join::download(link, &dest, &Progress::silent("live-sync-download"))
        .await
        .unwrap_or_else(|e| panic!("the host would not send its pack: {e}"));
    say(
        "downloaded",
        format!(
            "{tag}.geodepack — {} bytes",
            std::fs::metadata(&dest).unwrap().len()
        ),
    );
    dest
}

// ----------------------------------------------------------------- the player

/// The steps `commands::install_joined` takes with a downloaded archive, run
/// against a real scratch home and answering with the instance they made.
///
/// That function is in `commands.rs`, which is `#[cfg(feature = "desktop")]`,
/// so nothing under `tests/` can call it. What is copied is its order and its
/// checks, not its error wording. **Unlike `live_share_join`'s copy, the
/// client here is the real one** — that is the whole point of this file: a
/// reference in the pack has to be fetched from the provider it names.
async fn install_joined(
    client: &reqwest::Client,
    paths: &Paths,
    link: &Link,
    approved: &PackInfo,
    archive: &Path,
) -> (Instance, Vec<String>) {
    let described = instance::modpack::preview(archive.to_path_buf())
        .await
        .expect("the pack describes itself");
    join::check_matches(approved, &described)
        .expect("the pack is the setup the description promised");

    let manifest = pack::read_manifest(archive)
        .expect("the manifest reads")
        .expect("a pack from a Geode server carries a manifest");
    assert_eq!(
        manifest.kind,
        Kind::Server,
        "a server's pack has to arrive as a server's, or the join refuses it"
    );
    pack::check_server_downloads(archive)
        .await
        .expect("every file in the pack is one the player can get");

    let mut inst = instance::create(
        paths,
        described.name.clone(),
        described.mc_version,
        described.loader,
        described.loader_version,
    )
    .await
    .expect("the instance is created");
    inst.joined = Some(link.clone());
    if !approved.server_id.is_empty() {
        inst.joined_server_id = Some(approved.server_id.clone());
    }
    inst.save(paths).await.expect("the instance saves");

    let imported = pack::import(
        client,
        paths,
        &mut inst,
        archive,
        None,
        &Progress::silent("live-sync-import"),
    )
    .await;
    let revision = pack::pack_revision(&manifest).unwrap_or_else(|| approved.revision.clone());
    let missing = match imported {
        Ok(missing) => {
            if !revision.is_empty() {
                inst.joined_revision = Some(revision);
            }
            missing
        }
        Err(e) => panic!("the import failed: {e}"),
    };
    inst.save(paths).await.expect("the instance saves again");
    (inst, missing)
}

/// What `commands::bring_up_to_date` does once it has decided an update is
/// due: fetch the pack the host is serving now, apply it in place, and record
/// the revision the pack itself carried.
///
/// The decision is not copied — `fetch_due`/`update_due` live behind the
/// `desktop` feature and read an `AppState` — so each caller below states for
/// itself whether the revision had moved, which is the thing being asserted
/// anyway.
async fn update_from(
    client: &reqwest::Client,
    paths: &Paths,
    link: &Link,
    inst: &mut Instance,
    tag: &str,
) -> pack::UpdateReport {
    let archive = fetch_pack(paths, link, tag).await;
    let report = pack::update(
        client,
        paths,
        inst,
        &archive,
        None,
        &Progress::silent("live-sync-update"),
    )
    .await
    .unwrap_or_else(|e| panic!("the update failed: {e}"));
    if let Some(revision) = report.revision.clone() {
        inst.joined_revision = Some(revision);
    }
    inst.save(paths).await.expect("the instance saves");
    report
}

/// Every file directly inside the player's `mods/`, sorted.
fn mods_of(paths: &Paths, inst: &Instance) -> Vec<String> {
    let dir = paths.game_dir(&inst.id).join("mods");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn sha1_of(path: &Path) -> String {
    use sha1::{Digest, Sha1};
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{} unreadable: {e}", path.display()));
    hex::encode(Sha1::digest(&bytes))
}

/// The record a player's instance holds for one file, if it holds one.
fn record_for<'a>(
    inst: &'a Instance,
    kind: ContentKind,
    base: &str,
) -> Option<&'a geode_lib::instance::content::ContentRecord> {
    inst.content
        .iter()
        .find(|r| r.kind == kind && r.file_name == base)
}

// ------------------------------------------------------------------ the tests

/// The spine: a real server, real Modrinth mods, a real player, and six
/// changes an admin actually makes — each one asserted twice over, on the
/// revision the server now advertises and on what the player's instance holds
/// once it has updated.
///
/// What each step pins, in order:
///
/// 0. A joined instance receives a referenced mod by **fetching it from
///    Modrinth** — the pack carries no bytes for it — and receives a
///    hand-added jar as bytes, `options.txt` out of `client-files/`, and a
///    picked config file. The bytes that land are the bytes Modrinth
///    published, checked by SHA-1 against the version the API named.
/// 1. Adding a mod moves the revision, and the update brings it — including
///    through a published filename carrying a `+`, which the constructed URL
///    percent-encodes as `%2B`.
/// 2. Editing a picked `Enforce` config moves the revision, and the admin's
///    copy replaces the player's own edit.
/// 3. Turning a mod off on the server moves the revision, and the player's
///    copy goes off with it rather than being deleted.
/// 4. Deleting a mod moves the revision, and the player's copy and its record
///    both go.
/// 5. Forbidding a mod the *player* installed moves the revision, and their
///    jar is turned off with the admin's reason — without the server ever
///    holding that mod.
/// 6. Changing nothing does not move the revision, and an update run anyway
///    takes nothing away.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port, speaks real TLS, and downloads real mods from Modrinth"]
async fn a_servers_pack_tracks_its_admin_through_a_real_update_cycle() {
    let scratch = Scratch::new("cycle");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();
    let client = geode_lib::net::client();

    // ---- step 0: the admin's server, as the app would leave it
    let code = Code::generate();
    let mut share = a_share(&code);
    share.include_config = true;
    let mut server = a_shared_server(paths, &code, share.clone()).await;
    let run = paths.server_run(&server.id);

    let both_slug = env_or("GEODE_PACK_BOTH_MOD", DEFAULT_BOTH_MOD);
    let client_slug = env_or("GEODE_PACK_CLIENT_MOD", DEFAULT_CLIENT_MOD);
    let (both_file, both_version) =
        install_from_modrinth(&client, paths, &mut server, &both_slug, ContentKind::Mod).await;
    let (client_file, client_version) = install_from_modrinth(
        &client,
        paths,
        &mut server,
        &client_slug,
        ContentKind::ClientMod,
    )
    .await;

    // A jar on no provider, which is the other half of every real server's
    // mods folder and the one that has to travel as bytes.
    let by_hand = "geode-live-byhand.jar";
    tokio::fs::write(
        run.join("mods").join(by_hand),
        jar("geodelivebyhand", "Added By Hand", "*"),
    )
    .await
    .unwrap();

    // The player's own files: `options.txt` exists nowhere else on a server's
    // disk, and `client-files/` is the only way it can travel at all.
    tokio::fs::write(
        run.join("client-files").join("options.txt"),
        "fov:1.0\nrenderDistance:12\n",
    )
    .await
    .unwrap();
    tokio::fs::write(
        run.join("config").join("geode-live.toml"),
        "tick_rate = 20\n",
    )
    .await
    .unwrap();
    // A config file the admin did *not* pick, which must never travel.
    tokio::fs::write(
        run.join("config").join("secrets.toml"),
        "bot_token = \"not-for-players\"\n",
    )
    .await
    .unwrap();

    share
        .pick(&[
            Pick {
                path: "config/geode-live.toml".into(),
                policy: Policy::Enforce,
            },
            Pick {
                path: "client-files/options.txt".into(),
                policy: Policy::Enforce,
            },
        ])
        .expect("both picks are inside the two settings folders");
    geode_lib::server::share::save(paths, &server.id, &share)
        .await
        .unwrap();

    let pin = pin_of(paths).await;
    let host = Host::start(paths.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();
    let link = link_to(port, &code, pin);
    say("server", format!("{} {} fabric {}", server.name, server.mc_version, server.build));
    say("host", format!("listening on {}", host.local_addr));

    let joined_revision = revision_of(paths, &server).await;
    let (link, info) = preview_of(link).await;
    say("preview", format!(
        "{} mods, {} server-only, {} config files, revision {}",
        info.mods, info.server_mods, info.config_files, &info.revision[..12]
    ));
    assert_eq!(info.mods, 3, "three jars travel to the player");
    assert_eq!(
        info.config_files, 2,
        "the two picked settings files are what is promised"
    );

    let archive = fetch_pack(paths, &link, "join").await;
    for name in entries(&archive) {
        say("entry", name);
    }
    // The mod with a complete Modrinth record travels as a reference, so its
    // bytes are *not* in the pack. That is the claim nothing else checks: the
    // only copy a player can get is the URL the index names.
    let names = entries(&archive);
    assert!(
        !names.iter().any(|n| n.ends_with(&both_file)),
        "a Modrinth-recorded mod should travel as a reference, not as bytes: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with(by_hand)),
        "a jar on no provider has to travel as bytes: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("secrets.toml")),
        "a config file the admin did not pick travelled: {names:?}"
    );

    let (mut inst, missing) =
        install_joined(&client, paths, &link, &info, &archive).await;
    assert!(missing.is_empty(), "nothing should be left to fetch by hand: {missing:?}");
    let game = paths.game_dir(&inst.id);
    for name in mods_of(paths, &inst) {
        say("player mod", name);
    }

    // The referenced mod arrived, and arrived as the bytes Modrinth published
    // — which is what proves the constructed CDN URL resolved and that the
    // SHA-1 the index vouched for is the SHA-1 of what came back.
    let both_on_disk = game.join("mods").join(&both_file);
    assert!(
        both_on_disk.is_file(),
        "{both_file} was referenced and never fetched"
    );
    let published_sha1 = both_version
        .primary_file()
        .and_then(|f| f.sha1.clone())
        .expect("Modrinth published a sha1 for the file it serves");
    assert_eq!(
        sha1_of(&both_on_disk).to_lowercase(),
        published_sha1.to_lowercase(),
        "{both_file} landed, but not as the bytes Modrinth published"
    );
    // The client-only mod lives in `client-mods/` on the server and in `mods/`
    // in the player's game.
    let client_on_disk = game.join("mods").join(&client_file);
    assert!(
        client_on_disk.is_file(),
        "{client_file} did not reach the player's mods folder"
    );
    assert_eq!(
        sha1_of(&client_on_disk).to_lowercase(),
        client_version
            .primary_file()
            .and_then(|f| f.sha1.clone())
            .expect("Modrinth published a sha1")
            .to_lowercase(),
    );
    assert!(game.join("mods").join(by_hand).is_file(), "the bytes did not land");
    assert_eq!(
        std::fs::read_to_string(game.join("options.txt")).unwrap(),
        "fov:1.0\nrenderDistance:12\n",
        "options.txt travels out of client-files/ to the top of the game directory"
    );
    assert_eq!(
        std::fs::read_to_string(game.join("config").join("geode-live.toml")).unwrap(),
        "tick_rate = 20\n"
    );
    assert!(
        !game.join("config").join("secrets.toml").exists(),
        "a config file the admin did not pick landed on a player"
    );
    // Every one of them is the server's, which is what makes it the server's
    // to update and not the player's to delete.
    for (kind, base) in [
        (ContentKind::Mod, both_file.as_str()),
        (ContentKind::Mod, client_file.as_str()),
        (ContentKind::Mod, by_hand),
    ] {
        let record = record_for(&inst, kind, base)
            .unwrap_or_else(|| panic!("{base} arrived with no record"));
        assert_eq!(record.origin, Origin::Server, "{base} is the server's");
    }
    assert_eq!(
        inst.joined_revision.as_deref(),
        Some(joined_revision.as_str()),
        "the instance keeps the revision the pack it received carried"
    );
    say("step 0", "joined: references fetched, bytes landed, settings landed");

    // ---- step 1: the admin adds a mod
    let extra_slug = env_or("GEODE_PACK_EXTRA_MOD", DEFAULT_EXTRA_MOD);
    let (extra_file, extra_version) =
        install_from_modrinth(&client, paths, &mut server, &extra_slug, ContentKind::Mod).await;
    let after_add = revision_of(paths, &server).await;
    assert_ne!(
        after_add, joined_revision,
        "adding a mod has to move the revision, or no player ever hears about it"
    );
    let report = update_from(&client, paths, &link, &mut inst, "add").await;
    assert!(report.missing.is_empty(), "nothing left behind: {:?}", report.missing);
    let extra_on_disk = game.join("mods").join(&extra_file);
    assert!(
        extra_on_disk.is_file(),
        "{extra_file} was added to the server and did not reach the player"
    );
    assert_eq!(
        sha1_of(&extra_on_disk).to_lowercase(),
        extra_version
            .primary_file()
            .and_then(|f| f.sha1.clone())
            .expect("Modrinth published a sha1")
            .to_lowercase(),
        "{extra_file} landed, but not as the bytes Modrinth published"
    );
    assert_eq!(inst.joined_revision.as_deref(), Some(after_add.as_str()));
    say(
        "step 1",
        format!("added {extra_slug}: revision moved, {extra_file} fetched"),
    );
    if extra_file.contains('+') || both_file.contains('+') {
        say(
            "note",
            "a published filename carrying '+' round-tripped through the \
             constructed URL's %2B",
        );
    }

    // ---- step 2: the admin edits a picked Enforce config
    // The player edits it first, so what is asserted is that Enforce really
    // replaces their copy rather than that an absent file was written.
    tokio::fs::write(
        game.join("config").join("geode-live.toml"),
        "tick_rate = 1\n",
    )
    .await
    .unwrap();
    tokio::fs::write(
        run.join("config").join("geode-live.toml"),
        "tick_rate = 40\n",
    )
    .await
    .unwrap();
    let after_config = revision_of(paths, &server).await;
    assert_ne!(
        after_config, after_add,
        "a change to an Enforce config file's bytes has to move the revision"
    );
    let report = update_from(&client, paths, &link, &mut inst, "config").await;
    assert!(
        report.not_written.is_empty(),
        "the config should have been writable: {:?}",
        report.not_written
    );
    assert_eq!(
        std::fs::read_to_string(game.join("config").join("geode-live.toml")).unwrap(),
        "tick_rate = 40\n",
        "Enforce means the admin's copy replaces the player's on every update"
    );
    say("step 2", "config edited: revision moved, the admin's copy won");

    // ---- step 3: the admin turns a mod off
    let off_name = instance::content::set_enabled_in(&run.join("mods"), &both_file, false)
        .await
        .expect("the server's own mod can be turned off");
    server.save(paths).await.unwrap();
    let after_off = revision_of(paths, &server).await;
    assert_ne!(
        after_off, after_config,
        "turning a mod off changes what players run, so it has to move the revision"
    );
    let turned_off_pack = fetch_pack(paths, &link, "off-inspect").await;
    // Worth saying out loud, because it is why this pack is suddenly hundreds
    // of times bigger: `Carry::SERVER_PACK` has `off_state: false`, so a jar
    // the admin turned off stops being a reference and travels as bytes. A
    // player's copy therefore goes off without anybody fetching anything.
    assert!(
        entries(&turned_off_pack)
            .iter()
            .any(|n| n.ends_with(&format!("{both_file}.disabled"))),
        "a mod turned off on the server travels as bytes, not as a reference: {:?}",
        entries(&turned_off_pack)
    );
    let report = update_from(&client, paths, &link, &mut inst, "off").await;
    assert!(report.missing.is_empty(), "{:?}", report.missing);
    let player_mods = mods_of(paths, &inst);
    assert!(
        player_mods.iter().any(|n| n == &format!("{both_file}.disabled")),
        "the server turned {both_file} off; the player's copy should be off too: {player_mods:?}"
    );
    assert!(
        !player_mods.iter().any(|n| n == &both_file),
        "the enabled copy is still there beside the disabled one: {player_mods:?}"
    );
    say(
        "step 3",
        format!("turned off {off_name}: revision moved, the player's copy went off"),
    );

    // ---- step 4: the admin deletes the hand-added jar
    tokio::fs::remove_file(run.join("mods").join(by_hand))
        .await
        .unwrap();
    let after_delete = revision_of(paths, &server).await;
    assert_ne!(after_delete, after_off, "deleting a mod has to move the revision");
    let report = update_from(&client, paths, &link, &mut inst, "delete").await;
    assert!(report.missing.is_empty(), "{:?}", report.missing);
    assert!(
        !game.join("mods").join(by_hand).exists(),
        "a mod the server dropped is still in the player's mods folder"
    );
    assert!(
        record_for(&inst, ContentKind::Mod, by_hand).is_none(),
        "the record for a departed mod outlived the file"
    );
    say("step 4", "deleted the hand-added jar: revision moved, the player's copy went");

    // ---- step 5: the admin forbids a mod the player installed themselves
    let players_own = "geode-live-playersown.jar";
    tokio::fs::write(
        game.join("mods").join(players_own),
        jar("geodeliveplayersown", "The Player's Own", "client"),
    )
    .await
    .unwrap();
    share.forbidden = vec![Forbidden {
        provider: String::new(),
        project_id: String::new(),
        mod_id: "geodeliveplayersown".into(),
        reason: "it lets you see through walls".into(),
    }];
    geode_lib::server::share::save(paths, &server.id, &share)
        .await
        .unwrap();
    let after_ban = revision_of(paths, &server).await;
    assert_ne!(
        after_ban, after_delete,
        "a new rule about what players may run has to move the revision"
    );
    let report = update_from(&client, paths, &link, &mut inst, "ban").await;
    assert_eq!(report.banned.len(), 1, "the ban should name one mod: {:?}", report.banned);
    assert_eq!(report.banned[0].reason, "it lets you see through walls");
    let player_mods = mods_of(paths, &inst);
    assert!(
        player_mods.iter().any(|n| n == &format!("{players_own}.disabled")),
        "a forbidden mod of the player's own should be off: {player_mods:?}"
    );
    // The half of this step the doc comment claims and nothing asserted: the
    // ban reached a jar the server has never held a copy of, so what turned it
    // off is the rule and not the pack having sent the same mod.
    assert!(
        !run.join("mods").join(players_own).exists()
            && !run.join("client-mods").join(players_own).exists(),
        "the server holds this mod itself, so the player's copy could have been \
         turned off by the pack carrying it rather than by the rule"
    );
    say(
        "step 5",
        format!("forbade geodeliveplayersown: {} turned off with a reason", report.banned[0].name),
    );

    // ---- step 6: the admin changes nothing
    let unchanged = revision_of(paths, &server).await;
    assert_eq!(
        unchanged, after_ban,
        "asking twice with nothing changed has to give the same revision"
    );
    let before = mods_of(paths, &inst);
    let report = update_from(&client, paths, &link, &mut inst, "again").await;
    assert_eq!(
        mods_of(paths, &inst),
        before,
        "an update that nothing changed took something away"
    );
    assert_eq!(
        report.banned.len(),
        1,
        "a ban is a standing rule, so it is said again on every update"
    );
    assert_eq!(inst.joined_revision.as_deref(), Some(after_ban.as_str()));
    say("step 6", "nothing changed: same revision, same files, ban restated");

    host.stop().await;
}

/// The URL a server's pack constructs for a referenced mod is the URL Modrinth
/// actually serves, and the hashes beside it describe what that URL returns.
///
/// `export_server` identifies with `Ask::Nobody` on purpose — a lookup that
/// succeeded in the pack build and not in `server_pack_info` would advertise a
/// revision the pack does not have and push an update to every player — so
/// `PlatformRef::url` is always `None` for a server's pack and `write_mrpack`
/// builds the address itself out of the record's ids and the **name on the
/// admin's disk**. The offline suite pins that string
/// (`a_servers_pack_is_built_without_touching_the_network`) and nothing has
/// ever fetched it. This does, with a plain client and no Geode machinery in
/// the way, and checks what comes back against the index's own sha1 and size.
///
/// Both default slugs publish filenames containing `+`, which
/// `urlencoding::encode` writes as `%2B`: whether that round-trips through the
/// real CDN is an empirical question the code alone cannot answer, and this is
/// where it is answered.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads real mods from Modrinth and fetches the URLs a pack constructs"]
async fn the_url_a_server_pack_constructs_is_the_one_modrinth_serves() {
    let scratch = Scratch::new("urls");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();
    let client = geode_lib::net::client();

    let code = Code::generate();
    let mut server = a_shared_server(paths, &code, a_share(&code)).await;
    for (slug, kind) in [
        (env_or("GEODE_PACK_BOTH_MOD", DEFAULT_BOTH_MOD), ContentKind::Mod),
        (env_or("GEODE_PACK_EXTRA_MOD", DEFAULT_EXTRA_MOD), ContentKind::Mod),
        (
            env_or("GEODE_PACK_CLIENT_MOD", DEFAULT_CLIENT_MOD),
            ContentKind::ClientMod,
        ),
    ] {
        install_from_modrinth(&client, paths, &mut server, &slug, kind).await;
    }

    let dest = paths.root.join("urls.geodepack");
    pack::export_server(
        paths,
        &server,
        dest.clone(),
        &Progress::silent("live-sync-export"),
    )
    .await
    .expect("the pack is written");
    // Geode's own gate on a server pack's addresses: it checks the host and
    // the scheme and nothing else, which is exactly why fetching them here is
    // worth doing.
    pack::check_server_downloads(&dest)
        .await
        .expect("every URL is one Geode will fetch from");

    let files = indexed(&dest);
    assert_eq!(
        files.len(),
        3,
        "all three Modrinth mods should be references, not bytes: {files:#?}"
    );
    let mut plus_seen = false;
    for file in &files {
        let url = file
            .downloads
            .first()
            .unwrap_or_else(|| panic!("{} names no address", file.path));
        plus_seen |= url.contains("%2B");
        let expected = file
            .sha1
            .clone()
            .unwrap_or_else(|| panic!("{} vouches for no sha1", file.path));
        let response = client
            .get(url)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .unwrap_or_else(|e| panic!("fetching {url} failed outright: {e}"));
        let status = response.status();
        let bytes = response.bytes().await.expect("the whole answer").to_vec();
        say(
            "fetched",
            format!("{status} {} bytes  {url}", bytes.len()),
        );
        assert!(
            status.is_success(),
            "the pack points every player at {url}, and Modrinth answered {status}"
        );
        use sha1::{Digest, Sha1};
        assert_eq!(
            hex::encode(Sha1::digest(&bytes)).to_lowercase(),
            expected.to_lowercase(),
            "{} vouches for a sha1 that is not what {url} serves",
            file.path
        );
        if let Some(size) = file.size {
            assert_eq!(size, bytes.len() as u64, "{} claims the wrong size", file.path);
        }
    }
    assert!(
        plus_seen,
        "none of the default slugs published a filename with a '+' in it any \
         more, so the %2B half of this test proved nothing — pick a slug that \
         does with GEODE_PACK_BOTH_MOD or GEODE_PACK_EXTRA_MOD"
    );
}

/// A jar renamed on the admin's disk and then re-identified ships an address
/// that resolves, because the record carries the name the author published.
///
/// This is the other side of the test above, and the reason that one is worth
/// having. The URL in a server's pack is built rather than looked up, and it
/// used to be built from the name on the admin's disk:
/// `install::content::link_untracked_in` resolved the bytes by hash, got the
/// author's own `filename` back with them, and wrote a record keyed to the
/// on-disk name while throwing that filename away. Every player was then sent
/// to an address Modrinth has never heard of, and one dead reference fails the
/// whole update (`fetch_all` is fail-fast) rather than landing in a report —
/// so the admin heard nothing, because the export itself had succeeded.
///
/// What this test pinned until the fix was exactly that gap: the export
/// succeeded, `check_server_downloads` passed it, and the address answered 404
/// or 400. `ContentRecord::provider_file_name` closes it — recorded where the
/// matched file is already in hand, and preferred over the name on disk
/// wherever `write_mrpack` has to build an address. What is asserted now is
/// the whole of the fix, end to end against the real CDN: the record carries
/// the published name, the pack's address is built from it, the file still
/// travels as a reference rather than as bytes, and what the CDN serves is the
/// bytes the index vouches for.
///
/// The one thing deliberately *not* asserted is the path: the entry stays
/// `mods/<the admin's name>`, because where the file sits on their disk is
/// theirs to decide and the pack says where it goes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads a real mod from Modrinth and fetches the address a renamed jar ships"]
async fn a_renamed_and_relinked_jar_ships_the_published_name() {
    let scratch = Scratch::new("renamed");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();
    let client = geode_lib::net::client();

    let code = Code::generate();
    let mut server = a_shared_server(paths, &code, a_share(&code)).await;
    let slug = env_or("GEODE_PACK_BOTH_MOD", DEFAULT_BOTH_MOD);
    let (installed, _) =
        install_from_modrinth(&client, paths, &mut server, &slug, ContentKind::Mod).await;

    // The admin's rename. In the app this is the ordinary case of a jar that
    // arrived under a name of its own — a browser's `… (1).jar`, a GitHub
    // release asset, a copy made for load order — added through the Content
    // tab's own file picker, which copies it in under whatever it was called.
    let mods = paths.server_run(&server.id).join("mods");
    let renamed = "01-load-me-first.jar";
    tokio::fs::rename(mods.join(&installed), mods.join(renamed))
        .await
        .unwrap();
    // And the record goes with it, so the orphaned one cannot be what makes
    // this fail: this is the state "Identify files" leaves behind.
    server.content.clear();
    server.save(paths).await.unwrap();

    let report = geode_lib::install::content::link_untracked_in(
        &client,
        &[(ContentKind::Mod, mods.clone())],
        &mut server.content,
        &instance::jar::MetaCache::new(),
        None,
        &Progress::silent("live-sync-link"),
    )
    .await
    .expect("the identify pass runs");
    assert_eq!(
        report.identified, 1,
        "Modrinth should recognise the bytes by hash whatever the file is called"
    );
    server.save(paths).await.unwrap();
    let record = server
        .content
        .first()
        .expect("the identify pass wrote a record");
    assert_eq!(
        record.file_name, renamed,
        "the record is keyed to the name on disk, which is where the file is"
    );
    assert_eq!(
        record.provider_file_name.as_deref(),
        Some(installed.as_str()),
        "the hash match had the author's own filename in hand and the record \
         has to keep it — this is the field the whole fix rests on"
    );
    say(
        "relinked",
        format!(
            "{renamed} -> project {}, version {}, published as {installed}",
            record.project_id, record.version_id
        ),
    );

    let dest = paths.root.join("renamed.geodepack");
    pack::export_server(
        paths,
        &server,
        dest.clone(),
        &Progress::silent("live-sync-export"),
    )
    .await
    .expect("the export succeeds");
    pack::check_server_downloads(&dest)
        .await
        .expect("Geode's own gate passes it");

    let files = indexed(&dest);
    assert_eq!(files.len(), 1, "the jar travels as a reference: {files:#?}");
    assert_eq!(
        files[0].path,
        format!("mods/{renamed}"),
        "the pack puts the file where the admin keeps it"
    );
    let url = files[0].downloads.first().expect("an address").clone();
    assert!(
        !url.ends_with(renamed),
        "the address was built from the name on the admin's disk, which is the \
         bug this test used to pin: {url}"
    );
    assert!(
        url.ends_with(&urlencoding::encode(&installed).to_string()),
        "the address should end in the name Modrinth published, {installed}: {url}"
    );
    // And the bytes are not in the pack either, so this address is the only
    // copy a player can get — which is what makes it worth fetching.
    assert!(
        !entries(&dest).iter().any(|n| n.ends_with(renamed)),
        "the bytes travelled after all, which would make the address moot"
    );

    let response = client
        .get(&url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .unwrap_or_else(|e| panic!("fetching {url} failed outright: {e}"));
    let status = response.status();
    let bytes = response.bytes().await.expect("the whole answer").to_vec();
    say("fetched", format!("{status} {} bytes  {url}", bytes.len()));
    assert!(
        status.is_success(),
        "the pack points every player at {url}, and Modrinth answered {status} \
         — a 429 or a 503 means the run says nothing and should be repeated, \
         and a 404 means the published name is not reaching the address"
    );
    // Served *and* the right file: the index vouches for the bytes on the
    // admin's disk, and an address that resolves to some other artifact of the
    // same version would fail every player's checksum instead of their fetch.
    use sha1::{Digest, Sha1};
    let vouched = files[0].sha1.clone().expect("the index vouches for a sha1");
    assert_eq!(
        hex::encode(Sha1::digest(&bytes)).to_lowercase(),
        vouched.to_lowercase(),
        "{url} serves bytes the pack does not vouch for"
    );
    say(
        "finding",
        format!(
            "a jar renamed on the admin's disk ships {installed}, which answers \
             {status} with the bytes the pack vouches for"
        ),
    );
}

/// One reference the provider will not answer for costs the whole update
/// rather than landing in `UpdateReport.missing` — and the next update heals
/// it.
///
/// The error does name the file, and this test asserts that it does. What
/// never names it is the *report*, because there is no report: the update
/// returns `Err` and the player's Geode has a failure where it would have had
/// a list of things to fetch by hand.
///
/// `net::fetch_all` is fail-fast (`let label = res?`), `import_into_at`
/// propagates with a bare `?`, and `update`'s `outcome?` returns before any
/// `UpdateReport` is built. The only soft channel in the Modrinth arm fires
/// for an entry carrying *no* URL at all, and a constructed URL is always
/// there — so a dead reference cannot be reported, only thrown.
///
/// Both halves are asserted, because only the pair is the truth: the update
/// fails, and the files that did land are still the server's, the instance
/// stays on the build it had, and fixing the one bad record on the admin's
/// side makes the very next update bring everything. "Everything but one mod
/// is abandoned" is *not* asserted — it is not what happens.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port, speaks real TLS, and downloads real mods from Modrinth"]
async fn one_dead_reference_fails_the_update_rather_than_landing_in_missing() {
    let scratch = Scratch::new("dead");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();
    let client = geode_lib::net::client();

    let code = Code::generate();
    let mut server = a_shared_server(paths, &code, a_share(&code)).await;
    let live_slug = env_or("GEODE_PACK_BOTH_MOD", DEFAULT_BOTH_MOD);
    let dead_slug = env_or("GEODE_PACK_CLIENT_MOD", DEFAULT_CLIENT_MOD);
    let (live_file, _) =
        install_from_modrinth(&client, paths, &mut server, &live_slug, ContentKind::Mod).await;
    let (dead_file, dead_version) =
        install_from_modrinth(&client, paths, &mut server, &dead_slug, ContentKind::Mod).await;

    // The upstream version is gone: deleted, or a project taken down, after
    // the record was written. Reproduced by pointing the record at a
    // well-formed Modrinth version id that nothing is published at, which is
    // the same state the admin's disk would be in and needs nothing of
    // Modrinth's to be arranged.
    let real_version = dead_version.id.clone();
    let bogus = "AAAAAAAA".to_string();
    assert_ne!(real_version, bogus);
    for record in server.content.iter_mut() {
        if record.file_name == dead_file {
            record.version_id = bogus.clone();
        }
    }
    server.save(paths).await.unwrap();

    let pin = pin_of(paths).await;
    let host = Host::start(paths.clone(), 0, None).await.unwrap();
    let link = link_to(host.local_addr.port(), &code, pin);

    // A fresh instance, joined by hand rather than through `install_joined`,
    // because the import is the thing that is going to fail and the helper
    // asserts it does not.
    let (link, info) = preview_of(link).await;
    let archive = fetch_pack(paths, &link, "dead").await;
    let described = instance::modpack::preview(archive.clone()).await.unwrap();
    let mut inst = instance::create(
        paths,
        described.name.clone(),
        described.mc_version.clone(),
        described.loader,
        described.loader_version.clone(),
    )
    .await
    .unwrap();
    inst.joined = Some(link.clone());
    inst.joined_server_id = Some(info.server_id.clone());
    // A revision it is already on, so what the failure does to it can be seen.
    inst.joined_revision = Some("a-revision-from-before".into());
    inst.save(paths).await.unwrap();
    let held = inst.joined_revision.clone();
    let (was_mc, was_loader) = (inst.mc_version.clone(), inst.loader_version.clone());

    let failed = pack::update(
        &client,
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("live-sync-update"),
    )
    .await;
    let error = failed
        .err()
        .map(|e| e.to_string())
        .expect("one dead reference fails the whole update");
    say("failed", &error);
    assert!(
        error.contains(&dead_file),
        "the failure should at least name the file it was fetching: {error}"
    );
    inst.save(paths).await.unwrap();
    assert_eq!(
        inst.joined_revision, held,
        "a failed update must not record the revision, or nothing would ever fetch again"
    );
    assert_eq!(inst.mc_version, was_mc, "the instance moved version on a failed update");
    assert_eq!(inst.loader_version, was_loader);
    // What did land is still settled as the server's, which is the half of
    // this that is already pinned offline and worth re-proving live.
    //
    // Asserted over whatever is in the folder rather than over the live mod by
    // name. Which references finish before the dead one gives up is not this
    // test's to decide — `net::fetch_all` runs them together and returns the
    // first error — and `if the_live_one_landed { assert }` checks nothing at
    // all on the run where it did not. This holds either way: every whole jar
    // in the player's folder is one the server sent, with a record saying so.
    // A `.part` is a fetch that was cancelled rather than a file that landed.
    let game = paths.game_dir(&inst.id);
    let landed: Vec<String> = mods_of(paths, &inst)
        .into_iter()
        .filter(|name| name.ends_with(".jar"))
        .collect();
    say("landed", format!("{landed:?}"));
    for base in &landed {
        let record = record_for(&inst, ContentKind::Mod, base)
            .unwrap_or_else(|| panic!("{base} is in the player's mods folder with no record"));
        assert_eq!(record.origin, Origin::Server, "{base} is the server's");
    }

    // The admin fixes the record. The very next update brings everything,
    // which is what says the failure above was the one bad reference and not
    // the pack.
    for record in server.content.iter_mut() {
        if record.file_name == dead_file {
            record.version_id = real_version.clone();
        }
    }
    server.save(paths).await.unwrap();
    let report = update_from(&client, paths, &link, &mut inst, "healed").await;
    assert!(report.missing.is_empty(), "{:?}", report.missing);
    for file in [&live_file, &dead_file] {
        assert!(
            game.join("mods").join(file).is_file(),
            "{file} is still missing after the record was fixed"
        );
    }
    say("healed", "the record fixed, the next update brought both mods");
    host.stop().await;
}

/// A behaviour chosen for a folder whose files nobody receives does not move
/// the revision, and one chosen for a folder whose files do travel does.
///
/// `server_revision`'s settings loop is the one of its three that had no
/// travel filter, against its own comment directly above it: "Only a path
/// whose files actually travel. A behaviour chosen for a folder this pack does
/// not send changes nothing for anybody, and a revision that moved for it
/// would send every player a pack over a choice none of them can see." The two
/// loops beneath it filter — overrides on `policy_for`, config files on
/// `row.sent`.
///
/// The state is two clicks away: tick "Include the config folder", which is
/// the only thing that lets `client-files/` travel at all, narrow the picker
/// to `client-files/options.txt` alone, and the folder-level behaviour radio is
/// still in front of the admin, because `share_view` gates it on
/// `include_config` and never on what was picked.
///
/// Three steps, and the third is what stops the first two passing vacuously —
/// a filter that dropped *every* settings line would satisfy them:
///
/// 1. The folder's behaviour changes while no `config/` file travels: the
///    revision holds still, and the payload every player would re-download is
///    the same bytes and the same addresses.
/// 2. A `config/` file is picked: the revision moves, because a file nobody
///    had is now coming.
/// 3. The folder's behaviour changes again, now that it covers a file that
///    does travel, **between two behaviours that are both not Enforce**: the
///    revision moves. So the line is in the revision, and step 1 was a filter
///    doing its job rather than a line nobody writes.
///
/// The qualification in step 3 is the whole of it, and this test shipped
/// without it. Stepping *away from* Enforce changes the covered file's own
/// `config-file` line from `sha1:<hash>` to `sent`, so the revision moves
/// whether or not a settings line exists — measured, not reasoned about: with
/// `server_revision`'s settings loop deleted outright, an Enforce-to-Merge
/// control passes. Merge to ship-once leaves that line reading `sent` either
/// side, which leaves the folder's own line as the only thing that can move
/// the revision.
///
/// Needs no network — every mod here is a jar this file made — but it is
/// `#[ignore]`d with the rest of the file, because it builds real packs and
/// belongs to the same subject.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "builds real packs against a scratch home; needs no network"]
async fn a_folder_behaviour_moves_the_revision_only_when_its_files_travel() {
    let scratch = Scratch::new("policy");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let code = Code::generate();
    let mut share = a_share(&code);
    share.include_config = true;
    let server = a_shared_server(paths, &code, share.clone()).await;
    let run = paths.server_run(&server.id);

    // One jar so the pack is not empty, by hand so no provider is needed.
    tokio::fs::write(
        run.join("mods").join("geode-live-byhand.jar"),
        jar("geodelivebyhand", "Added By Hand", "*"),
    )
    .await
    .unwrap();
    // A config folder with files in it, neither of which the admin picks, and
    // one client file, which is the only thing that travels.
    tokio::fs::write(run.join("config").join("a.toml"), "a = 1\n")
        .await
        .unwrap();
    tokio::fs::write(run.join("config").join("b.toml"), "b = 2\n")
        .await
        .unwrap();
    tokio::fs::write(run.join("client-files").join("options.txt"), "fov:1.0\n")
        .await
        .unwrap();

    let options_pick = || Pick {
        path: "client-files/options.txt".into(),
        policy: Policy::ShipOnce,
    };
    share
        .pick(&[options_pick()])
        .expect("a client file is a settings file");
    geode_lib::server::share::save(paths, &server.id, &share)
        .await
        .unwrap();

    let before = revision_of(paths, &server).await;
    let first = paths.root.join("before.geodepack");
    pack::export_server(paths, &server, first.clone(), &Progress::silent("p1"))
        .await
        .unwrap();
    let info = pack::server_pack_info(paths, &server).await.unwrap();
    assert_eq!(
        info.config_files, 1,
        "only the one client file travels, so no config/ file reaches anybody"
    );
    for name in entries(&first) {
        say("entry", name);
    }
    assert!(
        !entries(&first).iter().any(|n| n.ends_with("a.toml")),
        "a config file nobody picked travelled: {:?}",
        entries(&first)
    );

    // ---- 1: the folder's radio, while no file under it travels
    for (step, policy) in [("merge", Policy::Merge), ("enforce", Policy::Enforce)] {
        share.send_config(true, Some(policy));
        geode_lib::server::share::save(paths, &server.id, &share)
            .await
            .unwrap();
        assert_eq!(
            share.settings.policy_for(Path::new("config")),
            policy,
            "the click has to have been recorded, or this step asserts nothing"
        );
        let after = revision_of(paths, &server).await;
        let second = paths.root.join(format!("after-{step}.geodepack"));
        pack::export_server(paths, &server, second.clone(), &Progress::silent("p2"))
            .await
            .unwrap();
        say(
            "policy",
            format!(
                "config -> {}: revision {} -> {}",
                policy.as_str(),
                &before[..12],
                &after[..12]
            ),
        );
        // The pair is the claim. The payload is identical, so a player who
        // re-downloaded would receive nothing that changes a file — and the
        // revision holds still, so no player is sent to fetch it.
        assert_eq!(
            payload(&first),
            payload(&second),
            "the bytes a player receives changed, so this step is not the case \
             the assertion below is about"
        );
        assert_eq!(
            index_facts(&first),
            index_facts(&second),
            "the addresses a player fetches from changed, same objection"
        );
        assert_eq!(
            after, before,
            "choosing {} for the config folder moved the revision while no file \
             under it travels — every joined player then re-downloads a pack \
             identical to the one they have. share/pack.rs's settings loop in \
             `server_revision` needs the same travel filter as the two loops \
             beneath it",
            policy.as_str()
        );
    }

    // ---- 2: a config file is picked, which is a change players do receive
    share
        .pick(&[
            options_pick(),
            Pick {
                path: "config/a.toml".into(),
                policy: Policy::Enforce,
            },
        ])
        .expect("a config file is a settings file");
    geode_lib::server::share::save(paths, &server.id, &share)
        .await
        .unwrap();
    let picked_one = revision_of(paths, &server).await;
    assert_ne!(
        picked_one, before,
        "picking a config file sends every player a file they did not have, so \
         the revision has to move"
    );
    let third = paths.root.join("picked.geodepack");
    pack::export_server(paths, &server, third.clone(), &Progress::silent("p3"))
        .await
        .unwrap();
    assert!(
        entries(&third).iter().any(|n| n.ends_with("a.toml")),
        "the picked config file should be in the pack: {:?}",
        entries(&third)
    );
    say("policy", format!("picked config/a.toml: revision -> {}", &picked_one[..12]));

    // ---- 3: the control. The folder now covers a file that travels, so its
    // behaviour is part of what players get and the line is in the revision.
    //
    // Not the obvious step, and the reason is worth writing down because this
    // test was shipped with the obvious one. `Settings::choose` writes nothing
    // when it is handed the behaviour already in force, so the pick above —
    // `config/a.toml` with Enforce, under a folder already set to Enforce —
    // wrote no row of its own, and that file takes its behaviour from the
    // folder (`settings_sent` asks `Settings::covering`). Clicking the folder
    // away from Enforce therefore changes that *file's* policy, and
    // `server_revision` writes an Enforce file as `config-file <rel>
    // sha1:<hash>` and every other as `config-file <rel> sent` — so an
    // Enforce-to-Merge control moves the revision on the config-file line and
    // says nothing whatever about the settings line. Measured: with the
    // settings loop deleted outright, it still passes.
    assert_eq!(share.settings.policy_for(Path::new("config")), Policy::Enforce);
    share.send_config(true, Some(Policy::Merge));
    geode_lib::server::share::save(paths, &server.id, &share)
        .await
        .unwrap();
    let merging = revision_of(paths, &server).await;
    assert_ne!(
        share.settings.policy_for(Path::new("config/a.toml")),
        Policy::Enforce,
        "the near side of the control has to be a behaviour that is not Enforce, \
         or the file's own config-file line carries a hash on one side of the \
         step and not the other"
    );
    say(
        "policy",
        format!(
            "config -> merge: revision {} -> {} (not the control: config/a.toml's \
             own line moved off its hash with it)",
            &picked_one[..12],
            &merging[..12]
        ),
    );

    // The control proper: merge to ship-once. Both sides are behaviours that
    // are not Enforce, so `config/a.toml`'s own line reads `sent` either way;
    // the client file's behaviour is its own and does not move; nothing has
    // been pushed, so there are no `setting` lines; and the mods and the
    // header are untouched. The folder's own `settings config <policy> <from>`
    // line is the only thing left that can move this.
    share.send_config(true, Some(Policy::ShipOnce));
    geode_lib::server::share::save(paths, &server.id, &share)
        .await
        .unwrap();
    for named in ["config", "config/a.toml"] {
        assert_ne!(
            share.settings.policy_for(Path::new(named)),
            Policy::Enforce,
            "{named} is Enforce on one side of the control, so config/a.toml's \
             config-file line carries a hash there and not on the other, and the \
             revision would move for that rather than for the folder's behaviour"
        );
    }
    let after_control = revision_of(paths, &server).await;
    say(
        "control",
        format!(
            "config -> ship-once with config/a.toml travelling: revision {} -> {}",
            &merging[..12],
            &after_control[..12]
        ),
    );
    assert_ne!(
        after_control, merging,
        "the folder's behaviour is the only thing that changed and it reaches a \
         file the pack carries, so it is what has to move the revision — if it \
         did not, the two assertions above are being satisfied by a revision \
         that mentions no behaviour at all and this test proves nothing"
    );
}

/// A join preview that promised no settings files can be followed by a pack
/// full of them, and nothing between the two refuses it.
///
/// `PackInfo::config_files` exists to tell a player, before they press Import,
/// that a server's own config is coming — "config was the one thing that
/// arrived, and kept arriving, with nobody ever told". But `/info` and `/pack`
/// are two requests, the webview holds the first for as long as the dialog is
/// open, and the only thing standing between them is
/// `share::join::check_matches`, which compares the Minecraft version, the
/// loader and the loader version and nothing else. `instance::modpack::
/// PackPreview` has no config field at all, so it could not compare one.
///
/// So this asserts the hole as it is: the player approves a description saying
/// `config_files: 0`, the admin turns config on, and the import that follows —
/// the real `check_matches`, the real manifest read, the real
/// `check_server_downloads`, the real `pack::import` — installs the admin's
/// config without a word. If a later change makes the gate see config, this
/// fails and should be rewritten rather than deleted.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port and speaks real TLS"]
async fn a_preview_that_promised_no_config_still_installs_the_admins_config() {
    let scratch = Scratch::new("consent");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();
    // Nothing in this pack is a reference — the one jar is on no provider — so
    // a client that cannot reach anything is what says the import fetched
    // nothing it should not have.
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
        .build()
        .unwrap();

    let code = Code::generate();
    let mut share = a_share(&code);
    let server = a_shared_server(paths, &code, share.clone()).await;
    let run = paths.server_run(&server.id);
    tokio::fs::write(
        run.join("mods").join("geode-live-byhand.jar"),
        jar("geodelivebyhand", "Added By Hand", "*"),
    )
    .await
    .unwrap();
    tokio::fs::write(
        run.join("config").join("geode-live.toml"),
        "tick_rate = 20\n",
    )
    .await
    .unwrap();

    let pin = pin_of(paths).await;
    let host = Host::start(paths.clone(), 0, None).await.unwrap();
    let link = link_to(host.local_addr.port(), &code, pin);

    // What the player is shown, and holds while the dialog is open.
    let (link, approved) = preview_of(link).await;
    assert_eq!(
        approved.config_files, 0,
        "this share sends no config, which is what the dialog says"
    );
    assert!(!approved.config_kept, "and nothing is kept in sync");
    say("approved", format!("{} config files promised", approved.config_files));

    // The admin ticks the box. The link and the password are untouched, so
    // what the player holds stays valid.
    //
    // Two clicks rather than one, and `at` is why. Enforce is what a share
    // that has chosen nothing already does — `Settings::policy_for` answers
    // Enforce for a path nothing covers — and `Settings::choose` writes
    // nothing when handed the behaviour already in force, so ticking the box
    // and asking for Enforce moves the push counter not at all. The
    // `settings_at` assertion at the end would then be comparing `Some(0)` to
    // `Some(0)`, which is a state the offline suite pins as its own case
    // (`assert_eq!(player.settings_at, Some(0))`) and which would hold
    // whatever the import did. So the admin makes a real push: Merge, then
    // Enforce — picking a behaviour and changing their mind, which is the
    // ordinary way two of them happen — and the behaviour the player ends up
    // under is still Enforce.
    share.send_config(true, Some(Policy::Merge));
    share.send_config(true, Some(Policy::Enforce));
    assert_eq!(
        share.settings.policy_for(Path::new("config")),
        Policy::Enforce,
        "the standing commitment asserted at the end is Enforce's"
    );
    assert!(
        share.settings.at > 0,
        "nothing was pushed, so the settings_at assertion at the end would hold \
         whatever the import did"
    );
    geode_lib::server::share::save(paths, &server.id, &share)
        .await
        .unwrap();
    say(
        "admin",
        format!(
            "turned the config folder on, with Enforce, at push {}",
            share.settings.at
        ),
    );

    let archive = fetch_pack(paths, &link, "consent").await;
    assert!(
        entries(&archive)
            .iter()
            .any(|n| n.ends_with("config/geode-live.toml")),
        "the pack built after the change should carry the config: {:?}",
        entries(&archive)
    );

    // The real sequence, with the *stale* approval.
    let (inst, missing) = install_joined(&client, paths, &link, &approved, &archive).await;
    assert!(missing.is_empty(), "{missing:?}");
    host.stop().await;

    let landed = paths
        .game_dir(&inst.id)
        .join("config")
        .join("geode-live.toml");
    assert!(
        landed.is_file(),
        "nothing refused the pack, so the admin's config should have landed"
    );
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), "tick_rate = 20\n");
    // And the standing commitment the player was never shown: Enforce means
    // the admin's copy replaces theirs on every update from here on.
    assert_eq!(
        inst.settings_at,
        Some(share.settings.at),
        "the instance caught up with a push the player was never told about"
    );
    assert!(
        inst.settings_at.is_some_and(|at| at > 0),
        "a push that never happened cannot be caught up with: {:?}",
        inst.settings_at
    );
    say(
        "finding",
        format!(
            "the player approved config_files: {} and received {}, with Enforce — \
             check_matches compares only the version and the loader",
            approved.config_files,
            landed.display()
        ),
    );
}

/// A jar of exactly `target` bytes carrying `declaration` as its
/// `fabric.mod.json`, padded with a stored entry so the length can be hit to
/// the byte.
///
/// Exactness is the whole point of it: the test below needs bytes that differ
/// from a recorded jar's while its *size* does not, because size is the only
/// thing a server's revision reads off a mod that has a complete provider
/// record. A zip comment cannot do it — the field is capped at 64 KiB and a
/// real mod jar is bigger than that — so the padding is a stored entry, whose
/// payload length is added to the file byte for byte.
fn jar_of_length(declaration: &[u8], target: u64) -> Vec<u8> {
    let build = |pad: usize| -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let deflated: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let stored: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("fabric.mod.json", deflated).unwrap();
        zip.write_all(declaration).unwrap();
        zip.start_file("geode-live-padding.bin", stored).unwrap();
        zip.write_all(&vec![0x61u8; pad]).unwrap();
        zip.finish().unwrap().into_inner()
    };
    let base = build(0).len() as u64;
    assert!(
        target >= base,
        "the real jar is {target} bytes and the replacement cannot be made smaller than {base}"
    );
    let out = build((target - base) as usize);
    assert_eq!(out.len() as u64, target, "the padding arithmetic is wrong");
    out
}

/// A server's index vouches for the admin's bytes while pointing at upstream's,
/// and the revision cannot see the two come apart.
///
/// `write_mrpack` writes each index entry's sha1, sha512 and size from the
/// bytes on the **admin's disk**, beside a URL built from the record's ids —
/// and `server_revision` identifies a mod with a complete provider record as
/// `provider:project:version:<length>` and never hashes it, which is
/// deliberate: it is what describes a pack of a few hundred Modrinth mods
/// without reading one of them. The instance side is protected from a stale
/// record by `identify` running a lookup and finding it stale
/// (`a_stale_record_is_embedded_rather_than_indexed`); a server's export runs
/// no lookup, by a decision with its own comment, so nothing on that side can
/// notice.
///
/// So: replace a recorded jar's bytes with the same number of bytes, keep the
/// record, and the revision holds still while the index now claims hashes for
/// bytes no address serves. Asserted here as the pair it is — the revision does
/// not move (the documented design), and a player who joins afterwards has
/// their import killed by a checksum (the undocumented consequence), with
/// nothing on the admin's side having said a word.
///
/// Reachable in the app without touching a shell: `add_local_server_content`
/// copies a picked file in under its own name with no collision check, so an
/// admin adding a rebuilt jar with the same filename over a provider-installed
/// one leaves exactly this state. The same-length part is what this test
/// arranges by hand; a rebuild that happened to differ in length would move the
/// revision and push the same broken pack to everybody instead.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port, speaks real TLS, and downloads a real mod from Modrinth"]
async fn a_recorded_jars_bytes_can_be_swapped_under_the_revision() {
    let scratch = Scratch::new("swapped");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();
    let client = geode_lib::net::client();

    let code = Code::generate();
    let mut server = a_shared_server(paths, &code, a_share(&code)).await;
    let slug = env_or("GEODE_PACK_BOTH_MOD", DEFAULT_BOTH_MOD);
    let (file, version) =
        install_from_modrinth(&client, paths, &mut server, &slug, ContentKind::Mod).await;
    let jar_path = paths.server_run(&server.id).join("mods").join(&file);

    let before = revision_of(paths, &server).await;
    let was = std::fs::read(&jar_path).unwrap();
    let published_sha1 = version
        .primary_file()
        .and_then(|f| f.sha1.clone())
        .expect("Modrinth published a sha1");
    assert_eq!(
        sha1_of(&jar_path).to_lowercase(),
        published_sha1.to_lowercase(),
        "the installed jar should be the bytes Modrinth published"
    );

    // The admin's replacement: a different build of the same mod, which here
    // is a jar declaring the same mod to the loader — so the side the pack
    // sends it with cannot be what moves the revision — and exactly as many
    // bytes as the one it replaces.
    let declaration = read_entry(&jar_path, "fabric.mod.json");
    let swapped = jar_of_length(&declaration, was.len() as u64);
    assert_ne!(swapped, was, "the replacement has to be different bytes");
    std::fs::write(&jar_path, &swapped).unwrap();
    assert_eq!(
        std::fs::metadata(&jar_path).unwrap().len(),
        was.len() as u64
    );
    say(
        "swapped",
        format!(
            "{file}: {} bytes, sha1 {} -> {}",
            was.len(),
            &published_sha1[..12],
            &sha1_of(&jar_path)[..12]
        ),
    );

    let after = revision_of(paths, &server).await;
    assert_eq!(
        after, before,
        "a provider-recorded mod is identified by provider:project:version:length \
         and never hashed, so a same-length replacement is invisible here — if \
         this ever starts failing, something began hashing recorded jars and the \
         cost of that is in `server_revision`'s own comment"
    );
    say("revision", format!("unchanged at {}", &after[..12]));

    // What the pack now says: the admin's hashes, upstream's address.
    let dest = paths.root.join("swapped.geodepack");
    pack::export_server(paths, &server, dest.clone(), &Progress::silent("p"))
        .await
        .expect("the export succeeds, and says nothing about any of this");
    pack::check_server_downloads(&dest)
        .await
        .expect("Geode's own gate on a server pack's addresses passes it");
    let files = indexed(&dest);
    assert_eq!(files.len(), 1, "the jar travels as a reference: {files:#?}");
    assert_eq!(
        files[0].sha1.as_deref().map(str::to_lowercase),
        Some(sha1_of(&jar_path).to_lowercase()),
        "the index vouches for the bytes on the admin's disk"
    );
    assert_ne!(
        files[0].sha1.as_deref().map(str::to_lowercase),
        Some(published_sha1.to_lowercase()),
        "and those are not the bytes the address it names will serve"
    );

    // The player who joins now. Served by the real host, through the real
    // player-side code, with a real client.
    let pin = pin_of(paths).await;
    let host = Host::start(paths.clone(), 0, None).await.unwrap();
    let (link, info) = preview_of(link_to(host.local_addr.port(), &code, pin)).await;
    let archive = fetch_pack(paths, &link, "swapped-join").await;
    let described = instance::modpack::preview(archive.clone()).await.unwrap();
    join::check_matches(&info, &described).expect("nothing about the setup changed");
    let mut inst = instance::create(
        paths,
        described.name.clone(),
        described.mc_version.clone(),
        described.loader,
        described.loader_version.clone(),
    )
    .await
    .unwrap();
    inst.joined = Some(link.clone());
    inst.save(paths).await.unwrap();
    let failed = pack::import(
        &client,
        paths,
        &mut inst,
        &archive,
        None,
        &Progress::silent("live-sync-import"),
    )
    .await;
    host.stop().await;

    let error = failed
        .err()
        .map(|e| e.to_string())
        .expect("the index vouches for bytes the address does not serve");
    say("import", &error);
    assert!(
        error.contains("checksum") && error.contains(&file),
        "the failure should be a checksum mismatch naming the file: {error}"
    );
    assert!(
        !paths.game_dir(&inst.id).join("mods").join(&file).is_file(),
        "a jar whose checksum did not match should not have been kept"
    );
    say(
        "finding",
        "the revision held still, the export and check_server_downloads both \
         passed, and every player joining from here on cannot import at all",
    );
}

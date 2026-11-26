//! The player's side of a join, against a real running host.
//!
//! `live_share_host` starts the same `Host::start` this file does, but drives
//! it with raw HTTP through a helper of its own — deliberately, because what
//! it measures is that two refusals are the same bytes, and an interpreted
//! error cannot show that. The consequence is that the code a real player runs
//! is barely in its path: `share::join`, `share::trust` and the link parser
//! appear there twice, in the two places it proves to itself that the port
//! works before telling a human to aim at it.
//!
//! This file is the other half. Every request here goes through
//! `share::join::preview` and `share::join::download` — the real client, with
//! the real pinned TLS and the real strict parser in front of it — and the
//! happy path carries on past the download into `instance::modpack::preview`,
//! `share::join::check_matches`, `share::pack::read_manifest`,
//! `share::pack::check_server_downloads` and `share::pack::import`, which is
//! the sequence `commands::install_joined` performs. That sequence lives in
//! `commands.rs`, behind the `desktop` feature, so no test anywhere runs it:
//! what a join actually *lands* has never been checked against a real host.
//!
//! Every test is `#[ignore]`d: each binds real ports and speaks real TLS over
//! the network stack.
//!
//! ```sh
//! cargo test --test live_share_join -- --ignored --nocapture
//!
//! # a bigger pack, and more patience for the machines that are not hosts
//! GEODE_JOIN_FILLER_MB=64 GEODE_JOIN_PATIENCE=90 \
//!   cargo test --test live_share_join -- --ignored --nocapture
//! ```
//!
//! Two knobs, both optional. `GEODE_JOIN_FILLER_MB` is how much
//! incompressible filler goes in the shared server's `mods/`, which is what
//! makes a pack take long enough to build that cancelling one mid-build is not
//! a race (default [`DEFAULT_FILLER_MB`]). `GEODE_JOIN_PATIENCE` is the number
//! of seconds a refusal is allowed to take before this file calls it a hang
//! (default [`DEFAULT_PATIENCE`]); it has to sit above `join`'s own 30-second
//! preview deadline, so lowering it below that makes the black-hole cases fail
//! for the wrong reason.
//!
//! **The helpers below are copied from `live_share_host.rs` on purpose.**
//! `Scratch`, `jar`, `share_a_server`, `pin_of`, `link_to` and `entries` are
//! that file's, near enough verbatim. Lifting them into a shared module would
//! mean editing `live_share_host.rs`, and an integration test's helpers are
//! not a library: two files that each stand a host up from nothing are two
//! files that can each be read start to finish, which is worth more here than
//! the duplication costs. The one place they differ is noted where it happens.
//!
//! What a green run does **not** prove:
//!
//! - **The player's screen.** Nothing drives the join dialog. This calls what
//!   the commands call, in the order they call it, which is not the same as
//!   somebody pressing the button.
//! - **Two networks.** Everything here is loopback. A player on the internet,
//!   the router mapping and carrier-grade NAT are all untouched — the gap
//!   `docs/sharing-and-hosting.md` records and `live_share_host.rs` exists to
//!   let a human close by hand.
//! - **The ceilings.** `MAX_PACK_BYTES` is two gigabytes and `join::download`
//!   takes no ceiling parameter the way `fetch::pack_within` does, so nothing
//!   short of really building and streaming two gigabytes exercises it. The
//!   host's own limits — the connection ceilings, the slow-download cut, the
//!   revocation sweep — belong to `share/host.rs`'s unit tests, which can
//!   shrink them; this file runs against the real ones.
//! - **A provider.** Every mod in the pack here is a hand-made jar that exists
//!   on no provider, so it travels as bytes and the import resolves nothing.
//!   That is on purpose: the import below is given a client that cannot reach
//!   anything, so a reference it tried to fetch would fail loudly rather than
//!   pass quietly. What a reference does on import is `live_pack_sync`'s.
//!
//! And one thing that looks as if it belongs here and does not: **`share::fetch`
//! is not on this path at all**. It downloads a `geode://pack?url=…&h=…` link —
//! a file the sharer put wherever they already put files — with no
//! `Authorization`, no pin and no host of Geode's involved, and it judges what
//! comes back by status, content type, zip magic and the SHA-256 in the link.
//! A `geode://join` host wants a bearer code over a pinned self-signed
//! certificate, so pointing `fetch::pack` at one exercises nothing but a TLS
//! failure. Its own unit tests drive it against a local server that can be
//! told to answer with a web page, a lie about its length or a hash that does
//! not match, which is the right way to test it; `live_pack_hostile` is where
//! a live one would go.

use std::collections::BTreeSet;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use geode_lib::error::Error;
use geode_lib::events::Recorder;
use geode_lib::instance;
use geode_lib::meta::Loader;
use geode_lib::net::Progress;
use geode_lib::paths::Paths;
use geode_lib::server::share::ServerShare;
use geode_lib::server::{Server, ServerKind};
use geode_lib::share::code::Code;
use geode_lib::share::host::Host;
use geode_lib::share::identity::Identity;
use geode_lib::share::join::{self, Verified};
use geode_lib::share::link::{Address, Link, Opened, Pin};
use geode_lib::share::manifest::{Kind, Listing};
use geode_lib::share::pack;

/// How much incompressible filler goes in the server's `mods/`, unless
/// `GEODE_JOIN_FILLER_MB` says otherwise.
///
/// Not decoration. [`a_cancelled_join_leaves_nothing_behind_and_asking_again_works`]
/// cancels while the host is still building, and the build is what takes time:
/// random bytes do not compress, so the host has to hash and deflate all of
/// them before it sends a byte. A pack of a few kilobytes is built inside one
/// poll of the cancel flag and the cancel loses the race.
const DEFAULT_FILLER_MB: usize = 32;

/// Seconds a refusal may take before this file calls it a hang, unless
/// `GEODE_JOIN_PATIENCE` says otherwise.
///
/// Above `join::PREVIEW_DEADLINE`, which is 30 seconds and private, because a
/// preview against a machine that never answers is *meant* to sit there for
/// that long and then give up. The bound is on hanging, not on being slow.
const DEFAULT_PATIENCE: u64 = 60;

// ---------------------------------------------------------------- the machine

/// A `GEODE_HOME` of its own under the system temporary directory, removed
/// when the test ends however it ends.
///
/// `paths::Scratch` is exactly this and is `#[cfg(test)]`, so it exists for
/// the crate's own unit tests and not for anything under `tests/`. The same
/// guard for the same reason: a `remove_dir_all` at the end of a test does not
/// run when the test panics, which is precisely when the directory is worth
/// having gone.
struct Scratch {
    paths: Paths,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("geode-live-join-{tag}-{}", uuid::Uuid::new_v4()));
        // Never the real one, and never inside it. The user's own data
        // directory holds their accounts, their instances, and the one
        // certificate every link they have ever handed out is pinned to — and
        // this file generates certificates and serves them to a socket.
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

    /// A scratch home with its directory tree made, which is what the app has
    /// by the time any of this runs.
    async fn ready(tag: &str) -> Self {
        let scratch = Self::new(tag);
        scratch.paths.ensure_all().await.unwrap();
        scratch
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.paths.root);
    }
}

/// `share/outgoing`, where the host writes the pack it is sending.
///
/// Derived here because `share::host::outgoing` is private and nothing public
/// reports it. It is the one window a process outside the host has onto a
/// request in flight, and it is a copy of a path the host owns — so if the
/// host ever moves it, the assertions below stop seeing anything rather than
/// failing.
fn outgoing(paths: &Paths) -> PathBuf {
    paths.share_dir().join("outgoing")
}

/// A real jar: a zip carrying a `fabric.mod.json` that `instance::jar::read`
/// understands, so the pack is built from a file the launcher can read rather
/// than from random bytes, and the side it declares is a side the preview has
/// to count.
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
    // Not a real class. What it is for is that a jar with more in it than its
    // declaration travels whole.
    zip.start_file(format!("{id}/Mod.class"), options).unwrap();
    zip.write_all(&[0xca, 0xfe, 0xba, 0xbe]).unwrap();
    zip.finish().unwrap().into_inner()
}

/// The mods a shared server here holds, by the folder they sit in.
const SERVER_MOD: &str = "geode-live-server";
const CLIENT_MOD: &str = "geode-live-client";

/// A shared Fabric server on disk, with a mod of its own, a mod for its
/// players, and `filler_mb` megabytes of a jar that does not compress.
///
/// Written the way the app writes one: `Server::save` for `server.json`, and
/// `server::share::save` for `share.json`, which is what `share::start` calls
/// once it has decided which code to use.
///
/// This differs from `live_share_host.rs`'s copy in one way: it takes a
/// [`Listing`]. The row a joined instance writes into the player's own
/// `servers.dat` is part of what "a pack a player could play" means — an
/// import that ends at a title screen and an empty server list is not one —
/// and that row travels in the description rather than in the pack, so only a
/// share that sets one can prove it arrived.
async fn share_a_server(
    paths: &Paths,
    code: &Code,
    filler_mb: usize,
    listing: Option<Listing>,
) -> Server {
    let mut server = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
    server.id = "survival".into();
    // For Fabric the build *is* the loader version a player needs, and
    // `pack::client_loader` refuses a server whose build is empty.
    server.build = "0.16.9".into();

    let run = paths.server_run(&server.id);
    for (folder, id, name, environment) in [
        ("mods", SERVER_MOD, "Geode Live (server side)", "server"),
        ("client-mods", CLIENT_MOD, "Geode Live (client only)", "client"),
    ] {
        let dir = run.join(folder);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join(format!("{id}.jar")), jar(id, name, environment))
            .await
            .unwrap();
    }
    if filler_mb > 0 {
        // Random bytes rather than a jar, because the point of them is that
        // they do not compress: the pack has to end up the size the filler is,
        // and the host has to spend real time building it.
        let mut bytes = vec![0u8; filler_mb << 20];
        getrandom::fill(&mut bytes).unwrap();
        tokio::fs::write(run.join("mods").join("filler.jar"), bytes)
            .await
            .unwrap();
    }
    server.save(paths).await.unwrap();

    geode_lib::server::share::save(
        paths,
        &server.id,
        &ServerShare {
            code: code.clone(),
            include_config: false,
            settings: Default::default(),
            picked: Vec::new(),
            listing,
            forbidden: Vec::new(),
            shared: true,
        },
    )
    .await
    .unwrap();
    server
}

/// This machine's sharing certificate, which is the fingerprint a player pins.
async fn pin_of(paths: &Paths) -> Pin {
    Identity::load_or_create(paths).await.unwrap().pin()
}

/// A link to a host, built the way `service::local_link_for` builds one: the
/// address read back through the strict parser, because nothing here is
/// special enough to skip it.
fn link_to(address: &str, port: u16, code: &Code, pin: Pin) -> Link {
    Link {
        address: Address::parse(address, port).expect("an address the parser accepts"),
        code: code.clone(),
        pin: Some(pin),
    }
}

/// Every entry in a `.geodepack`, sorted.
fn entries(pack: &Path) -> Vec<String> {
    let file = std::fs::File::open(pack).expect("the pack was written");
    let zip = zip::ZipArchive::new(file).expect("the pack opens as a zip");
    let mut names: Vec<String> = zip.file_names().map(str::to_string).collect();
    names.sort();
    names
}

/// One entry's bytes, decompressed.
fn carried(pack: &Path, name: &str) -> Vec<u8> {
    use std::io::Read;
    let file = std::fs::File::open(pack).expect("the pack was written");
    let mut zip = zip::ZipArchive::new(file).expect("the pack opens as a zip");
    let mut entry = zip
        .by_name(name)
        .unwrap_or_else(|e| panic!("{name} is not in {}: {e}", pack.display()));
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("the entry reads");
    bytes
}

/// A well-formed code that is not the one this server has, so a refusal of it
/// cannot be about the shape of what arrived.
fn a_different_code(from: &Code) -> Code {
    let other = Code::parse("GEODE-00000-00000").expect("a well-formed code");
    assert_ne!(other.raw(), from.raw(), "pick another one");
    other
}

fn filler_mb() -> usize {
    std::env::var("GEODE_JOIN_FILLER_MB")
        .ok()
        .and_then(|mb| mb.parse().ok())
        .unwrap_or(DEFAULT_FILLER_MB)
}

/// How long a refusal may take before it is a hang.
fn patience() -> Duration {
    Duration::from_secs(
        std::env::var("GEODE_JOIN_PATIENCE")
            .ok()
            .and_then(|seconds| seconds.parse().ok())
            .unwrap_or(DEFAULT_PATIENCE),
    )
}

/// Run `work` and fail if it has not finished inside [`patience`], printing how
/// long it did take.
///
/// The point of every impostor below is that the client *stops*. A test that
/// only asserted the error would pass just as well against a client that took
/// an hour to produce it, which is not a client a player can use.
async fn within<T>(what: &str, work: impl Future<Output = T>) -> T {
    let limit = patience();
    let started = Instant::now();
    let done = tokio::time::timeout(limit, work)
        .await
        .unwrap_or_else(|_| panic!("{what}: still waiting on the server after {limit:?}"));
    println!("waited     {:>8.2}s  {what}", started.elapsed().as_secs_f64());
    done
}

/// Whether an error reads as a sentence written for a player rather than as a
/// transport error that escaped.
///
/// `Error` serialises to its `Display` string for the frontend, so whatever is
/// in here is what somebody reads on screen. The debris checked for is what
/// arrives when a `reqwest::Error` or a `rustls::Error` is formatted straight
/// into a message.
fn reads_as_copy(message: &str) -> std::result::Result<(), String> {
    for debris in [
        "InvalidCertificate",
        "ApplicationVerificationFailure",
        "BadSignature",
        "rustls",
        "Custom {",
        "Error {",
        "kind:",
        "source:",
    ] {
        if message.contains(debris) {
            return Err(format!("it carries {debris:?}: {message}"));
        }
    }
    Ok(())
}

/// Whether an error says which address let the player down.
///
/// Only asked of the refusals `join::connect_error` writes, which are the ones
/// where nothing readable came back and the address is the whole of what there
/// is to say. The refusals that *read* an answer say what the answer was
/// instead — "it is not a Geode server sharing a pack" — and deliberately do
/// not repeat an address the dialog is already showing.
fn names_the_address(message: &str, address: &str) -> std::result::Result<(), String> {
    if message.contains(address) {
        return Ok(());
    }
    Err(format!("it never says which address failed: {message}"))
}

// --------------------------------------------------- machines that are not it

/// A TLS configuration that shows `shown`'s certificate but signs its
/// handshakes with `signer`'s key.
///
/// Given one identity twice it is that machine. Given two it is the attack a
/// fingerprint check alone does not stop: a certificate is public — anyone who
/// connects is shown it — so an impostor can serve the real one and sign with a
/// key of its own, and only checking the handshake signature against the
/// certificate catches it. `share::trust` owns that check and tests it against
/// a server built like this one; what is new here is driving it from
/// `join::preview`, so the sentence a player is shown is checked too.
fn presenting(shown: &Identity, signer: &Identity) -> Arc<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let key = provider
        .key_provider
        .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            signer.key_der.clone(),
        )))
        .expect("a key rustls can load");
    let certified = CertifiedKey::new(vec![CertificateDer::from(shown.cert_der.clone())], key);
    assert_eq!(
        certified.keys_match().is_ok(),
        shown.cert_der == signer.cert_der,
        "the impostor is not the impostor it claims to be"
    );
    Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default protocol versions")
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(SingleCertAndKey::from(certified))),
    )
}

/// Read a whole request, or say that none arrived.
async fn read_request<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> bool {
    let (mut seen, mut buf) = (Vec::new(), [0u8; 1024]);
    while !seen.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return false,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
        }
    }
    true
}

/// What a machine that is not the host does when a player's Geode connects.
enum Answers {
    /// Accepts the connection and then never reads or writes anything, so the
    /// TLS handshake never completes. A port something holds and does not
    /// speak on: a firewall swallowing the reply, a service that has wedged.
    Never,
    /// Answers whatever arrives with these bytes, in the clear and without
    /// waiting for a whole request.
    ///
    /// Not waiting is the faithful part. A TLS `ClientHello` holds no
    /// `\r\n\r\n`, so a listener that waited for one would never answer and
    /// would be a second [`Answers::Never`] wearing a different label — which
    /// is exactly the mistake the first run of this file made. A real HTTP
    /// server reads the handshake as a request line it cannot parse and says
    /// so at once.
    AtOnce(Vec<u8>),
    /// Completes TLS with `config` and reads a whole request. `None` then
    /// never answers and never hangs up either — which is what the admin's own
    /// machine building a very large pack looks like, forever. `Some` writes
    /// the bytes and closes, so a reply whose `Content-Length` is longer than
    /// itself is a host that stopped mid-pack.
    OverTls(Arc<rustls::ServerConfig>, Option<Vec<u8>>),
}

/// A listener that is not a Geode host, however convincingly it answers.
async fn impostor(answers: Answers) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let answers = Arc::new(answers);
    tokio::spawn(async move {
        // Held rather than dropped: a connection this listener is finished
        // with is closed on purpose, and a mute one is kept open on purpose.
        let mut mute = Vec::new();
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                continue;
            };
            let answers = answers.clone();
            match answers.as_ref() {
                Answers::Never => mute.push(tcp),
                Answers::AtOnce(_) | Answers::OverTls(_, _) => {
                    tokio::spawn(async move {
                        match answers.as_ref() {
                            Answers::Never => unreachable!("held above"),
                            Answers::AtOnce(reply) => {
                                let mut stream = tcp;
                                let mut buf = [0u8; 1024];
                                let _ = stream.read(&mut buf).await;
                                let _ = stream.write_all(reply).await;
                                let _ = stream.flush().await;
                            }
                            Answers::OverTls(config, reply) => {
                                let acceptor = TlsAcceptor::from(config.clone());
                                let Ok(mut stream) = acceptor.accept(tcp).await else {
                                    return;
                                };
                                if !read_request(&mut stream).await {
                                    return;
                                }
                                match reply {
                                    // Never answers, and never hangs up.
                                    None => std::future::pending::<()>().await,
                                    Some(reply) => {
                                        let _ = stream.write_all(reply).await;
                                        let _ = stream.flush().await;
                                    }
                                }
                            }
                        }
                    });
                }
            }
        }
    });
    port
}

/// One reply, written by hand so it can claim a length its body does not have.
fn reply(status: &str, content_type: &str, claimed: usize, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {claimed}\r\n\r\n"
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// A whole reply, whose claimed length is the body it carries.
fn whole(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    reply(status, content_type, body.len(), body)
}

// ------------------------------------------------------- the rest of the join

/// The steps `commands::install_joined` takes with a downloaded archive, run
/// against a real scratch home and answering with the instance they made.
///
/// That function is in `commands.rs`, which is `#[cfg(feature = "desktop")]`,
/// so nothing under `tests/` can call it and no test runs it. What is copied
/// here is its order and its checks, not its error wording: the point is that
/// a pack this host built passes every one of them.
///
/// Which is also the limit of what this can say. It is a copy, so it proves
/// the real host's pack satisfies these checks — not that `install_joined`
/// still makes them, or still makes them in this order. Only reading that
/// function says so, and if it changes, this stops being a copy of it without
/// anything failing. A test that asserted the order against this copy would be
/// asserting the copy.
async fn install_joined(
    paths: &Paths,
    link: &Link,
    approved: &geode_lib::share::host::PackInfo,
    archive: &Path,
) -> (instance::Instance, Vec<String>) {
    let described = instance::modpack::preview(archive.to_path_buf())
        .await
        .expect("the pack describes itself");
    // What gets installed must be what the player said yes to.
    join::check_matches(approved, &described)
        .expect("the pack is the setup the description promised");

    // A joined pack must be a server's: a personal pack's manifest may set the
    // player's memory and Java arguments, and a server has no business doing
    // that.
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
        // Nothing in this pack is a reference, and the pass that names what
        // arrived must not go asking: a client that cannot reach anything is
        // what says so.
        &reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap(),
        paths,
        &mut inst,
        archive,
        None,
        &Progress::silent("live-join"),
    )
    .await;
    let revision = pack::pack_revision(&manifest).unwrap_or_else(|| approved.revision.clone());
    if imported.is_ok() && !revision.is_empty() {
        inst.joined_revision = Some(revision);
    }
    inst.save(paths).await.expect("the instance saves again");

    let missing = imported.expect("the import finished");
    (inst, missing)
}

/// Wait until the host has begun building a pack, and answer with its name.
///
/// `share/outgoing` is the one window a process outside the host has onto a
/// request in flight, and the file appears at the *start* of the build:
/// `modpack::lay_out` creates the zip and then deflates the server's mods into
/// it, which for a pack of incompressible filler is seconds of work. So a
/// cancel sent the moment this returns lands with the host mid-build, which is
/// where it is meant to land — rather than wherever the clock happened to be.
async fn building_started(paths: &Paths) -> String {
    let dir = outgoing(paths);
    within("the host starting to build a pack", async {
        loop {
            let seen = std::fs::read_dir(&dir).ok().and_then(|entries| {
                entries
                    .flatten()
                    .next()
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
            });
            if let Some(seen) = seen {
                println!("building   {seen}");
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// Wait for the host to let go of every pack it built, and say how long it
/// took.
///
/// The host builds each download's pack to a file of its own under
/// `share/outgoing` and removes it when the response that was streaming it is
/// dropped — which, for a player who hung up, is the only thing that ever
/// will. This is how that is seen from outside the process.
async fn outgoing_empties(paths: &Paths, within: Duration) {
    let dir = outgoing(paths);
    let started = Instant::now();
    loop {
        let left = std::fs::read_dir(&dir).map_or(0, Iterator::count);
        if left == 0 {
            println!(
                "outgoing   emptied after {:.2}s",
                started.elapsed().as_secs_f64()
            );
            return;
        }
        assert!(
            started.elapsed() < within,
            "{left} built pack(s) are still in {} after {within:?}",
            dir.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until a job has reported `stage`, so a cancel lands where it is meant
/// to rather than wherever the clock happens to be.
async fn until_stage(recorder: &Recorder, stage: &str) {
    within(&format!("reaching the {stage:?} stage"), async {
        loop {
            if recorder
                .events()
                .iter()
                .any(|(_, event)| event["stage"] == stage)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

// ------------------------------------------------------------------ the tests

/// The whole thing: a real server shared, a real host serving it, the real
/// client fetching it, and an instance a player could press Play on.
///
/// The link is written by `Link::to_url` and read back by `Opened::parse`,
/// which is the deep-link entry point — the one function that decides which
/// kind of `geode://` link arrived — so what the player fetches from is what
/// came out of the parser and not the struct the admin's side built.
///
/// What this guards: `host::pack` and the description `host::respond` serves;
/// the parse in `join::preview` that turns that description into a `PackInfo`,
/// so a server answering with anything else is not a Geode server; the body
/// `join::download` writes chunk by chunk; and `commands::install_joined`'s
/// sequence, which exists nowhere a test can reach it. Break the pack
/// builder's side classification and
/// `server_only.side` below goes `None`; break the import's ownership tagging
/// and `Origin::Server` goes `User`, which is the difference between a player
/// being able to delete the server's anti-cheat and not.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port and speaks real TLS"]
async fn a_player_joins_a_shared_server_and_lands_an_instance_they_could_play() {
    let admin_home = Scratch::ready("e2e-admin").await;
    let player_home = Scratch::ready("e2e-player").await;
    let (admin, player) = (&admin_home.paths, &player_home.paths);

    let code = Code::generate();
    let listing = Listing {
        name: "Survival Club".into(),
        address: "play.example.com:25566".into(),
    };
    // No filler: nothing here waits on the build, and 32 MB of random bytes
    // through the importer is time spent proving nothing.
    let server = share_a_server(admin, &code, 0, Some(listing.clone())).await;
    let pin = pin_of(admin).await;
    let host = Host::start(admin.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();
    println!("host       listening on {}", host.local_addr);

    // The admin copies a link; the player's Geode reads it back. `Opened` is
    // what the deep link and the webview both go through, and it has to pick
    // the join parser for this one.
    let written = link_to("127.0.0.1", port, &code, pin).to_url();
    println!("link       {written}");
    let Opened::Join(link) = Opened::parse(&written).expect("a link Geode wrote parses") else {
        panic!("the parser read a join link as something else");
    };
    assert_eq!(
        link,
        link_to("127.0.0.1", port, &code, pin),
        "the link the player holds is not the one the admin copied"
    );

    let preview = join::preview(link.clone())
        .await
        .unwrap_or_else(|e| panic!("the preview failed: {e}"));
    assert!(
        matches!(preview.verified, Verified::Pinned),
        "a link carrying a fingerprint is pinned, not trusted on first use"
    );
    assert_eq!(preview.info.name, server.name);
    assert_eq!(preview.info.server_id, server.id);
    assert_eq!(preview.info.mods, 2, "both jars travel to the player");
    assert_eq!(preview.info.server_mods, 1);
    assert_eq!(preview.info.config_files, 0, "this share sends no config");
    assert_eq!(
        preview.info.listing.as_ref(),
        Some(&listing),
        "the row the admin wants in players' lists crossed the wire"
    );

    // Downloaded where the app downloads one, under the directory a launch
    // clears, so the file lands on the real path rather than a convenient one.
    let archive = join::incoming_dir(player).join(format!("{}.geodepack", uuid::Uuid::new_v4()));
    join::download(&preview.link, &archive, &Progress::silent("live-join"))
        .await
        .unwrap_or_else(|e| panic!("the download failed: {e}"));
    let bytes = std::fs::metadata(&archive).unwrap().len();
    println!("pack       {bytes} bytes at {}", archive.display());
    for name in entries(&archive) {
        println!("entry      {name}");
    }

    let (inst, missing) = install_joined(player, &link, &preview.info, &archive).await;
    assert!(missing.is_empty(), "nothing should be left to fetch: {missing:?}");
    println!("instance   {} ({})", inst.name, inst.id);

    // What the player actually has.
    assert_eq!(inst.loader, Loader::Fabric);
    assert_eq!(inst.loader_version, "0.16.9");
    assert_eq!(inst.mc_version, "1.21.1");
    assert_eq!(
        inst.joined.as_ref().map(|joined| &joined.address),
        Some(&link.address),
        "a joined instance has to remember where it came from, or it never updates"
    );
    assert_eq!(inst.joined_server_id.as_deref(), Some(server.id.as_str()));
    assert_eq!(
        inst.joined_revision.as_deref(),
        Some(preview.info.revision.as_str()),
        "the revision the pack carries is what says whether this instance is behind"
    );

    let mods = player.game_dir(&inst.id).join("mods");
    for id in [SERVER_MOD, CLIENT_MOD] {
        let jar = mods.join(format!("{id}.jar"));
        assert!(jar.is_file(), "{} is not on disk", jar.display());
        let record = inst
            .content
            .iter()
            .find(|record| record.file_name == format!("{id}.jar"))
            .unwrap_or_else(|| panic!("{id}.jar arrived with no record of where it came from"));
        assert_eq!(
            record.origin,
            instance::content::Origin::Server,
            "{id}.jar is the server's, and a player must not be able to delete it"
        );
    }
    let server_only = inst
        .content
        .iter()
        .find(|record| record.file_name == format!("{SERVER_MOD}.jar"))
        .expect("the server-side jar has a record");
    assert_eq!(
        server_only.side,
        Some(instance::jar::Side::Server),
        "the server's own classification of its mods did not arrive"
    );

    // And the server is in the player's multiplayer list, so the import ends
    // somewhere they can join rather than at an empty list.
    assert_eq!(
        inst.joined_listing_address.as_deref(),
        Some(listing.address.as_str())
    );
    let list = std::fs::read(instance::multiplayer::file(&player.game_dir(&inst.id)))
        .expect("no multiplayer list was written");
    let text = String::from_utf8_lossy(&list);
    assert!(text.contains(&listing.name), "{text}");
    assert!(text.contains(&listing.address), "{text}");
    assert!(
        !text.contains(&port.to_string()),
        "the pack port reached the player's server list: {text}"
    );

    host.stop().await;
}

/// A fingerprint that belongs to a different certificate is refused, and the
/// refusal is a sentence rather than a TLS error.
///
/// The wrong fingerprint comes from a second real host with a second real
/// identity, not from a mutated hex string: a pin that is merely malformed is
/// refused by `Pin::from_hex` before any connection is made, which proves
/// nothing about the handshake. Note also that the tampered link below *parses*
/// — the parser's job is shape, not truth — so the only thing standing between
/// a player and the wrong machine is the verifier in `share::trust`.
///
/// Guards `MachineCert::verify_server_cert`, where a fingerprint that is not
/// the one expected becomes `ApplicationVerificationFailure`, and
/// `join::connect_error`, which turns that into the one sentence worth being
/// loud about. Make the verifier return `Ok(ServerCertVerified::assertion())`
/// unconditionally and the preview below succeeds.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds two ports and speaks real TLS"]
async fn a_certificate_from_another_machine_is_refused_in_words_a_player_can_read() {
    let mine_home = Scratch::ready("pin-mine").await;
    let theirs_home = Scratch::ready("pin-theirs").await;
    let (mine, theirs) = (&mine_home.paths, &theirs_home.paths);

    let code = Code::generate();
    share_a_server(mine, &code, 0, None).await;
    share_a_server(theirs, &code, 0, None).await;
    let (my_pin, their_pin) = (pin_of(mine).await, pin_of(theirs).await);
    assert_ne!(
        my_pin, their_pin,
        "two machines must not share a sharing certificate"
    );

    let host = Host::start(mine.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();
    let address = format!("127.0.0.1:{port}");

    // The right pin, so the rest of the test is about the pin and not the host.
    let right = link_to("127.0.0.1", port, &code, my_pin);
    within("the correct pin", join::preview(right.clone()))
        .await
        .unwrap_or_else(|e| panic!("the correct pin was refused: {e}"));

    // A link with the other machine's fingerprint in it. Well-formed, and a
    // link the parser is happy to hand on.
    let wrong = Link {
        pin: Some(their_pin),
        ..right.clone()
    };
    let Opened::Join(wrong) = Opened::parse(&wrong.to_url())
        .expect("a link naming the wrong certificate still parses — shape is all the parser knows")
    else {
        panic!("the parser read a join link as something else");
    };

    let err = within("the wrong pin, previewing", join::preview(wrong.clone()))
        .await
        .expect_err("the wrong machine's certificate was accepted");
    let said = err.to_string();
    println!("refused    {said}");
    assert!(
        said.contains("did not prove it is the server this link was made for"),
        "{said}"
    );
    reads_as_copy(&said).unwrap_or_else(|why| panic!("{why}"));
    names_the_address(&said, &address).unwrap_or_else(|why| panic!("{why}"));

    // And the download, which is the request that would actually take jars
    // from whoever answered.
    let archive = theirs.root.join("never.geodepack");
    let err = within(
        "the wrong pin, downloading",
        join::download(&wrong, &archive, &Progress::silent("live-join")),
    )
    .await
    .expect_err("the wrong machine's certificate was accepted for a download");
    let said = err.to_string();
    println!("refused    {said}");
    assert!(said.contains("nothing was downloaded"), "{said}");
    reads_as_copy(&said).unwrap_or_else(|why| panic!("{why}"));
    names_the_address(&said, &address).unwrap_or_else(|why| panic!("{why}"));
    assert!(
        !archive.exists(),
        "{} was created for a machine that never proved who it was",
        archive.display()
    );

    host.stop().await;
}

/// The attack a fingerprint check alone does not stop, driven from the
/// player's own path: a machine serving the admin's real certificate, which is
/// public, and signing the handshake with a key of its own.
///
/// Guards `MachineCert::verify_tls12_signature` and
/// `MachineCert::verify_tls13_signature`, and the `MachineCert::proven` call
/// inside each that only records a fingerprint once the key is proved.
/// Replace either with `Ok(HandshakeSignatureValid::assertion())` and this
/// impostor is let in, with the pin matching all the way.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port and speaks real TLS"]
async fn a_copied_certificate_without_its_key_is_refused_on_the_players_own_path() {
    let admin_home = Scratch::ready("replay-admin").await;
    let thief_home = Scratch::ready("replay-thief").await;
    let admin = Identity::load_or_create(&admin_home.paths).await.unwrap();
    let thief = Identity::load_or_create(&thief_home.paths).await.unwrap();

    let port = impostor(Answers::OverTls(
        presenting(&admin, &thief),
        Some(whole("200 OK", "application/json", b"{}")),
    ))
    .await;
    let address = format!("127.0.0.1:{port}");
    let code = Code::generate();

    // Pinned to the certificate the impostor is showing, which is the admin's
    // own: the fingerprint matches and the handshake signature does not.
    let pinned = link_to("127.0.0.1", port, &code, admin.pin());
    let err = within("a copied certificate", join::preview(pinned))
        .await
        .expect_err("an impostor holding only the certificate was let in");
    let said = err.to_string();
    println!("refused    {said}");
    assert!(
        said.contains("did not prove it is the server this link was made for"),
        "{said}"
    );
    reads_as_copy(&said).unwrap_or_else(|why| panic!("{why}"));
    names_the_address(&said, &address).unwrap_or_else(|why| panic!("{why}"));

    // And a typed address, which has nothing to pin yet, must not come away
    // having learnt this impostor's fingerprint as the machine's.
    let typed = join::JoinInput::Parts {
        address: address.clone(),
        code: code.display(),
    }
    .into_link(25590)
    .expect("an address and a code the parser accepts");
    assert!(typed.pin.is_none(), "a typed address carries no fingerprint");
    let err = within("a copied certificate, on first use", join::preview(typed))
        .await
        .expect_err("first use accepted an impostor holding only the certificate");
    println!("refused    {err}");
    reads_as_copy(&err.to_string()).unwrap_or_else(|why| panic!("{why}"));
    names_the_address(&err.to_string(), &address).unwrap_or_else(|why| panic!("{why}"));
}

/// Six machines that are not the host, and the client stops at every one of
/// them.
///
/// The cases that answer at all serve the admin's own certificate, so the pin
/// matches and the only thing left to refuse them is what they say. Each is a
/// real shape: a port nothing holds, a port something holds and never speaks
/// on, a web server where the pack port should be, the admin's machine with
/// something else listening on the port, a reply that is not HTTP, and a
/// perfectly good HTTP reply that is not a pack description.
///
/// Guards the status table in `join::request`, where a 404 is told apart from
/// a refused password and from a Geode that explained itself; the parse of the
/// description in `join::preview`; and the two deadlines that make this a
/// refusal rather than a wait, `join::PREVIEW_DEADLINE` and
/// `trust::CONNECT_TIMEOUT`. Remove the preview deadline and the silent cases
/// below sit for five minutes on `trust::READ_TIMEOUT` instead.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds several ports and speaks real TLS"]
async fn something_that_answers_but_is_not_the_host_is_refused_rather_than_waited_on() {
    let admin_home = Scratch::ready("impostors").await;
    let admin = Identity::load_or_create(&admin_home.paths).await.unwrap();
    let genuine = || admin.server_config().unwrap();
    let code = Code::generate();

    // A port nothing is listening on. Port 1 rather than a `:0` bind read back
    // and dropped, which the kernel may hand to something else in between.
    let nothing = 1u16;
    let cases: Vec<(&str, u16, Option<&str>)> = vec![
        ("nothing listening at all", nothing, None),
        (
            "accepts the connection and says nothing",
            impostor(Answers::Never).await,
            None,
        ),
        (
            "a web server where the pack port should be",
            impostor(Answers::AtOnce(whole(
                "400 Bad Request",
                "text/html",
                b"<html>Bad Request</html>",
            )))
            .await,
            None,
        ),
        (
            "the right machine, something else on the port",
            impostor(Answers::OverTls(
                genuine(),
                Some(whole("404 Not Found", "text/plain", b"nope")),
            ))
            .await,
            Some("something answered at that address, but it is not a Geode server sharing a pack"),
        ),
        (
            "the right machine, a reply that is not HTTP",
            impostor(Answers::OverTls(
                genuine(),
                Some(b"not http at all\r\n\r\n".to_vec()),
            ))
            .await,
            None,
        ),
        (
            "the right machine, an answer that is not a pack description",
            impostor(Answers::OverTls(
                genuine(),
                Some(whole("200 OK", "application/json", b"{}")),
            ))
            .await,
            Some("the server's answer was not a Geode pack description"),
        ),
        (
            "the right machine, building a pack forever",
            impostor(Answers::OverTls(genuine(), None)).await,
            None,
        ),
    ];

    for (what, port, must_say) in cases {
        let link = link_to("127.0.0.1", port, &code, admin.pin());
        let address = format!("127.0.0.1:{port}");
        let err = match within(what, join::preview(link)).await {
            Ok(preview) => panic!("{what} was taken for a Geode server: {:?}", preview.info),
            Err(err) => err,
        };
        let said = err.to_string();
        println!("refused    {what}: {said}");
        reads_as_copy(&said).unwrap_or_else(|why| panic!("{what}: {why}"));
        match must_say {
            // An answer arrived and was read, so what it was is the thing to
            // say.
            Some(must_say) => assert!(said.contains(must_say), "{what}: {said}"),
            // Nothing readable came back, so `connect_error` wrote this and the
            // address is the whole of what there is to tell the player.
            None => names_the_address(&said, &address).unwrap_or_else(|why| panic!("{what}: {why}")),
        }
    }
}

/// A host that declares a pack's length and then stops sending it is reported
/// as interrupted, and asking again lands a whole pack over whatever the first
/// attempt left behind.
///
/// The real `Host` cannot lie about a length — it reads the file's own size
/// into `Content-Length` — so the liar here is an impostor serving the admin's
/// certificate, which is also exactly what a connection dropped mid-pack looks
/// like from the player's side.
///
/// Guards the chunk error in `join::download`, where a broken body stream
/// becomes "the download was interrupted" rather than a transport error, and
/// the `File::create` beside it, which truncates rather than appends. Make
/// that an append and the second download below hands the importer the stump
/// with a whole pack glued onto the end of it, which opens as neither.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds ports and speaks real TLS"]
async fn a_host_that_stops_mid_pack_is_reported_and_asking_again_still_lands_a_whole_pack() {
    let admin_home = Scratch::ready("truncated-admin").await;
    let player_home = Scratch::ready("truncated-player").await;
    let (admin, player) = (&admin_home.paths, &player_home.paths);

    let code = Code::generate();
    let server = share_a_server(admin, &code, 0, None).await;
    let pin = pin_of(admin).await;

    // Claims a hundred kilobytes of zip and sends four bytes of it.
    let stump = b"PK\x03\x04";
    let liar = impostor(Answers::OverTls(
        Identity::load_or_create(admin)
            .await
            .unwrap()
            .server_config()
            .unwrap(),
        Some(reply("200 OK", "application/zip", 100_000, stump)),
    ))
    .await;

    let archive = join::incoming_dir(player).join("retried.geodepack");
    let err = within(
        "a host that stops mid-pack",
        join::download(
            &link_to("127.0.0.1", liar, &code, pin),
            &archive,
            &Progress::silent("live-join"),
        ),
    )
    .await
    .expect_err("a pack that stopped partway was taken as whole");
    let said = err.to_string();
    println!("refused    {said}");
    assert!(said.contains("the download was interrupted"), "{said}");
    // Whatever arrived is still on disk — `join::download` does not remove it,
    // its one caller does, and a launch clears the directory it is in. What
    // matters here is that it cannot survive into the retry.
    let left = std::fs::metadata(&archive).map(|meta| meta.len()).ok();
    println!("left       {left:?} bytes at {}", archive.display());

    // The same path, the real host, asked again.
    let host = Host::start(admin.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();
    let link = link_to("127.0.0.1", port, &code, pin);
    let preview = join::preview(link.clone())
        .await
        .unwrap_or_else(|e| panic!("the retry's preview failed: {e}"));
    join::download(&preview.link, &archive, &Progress::silent("live-join"))
        .await
        .unwrap_or_else(|e| panic!("the retry failed: {e}"));

    let names = entries(&archive);
    assert!(
        names.iter().any(|name| name == "geode/manifest.json"),
        "the retried pack is not a whole pack: {names:?}"
    );
    let (inst, missing) = install_joined(player, &link, &preview.info, &archive).await;
    assert!(missing.is_empty(), "{missing:?}");
    assert_eq!(inst.joined_server_id.as_deref(), Some(server.id.as_str()));
    println!("instance   {} imported from the retried pack", inst.id);

    host.stop().await;
}

/// The code is what decides, and a wrong one is refused the same way whichever
/// route it was presented on — while a code that is not a code never reaches
/// the host at all.
///
/// Three things are being told apart. A code that matches gets the pack. A
/// well-formed code that belongs to nobody is refused, and the client says the
/// one thing a player can act on: ask the admin for the current link. And a
/// code of the wrong length is refused by `Code::parse` before a link exists
/// to fetch with, so the host is never asked — which is why there is no
/// wrong-length case on the wire below, and why trying to write one would be a
/// test of something the design prevents.
///
/// Guards the `UNAUTHORIZED` arm of `join::request`, where a 401 becomes
/// `Error::PasswordRefused` rather than a status or whatever the body said,
/// and the length check that ends `Code::parse`, where anything but ten
/// characters is no code. `live_share_host` proves the host's two refusals are
/// the same bytes; this proves the client turns both into the same one thing,
/// which that file cannot see because it does not go through the client.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port and speaks real TLS"]
async fn the_code_decides_and_a_wrong_one_is_refused_the_same_way_on_both_routes() {
    let admin_home = Scratch::ready("codes-admin").await;
    let player_home = Scratch::ready("codes-player").await;
    let (admin, player) = (&admin_home.paths, &player_home.paths);

    let code = Code::generate();
    share_a_server(admin, &code, 0, None).await;
    let pin = pin_of(admin).await;
    let host = Host::start(admin.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();

    // The code that matches.
    let right = link_to("127.0.0.1", port, &code, pin);
    let preview = join::preview(right)
        .await
        .unwrap_or_else(|e| panic!("the right code was refused: {e}"));

    // One that does not. Well-formed, so the refusal cannot be about its shape.
    let wrong = link_to("127.0.0.1", port, &a_different_code(&code), pin);
    let refused_preview = join::preview(wrong.clone())
        .await
        .expect_err("somebody else's code described the pack");
    let archive = join::incoming_dir(player).join("never.geodepack");
    let refused_download = join::download(&wrong, &archive, &Progress::silent("live-join"))
        .await
        .expect_err("somebody else's code downloaded the pack");

    println!("refused    {refused_preview}");
    // One variant for both routes is the whole claim. A refused password has a
    // variant of its own because pressing Play has to tell it from a server
    // that is simply not there — one is fixed by opening a new link, the other
    // by waiting — and a client that read anything out of the refusal's body
    // would be able to say which of the two requests it had made.
    for (route, refused) in [("preview", &refused_preview), ("download", &refused_download)] {
        assert!(
            matches!(refused, Error::PasswordRefused),
            "the {route} reported a wrong password as something else: {refused:?}"
        );
    }
    assert!(
        refused_preview.to_string().contains("ask the admin"),
        "a refused password has to say what to do about it: {refused_preview}"
    );
    assert!(
        !archive.exists(),
        "{} was created for a code the host refused",
        archive.display()
    );

    // A code of the wrong length never gets as far as a request. The address
    // is the live host's, so if a link were somehow built from it, it would
    // reach a port that really answers.
    for not_a_code in ["GEODE-00000-0000", "GEODE-00000-000000", "", "7K2QF9XWMP1"] {
        let err = join::JoinInput::Parts {
            address: format!("127.0.0.1:{port}"),
            code: not_a_code.to_string(),
        }
        .into_link(25590)
        .expect_err(&format!("{not_a_code:?} was taken for a share code"));
        assert!(
            err.to_string().contains("GEODE-XXXXX-XXXXX"),
            "a password that is not one should say what one looks like: {err}"
        );
    }
    // And the same through the link parser, which is what a pasted link goes
    // through.
    let tampered = format!(
        "geode://join?addr=127.0.0.1:{port}&code=GEODE-00000-0000&pin={}",
        pin.to_hex()
    );
    let err = Link::parse(&tampered).expect_err("a link with a short code parsed");
    assert!(err.to_string().contains("password is not valid"), "{err}");

    // The pack still comes down for the code that is right, after all of that.
    let dest = join::incoming_dir(player).join("after.geodepack");
    join::download(&preview.link, &dest, &Progress::silent("live-join"))
        .await
        .unwrap_or_else(|e| panic!("the right code stopped working: {e}"));
    assert!(std::fs::metadata(&dest).unwrap().len() > 0);

    host.stop().await;
}

/// A player who hangs up while the host is building leaves nothing on either
/// machine, and asking again works.
///
/// Cancelled during the build rather than during the body, because that is the
/// half that is not a race: the host hashes and deflates every jar before it
/// sends a byte, and the filler is there to make that take long enough to
/// catch. The body over loopback is gone in milliseconds, and a test that
/// tried to cancel inside it would pass or fail on the clock.
///
/// Guards the spawned build in `host::pack` — a task of its own that keeps its
/// `TempFile` cleanup, so a player who hung up mid-build has their pack built
/// and then removed. Drop the cleanup, or let the build be owned by the
/// request it came from, and `share/outgoing` fills up with packs nobody
/// received. Also guards the stage `join::download` reports before it asks for
/// anything, and `join::unless_cancelled`: the job is on screen to cancel
/// before the server has answered, and the cancel is noticed while it is still
/// silent.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "builds a real pack and binds a port"]
async fn a_cancelled_join_leaves_nothing_behind_and_asking_again_works() {
    let admin_home = Scratch::ready("cancel-admin").await;
    let player_home = Scratch::ready("cancel-player").await;
    let (admin, player) = (&admin_home.paths, &player_home.paths);

    let code = Code::generate();
    let filler = filler_mb();
    println!("filler     {filler} MiB of incompressible mods");
    share_a_server(admin, &code, filler, None).await;
    let pin = pin_of(admin).await;
    let host = Host::start(admin.clone(), 0, None).await.unwrap();
    let link = link_to("127.0.0.1", host.local_addr.port(), &code, pin);

    let recorder = Arc::new(Recorder::default());
    let progress = Progress::new(recorder.clone(), "join:cancelled");
    let archive = join::incoming_dir(player).join("cancelled.geodepack");
    let running = tokio::spawn({
        let (link, archive, progress) = (link.clone(), archive.clone(), progress.clone());
        async move { join::download(&link, &archive, &progress).await }
    });

    // On screen before the server has answered, which is the only reason there
    // is anything to press Cancel on while a big pack is being built.
    until_stage(&recorder, "Waiting for the server").await;
    // And the host really is building one, so what follows is not a vacuous
    // "nothing was left behind because nothing was ever made".
    let built = building_started(admin).await;
    progress.cancel_flag().store(true, Ordering::Relaxed);
    let outcome = within("cancelling a join mid-build", running).await.unwrap();
    assert!(
        matches!(outcome, Err(Error::Cancelled)),
        "a cancelled join must come back cancelled, not failed: {outcome:?}"
    );
    assert!(
        !archive.exists(),
        "{} was created before the server had answered",
        archive.display()
    );

    // The host finishes the build it started and then throws the pack away,
    // because nobody is left to send it to.
    outgoing_empties(admin, patience()).await;
    assert!(
        !outgoing(admin).join(&built).exists(),
        "{built} is still on the admin's disk after the player hung up"
    );

    // And asking again works, with the same code and the same link.
    let preview = join::preview(link.clone())
        .await
        .unwrap_or_else(|e| panic!("the preview after a cancel failed: {e}"));
    join::download(&preview.link, &archive, &Progress::silent("live-join"))
        .await
        .unwrap_or_else(|e| panic!("the download after a cancel failed: {e}"));
    let names = entries(&archive);
    assert!(
        names.iter().any(|name| name == "geode/manifest.json"),
        "{names:?}"
    );
    println!(
        "pack       {} bytes after asking again",
        std::fs::metadata(&archive).unwrap().len()
    );

    // Nothing of that download is left on the admin's machine either.
    outgoing_empties(admin, patience()).await;

    // And the directory the app clears at launch is what any stump would have
    // been in, so a join interrupted by Geode stopping leaves no pack behind.
    // The one thing it may not reach past is the share directory itself, which
    // is where this machine's own sharing identity lives.
    join::clear_incoming(player).await;
    assert!(
        !join::incoming_dir(player).exists(),
        "{} outlived clear_incoming",
        join::incoming_dir(player).display()
    );
    assert!(
        player.share_dir().is_dir(),
        "clearing the incoming directory took the share directory with it"
    );

    host.stop().await;
}

/// Two players download from one host at the same time, and each gets a whole
/// pack rather than half of the other's.
///
/// `MAX_BUILDS` is two, so both builds run together — which is the case where
/// a pack built to a shared path would have one request streaming a file the
/// other was still writing.
///
/// Guards the per-request pack filename in `host::pack`. Drop the UUID from
/// it and the two downloads below race on one file: both still "succeed", and
/// what arrives is some interleaving of the two builds. Comparing each
/// carried file against the admin's own copy of it is what catches that — two
/// downloads that merely returned `Ok` would not, and neither would comparing
/// the two packs only to each other.
///
/// Note what is **not** asserted: that the two packs are the same bytes. They
/// are not, reliably. `modrinth.index.json` carries its `dependencies` as a
/// `serde_json` object built from a `HashMap`, whose iteration order is not
/// stable between two instances, so two builds of one server write the same
/// pair of keys in either order — same length, different bytes, and a deflate
/// stream that differs by a byte or so. Measured: 33561285 against 33561284.
/// Nothing depends on a pack being byte-reproducible — a server's revision is
/// computed from its records rather than from the zip, exactly so that a
/// rebuild is not an update — so this is a fact about the format and not a
/// defect. It is asserted below as what it is, because a test that compared
/// whole files would fail about one time in two for a reason that is not the
/// one it is looking for.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "builds two real packs at once and binds a port"]
async fn two_players_download_at_once_and_both_get_a_whole_pack() {
    let admin_home = Scratch::ready("pair-admin").await;
    let first_home = Scratch::ready("pair-first").await;
    let second_home = Scratch::ready("pair-second").await;
    let admin = &admin_home.paths;

    let code = Code::generate();
    let filler = filler_mb();
    println!("filler     {filler} MiB of incompressible mods");
    share_a_server(admin, &code, filler, None).await;
    let pin = pin_of(admin).await;
    let host = Host::start(admin.clone(), 0, None).await.unwrap();
    let link = link_to("127.0.0.1", host.local_addr.port(), &code, pin);

    let players = [("first", &first_home), ("second", &second_home)];
    let began = Instant::now();
    let fetching: Vec<_> = players
        .iter()
        .map(|(who, home)| {
            let (who, link) = (who.to_string(), link.clone());
            let dest = join::incoming_dir(&home.paths).join(format!("{who}.geodepack"));
            tokio::spawn(async move {
                // When this player's request was in flight, so the test can
                // say the two of them really were at once rather than one
                // after the other.
                let asked = Instant::now();
                let preview = join::preview(link).await?;
                join::download(&preview.link, &dest, &Progress::silent(&who)).await?;
                Ok::<(PathBuf, Duration, Duration), Error>((
                    dest,
                    asked - began,
                    Instant::now() - began,
                ))
            })
        })
        .collect();

    let mut landed = Vec::new();
    let mut windows = Vec::new();
    for (task, (who, _)) in fetching.into_iter().zip(players) {
        let (dest, asked, done) = within(&format!("{who}'s download"), task)
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{who} could not join: {e}"));
        println!(
            "pack       {who}: {} bytes, asked at {:.2}s, had it at {:.2}s",
            std::fs::metadata(&dest).unwrap().len(),
            asked.as_secs_f64(),
            done.as_secs_f64()
        );
        windows.push((who, asked, done));
        landed.push((who, dest));
    }

    // Both requests were open at the same time, which is the whole premise:
    // one host, two players, two packs being built. Two tasks spawned is not
    // the same as two requests in flight, and a host that served them one
    // after the other would pass every assertion below.
    let [(_, first_asked, first_done), (second, second_asked, second_done)] = windows[..] else {
        panic!("two players, two windows");
    };
    assert!(
        first_asked < second_done && second_asked < first_done,
        "{second} only started at {second_asked:?} after the other had finished at {first_done:?}, \
         so this is two downloads in a row and not two at once"
    );

    // Every file the admin shares, checked against the admin's own copy of it.
    // The filler is 32 MiB of random bytes that compress to nothing, so it is
    // the entry an interleaved build would mangle, and the only way to know it
    // arrived whole is to have the original to hand.
    let run = admin.server_run("survival");
    let shared: Vec<(String, Vec<u8>)> = [
        (format!("overrides/mods/{SERVER_MOD}.jar"), run.join("mods").join(format!("{SERVER_MOD}.jar"))),
        (format!("client-overrides/mods/{CLIENT_MOD}.jar"), run.join("client-mods").join(format!("{CLIENT_MOD}.jar"))),
        ("overrides/mods/filler.jar".into(), run.join("mods").join("filler.jar")),
    ]
    .into_iter()
    .map(|(name, source)| {
        let bytes = std::fs::read(&source)
            .unwrap_or_else(|e| panic!("{} is not on the admin's disk: {e}", source.display()));
        (name, bytes)
    })
    .collect();

    for (who, dest) in &landed {
        for (name, bytes) in &shared {
            let arrived = carried(dest, name);
            assert_eq!(
                arrived.len(),
                bytes.len(),
                "{who} received {} bytes of {name} where the admin has {}",
                arrived.len(),
                bytes.len()
            );
            assert!(
                arrived == *bytes,
                "{who}'s {name} is not the file the admin shared, which is what two \
                 builds sharing one file on the admin's disk looks like"
            );
        }
        println!("whole      {who}: every shared file arrived byte for byte");
    }

    // The same pack, down to the entry list and the index — whose object keys
    // are compared as JSON rather than as bytes, for the reason in this test's
    // own documentation.
    let (first, second) = (&landed[0].1, &landed[1].1);
    assert_eq!(entries(first), entries(second));
    assert!(
        entries(first).iter().any(|name| name == "geode/manifest.json"),
        "{:?}",
        entries(first)
    );
    assert_eq!(
        carried(first, "geode/manifest.json"),
        carried(second, "geode/manifest.json"),
        "the two players were told different things about the same server"
    );
    let raw_index = |pack: &Path| carried(pack, "modrinth.index.json");
    let index = |pack: &Path| {
        serde_json::from_slice::<serde_json::Value>(&raw_index(pack)).expect("the index is JSON")
    };
    assert_eq!(index(first), index(second));
    // Printed rather than asserted, because it is the measurement behind this
    // test's documentation: two indexes that parse to the same value and are
    // not the same bytes are the same keys written in a different order, which
    // is where the whole pack's one-byte difference comes from. Asserting
    // either way would be asserting a `HashMap`'s seed.
    println!(
        "index      {} and {} bytes, {}",
        raw_index(first).len(),
        raw_index(second).len(),
        if raw_index(first) == raw_index(second) {
            "identical this run"
        } else {
            "the same value written differently"
        }
    );

    // And neither pack is still on the admin's disk.
    outgoing_empties(admin, patience()).await;
    // Two files, not one: the only thing the host's naming promises from
    // outside the process.
    assert_eq!(
        BTreeSet::from([first, second]).len(),
        2,
        "the two players downloaded to one path"
    );

    host.stop().await;
}

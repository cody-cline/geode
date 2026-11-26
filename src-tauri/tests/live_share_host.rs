//! The pack host over a real network: a real port, real TLS, another machine.
//!
//! `share/host.rs` is the one listener strangers reach, and until this file
//! there was no way to point it at a second machine. The admin's side of a
//! join lives behind the desktop window, and `geoded` serves no sharing
//! routes at all, so nothing outside the app could start the listener — which
//! means the listener had never been asked to answer anything that was not
//! loopback. This starts the real one, with the same `Host::start` call
//! `share::service` makes, against a scratch `GEODE_HOME`; prints what
//! another machine needs in order to reach it; and holds the port open while
//! somebody does.
//!
//! Every test here is `#[ignore]`d: each binds a real port, and the harness
//! talks to the network.
//!
//! ```sh
//! # the assertions, which need nothing but loopback
//! cargo test --test live_share_host -- --ignored --nocapture --skip serves_its_pack
//!
//! # the harness: hold the real pack port open for five minutes, print the link
//! cargo test --test live_share_host serves_its_pack -- --ignored --nocapture
//!
//! # twenty minutes, on a port nothing else holds, aimed at a known machine,
//! # and failing unless a download really arrives from somewhere
//! GEODE_SHARE_SECONDS=1200 GEODE_SHARE_PORT=25591 \
//!   GEODE_SHARE_PEER=192.168.6.161 GEODE_SHARE_CODE=GEODE-TEST1-TEST2 \
//!   GEODE_SHARE_EXPECT_DOWNLOAD=1 \
//!   cargo test --test live_share_host serves_its_pack -- --ignored --nocapture
//! ```
//!
//! The harness reads five things from the environment, all optional:
//! `GEODE_SHARE_PORT` (default the app's own 25590), `GEODE_SHARE_CODE` (a
//! code to type on the other machine, rather than the random one it prints),
//! `GEODE_SHARE_SECONDS` (how long to stay up), `GEODE_SHARE_PEER` (the
//! machine that is going to connect, which is what makes the address it
//! prints the right one on a machine holding a VPN open), and
//! `GEODE_SHARE_FILLER_MB` (how big to make the pack). Setting
//! `GEODE_SHARE_EXPECT_DOWNLOAD` turns the wait into an assertion.
//!
//! What a green run does **not** prove:
//!
//! - **Two networks.** Everything reached here is one LAN or loopback. The
//!   router mapping (`share/portmap.rs`), carrier-grade NAT, and a player
//!   somewhere on the internet are all untouched — the gap
//!   `docs/sharing-and-hosting.md` already records.
//! - **The admin's screen.** Nothing drives the Share tab, so neither the link
//!   a human actually copies nor `share::service`'s decision about when to
//!   start and stop the listener is exercised. This calls `Host::start`
//!   directly, which is what the service calls once it has decided.
//! - **A refusal arriving from elsewhere.** The listener keeps no request log
//!   and exposes none — see [`watch`] — so a 401 served to the other machine
//!   leaves no trace this side. Downloads are the only requests a harness can
//!   count.
//! - **The limits.** The connection ceilings, the slow-download cut and the
//!   revocation sweep belong to `share/host.rs`'s own unit tests, which can
//!   shrink them to something a test can wait out. This file runs against the
//!   real ones and waits none of them out.

use std::collections::BTreeSet;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderName, CACHE_CONTROL, CONTENT_TYPE};
use reqwest::StatusCode;

use geode_lib::events::{EventSink, Events};
use geode_lib::net::Progress;
use geode_lib::paths::Paths;
use geode_lib::server::share::ServerShare;
use geode_lib::server::{Server, ServerKind};
use geode_lib::share::code::Code;
use geode_lib::share::host::{Host, INFO_PATH, PACK_PATH};
use geode_lib::share::identity::Identity;
use geode_lib::share::join::{self, Verified};
use geode_lib::share::link::{Address, Link, Pin};
use geode_lib::share::trust::{Trust, TrustedClient};

/// The fixed pause before a refusal, mirrored from the private
/// `REFUSAL_DELAY` in `share::host`. The assertions below allow for the clock
/// rather than pinning the number: what is load-bearing is that the pause
/// happened at all, and that it was the same pause whichever way the code was
/// wrong.
const REFUSAL_DELAY: Duration = Duration::from_millis(400);

/// What the host answers a request whose code it does not accept, byte for
/// byte. Written out rather than read from the crate, because the point of
/// every refusal assertion here is that this exact body is what arrives.
const REFUSAL: &str = r#"{"error":"that password is not accepted here"}"#;

/// How long the harness waits for somebody, unless `GEODE_SHARE_SECONDS` says
/// otherwise.
const DEFAULT_STAY: u64 = 300;

/// How much incompressible filler the harness puts in the server's `mods/`,
/// unless `GEODE_SHARE_FILLER_MB` says otherwise.
///
/// Not decoration, and not only realism. [`watch`] can see a download at all
/// only while the pack file for it exists, and a pack of a few kilobytes is
/// built and sent inside a single poll — measured: three real downloads of a
/// 1.4 KB pack were seen as none. A pack this size takes long enough to build
/// and hand over that nothing is missed, and it is also the size a real
/// server's pack actually is.
const DEFAULT_FILLER_MB: usize = 32;

// ---------------------------------------------------------------- the machine

/// A `GEODE_HOME` of its own under the system temporary directory, removed
/// when the test ends however it ends.
///
/// `paths::Scratch` is exactly this and is `#[cfg(test)]`, so it exists for
/// the crate's own unit tests and not for anything under `tests/`. The copy
/// lives here rather than as a change to `paths.rs`, and it is the same guard
/// for the same reason: a `remove_dir_all` at the end of a test does not run
/// when the test panics, which is precisely when the directory is worth
/// having gone.
struct Scratch {
    paths: Paths,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("geode-live-share-{tag}-{}", uuid::Uuid::new_v4()));
        // Never the real one, and never inside it. The user's own data
        // directory holds their accounts, their instances, and the one
        // certificate every link they have ever handed out is pinned to —
        // and this file generates a certificate and serves it to the network.
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

/// `share/outgoing`, where the host writes the pack it is sending.
///
/// Derived here because `share::host::outgoing` is private and nothing public
/// reports it. It is the one window a process outside the host has onto a
/// request in flight, which is why the harness watches it and why the
/// refusal tests can say that nothing was built — and it is a copy of a path
/// the host owns, so if the host ever moves it this stops seeing anything
/// rather than failing.
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

/// A shared Fabric server on disk, with a mod of its own, a mod for its
/// players, and `filler_mb` megabytes of a jar that does not compress.
///
/// Written the way the app writes one: `Server::save` for `server.json`, and
/// `server::share::save` for `share.json`, which is what `share::start` calls
/// once it has decided which code to use. The jars are what the pack is then
/// built from — disk is the source of truth for content, so none of them
/// needs a record.
async fn share_a_server(paths: &Paths, code: &Code, filler_mb: usize) -> Server {
    let mut server = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
    server.id = "survival".into();
    // For Fabric the build *is* the loader version a player needs, and
    // `pack::client_loader` refuses a server whose build is empty.
    server.build = "0.16.9".into();

    let run = paths.server_run(&server.id);
    for (folder, id, name, environment) in [
        ("mods", "geode-live-server", "Geode Live (server side)", "server"),
        ("client-mods", "geode-live-client", "Geode Live (client only)", "client"),
    ] {
        let dir = run.join(folder);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join(format!("{id}.jar")), jar(id, name, environment))
            .await
            .unwrap();
    }
    if filler_mb > 0 {
        // Random bytes rather than a jar, because the point of them is that
        // they do not compress: the pack has to end up the size the filler is.
        // `host.rs`'s own tests fill a `filler.jar` the same way.
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
            listing: None,
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

/// An address written the way `Address::parse` and a URL both want it: an
/// IPv6 address is full of colons, so the brackets are what keep it apart
/// from the port. Unbracketed, the parser refuses it outright — which is the
/// right answer, since `2001:db8::1:25590` may or may not end in a port.
fn as_typed(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// A link to this host, built the way `service::local_link_for` builds one:
/// the address read back through the strict parser, because nothing here is
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

// --------------------------------------------------------------- the requests

/// One reply from the host, as the wire saw it.
#[derive(Debug)]
struct Reply {
    status: StatusCode,
    content_type: String,
    cache_control: String,
    body: Vec<u8>,
    took: Duration,
}

fn header(headers: &HeaderMap, name: HeaderName) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// One request to the host over the real pinned TLS, reported uninterpreted.
///
/// Deliberately not through `share::join`, which is the player's side and maps
/// a 401 to `Error::PasswordRefused`: what is being measured is that two
/// refusals are the same bytes, and an interpreted error cannot show that.
/// The client is still the real one, because a pinned certificate is the only
/// way in.
async fn ask(port: u16, pin: Pin, path: &str, code: Option<&str>) -> Reply {
    let trusted = TrustedClient::new(Trust::Pinned(pin)).expect("a pinned client");
    let mut request = trusted
        .client
        .get(format!("https://127.0.0.1:{port}{path}"))
        .timeout(Duration::from_secs(30));
    if let Some(code) = code {
        request = request.bearer_auth(code);
    }
    let started = Instant::now();
    let response = request.send().await.expect("the host answered");
    let status = response.status();
    let content_type = header(response.headers(), CONTENT_TYPE);
    let cache_control = header(response.headers(), CACHE_CONTROL);
    let body = response.bytes().await.expect("the whole answer").to_vec();
    Reply {
        status,
        content_type,
        cache_control,
        body,
        took: started.elapsed(),
    }
}

/// A well-formed code that is not the one this server has, so a refusal of it
/// cannot be about the shape of what arrived.
fn a_different_code(from: &Code) -> Code {
    let other = Code::parse("GEODE-00000-00000").expect("a well-formed code");
    assert_ne!(other.raw(), from.raw(), "pick another one");
    other
}

/// The address this machine would use to reach `toward`, asked of the OS the
/// way `share::portmap` asks it: connecting a UDP socket sends nothing, it
/// only makes the OS resolve the route and name the source address it would
/// use.
///
/// `portmap::local_address` is the library's own answer to this, and it
/// answers only toward a router UPnP really found and only when the result is
/// a private IPv4 — neither of which a test can arrange — so the question is
/// put here instead.
fn route_source(toward: IpAddr) -> Option<IpAddr> {
    let any: IpAddr = match toward {
        IpAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    let socket = std::net::UdpSocket::bind((any, 0)).ok()?;
    // The port is irrelevant; nothing is ever sent to it.
    socket.connect((toward, 9)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

/// Where another machine should aim, with how each answer was arrived at.
///
/// The route toward the machine that is actually going to connect is the only
/// answer that is right on a machine holding a VPN open: there the default
/// route leaves down the tunnel, and the address at the far end of it reaches
/// nobody on the LAN. That is the trap `portmap::local_address` documents, so
/// `GEODE_SHARE_PEER` is how to avoid it here.
fn reachable_addresses() -> Vec<(IpAddr, String)> {
    let mut found: Vec<(IpAddr, String)> = Vec::new();
    let mut add = |ip: Option<IpAddr>, how: &str| {
        if let Some(ip) = ip {
            if !found.iter().any(|(seen, _)| *seen == ip) {
                found.push((ip, how.to_string()));
            }
        }
    };
    if let Some(peer) = std::env::var("GEODE_SHARE_PEER")
        .ok()
        .and_then(|peer| peer.parse::<IpAddr>().ok())
    {
        add(route_source(peer), &format!("the route toward {peer}"));
    }
    add(
        route_source(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))),
        "the default route",
    );
    found
}

/// Prints what the host reports to the admin, as it reports it.
struct Printing;

impl Events for Printing {
    fn emit(&self, topic: &str, payload: serde_json::Value) {
        println!("event      {topic} {payload}");
    }
}

/// Hold the port open for `stay`, printing what can be seen of the requests
/// that arrive, and answer with how many packs left this machine.
///
/// **The listener keeps no request log and exposes none.** Nothing in
/// `share/host.rs` reports a request to its caller: the `EventSink` it is
/// handed carries exactly one thing, a pack that would not build
/// (`host::PACK_FAILED_EVENT`), and a refusal — the thing a harness most
/// wants to count — reaches nothing but the socket it came in on. What *is*
/// visible from outside the process is the pack file the host writes under
/// `share/outgoing` for exactly as long as a download is being built and
/// sent, so that is what this watches: one line as a download starts, one as
/// it ends, and nothing at all for a preview, a 401, or a connection that
/// never spoke. Counting those needs either a counter the host does not keep
/// or a packet capture running beside it.
///
/// A download quicker than one poll is missed outright, which is a measured
/// fact and not a worry: three real downloads of a 1.4 KB pack were counted
/// as none. [`DEFAULT_FILLER_MB`] is what keeps this window wide enough to
/// see through.
async fn watch(paths: &Paths, stay: Duration) -> usize {
    let until = Instant::now() + stay;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut downloads = 0usize;
    let mut beat = Instant::now();
    while Instant::now() < until {
        let now: BTreeSet<String> = std::fs::read_dir(outgoing(paths))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        for started in now.difference(&seen) {
            downloads += 1;
            println!("sending    {started}");
        }
        for finished in seen.difference(&now) {
            println!("sent       {finished}");
        }
        seen = now;
        if beat.elapsed() >= Duration::from_secs(30) {
            beat = Instant::now();
            println!(
                "waiting    {}s left, {downloads} download(s) so far",
                until.saturating_duration_since(Instant::now()).as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    downloads
}

// ------------------------------------------------------------------ the tests

/// The harness. Everything above it is assertions; this one is a port held
/// open for a human on another machine, with the assertions run first so that
/// what it prints can be trusted.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds the real pack port and waits for another machine"]
async fn serves_its_pack_to_another_machine() {
    let scratch = Scratch::new("harness");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let code = match std::env::var("GEODE_SHARE_CODE") {
        Ok(typed) => Code::parse(&typed)
            .expect("GEODE_SHARE_CODE is not a share code — it looks like GEODE-XXXXX-XXXXX"),
        Err(_) => Code::generate(),
    };
    let filler_mb = std::env::var("GEODE_SHARE_FILLER_MB")
        .ok()
        .and_then(|mb| mb.parse().ok())
        .unwrap_or(DEFAULT_FILLER_MB);
    let server = share_a_server(paths, &code, filler_mb).await;
    let pin = pin_of(paths).await;

    let asked_for: u16 = std::env::var("GEODE_SHARE_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| geode_lib::config::ShareConfig::default().port);
    let stay = Duration::from_secs(
        std::env::var("GEODE_SHARE_SECONDS")
            .ok()
            .and_then(|seconds| seconds.parse().ok())
            .unwrap_or(DEFAULT_STAY),
    );

    let sink: EventSink = Arc::new(Printing);
    let host = Host::start(paths.clone(), asked_for, Some(sink))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let port = host.local_addr.port();
    if asked_for != 0 {
        assert_eq!(port, asked_for, "the host bound a port nobody asked for");
    }
    assert_ne!(port, 0, "nothing is listening");
    // Listening, not merely bound: a TCP connection the kernel accepts.
    drop(
        TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
            .expect("the listener accepts connections"),
    );

    // Proved to ourselves before anybody else is told to try it: the real
    // player-side code, over the real TLS, against this listener.
    let loopback = link_to("127.0.0.1", port, &code, pin);
    let preview = join::preview(loopback)
        .await
        .unwrap_or_else(|e| panic!("the host would not describe its pack: {e}"));
    let dest = paths.root.join("loopback.geodepack");
    join::download(&preview.link, &dest, &Progress::silent("live-share"))
        .await
        .unwrap_or_else(|e| panic!("the host would not send its pack: {e}"));
    let bytes = std::fs::metadata(&dest).unwrap().len();

    println!();
    println!("== geode pack host, live");
    println!("home       {}", paths.root.display());
    println!(
        "server     {} — {} {} (loader {})",
        server.name,
        server.kind.as_str(),
        server.mc_version,
        server.build
    );
    println!("bound      {}", host.local_addr);
    println!("port       {port}");
    println!("code       {}", code.display());
    println!("code-raw   {}", code.raw());
    println!("pin        {}", pin.to_hex());
    println!("loopback   {bytes} bytes of pack downloaded over TLS from this machine");
    println!(
        "preview    {} mods, {} of them server-only, revision {}",
        preview.info.mods, preview.info.server_mods, preview.info.revision
    );
    let addresses = reachable_addresses();
    for (ip, how) in &addresses {
        println!("address    {ip}   ({how})");
    }
    if addresses.is_empty() {
        println!("address    none found — ask the OS yourself and use the port above");
    }
    for (ip, _) in &addresses {
        println!(
            "link       {}",
            link_to(&as_typed(*ip), port, &code, pin).to_url()
        );
    }
    println!(
        "link-local {}",
        link_to("127.0.0.1", port, &code, pin).to_url()
    );
    if let Some((ip, _)) = addresses.first() {
        let host = as_typed(*ip);
        // `--insecure` is how curl is told to skip the pin, which no Geode
        // ever does: it is here to separate "the port answers" from "the
        // certificate is the one in the link". The line below is the check it
        // skips — its output should be the fingerprint printed above.
        println!(
            "curl       curl --insecure --header \"Authorization: Bearer {}\" https://{host}:{port}{INFO_PATH}",
            code.raw()
        );
        println!(
            "verify     openssl s_client -connect {host}:{port} </dev/null 2>/dev/null | openssl x509 -noout -fingerprint -sha256"
        );
    }
    println!("staying    {}s", stay.as_secs());
    println!();

    let downloads = watch(paths, stay).await;
    host.stop().await;
    println!();
    println!("downloads  {downloads} pack(s) left this machine while the harness was up");

    if std::env::var_os("GEODE_SHARE_EXPECT_DOWNLOAD").is_some() {
        assert!(
            downloads > 0,
            "GEODE_SHARE_EXPECT_DOWNLOAD was set and no pack was downloaded in {}s",
            stay.as_secs()
        );
    }
}

/// A wrong code and no code at all are the same answer, to the byte and to
/// the pause: "the same body and the same delay whichever was wrong".
///
/// Three ways of being wrong, because they take different routes through
/// `Code::matches` — nothing presented at all, a code that parses and is
/// somebody else's, and something that is not a code at all — and both
/// routes, because a scanner should not be able to tell them apart either.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port and speaks real TLS"]
async fn a_missing_code_and_a_wrong_one_are_refused_identically() {
    let scratch = Scratch::new("refusals");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let code = Code::generate();
    share_a_server(paths, &code, 0).await;
    let pin = pin_of(paths).await;
    let host = Host::start(paths.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();
    let wrong = a_different_code(&code);

    let mut replies = Vec::new();
    for path in [INFO_PATH, PACK_PATH] {
        for (how, presented) in [
            ("no code at all", None),
            ("somebody else's code", Some(wrong.raw())),
            ("something that is not a code", Some("not a code at all")),
        ] {
            replies.push((format!("{path} with {how}"), ask(port, pin, path, presented).await));
        }
    }
    host.stop().await;

    let (first, floor) = (&replies[0].1, REFUSAL_DELAY - Duration::from_millis(100));
    for (what, reply) in &replies {
        println!("{:<6} {:>5}ms  {what}", reply.status.as_u16(), reply.took.as_millis());
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{what}");
        assert_eq!(
            String::from_utf8_lossy(&reply.body),
            REFUSAL,
            "{what} was told something other than the uniform refusal"
        );
        assert_eq!(reply.body, first.body, "{what} got a different body");
        assert_eq!(reply.content_type, first.content_type, "{what}");
        assert_eq!(reply.cache_control, first.cache_control, "{what}");
        assert!(
            reply.took >= floor,
            "{what} was refused in {:?}, before the fixed pause had run",
            reply.took
        );
    }

    // The pause is uniform as well as present. Loopback and a fixed sleep, so
    // the band is generous and still far inside anything that would leak which
    // half of the check failed.
    let slowest = replies.iter().map(|(_, r)| r.took).max().unwrap();
    let fastest = replies.iter().map(|(_, r)| r.took).min().unwrap();
    assert!(
        slowest - fastest < Duration::from_millis(250),
        "refusals took between {fastest:?} and {slowest:?}, which is not one fixed pause"
    );
}

/// The code that does match gets the pack: the preview a player is shown, and
/// then the bytes, through the real player-side code in `share::join`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port and speaks real TLS"]
async fn the_right_code_downloads_the_pack() {
    let scratch = Scratch::new("pack");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let code = Code::generate();
    let server = share_a_server(paths, &code, 0).await;
    let pin = pin_of(paths).await;
    let host = Host::start(paths.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();

    let preview = join::preview(link_to("127.0.0.1", port, &code, pin))
        .await
        .unwrap_or_else(|e| panic!("the preview failed: {e}"));
    assert!(
        matches!(preview.verified, Verified::Pinned),
        "a link carrying a fingerprint is pinned, not trusted on first use"
    );
    assert_eq!(preview.info.name, server.name);
    assert_eq!(preview.info.mc_version, "1.21.1");
    assert_eq!(preview.info.loader, "fabric");
    assert_eq!(preview.info.loader_version, "0.16.9");
    assert_eq!(preview.info.server_kind, "fabric");
    assert_eq!(preview.info.server_id, server.id);
    assert_eq!(preview.info.mods, 2, "both jars travel to the player");
    assert_eq!(
        preview.info.server_mods, 1,
        "the one that declared itself server-only is counted as one"
    );
    assert_eq!(preview.info.config_files, 0, "this share sends no config");
    assert!(
        !preview.info.revision.is_empty(),
        "a pack a player can check for changes carries a revision"
    );

    let dest = paths.root.join("downloaded.geodepack");
    join::download(&preview.link, &dest, &Progress::silent("live-share"))
        .await
        .unwrap_or_else(|e| panic!("the download failed: {e}"));
    host.stop().await;

    let names = entries(&dest);
    println!("pack       {} bytes", std::fs::metadata(&dest).unwrap().len());
    for name in &names {
        println!("entry      {name}");
    }
    // `modrinth.index.json` is what other launchers read, and deleting
    // `geode/` has to leave a pack Prism installs — so both are present and
    // neither is optional.
    assert!(names.iter().any(|name| name == "modrinth.index.json"));
    assert!(names.iter().any(|name| name == "geode/manifest.json"));
    // Neither jar is on any provider, so neither can travel as a reference
    // and both arrive as bytes.
    assert!(
        names.iter().any(|name| name.ends_with("geode-live-server.jar")),
        "the server's own mod is missing: {names:?}"
    );
    assert!(
        names.iter().any(|name| name.ends_with("geode-live-client.jar")),
        "the client-only mod is missing: {names:?}"
    );
}

/// Nothing is built until a code has matched.
///
/// `share/outgoing` is the whole of what a pack build leaves outside the
/// process, and the host clears it as it starts, so its absence after a run
/// of refusals is the claim — and its appearance after one accepted download
/// is what makes that absence mean anything, rather than being a directory
/// this host never uses.
///
/// Note what this does *not* say. Matching a code at all means reading every
/// `servers/*/share.json`, so "nothing is read from disk" cannot be true as
/// written; what is true is that nothing is *built*, and that the server's own
/// files are never reached — which is the next test.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port and speaks real TLS"]
async fn nothing_is_built_before_a_code_matches() {
    let scratch = Scratch::new("outgoing");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let code = Code::generate();
    share_a_server(paths, &code, 0).await;
    let pin = pin_of(paths).await;
    let host = Host::start(paths.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();
    let wrong = a_different_code(&code);

    let dir = outgoing(paths);
    assert!(
        !dir.exists(),
        "the host is meant to clear {} as it starts",
        dir.display()
    );

    for path in [INFO_PATH, PACK_PATH] {
        for presented in [None, Some(wrong.raw()), Some("not a code at all")] {
            let reply = ask(port, pin, path, presented).await;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
            assert!(
                !dir.exists(),
                "a refused {path} request created {}",
                dir.display()
            );
        }
    }

    let preview = join::preview(link_to("127.0.0.1", port, &code, pin))
        .await
        .unwrap();
    assert!(
        !dir.exists(),
        "a preview describes the pack without writing one"
    );

    let dest = paths.root.join("downloaded.geodepack");
    join::download(&preview.link, &dest, &Progress::silent("live-share"))
        .await
        .unwrap();
    assert!(
        dir.exists(),
        "an accepted download is meant to build its pack in {}",
        dir.display()
    );

    // And the file goes with the response that sent it, rather than
    // accumulating: the same thing `host.rs`'s own tests wait for.
    let waiting = Instant::now();
    while std::fs::read_dir(&dir).map_or(0, Iterator::count) > 0 {
        assert!(
            waiting.elapsed() < Duration::from_secs(5),
            "the pack that was sent is still in {}",
            dir.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    host.stop().await;
}

/// Sets a directory's mode and puts it back, whatever happens: a scratch
/// directory nobody can read is one [`Scratch`] cannot remove.
#[cfg(unix)]
struct Unreadable(PathBuf, std::fs::Permissions);

#[cfg(unix)]
impl Unreadable {
    fn new(dir: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let was = std::fs::metadata(dir).unwrap().permissions();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        Self(dir.to_path_buf(), was)
    }
}

#[cfg(unix)]
impl Drop for Unreadable {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, self.1.clone());
    }
}

/// The stronger half of the same rule: a refusal never reaches the server's
/// own files.
///
/// With the server's `run/` folder unreadable, the code that does match fails
/// — which is what proves the folder is read on that side of the match — while
/// a code that does not match is refused in exactly the same words as before.
/// The refusal is therefore not merely cheap; it never got as far as the
/// files.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "binds a port and speaks real TLS"]
async fn a_refusal_never_reaches_the_servers_own_files() {
    let scratch = Scratch::new("unreadable");
    let paths = &scratch.paths;
    paths.ensure_all().await.unwrap();

    let code = Code::generate();
    let server = share_a_server(paths, &code, 0).await;
    let pin = pin_of(paths).await;
    let host = Host::start(paths.clone(), 0, None).await.unwrap();
    let port = host.local_addr.port();
    let wrong = a_different_code(&code);

    let run = paths.server_run(&server.id);
    let locked = Unreadable::new(&run);
    // Root reads a directory whose mode is zero, so there the pair below would
    // prove nothing. Ask rather than guess at who is running this.
    if std::fs::read_dir(&run).is_ok() {
        drop(locked);
        host.stop().await;
        println!("skipped    this user can read a directory with no permissions at all");
        return;
    }

    let refused = ask(port, pin, INFO_PATH, Some(wrong.raw())).await;
    let accepted = ask(port, pin, INFO_PATH, Some(code.raw())).await;
    // Before any assertion, so a failure still leaves a removable directory.
    drop(locked);
    host.stop().await;

    println!("refused    {} in {:?}", refused.status, refused.took);
    println!("accepted   {} in {:?}", accepted.status, accepted.took);
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        String::from_utf8_lossy(&refused.body),
        REFUSAL,
        "an unreadable server changed what a refusal says, so the refusal read it"
    );
    assert_eq!(
        accepted.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "with the server's files unreadable the matched code should fail — if it \
         succeeded, this test proves nothing about what the refusal avoided"
    );
}

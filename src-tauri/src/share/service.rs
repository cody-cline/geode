//! Sharing while it runs: the pack server and its router mapping, kept in step
//! with what is shared and how.
//!
//! Nothing listens while nothing is shared. Sharing a first server starts the
//! pack server and, unless the admin chose to forward the port themselves,
//! asks the router to open it; unsharing the last stops both and gives the
//! port back. Every change to the running state is announced, so the UI never
//! has to poll to find out whether players can reach the admin.

use std::net::IpAddr;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{watch, Mutex};

use crate::config::{PortMapping, ShareConfig};
use crate::events::{self, EventSink};
use crate::paths::Paths;
use crate::server::share::ServerShare;

use super::host::Host;
use super::identity::Identity;
use super::link::{Address, Link, Pin};
use super::portmap::{self, PortMapper, Reachability};

/// Sent with a [`ShareStatus`] whenever the running state changes.
pub const STATUS_EVENT: &str = "geode://share-status";

/// How often manual setup asks the router for its public address again, and
/// how soon it tries again after the router did not answer.
const DETECT_EVERY: Duration = Duration::from_secs(30 * 60);
const DETECT_RETRY: Duration = Duration::from_secs(60);

/// Everything the UI shows about whether players can reach this machine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareStatus {
    /// Which router setup the admin chose. The mapping below cannot say this
    /// on its own: one that never started because nothing listens is `off` in
    /// either mode, and telling an admin who chose automatic that setup is
    /// manual sends them to fix the wrong thing.
    pub mode: PortMapping,
    /// With manual setup, the public address the router gave when asked.
    pub detected: Option<Detected>,
    /// With manual setup, whether the router is still being asked for the
    /// first time — so no `detected` address means nothing yet, not that the
    /// router would not say.
    pub asking: bool,
    pub listening: bool,
    pub port: u16,
    pub listen_error: Option<String>,
    pub mapping: portmap::Status,
    /// The address links are made with, when one is known.
    pub address: Option<String>,
    /// The certificate fingerprint players pin, as hex.
    pub fingerprint: String,
}

/// A public address the router reported, without being asked to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    pub ip: IpAddr,
    pub reachability: Reachability,
}

/// What asking the router for its public address has come to so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Detection {
    /// Whether an ask has finished, answered or not.
    asked: bool,
    found: Option<Detected>,
}

impl Detection {
    /// After another ask. An address the router gave before is kept when this
    /// ask went unanswered: one request lost on busy Wi-Fi does not mean the
    /// address changed, and forgetting it would take a working link away.
    fn after(self, read: Option<Detected>) -> Self {
        Self {
            asked: true,
            found: read.or(self.found),
        }
    }
}

#[derive(Default)]
pub struct ShareService {
    /// Whole reconciles, one at a time — held while the settings are read, so
    /// the last reconcile to run is always the one that saw the latest ones.
    serial: Mutex<()>,
    inner: Mutex<Inner>,
    /// Never contact the router, not even to ask. Only tests set this: they run
    /// on a real network, and asking a real router anything is not theirs to do.
    quiet: bool,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    /// Why the pack server could not start, until it next can.
    listen_error: Option<String>,
    /// Loaded once. The identity never changes while the app runs.
    pin: Option<Pin>,
}

struct Running {
    host: Host,
    port: u16,
    mapper: Option<PortMapper>,
    /// With manual setup: what the router says the public address is.
    detected: Option<watch::Receiver<Detection>>,
    /// Re-announces the status as the mapping or the detected address changes.
    watcher: Option<tokio::task::JoinHandle<()>>,
}

/// Whether anything is shared, as far as can be told.
enum Wanted {
    Share,
    Stop,
    /// A share file exists that could not be read. Stopping would cut players
    /// off from a server that is still shared; starting would guess.
    Unknown,
}

impl ShareService {
    /// Bring the running state in line with what is shared and the settings,
    /// reading the settings only once it is this call's turn.
    pub async fn reconcile(&self, state: &crate::state::AppState, sink: Option<EventSink>) {
        let _turn = self.serial.lock().await;
        let config = state.config().await.share;
        self.reconcile_with(&state.paths, &config, sink).await;
    }

    /// The same, with the settings given. What tests call; the app goes through
    /// [`ShareService::reconcile`] so concurrent reconciles cannot apply stale
    /// settings last.
    pub async fn reconcile_with(&self, paths: &Paths, config: &ShareConfig, sink: Option<EventSink>) {
        let wanted = match crate::server::share::list(paths).await {
            Ok(shared) if !shared.servers.is_empty() => Wanted::Share,
            Ok(shared) if shared.unreadable.is_empty() => Wanted::Stop,
            _ => Wanted::Unknown,
        };
        let pin = self.pin(paths).await;
        let mut inner = self.inner.lock().await;

        let stale = inner.running.as_ref().is_some_and(|running| {
            matches!(wanted, Wanted::Stop) || running.port != config.port
        });
        if stale {
            if let Some(running) = inner.running.take() {
                stop(running).await;
            }
        }

        match wanted {
            Wanted::Stop => inner.listen_error = None,
            Wanted::Share if inner.running.is_none() => {
                match Host::start(paths.clone(), config.port).await {
                    Ok(host) => {
                        inner.listen_error = None;
                        inner.running = Some(Running {
                            host,
                            port: config.port,
                            mapper: None,
                            detected: None,
                            watcher: None,
                        });
                    }
                    Err(e) => inner.listen_error = Some(e.to_string()),
                }
            }
            _ => {}
        }

        if let Some(running) = inner.running.as_mut() {
            // Whatever brought this reconcile about may have taken a code away
            // — a server unshared, or deleted — and a listener kept running
            // must not keep serving the connections that showed it.
            running.host.cut_revoked(paths).await;

            // Whichever way the router is handled, today's settings decide it,
            // and the watcher is restarted so what it announces uses them too.
            if let Some(watcher) = running.watcher.take() {
                watcher.abort();
            }
            match config.mapping {
                PortMapping::Auto => {
                    running.detected = None;
                    if running.mapper.is_none() {
                        running.mapper = Some(PortMapper::start(config.port));
                    }
                }
                PortMapping::Manual => {
                    if let Some(mapper) = running.mapper.take() {
                        mapper.stop().await;
                    }
                    if running.detected.is_none() && !self.quiet {
                        running.detected = Some(detect());
                    }
                }
            }
            running.watcher = Some(tokio::spawn(announce(
                running.mapper.as_ref().map(PortMapper::status),
                running.detected.clone(),
                config.clone(),
                pin,
                sink.clone(),
            )));
        }

        let status = assemble(&inner, config, pin);
        drop(inner);
        events::emit(&sink, STATUS_EVENT, &status);
    }

    pub async fn status(&self, paths: &Paths, config: &ShareConfig) -> ShareStatus {
        let pin = self.pin(paths).await;
        let inner = self.inner.lock().await;
        assemble(&inner, config, pin)
    }

    /// The link for a shared server, when there is an address to put in it.
    pub async fn link_for(
        &self,
        paths: &Paths,
        config: &ShareConfig,
        share: &ServerShare,
    ) -> Option<Link> {
        let status = self.status(paths, config).await;
        let address = reachable_address(config, &status.mapping, status.detected)?;
        // Not while nothing listens on the port: a link would lead nowhere,
        // however good the address in it.
        if !status.listening {
            return None;
        }
        let pin = match address {
            Address::Direct { .. } => Some(self.pin(paths).await?),
            Address::Https { .. } => None,
        };
        Some(Link {
            address,
            code: share.code.clone(),
            pin,
        })
    }

    /// Cut every preview and download whose server is no longer shared as it
    /// was when it came in. For a change to a share that needs no reconcile —
    /// a new code, or whether config is sent — but must still reach the
    /// players already being served, not only their next request.
    pub async fn cut_revoked(&self, paths: &Paths) {
        if let Some(running) = self.inner.lock().await.running.as_ref() {
            running.host.cut_revoked(paths).await;
        }
    }

    /// Stop everything and give the port back, for when the app exits. The
    /// mapping's removal is bounded, so a router that stops answering cannot
    /// hold the exit up; its lease is the safety net.
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(running) = inner.running.take() {
            stop(running).await;
        }
    }

    async fn pin(&self, paths: &Paths) -> Option<Pin> {
        if let Some(pin) = self.inner.lock().await.pin {
            return Some(pin);
        }
        let pin = Identity::load_or_create(paths).await.ok()?.pin();
        self.inner.lock().await.pin = Some(pin);
        Some(pin)
    }
}

async fn stop(mut running: Running) {
    if let Some(watcher) = running.watcher.take() {
        watcher.abort();
    }
    // The listener first: its connections are cut at once, where removing the
    // mapping can wait seconds on a slow router — and a download of a server
    // just unshared must not run on for them.
    let mapper = running.mapper.take();
    running.host.stop().await;
    if let Some(mapper) = mapper {
        mapper.stop().await;
    }
}

/// Ask the router for its public address now and then, changing nothing.
///
/// Manual setup means the admin forwards the port; it does not have to mean
/// they also look up their own address and type it in before a link exists.
fn detect() -> watch::Receiver<Detection> {
    let (publish, detected) = watch::channel(Detection::default());
    tauri::async_runtime::spawn(async move {
        loop {
            let read = portmap::read_external_ip().await.ok().map(|ip| Detected {
                ip,
                reachability: portmap::classify(ip),
            });
            let wait = if read.is_some() {
                DETECT_EVERY
            } else {
                DETECT_RETRY
            };
            let next = publish.borrow().after(read);
            if publish.send(next).is_err() {
                return; // nobody is listening any more
            }
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = publish.closed() => return,
            }
        }
    });
    detected
}

/// Announce every change in the mapping or the detected address.
async fn announce(
    mut mapping: Option<watch::Receiver<portmap::Status>>,
    mut detected: Option<watch::Receiver<Detection>>,
    config: ShareConfig,
    pin: Option<Pin>,
    sink: Option<EventSink>,
) {
    loop {
        let changed = async {
            match (&mut mapping, &mut detected) {
                (Some(m), _) => m.changed().await.is_ok(),
                (None, Some(d)) => d.changed().await.is_ok(),
                (None, None) => false,
            }
        };
        if !changed.await {
            return;
        }
        let current = mapping
            .as_ref()
            .map(|m| m.borrow().clone())
            .unwrap_or(portmap::Status::Off);
        let detection = detected.as_ref().map(|d| *d.borrow());
        let found = detection.and_then(|d| d.found);
        let status = ShareStatus {
            mode: config.mapping,
            detected: found,
            asking: detection.is_some_and(|d| !d.asked),
            listening: true,
            port: config.port,
            listen_error: None,
            address: reachable_address(&config, &current, found).map(|a| a.display()),
            mapping: current,
            fingerprint: pin.map(|p| p.to_hex()).unwrap_or_default(),
        };
        events::emit(&sink, STATUS_EVENT, &status);
    }
}

fn assemble(inner: &Inner, config: &ShareConfig, pin: Option<Pin>) -> ShareStatus {
    // Listening means listening on the port the settings name now. A listener
    // left on an old port is not something a link built from these settings
    // could reach.
    let running = inner.running.as_ref().filter(|running| running.port == config.port);
    let mapping = running
        .and_then(|running| running.mapper.as_ref())
        .map(|mapper| mapper.status().borrow().clone())
        .unwrap_or(portmap::Status::Off);
    let detection = running
        .and_then(|running| running.detected.as_ref())
        .map(|d| *d.borrow());
    let detected = detection.and_then(|d| d.found);
    ShareStatus {
        mode: config.mapping,
        detected,
        asking: detection.is_some_and(|d| !d.asked),
        listening: running.is_some(),
        port: config.port,
        listen_error: inner.listen_error.clone(),
        // Nothing to give players while nothing listens, even with a public
        // address set.
        address: running
            .and_then(|_| reachable_address(config, &mapping, detected))
            .map(|a| a.display()),
        mapping,
        fingerprint: pin.map(|p| p.to_hex()).unwrap_or_default(),
    }
}

/// The address players should use, if one is known to work.
///
/// The admin's own public address wins whenever it is set: they know about
/// the domain or tunnel in front of this machine, and the router does not.
/// Otherwise only an address the router reported *and* that is actually public
/// is offered — a link built from a carrier-grade NAT address would be a link
/// that cannot work, handed out as if it could.
fn reachable_address(
    config: &ShareConfig,
    mapping: &portmap::Status,
    detected: Option<Detected>,
) -> Option<Address> {
    if let Some(text) = config
        .public_address
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Address::parse(text, config.port).ok();
    }
    let (ip, port) = match (mapping, detected) {
        (
            portmap::Status::Mapped {
                external_ip,
                port,
                reachability: Reachability::Public,
            },
            _,
        ) => (*external_ip, *port),
        // Manual setup: the admin forwarded the share port themselves, so the
        // address the router reports is reached on that port.
        (
            portmap::Status::Off,
            Some(Detected {
                ip,
                reachability: Reachability::Public,
            }),
        ) if config.mapping == PortMapping::Manual => (ip, config.port),
        _ => return None,
    };
    // Through the parser rather than built by hand, so an IPv6 address is
    // written the same way everywhere a link is read.
    Address::parse(&std::net::SocketAddr::new(ip, port).to_string(), port).ok()
}

#[cfg(test)]
mod tests {
    //! Sharing end to end, over real TLS on loopback: an admin shares a server,
    //! a player previews, downloads and imports it.
    //!
    //! Every test here runs with the mapping set to **manual**. Automatic
    //! mapping asks the real router on the real network to open a port, which
    //! no test has any business doing; `portmap` is tested against a mock
    //! gateway instead.

    use super::*;
    use crate::config::{PortMapping, ShareConfig};
    use crate::instance::content::{ContentKind, Origin};
    use crate::net::Progress;
    use crate::server::share::{self as server_share, ServerShare};
    use crate::server::{Server, ServerKind};
    use crate::share::code::Code;
    use crate::share::join;
    use crate::share::link::{Address, Link, Pin};
    use std::io::{Read, Write};
    use std::path::Path;

    fn paths(tag: &str) -> Paths {
        Paths {
            root: std::env::temp_dir().join(format!("geode-e2e-{tag}-{}", std::process::id())),
        }
    }

    /// A Fabric mod jar declaring which side it runs on.
    fn fabric_jar(id: &str, environment: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("fabric.mod.json", options).unwrap();
            write!(
                zip,
                r#"{{"schemaVersion":1,"id":"{id}","version":"1.0.0","environment":"{environment}"}}"#
            )
            .unwrap();
            zip.finish().unwrap();
        }
        bytes
    }

    /// A shared Fabric server on disk, with things that must reach a player and
    /// things that must never leave the admin's machine.
    async fn shared_server(paths: &Paths) -> (Server, ServerShare) {
        let _ = tokio::fs::remove_dir_all(&paths.root).await;
        let mut server = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
        server.id = "survival".into();
        server.build = "0.16.9".into();
        let share = ServerShare {
            code: Code::generate(),
            include_config: false,
            shared: true,
        };
        let run = paths.server_run(&server.id);
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("server.properties", b"enable-rcon=true\nrcon.password=hunter2\n".to_vec()),
            ("ops.json", br#"[{"name":"admin"}]"#.to_vec()),
            ("config/tokens.toml", b"discord = \"hunter2\"\n".to_vec()),
            ("mods/homemade.jar", fabric_jar("homemade", "*")),
            ("mods/server-only.jar", fabric_jar("serveronly", "server")),
        ];
        for (rel, bytes) in files {
            let path = run.join(rel);
            tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
            tokio::fs::write(path, bytes).await.unwrap();
        }
        server.save(paths).await.unwrap();
        server_share::save(paths, &server.id, &share).await.unwrap();
        (server, share)
    }

    /// The service as the app runs it, minus any contact with the router.
    fn service() -> ShareService {
        ShareService {
            quiet: true,
            ..Default::default()
        }
    }

    async fn pin_of(paths: &Paths) -> Pin {
        Identity::load_or_create(paths).await.unwrap().pin()
    }

    /// A link the way an admin's Geode writes one, read back the way a
    /// player's Geode reads one — so the real parser is in the path.
    fn link(port: u16, code: &Code, pin: Pin) -> Link {
        let written = Link {
            address: Address::Direct { host: "127.0.0.1".into(), port },
            code: code.clone(),
            pin: Some(pin),
        };
        Link::parse(&written.to_url()).expect("a link Geode writes parses")
    }

    fn entries(archive: &Path) -> Vec<(String, Vec<u8>)> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        (0..zip.len())
            .map(|i| {
                let mut entry = zip.by_index(i).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (entry.name().to_string(), bytes)
            })
            .collect()
    }

    /// The whole feature, as a player meets it: preview, download, import.
    #[tokio::test]
    async fn a_player_joins_a_shared_server_end_to_end() {
        let admin = paths("join");
        let (_, share) = shared_server(&admin).await;
        let host = Host::start(admin.clone(), 0).await.unwrap();
        let link = link(host.local_addr.port(), &share.code, pin_of(&admin).await);

        // What the player is shown before deciding.
        let preview = join::preview(link.clone()).await.unwrap();
        assert_eq!(preview.info.name, "Survival");
        assert_eq!(preview.info.mc_version, "1.21.1");
        assert_eq!(preview.info.loader, "fabric");
        assert_eq!(preview.info.loader_version, "0.16.9");
        assert_eq!(
            preview.info.mods, 2,
            "every mod in the server's mods folder: {:?}",
            preview.info
        );
        assert_eq!(preview.verified, join::Verified::Pinned);

        // What actually crosses the network.
        let player = paths("join-player");
        let _ = tokio::fs::remove_dir_all(&player.root).await;
        let archive = player.root.join("incoming.geodepack");
        join::download(&preview.link, &archive, &Progress::silent("t")).await.unwrap();

        let sent = entries(&archive);
        let names: Vec<&str> = sent.iter().map(|(n, _)| n.as_str()).collect();
        for jar in [
            "overrides/mods/homemade.jar",
            "overrides/mods/server-only.jar",
        ] {
            assert!(names.contains(&jar), "{jar} was not sent: {names:?}");
        }
        for never in ["server.properties", "ops.json", "tokens.toml"] {
            assert!(!names.iter().any(|n| n.contains(never)), "{never} was sent: {names:?}");
        }
        // Belt and braces: the secret appears in no entry's contents either.
        for (name, bytes) in &sent {
            assert!(
                !bytes.windows(7).any(|w| w == b"hunter2"),
                "a secret travelled inside {name}"
            );
        }

        // And it imports as the server's, through the same path as the app.
        let described = crate::instance::modpack::preview(archive.clone()).await.unwrap();
        let mut instance = crate::instance::create(
            &player,
            described.name,
            described.mc_version,
            described.loader,
            described.loader_version,
        )
        .await
        .unwrap();
        let missing = crate::share::pack::import(
            &reqwest::Client::new(),
            &player,
            &mut instance,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(missing.is_empty(), "{missing:?}");
        assert_eq!(instance.loader, crate::meta::Loader::Fabric);
        assert_eq!(instance.loader_version, "0.16.9");

        let mods = player.game_dir(&instance.id).join("mods");
        assert!(mods.join("homemade.jar").is_file());
        let record = instance
            .content
            .iter()
            .find(|r| r.file_name == "homemade.jar")
            .expect("the server's jar has a record");
        assert_eq!(record.origin, Origin::Server);
        crate::instance::content::remove_in(&mods, &mut instance.content, ContentKind::Mod, "homemade.jar")
            .await
            .expect_err("a player cannot delete the server's mod");

        host.stop().await;
    }

    /// Everything that should turn a player away does, and says why.
    #[tokio::test]
    async fn a_wrong_password_the_wrong_machine_and_a_replaced_password_are_refused() {
        let admin = paths("refuse");
        let (server, share) = shared_server(&admin).await;
        let host = Host::start(admin.clone(), 0).await.unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&admin).await;
        let original = share.code.clone();

        let err = join::preview(link(port, &Code::generate(), pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );

        let impostor = Pin([7u8; 32]);
        let err = join::preview(link(port, &original, impostor)).await.unwrap_err();
        assert!(err.to_string().contains("did not prove"), "{err}");

        // A new password cuts off the old one on the very next request.
        let share = server_share::replace_password(&admin, &server.id)
            .await
            .unwrap();
        let err = join::preview(link(port, &original, pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );
        join::preview(link(port, &share.code, pin)).await.unwrap();

        // And a whole-server save made from a copy loaded before the rotation —
        // a backup finishing, an install saving — cannot bring the old
        // password back, because the password does not live in the file it
        // writes.
        let stale = server.clone();
        stale.save(&admin).await.unwrap();
        let err = join::preview(link(port, &original, pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );

        // Stopping sharing cuts everyone off, with the password kept.
        let current = share.code.clone();
        server_share::stop(&admin, &server.id).await.unwrap();
        let err = join::preview(link(port, &current, pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );

        // Sharing again brings the same link back, and not the replaced one.
        server_share::start(&admin, &server.id).await.unwrap();
        join::preview(link(port, &current, pin)).await.unwrap();
        let err = join::preview(link(port, &original, pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );

        // Nothing but the two paths is served, even with a good connection.
        let client = crate::share::trust::TrustedClient::new(crate::share::trust::Trust::Pinned(pin)).unwrap();
        let response = client
            .client
            .get(format!("https://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

        host.stop().await;
    }

    /// A typed address trusts the first certificate it sees, and the download
    /// that follows is pinned to exactly that one.
    #[tokio::test]
    async fn a_typed_address_is_pinned_to_the_machine_that_first_answered() {
        let admin = paths("typed");
        let (_, share) = shared_server(&admin).await;
        let host = Host::start(admin.clone(), 0).await.unwrap();
        let port = host.local_addr.port();

        let typed = join::JoinInput::Parts {
            address: format!("  127.0.0.1:{port} "),
            code: share.code.display(),
        }
        .into_link(25590)
        .unwrap();
        assert!(typed.pin.is_none());

        // Unverified links do not get a download.
        let archive = paths("typed-player").root.join("p.geodepack");
        join::download(&typed, &archive, &Progress::silent("t"))
            .await
            .expect_err("a direct address with no fingerprint must preview first");

        let preview = join::preview(typed).await.unwrap();
        assert_eq!(preview.verified, join::Verified::FirstUse);
        assert_eq!(preview.link.pin, Some(pin_of(&admin).await));
        join::download(&preview.link, &archive, &Progress::silent("t")).await.unwrap();

        host.stop().await;
    }

    fn manual(port: u16, public_address: Option<String>) -> ShareConfig {
        let config = ShareConfig {
            port,
            mapping: PortMapping::Manual,
            public_address,
        };
        // The guard every test here relies on: never ask a real router.
        assert_eq!(config.mapping, PortMapping::Manual);
        config
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn port_is_free(port: u16) -> bool {
        std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
    }

    /// The service listens exactly while something is shared, follows a port
    /// change, gives the port back, and builds working links.
    #[tokio::test]
    async fn sharing_listens_only_while_something_is_shared() {
        let admin = paths("service");
        let (server, share) = shared_server(&admin).await;
        let service = service();

        // Nothing shared: nothing listens.
        server_share::stop(&admin, &server.id).await.unwrap();
        let first = free_port();
        let config = manual(first, Some(format!("127.0.0.1:{first}")));
        service.reconcile_with(&admin, &config, None).await;
        assert!(!service.status(&admin, &config).await.listening);

        // Shared again: listening, with the same password, and a link that
        // works.
        assert_eq!(
            server_share::start(&admin, &server.id).await.unwrap(),
            share
        );
        service.reconcile_with(&admin, &config, None).await;
        let status = service.status(&admin, &config).await;
        assert!(status.listening, "{status:?}");
        assert_eq!(status.mapping, portmap::Status::Off, "manual means the router is left alone");
        assert_eq!(status.address.as_deref(), Some(format!("127.0.0.1:{first}").as_str()));
        assert!(!port_is_free(first));

        let link = service.link_for(&admin, &config, &share).await.expect("an address is set");
        assert_eq!(link.pin, Some(pin_of(&admin).await));
        join::preview(link).await.unwrap();

        // No public address and no mapping: no link, rather than a wrong one.
        let blind = manual(first, None);
        assert!(service.link_for(&admin, &blind, &share).await.is_none());

        // A new port moves the listener and frees the old one.
        let second = free_port();
        let moved = manual(second, Some(format!("127.0.0.1:{second}")));
        service.reconcile_with(&admin, &moved, None).await;
        assert!(port_is_free(first), "the old port was not given back");
        let link = service.link_for(&admin, &moved, &share).await.unwrap();
        join::preview(link).await.unwrap();

        // A share file that cannot be read is not the same as nothing shared:
        // stopping would cut players off from a server that still is.
        tokio::fs::write(server_share::path(&admin, &server.id), b"{ not json").await.unwrap();
        service.reconcile_with(&admin, &moved, None).await;
        assert!(service.status(&admin, &moved).await.listening, "an unreadable share stopped the listener");
        server_share::save(&admin, &server.id, &share).await.unwrap();

        // Stopped again: stops, and the port is free.
        server_share::stop(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &moved, None).await;
        assert!(!service.status(&admin, &moved).await.listening);
        assert!(port_is_free(second), "the port was not given back");

        // And sharing again listens again, on the same password.
        server_share::start(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &moved, None).await;
        assert!(service.status(&admin, &moved).await.listening);
        let link = service.link_for(&admin, &moved, &share).await.unwrap();
        join::preview(link).await.unwrap();

        service.shutdown().await;
        let _ = tokio::fs::remove_dir_all(&admin.root).await;
    }

    /// Unsharing one server while another stays shared keeps the listener up
    /// — and cuts off a download of the one unshared there and then, not only
    /// its next request.
    #[tokio::test]
    async fn unsharing_a_server_cuts_off_its_downloads_while_another_stays_shared() {
        use crate::share::host::tests::{ask_for_pack, read_rest};

        let admin = paths("unshare-cut");
        let (server, share) = shared_server(&admin).await;
        // Megabytes that do not compress, so the download outlasts every buffer
        // between the two ends and is still under way when the server is unshared.
        let mut filler = vec![0u8; 4 * 1024 * 1024];
        getrandom::fill(&mut filler).unwrap();
        tokio::fs::write(admin.server_run(&server.id).join("mods/filler.jar"), filler)
            .await
            .unwrap();
        let mut other = Server::new("Creative".into(), ServerKind::Fabric, "1.21.1".into());
        other.id = "creative".into();
        other.build = "0.16.9".into();
        other.save(&admin).await.unwrap();
        let other_share = ServerShare {
            code: Code::generate(),
            include_config: false,
            shared: true,
        };
        server_share::save(&admin, &other.id, &other_share)
            .await
            .unwrap();

        let service = service();
        let config = manual(free_port(), None);
        service.reconcile_with(&admin, &config, None).await;
        assert!(service.status(&admin, &config).await.listening);

        let (mut tls, received, length) = ask_for_pack(config.port, &share.code).await;
        server_share::stop(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &config, None).await;
        assert!(
            service.status(&admin, &config).await.listening,
            "the listener stopped with a server still shared"
        );

        let (received, _) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "a download of an unshared server ran to its end"
        );

        service.shutdown().await;
        let _ = tokio::fs::remove_dir_all(&admin.root).await;
    }

    /// Unsharing the last shared server stops the listener, and removing the
    /// router's mapping on the way can wait seconds on a slow router. A
    /// download of the server unshared is cut before that wait, not after.
    #[tokio::test]
    async fn stopping_cuts_off_downloads_before_waiting_on_the_router() {
        use crate::share::host::tests::{ask_for_pack, outgoing_files, read_rest};
        use crate::share::portmap::tests::a_mapping_whose_removal_hangs;

        let admin = paths("stop-cut");
        let (server, share) = shared_server(&admin).await;
        // Megabytes that do not compress, so the download outlasts every buffer
        // between the two ends and is still under way when sharing stops.
        let mut filler = vec![0u8; 4 * 1024 * 1024];
        getrandom::fill(&mut filler).unwrap();
        tokio::fs::write(admin.server_run(&server.id).join("mods/filler.jar"), filler)
            .await
            .unwrap();

        let host = Host::start(admin.clone(), 0).await.unwrap();
        let port = host.local_addr.port();
        let (mapper, _router, removal) = a_mapping_whose_removal_hangs().await;
        let running = Running {
            host,
            port,
            mapper: Some(mapper),
            detected: None,
            watcher: None,
        };

        let (mut tls, received, length) = ask_for_pack(port, &share.code).await;
        let stopping = tokio::spawn(stop(running));
        // Nothing is read meanwhile, so the download is still waiting on the
        // player, and only cutting it removes its pack file.
        let waiting = std::time::Instant::now();
        while outgoing_files(&admin) > 0 {
            assert!(
                waiting.elapsed() < removal / 2,
                "the download ran on while the router was waited on"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!stopping.is_finished(), "the router was not waited on");
        let (received, _) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "a download ran to its end as sharing stopped"
        );
        stopping.await.unwrap();

        let _ = tokio::fs::remove_dir_all(&admin.root).await;
    }

    /// A new code needs no reconcile, so what cuts off a download started with
    /// the old one is the service checking codes again once the new one is
    /// saved — which is what replacing a code does.
    #[tokio::test]
    async fn a_new_code_cuts_off_downloads_through_the_service() {
        use crate::share::host::tests::{ask_for_pack, read_rest};

        let admin = paths("rotate-cut");
        let (server, mut share) = shared_server(&admin).await;
        // Megabytes that do not compress, so the download outlasts every buffer
        // between the two ends and is still under way when the code changes.
        let mut filler = vec![0u8; 4 * 1024 * 1024];
        getrandom::fill(&mut filler).unwrap();
        tokio::fs::write(admin.server_run(&server.id).join("mods/filler.jar"), filler)
            .await
            .unwrap();

        let service = service();
        let config = manual(free_port(), None);
        service.reconcile_with(&admin, &config, None).await;
        assert!(service.status(&admin, &config).await.listening);

        let (mut tls, received, length) = ask_for_pack(config.port, &share.code).await;
        share.code = Code::generate();
        server_share::save(&admin, &server.id, &share)
            .await
            .unwrap();
        service.cut_revoked(&admin).await;

        let (received, _) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "a download ran to its end on a code replaced"
        );

        service.shutdown().await;
        let _ = tokio::fs::remove_dir_all(&admin.root).await;
    }

    /// A link is only ever built from an address players can actually reach.
    /// The router saying yes is not enough: behind carrier-grade NAT or a second
    /// router, the address it reports goes nowhere, and a link made from it
    /// would be handed out as if it worked.
    #[test]
    fn links_are_only_built_from_an_address_that_works() {
        let mapped = |ip: &str, reachability| portmap::Status::Mapped {
            external_ip: ip.parse().unwrap(),
            port: 25590,
            reachability,
        };
        let auto = ShareConfig {
            port: 25590,
            mapping: PortMapping::Auto,
            public_address: None,
        };

        assert_eq!(
            reachable_address(&auto, &mapped("203.0.113.7", Reachability::Public), None).map(|a| a.display()),
            Some("203.0.113.7:25590".into())
        );
        for (ip, unreachable) in [
            ("100.64.1.2", Reachability::Cgnat),
            ("192.168.1.2", Reachability::DoubleNat),
            ("0.0.0.0", Reachability::NoIpv4),
        ] {
            assert_eq!(reachable_address(&auto, &mapped(ip, unreachable), None), None, "{ip}");
        }
        for status in [
            portmap::Status::Off,
            portmap::Status::Searching,
            portmap::Status::Failed(portmap::Failure::NoGateway),
        ] {
            assert_eq!(reachable_address(&auto, &status, None), None, "{status:?}");
        }

        // The admin's own address wins, even over a working mapping: they know
        // about the tunnel or domain in front of this machine, the router does not.
        let tunnelled = ShareConfig {
            public_address: Some("https://pack.example.com".into()),
            ..auto.clone()
        };
        assert_eq!(
            reachable_address(&tunnelled, &mapped("203.0.113.7", Reachability::Public), None),
            Some(Address::Https { url: "https://pack.example.com".into() })
        );
        // And it is what makes a link possible at all behind CGNAT.
        let behind_cgnat = ShareConfig {
            public_address: Some("mc.example.com".into()),
            ..auto
        };
        assert_eq!(
            reachable_address(&behind_cgnat, &mapped("100.64.1.2", Reachability::Cgnat), None).map(|a| a.display()),
            Some("mc.example.com:25590".into())
        );
    }

    /// Manual setup still gets a link without the admin typing their address:
    /// the router is asked, and a public answer is used on the forwarded port.
    /// A private or shared answer is not.
    #[test]
    fn manual_setup_links_from_a_detected_public_address_only() {
        let manual_setup = ShareConfig {
            port: 38033,
            mapping: PortMapping::Manual,
            public_address: None,
        };
        let detected = |ip: &str, reachability| Some(Detected { ip: ip.parse().unwrap(), reachability });

        assert_eq!(
            reachable_address(&manual_setup, &portmap::Status::Off, detected("203.0.113.7", Reachability::Public))
                .map(|a| a.display()),
            Some("203.0.113.7:38033".into()),
            "the forwarded port, not a default"
        );
        for private in [detected("100.64.1.2", Reachability::Cgnat), detected("192.168.1.2", Reachability::DoubleNat)] {
            assert_eq!(reachable_address(&manual_setup, &portmap::Status::Off, private), None);
        }
        assert_eq!(reachable_address(&manual_setup, &portmap::Status::Off, None), None);

        // Automatic setup never uses a detection: the mapping is its source.
        let auto_setup = ShareConfig { mapping: PortMapping::Auto, ..manual_setup };
        assert_eq!(
            reachable_address(&auto_setup, &portmap::Status::Off, detected("203.0.113.7", Reachability::Public)),
            None
        );
    }

    /// A port something else holds is reported, not a crash.
    #[tokio::test]
    async fn a_port_already_in_use_is_reported() {
        let admin = paths("busy");
        let (_, share) = shared_server(&admin).await;
        let squatter = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let taken = squatter.local_addr().unwrap().port();

        let service = service();
        let config = manual(taken, None);
        service.reconcile_with(&admin, &config, None).await;
        let status = service.status(&admin, &config).await;
        assert!(!status.listening);
        assert!(
            status
                .listen_error
                .as_deref()
                .is_some_and(|e| e.contains(&taken.to_string())),
            "{status:?}"
        );

        // A public address set in Settings does not make a link to a port
        // nothing is listening on.
        let tunnelled = manual(taken, Some("mc.example.com".into()));
        service.reconcile_with(&admin, &tunnelled, None).await;
        let status = service.status(&admin, &tunnelled).await;
        assert!(!status.listening);
        assert_eq!(
            status.address, None,
            "an address to read out for a dead port"
        );
        assert!(
            service.link_for(&admin, &tunnelled, &share).await.is_none(),
            "a link handed out for a dead port"
        );
        drop(squatter);
    }

    /// Asking the router again now and then must not forget what it said
    /// before because one ask went unanswered; and before the first ask is
    /// over, nothing has been learned either way.
    #[test]
    fn an_unanswered_ask_keeps_the_address_the_router_gave_before() {
        let public = Detected {
            ip: "203.0.113.7".parse().unwrap(),
            reachability: Reachability::Public,
        };
        let moved = Detected {
            ip: "198.51.100.4".parse().unwrap(),
            ..public
        };
        let start = Detection::default();
        assert!(!start.asked);

        let unanswered = start.after(None);
        assert_eq!(
            unanswered,
            Detection {
                asked: true,
                found: None
            }
        );
        let answered = unanswered.after(Some(public));
        assert_eq!(answered.found, Some(public));
        assert_eq!(
            answered.after(None).found,
            Some(public),
            "one lost ask forgot the address"
        );
        assert_eq!(
            answered.after(Some(moved)).found,
            Some(moved),
            "a new address was not taken"
        );
    }
}

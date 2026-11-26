//! A real server, running: the console, the probe, the player lists, backups
//! and the scheduler.
//!
//! `tests/live_server.rs` proves Geode can *find* server software. Nothing
//! proved it could live with a server afterwards. Everything in this file
//! installs a real Paper server into a scratch `GEODE_HOME`, starts the real
//! JVM through the one `server::run::start` the app and the host both press,
//! and talks to it down the same stdin pipe the console pane uses.
//!
//! Every test is `#[ignore]`d: each one downloads a server, starts a JVM and
//! binds a port.
//!
//! ```sh
//! # the whole suite, serialised by its own lock, against /tmp
//! cargo test --test live_server_lifecycle -- --ignored --nocapture
//!
//! # one test
//! cargo test --test live_server_lifecycle a_world_comes_back -- --ignored --nocapture
//!
//! # another version, another kind, a pinned port, a kept scratch root
//! GEODE_HOME=/tmp/geode-lifecycle GEODE_TEST_SERVER_KIND=purpur \
//!   GEODE_TEST_SERVER_VERSION=1.21.4 GEODE_TEST_SERVER_PORT=25781 \
//!   cargo test --test live_server_lifecycle -- --ignored --nocapture
//! ```
//!
//! The knobs, all optional:
//!
//! | variable | default | what it moves |
//! |---|---|---|
//! | `GEODE_HOME` | `/tmp/geode-server-lifecycle` | the scratch root. Kept between runs on purpose: the jar, the world and the command tree are expensive and re-usable. Delete it to start clean. |
//! | `GEODE_TEST_SERVER_KIND` | `paper` | any `ServerKind` name. Must be one that installs from a single jar — Forge, NeoForge and Spigot need an installer or a build and are out of scope here. |
//! | `GEODE_TEST_SERVER_VERSION` | `1.21.1` | the Minecraft version. |
//! | `GEODE_TEST_SERVER_PORT` | first free in 25781–25880 | the game port. Away from 25565–25575 by default, which a developer's own servers tend to hold. |
//! | `GEODE_TEST_SERVER_HEAP_MB` | `1536` | `-Xmx` for the test server. |
//! | `GEODE_TEST_BOOT_SECONDS` | `360` | how long a start gets to reach `Done (`. Generous because a Paper server's first boot downloads and patches Mojang's own jar before it generates anything. |
//! | `GEODE_TEST_STOP_SECONDS` | `180` | how long a stop gets before the process is killed. |
//! | `GEODE_TEST_PLUGIN` | `chunky` | a Modrinth plugin slug that registers a command, used to prove the probe reads commands vanilla has never heard of. |
//! | `GEODE_TEST_PLUGIN_COMMAND` | `chunky` | the root command that plugin registers. |
//!
//! What a green run does **not** prove:
//!
//! - **A player.** Nothing connects, so `usercache.json` is never written by a
//!   real login, and the branch of [`server::players`] that reads it is
//!   untouched here — there is no way to make an authenticated client join
//!   from a test. Nor is a kick, for the same reason: `/kick` needs somebody
//!   to kick, and a kick of nobody is an error reply rather than the path.
//! - **Mojang being unreachable.** The "reported rather than guessed" branch
//!   is exercised through a name Mojang answers `404` for. A lookup that times
//!   out would need the endpoint, which is a `const`, pointed somewhere else.
//! - **`players::say`'s own silence refusal.** Every *command* a
//!   vanilla-derived server has answers the console —
//!   `MinecraftServer.acceptsSuccess` and `acceptsFailure` are both
//!   unconditionally `true`, so not even `sendCommandFeedback=false` quiets
//!   one — so no real verb can be made silent here. What is live-tested is the
//!   `ask` contract underneath it, which is where the distinction is actually
//!   made; the refusal built on top of it stays covered by
//!   `players.rs`'s own fake-console test.
//! - **The restore refusal.** `backup::restore` has no guard of its own; the
//!   "stop the server first" refusal is written twice, once in
//!   `commands::restore_server_backup` and once in `daemon::api::restore`, and
//!   neither a Tauri `State` nor a `Host` can be built here. See the note on
//!   [`a_thinned_backup_restores_to_a_server_that_still_boots`].
//! - **Anything but Linux.** One platform, one JVM, one filesystem.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use geode_lib::events::{EventSink, Events};
use geode_lib::net::{self, Download, Progress};
use geode_lib::paths::Paths;
use geode_lib::providers::{self, Provider};
use geode_lib::server::players::{Manager, PlayerList, Via};
use geode_lib::server::properties::{self, Property};
use geode_lib::server::run::ServerRegistry;
use geode_lib::server::schedule::{Schedule, ScheduleAction};
use geode_lib::server::{self, backup, probe, run, schedule, Server, ServerKind};

/// The one server every test here shares.
const ID: &str = "lifecycle";
const NAME: &str = "Lifecycle";

/// Mojang's one fixed point: account number one.
const NOTCH_UUID: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

// ---------------------------------------------------------------------------
// environment
// ---------------------------------------------------------------------------

fn env_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_or(key: &str, default: &str) -> String {
    env_var(key).unwrap_or_else(|| default.to_string())
}

fn env_secs(key: &str, default: u64) -> Duration {
    Duration::from_secs(
        env_var(key)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(default),
    )
}

fn root() -> PathBuf {
    env_var("GEODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("geode-server-lifecycle"))
}

fn kind() -> ServerKind {
    let name = env_or("GEODE_TEST_SERVER_KIND", "paper");
    ServerKind::parse(&name).unwrap_or_else(|| panic!("{name} is not a server kind"))
}

fn mc_version() -> String {
    env_or("GEODE_TEST_SERVER_VERSION", "1.21.1")
}

/// The game port, decided once per process so every test agrees on it.
fn port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| {
        if let Some(asked) = env_var("GEODE_TEST_SERVER_PORT").and_then(|v| v.parse().ok()) {
            return asked;
        }
        // Deliberately away from 25565–25575: a developer running this very
        // likely has their own servers on those.
        (25781..25881)
            .find(|candidate| std::net::TcpListener::bind(("127.0.0.1", *candidate)).is_ok())
            .expect("no free port in 25781-25880")
    })
}

// ---------------------------------------------------------------------------
// the console, as the app hears it
// ---------------------------------------------------------------------------

/// Everything the server said, collected through the same `EventSink` the
/// console pane listens on.
///
/// Not [`geode_lib::events::Recorder`], because the waits below need to search
/// the lines as they arrive and to be cleared between starts.
#[derive(Default)]
struct Log {
    lines: Mutex<Vec<String>>,
    topics: Mutex<Vec<String>>,
}

/// A poisoned lock here means an assertion failed while holding it, and the
/// collected text is still exactly what it was — there is nothing to protect.
fn guard<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|e| e.into_inner())
}

impl Log {
    fn clear(&self) {
        guard(&self.lines).clear();
        guard(&self.topics).clear();
    }

    fn lines(&self) -> Vec<String> {
        guard(&self.lines).clone()
    }

    fn topics(&self) -> Vec<String> {
        guard(&self.topics).clone()
    }

    fn has(&self, needle: &str) -> bool {
        guard(&self.lines).iter().any(|l| l.contains(needle))
    }

    /// The last few lines, for a failure message worth reading.
    fn tail(&self, n: usize) -> String {
        let lines = self.lines();
        lines
            .iter()
            .rev()
            .take(n)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Events for Log {
    fn emit(&self, topic: &str, payload: serde_json::Value) {
        guard(&self.topics).push(topic.to_string());
        if topic == "geode://server-log" {
            if let Some(line) = payload.get("line").and_then(|l| l.as_str()) {
                guard(&self.lines).push(line.to_string());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// one server at a time
// ---------------------------------------------------------------------------

/// Taken for the whole of each test.
///
/// These tests share one install — a 50 MB jar and a generated world are not
/// worth ten copies of — and every one of them starts, stops or rewrites it.
/// Cargo runs tests on threads of the same process, so a flag in that process
/// is all the exclusion needed, and unlike a `std::sync::Mutex` it cannot be
/// poisoned by a failing assertion and unlike a `tokio::sync::Mutex` it is not
/// waited on in one test's runtime and released in another's.
static BUSY: AtomicBool = AtomicBool::new(false);

struct Busy;

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

async fn exclusive() -> Busy {
    let deadline = Instant::now() + Duration::from_secs(3600);
    while BUSY.swap(true, Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "another test has held the server for an hour"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Busy
}

/// The installed server, and everything needed to drive it.
struct Live {
    paths: Paths,
    http: reqwest::Client,
    registry: ServerRegistry,
    log: std::sync::Arc<Log>,
    sink: EventSink,
    /// The pid of the process this test started, so the guard below can be
    /// certain of what it kills.
    pid: Mutex<Option<u32>>,
    _busy: Busy,
}

/// A test that fails part-way through must not leave a JVM behind, and a
/// `Drop` cannot await — so this is the one place that reaches past the
/// registry to the operating system.
///
/// Only when the registry still holds the server: its exit task deregisters
/// the moment the process ends, so a registered server is one that is alive
/// and ours, and a pid cannot have been handed to anything else yet.
impl Drop for Live {
    fn drop(&mut self) {
        if !self.registry.is_running(ID) {
            return;
        }
        let pid = *guard(&self.pid);
        eprintln!("== the test left the server running; killing pid {pid:?}");
        self.registry.kill(ID);
        #[cfg(unix)]
        {
            if let Some(pid) = pid {
                // SAFETY: an ordinary signal to a pid this process spawned and
                // that the registry still vouches for.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
    }
}

impl Live {
    fn run_dir(&self) -> PathBuf {
        self.paths.server_run(ID)
    }

    fn manager(&self) -> Manager<'_> {
        Manager {
            paths: &self.paths,
            servers: &self.registry,
            http: &self.http,
        }
    }

    async fn server(&self) -> Server {
        server::load(&self.paths, ID).await.expect("server.json")
    }

    /// Start the server and wait until it says it has finished starting.
    async fn start(&self) {
        self.start_under(None).await
    }

    /// [`Live::start`] with a host's heap ceiling — the argument the app always
    /// passes as `None` and `geoded` fills in from `GEODE_MAX_HEAP_MB`.
    async fn start_under(&self, ceiling: Option<u32>) {
        assert!(
            !self.registry.is_running(ID),
            "the server was already running"
        );
        self.log.clear();
        let started = Instant::now();
        let outcome = run::start(
            &self.sink,
            &self.paths,
            &self.http,
            &self.registry,
            ID,
            ceiling,
        )
        .await
        .unwrap_or_else(|e| panic!("start: {e}"));
        *guard(&self.pid) = Some(outcome.pid);
        println!("== started pid {} on port {}", outcome.pid, port());

        let deadline = Instant::now() + env_secs("GEODE_TEST_BOOT_SECONDS", 240);
        while Instant::now() < deadline {
            if self.log.has("Done (") {
                println!("== ready in {:?}", started.elapsed());
                return;
            }
            if !self.registry.is_running(ID) {
                panic!(
                    "the server exited while starting:\n{}",
                    self.log.tail(30)
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        self.stop().await;
        panic!("the server never finished starting:\n{}", self.log.tail(30));
    }

    /// Ask it to stop the way the Stop button does, and insist.
    async fn stop(&self) {
        if !self.registry.is_running(ID) {
            return;
        }
        let started = Instant::now();
        self.registry.stop(ID);
        let deadline = Instant::now() + env_secs("GEODE_TEST_STOP_SECONDS", 180);
        while Instant::now() < deadline {
            if !self.registry.is_running(ID) {
                println!("== stopped in {:?}", started.elapsed());
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        eprintln!("== the server would not stop; killing it");
        self.registry.kill(ID);
        for _ in 0..60 {
            if !self.registry.is_running(ID) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("the server is still running after a kill");
    }

    /// Wait for a line the **server** wrote, and hand it back.
    ///
    /// Geode's own `[Geode] ...` announcements go down the same event topic and
    /// quote what they are about, so a plain substring search over the console
    /// finds Geode talking about a command rather than the server answering it.
    async fn wait_for_echo(&self, needle: &str, within: Duration) -> Vec<String> {
        let deadline = Instant::now() + within;
        loop {
            let echoed: Vec<String> = self
                .log
                .lines()
                .into_iter()
                .filter(|l| l.contains(needle) && !l.contains("[Geode]"))
                .collect();
            if !echoed.is_empty() {
                return echoed;
            }
            if Instant::now() >= deadline {
                panic!(
                    "the server itself never mentioned “{needle}” within {within:?}:\n{}",
                    self.log.tail(30)
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Everything one of the four player files holds now, read off disk rather
    /// than through the manager, so a test can see what the *server* wrote.
    async fn file_rows(&self, name: &str) -> Vec<serde_json::Value> {
        let path = self.run_dir().join(name);
        let text = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        if text.trim().is_empty() {
            return Vec::new();
        }
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}\n{text}"))
    }

    async fn write_rows(&self, name: &str, rows: &[serde_json::Value]) {
        tokio::fs::write(
            self.run_dir().join(name),
            serde_json::to_vec_pretty(rows).expect("rows"),
        )
        .await
        .expect("write rows");
    }

    async fn set_property(&self, key: &str, value: &str) {
        properties::write(
            &self.paths,
            ID,
            &[Property {
                key: key.to_string(),
                value: value.to_string(),
            }],
        )
        .await
        .unwrap_or_else(|e| panic!("writing {key}: {e}"));
    }

    async fn backup(&self) -> backup::BackupInfo {
        backup::create(
            &self.paths,
            &self.registry,
            ID,
            &Progress::silent("lifecycle"),
        )
        .await
        .unwrap_or_else(|e| panic!("backup: {e}"))
    }

    fn backup_path(&self, name: &str) -> PathBuf {
        self.paths.server_backups(ID).join(name)
    }
}

// ---------------------------------------------------------------------------
// the fixture
// ---------------------------------------------------------------------------

/// The installed server, stopped, with its settings, properties and the four
/// player files back at a known state.
///
/// Every test calls this, so no test depends on the order they ran in. What it
/// does *not* reset is the jar, the world and the command tree, which are what
/// make a second run of the suite minutes rather than an hour.
async fn live() -> Live {
    let busy = exclusive().await;
    let paths = Paths { root: root() };
    paths.ensure_all().await.expect("scratch dirs");
    let log = std::sync::Arc::new(Log::default());
    let sink: EventSink = log.clone();

    let live = Live {
        paths,
        http: net::client(),
        registry: ServerRegistry::default(),
        log,
        sink,
        pid: Mutex::new(None),
        _busy: busy,
    };

    install(&live).await;
    reset(&live).await;
    ensure_world_and_tree(&live).await;
    live.log.clear();
    live
}

/// Install the server software, once per scratch root.
async fn install(live: &Live) {
    let kind = kind();
    let version = mc_version();

    let existing = server::load(&live.paths, ID).await.ok();
    if let Some(server) = &existing {
        let there = !server.jar.is_empty()
            && tokio::fs::metadata(live.run_dir().join(&server.jar))
                .await
                .is_ok();
        if there && server.kind == kind && server.mc_version == version {
            return;
        }
    }

    let mut server = match existing {
        Some(mut server) => {
            // Re-pointed at another kind or version by the environment.
            server.kind = kind;
            server.mc_version = version.clone();
            server
        }
        None => {
            let made = server::create(&live.paths, NAME.into(), kind, version.clone())
                .await
                .expect("create the server");
            assert_eq!(
                made.id, ID,
                "the scratch root already holds a server called {NAME}"
            );
            made
        }
    };

    println!("== installing {} {version}", kind.display_name());
    let started = Instant::now();
    server::install::install_jar(
        &live.http,
        &live.paths,
        &mut server,
        &Progress::silent("lifecycle-install"),
    )
    .await
    .unwrap_or_else(|e| panic!("installing {} {version}: {e}", kind.display_name()));
    println!(
        "== installed {} (build {}) in {:?}",
        server.jar,
        server.build,
        started.elapsed()
    );

    server::accept_eula(&live.paths, &mut server)
        .await
        .expect("accept the EULA");
    install_plugin(live, &server).await;
}

/// A plugin that registers a command of its own, so the probe has something to
/// find that no vanilla server has.
async fn install_plugin(live: &Live, server: &Server) {
    let slug = env_or("GEODE_TEST_PLUGIN", "chunky");
    let dir = live.run_dir().join("plugins");
    tokio::fs::create_dir_all(&dir).await.expect("plugins dir");

    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_name().to_string_lossy().ends_with(".jar") {
                return;
            }
        }
    }

    let loader = server
        .kind
        .plugin_loaders()
        .first()
        .copied()
        .unwrap_or("paper");
    let versions = providers::versions(
        &live.http,
        Provider::Modrinth,
        &slug,
        Some(&server.mc_version),
        Some(loader),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("asking Modrinth for {slug}: {e}"));
    let newest = versions.first().unwrap_or_else(|| {
        panic!(
            "{slug} publishes no {loader} build for {} — point GEODE_TEST_PLUGIN at one that does",
            server.mc_version
        )
    });
    let file = newest
        .primary_file()
        .unwrap_or_else(|| panic!("{slug} {} carries no file", newest.version_number));

    net::fetch_one(
        &live.http,
        &Download::new(file.url.clone(), dir.join(&file.filename))
            .sha1(file.sha1.clone())
            .size(file.size),
    )
    .await
    .unwrap_or_else(|e| panic!("downloading {}: {e}", file.filename));
    println!("== installed the plugin {}", file.filename);
}

/// Put the settings, the properties and the four player files back.
async fn reset(live: &Live) {
    live.stop().await;

    let mut server = live.server().await;
    server.settings.memory_mb = env_or("GEODE_TEST_SERVER_HEAP_MB", "1536")
        .parse()
        .unwrap_or(1536);
    server.settings.auto_restart = false;
    server.settings.read_commands = true;
    server.settings.keep_logs = Some(5);
    // None, so nothing prunes a backup a test has just taken and is about to
    // read.
    server.settings.keep_backups = None;
    server.settings.java_path = None;
    server.schedules = Vec::new();
    server.eula_accepted = true;
    server.save(&live.paths).await.expect("save settings");

    // A flat world on loopback, so a boot is quick and nothing outside this
    // machine can reach it.
    for (key, value) in [
        ("server-port", port().to_string()),
        ("server-ip", "127.0.0.1".into()),
        ("online-mode", "true".into()),
        ("white-list", "false".into()),
        ("level-name", "world".into()),
        ("level-type", "minecraft\\:flat".into()),
        ("view-distance", "4".into()),
        ("simulation-distance", "4".into()),
        ("max-players", "20".into()),
        ("spawn-protection", "0".into()),
        ("motd", "Geode live lifecycle test".into()),
    ] {
        live.set_property(key, &value).await;
    }

    for file in [
        "whitelist.json",
        "ops.json",
        "banned-players.json",
        "banned-ips.json",
    ] {
        live.write_rows(file, &[]).await;
    }
}

/// A world on disk and a command tree, which the rest of the suite reads.
///
/// Both are written by the server itself, so the only way to have them is to
/// have run it. Self-healing rather than part of the install: the test that
/// proves a server with no tree is offered nothing deletes the tree, and the
/// next test to need one puts it back.
async fn ensure_world_and_tree(live: &Live) {
    let world = live.run_dir().join("world").join("level.dat");
    let tree = probe::read(&live.paths, ID).await;
    if tokio::fs::metadata(&world).await.is_ok() && !tree.is_empty() {
        return;
    }

    println!("== booting once to generate the world and the command tree");
    live.start().await;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if !probe::read(&live.paths, ID).await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    live.stop().await;

    assert!(
        tokio::fs::metadata(&world).await.is_ok(),
        "the server did not write {}",
        world.display()
    );
    assert!(
        !probe::read(&live.paths, ID).await.is_empty(),
        "the probe wrote no command tree — the agent did not attach"
    );
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

/// Every entry in a zip, by name.
fn zip_names(archive: &Path) -> BTreeSet<String> {
    let file = std::fs::File::open(archive)
        .unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
    let mut zip = zip::ZipArchive::new(file).expect("a zip");
    let mut names = BTreeSet::new();
    for i in 0..zip.len() {
        names.insert(zip.by_index(i).expect("entry").name().to_string());
    }
    names
}

async fn sha256_of(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = tokio::fs::read(path)
        .await
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    hex::encode(Sha256::digest(&bytes))
}

/// The names the four files record, as the server spells them.
fn names_in(rows: &[serde_json::Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| r.get("name").and_then(|n| n.as_str()))
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// the console
// ---------------------------------------------------------------------------

/// The mechanism behind the rule CLAUDE.md calls load-bearing: silence and
/// "nothing is running" are two different answers, from one real server.
///
/// `ask` has three outcomes and a screen acts differently on each. `None` is
/// no process. `Some(vec![])` is a command that went down the pipe and was
/// answered with nothing — which `players::say` turns into "Geode cannot tell
/// whether it worked", because a screen that shows a ban as done when the
/// server never acknowledged it has lied to somebody about who can get in.
/// `Some(lines)` is the server's own words, reported as its own words.
///
/// The silence is real rather than arranged. Paper's console reader is
/// `net.minecrell.terminalconsole.SimpleTerminalConsole`, whose `processInput`
/// trims the line and runs nothing when what is left is empty — so a blank
/// line is read, discarded, and answered with nothing at all. Every *command*
/// a vanilla-derived server has answers the console (`MinecraftServer`'s
/// `acceptsSuccess` and `acceptsFailure` are both unconditionally true, so
/// even the `sendCommandFeedback` game rule cannot quiet it), which is why
/// `players::say`'s own refusal can only be reached from a fake console —
/// `players.rs`'s `a_console_command_the_server_never_answers_is_not_reported_as_done`
/// does that, and this pins the live half underneath it.
///
/// Guards `run.rs`'s `ask`: the `map(...)?` that yields `None` when the id is
/// not in the registry, and the `Some(said)` it returns after the reply window
/// whether anything arrived or not.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs and runs a real Minecraft server"]
async fn the_console_tells_a_reply_from_silence_and_silence_from_nothing_running() {
    let live = live().await;

    // Not running at all.
    assert!(
        live.registry.ask(ID, "list").await.is_none(),
        "a stopped server should answer None, not silence"
    );

    live.start().await;

    // A reply: down the stdin pipe, through the server, back out of its stdout
    // and into the broadcast the console pane reads.
    let said = live
        .registry
        .ask(ID, "list")
        .await
        .expect("the server is running");
    assert!(
        !said.is_empty(),
        "`list` on a live server said nothing:\n{}",
        live.log.tail(20)
    );
    println!("== /list said: {said:?}");

    // And the reply really is in the shape `read_online` claims, off a real
    // server rather than a fixture: nobody is on, and that is a list of nobody
    // rather than an unreadable answer.
    let online = geode_lib::server::players::read_online(&said)
        .unwrap_or_else(|| panic!("a real /list reply was unreadable: {said:?}"));
    assert!(online.is_empty(), "nobody is connected, yet {online:?}");

    // Silence: a line the console reader throws away. The same `Some`, with
    // nothing in it — which is the distinction the whole rule rests on.
    let nothing = live
        .registry
        .ask(ID, "")
        .await
        .expect("the server is still running, so this is not None");
    assert!(
        nothing.is_empty(),
        "a blank console line should be answered with nothing, and this came back: {nothing:?}"
    );
    println!("== a blank line was answered with silence, not with a refusal");

    // The positive half of the rule at the layer above: what the server said is
    // what is reported, as it said it, and never classified into success.
    let change = live
        .manager()
        .add(ID, PlayerList::Whitelist, "Dinnerbone", None, None)
        .await
        .expect("a speaking console");
    assert_eq!(change.via, Via::Console);
    assert!(
        !change.said.is_empty(),
        "a console change must be reported as what the server said"
    );
    println!("== the server said: {:?}", change.said);

    let off = live
        .manager()
        .remove(ID, PlayerList::Whitelist, "Dinnerbone", None)
        .await
        .expect("a speaking console");
    assert_eq!(off.via, Via::Console);
    assert!(!off.said.is_empty());

    live.stop().await;
    // The console was the app's own event stream throughout, not a pipe this
    // test read for itself.
    assert!(
        live.log.topics().iter().any(|t| t == "geode://server-log"),
        "nothing arrived as a console event"
    );
}

// ---------------------------------------------------------------------------
// the probe
// ---------------------------------------------------------------------------

/// What may be asked of a console is read off the tree, never off the kind.
///
/// A running Paper server is right there and would take `/whitelist add`
/// happily. With no tree, Geode offers nothing anyway — and says why, rather
/// than assuming a server it has not read is vanilla.
///
/// Guards `players.rs`'s `through_console`, where `if !allowed` refuses, the
/// `who_is_online` gate in `online`, and `probe::read`'s empty answer for a
/// tree that is not there.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs and runs a real Minecraft server"]
async fn a_server_with_no_command_tree_is_offered_nothing_rather_than_assumed_vanilla() {
    let live = live().await;

    // The one way a real server has no tree: it has never run with the probe.
    let tree_file = probe::tree_file(&live.paths, ID);
    tokio::fs::remove_file(&tree_file).await.expect("the tree");
    let mut server = live.server().await;
    server.settings.read_commands = false;
    server.save(&live.paths).await.expect("save");

    live.start().await;

    assert!(
        probe::read(&live.paths, ID).await.is_empty(),
        "a server started without the probe should have no tree"
    );
    assert!(
        tokio::fs::metadata(&tree_file).await.is_err(),
        "something wrote {} with the probe turned off",
        tree_file.display()
    );

    let lists = live.manager().lists(ID).await.expect("lists");
    assert!(lists.running, "the server is running");
    assert!(
        !lists.console.known,
        "with no tree, nothing about the console is known"
    );
    for (what, offered) in [
        ("whitelist add", lists.console.whitelist_add),
        ("op", lists.console.op),
        ("ban", lists.console.ban),
        ("ban-ip", lists.console.ban_ip),
        ("kick", lists.console.kick),
        ("list", lists.console.who_is_online),
    ] {
        assert!(!offered, "{what} was offered off no tree at all");
    }

    let before = live.file_rows("whitelist.json").await.len();
    let refused = live
        .manager()
        .add(ID, PlayerList::Whitelist, "Notch", None, None)
        .await
        .expect_err("nothing should be guessed at");
    let text = refused.to_string();
    assert!(
        text.contains("has not seen a `whitelist` command") && text.contains("Read commands"),
        "the refusal should say what it has not seen and what to do: {text}"
    );
    println!("== {text}");

    let online = live
        .manager()
        .online(ID)
        .await
        .expect_err("`list` was never seen either");
    assert!(
        online.to_string().contains("has not seen a `list` command"),
        "{online}"
    );

    // And nothing was written behind the running server's back on the way to
    // that refusal.
    assert_eq!(
        live.file_rows("whitelist.json").await.len(),
        before,
        "a refusal wrote to whitelist.json anyway"
    );

    live.stop().await;
}

/// The tree is this server's own, including a command no vanilla server has.
///
/// Three separate claims, and each one fails differently:
///
/// * the agent attaches to a real Paper JVM and writes a tree at all;
/// * that tree holds commands that exist only because *this* server is Paper
///   and has a plugin installed — so it cannot have come from a list of what
///   a server kind is assumed to have;
/// * `probe::read` drops the namespaced twin Bukkit registers beside every
///   plugin command, which until now was only ever checked against a
///   hand-written fixture.
///
/// Guards `probe.rs`: `agent_arg`, `children_of`, and
/// `roots.retain(|node| !node.name.contains(':'))`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs and runs a real Minecraft server"]
async fn the_probe_reads_this_servers_own_tree_including_a_plugins_own_command() {
    let live = live().await;
    let plugin_command = env_or("GEODE_TEST_PLUGIN_COMMAND", "chunky");

    // The tree the fixture guarantees was written by a real run; re-read it
    // here rather than trusting a cached one.
    let tree = probe::read(&live.paths, ID).await;
    assert!(
        tree.len() > 50,
        "a vanilla-derived server has ninety-odd commands, not {}",
        tree.len()
    );

    let roots: BTreeSet<&str> = tree.iter().map(|n| n.name.as_str()).collect();
    println!("== {} root commands", roots.len());

    // Bukkit's own, which Mojang's server has never had: if this list were
    // derived from "it is a Minecraft server", none of these would be here.
    for bukkit_only in ["plugins", "version", "reload"] {
        assert!(
            roots.contains(bukkit_only),
            "/{bukkit_only} is a Bukkit command and this is a Bukkit server, yet the tree has only {:?}",
            roots.iter().take(20).collect::<Vec<_>>()
        );
    }

    // And the one that cannot be known from the software at all: a plugin put
    // it there.
    assert!(
        roots.contains(plugin_command.as_str()),
        "/{plugin_command} comes from the installed plugin and nothing else, yet it is absent"
    );
    println!("== the plugin's own /{plugin_command} is in the tree");

    // Vanilla's, so the tree is a whole tree and not only the odd additions.
    for vanilla in ["whitelist", "ban", "pardon", "op", "deop", "kick", "list"] {
        assert!(roots.contains(vanilla), "/{vanilla} is missing");
    }

    // The shape, not just the names — which is what makes this better than
    // scraping `help`.
    let whitelist = tree
        .iter()
        .find(|n| n.name == "whitelist")
        .expect("/whitelist");
    let children: BTreeSet<&str> = whitelist
        .children
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    for word in ["add", "remove", "on", "off"] {
        assert!(
            children.contains(word),
            "/whitelist {word} is missing from {children:?}"
        );
    }

    // What the screen is allowed to offer, read off that tree.
    let console = live
        .manager()
        .lists(ID)
        .await
        .expect("lists")
        .console;
    assert!(console.known);
    for (what, offered) in [
        ("whitelist add", console.whitelist_add),
        ("whitelist remove", console.whitelist_remove),
        ("whitelist on/off", console.whitelist_toggle),
        ("op", console.op),
        ("deop", console.deop),
        ("ban", console.ban),
        ("pardon", console.pardon),
        ("ban-ip", console.ban_ip),
        ("pardon-ip", console.pardon_ip),
        ("kick", console.kick),
        ("list", console.who_is_online),
    ] {
        assert!(offered, "{what} is in the tree and was not offered");
    }

    // The namespaced twin, against real output. Bukkit registers every command
    // a second time under a namespace, and the raw file proves they are there
    // — which is the only way to know the filter did anything.
    let raw: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(probe::tree_file(&live.paths, ID))
            .await
            .expect("the tree file"),
    )
    .expect("the tree is json");
    let raw_roots: Vec<&String> = raw
        .get("children")
        .and_then(|c| c.as_object())
        .expect("a root with children")
        .keys()
        .collect();
    let namespaced: Vec<&&String> = raw_roots.iter().filter(|k| k.contains(':')).collect();
    assert!(
        !namespaced.is_empty(),
        "this server registered no namespaced commands, so nothing here is being filtered"
    );
    println!(
        "== {} of {} raw roots are namespaced, e.g. {:?}",
        namespaced.len(),
        raw_roots.len(),
        namespaced.iter().take(4).collect::<Vec<_>>()
    );
    assert!(
        tree.iter().all(|n| !n.name.contains(':')),
        "a namespaced twin reached the console's list"
    );
}

// ---------------------------------------------------------------------------
// the player lists
// ---------------------------------------------------------------------------

/// Both routes, and the reason there are two.
///
/// Minecraft's own player files are the server's while it runs: it holds them
/// in memory and writes them out whenever it changes one, so an edit made
/// behind its back is lost at its next save. This proves that is true of a
/// real server — a row written straight into `whitelist.json` under a running
/// Paper server is gone the moment the server next writes the file — and that
/// `players.rs` takes the console route instead, and says which route it took.
///
/// Guards `players.rs`'s `add`/`remove`: the `if self.servers.is_running(id)`
/// branch at the top of each, `Via::Console` against `Via::File`, and the
/// level refusal while running.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs and runs a real Minecraft server"]
async fn a_player_list_goes_through_the_console_while_it_runs_and_the_file_while_stopped() {
    let live = live().await;

    // --- stopped: the files are ours ---

    let change = live
        .manager()
        .add(ID, PlayerList::Whitelist, "Notch", None, None)
        .await
        .expect("a stopped server's whitelist is ours to write");
    assert_eq!(change.via, Via::File);
    assert!(
        change.said.is_empty(),
        "nothing was asked of a console that is not there"
    );

    // Keyed by the UUID Mojang issued, because an online-mode server keys
    // players by that and nothing else can say what it is.
    let rows = live.file_rows("whitelist.json").await;
    let notch = rows
        .iter()
        .find(|r| r.get("name").and_then(|n| n.as_str()) == Some("Notch"))
        .expect("Notch is on the whitelist");
    assert_eq!(
        notch.get("uuid").and_then(|u| u.as_str()),
        Some(NOTCH_UUID),
        "the row is not keyed by the UUID Mojang issued: {notch}"
    );

    // The one thing only the file route can do: `/op` takes no level.
    let ops = live
        .manager()
        .add(ID, PlayerList::Ops, "Notch", None, Some(3))
        .await
        .expect("an operator's level, while it is stopped");
    assert_eq!(ops.via, Via::File);
    assert_eq!(
        live.file_rows("ops.json").await[0]
            .get("level")
            .and_then(|l| l.as_u64()),
        Some(3)
    );

    let bad = live
        .manager()
        .add(ID, PlayerList::Ops, "Dinnerbone", None, Some(5))
        .await
        .expect_err("level 5 does not exist");
    assert!(bad.to_string().contains("1 to 4"), "{bad}");

    // --- running: the files are the server's ---

    live.start().await;

    let refused = live
        .manager()
        .add(ID, PlayerList::Ops, "Dinnerbone", None, Some(2))
        .await
        .expect_err("a level cannot be set through a console");
    assert!(
        refused
            .to_string()
            .contains("only be set while the server is stopped"),
        "{refused}"
    );

    // A row written behind the running server's back — the exact mistake the
    // whole module exists to avoid.
    let mut behind_its_back = live.file_rows("whitelist.json").await;
    behind_its_back.push(serde_json::json!({
        "uuid": geode_lib::auth::offline_uuid("GeodeGhost"),
        "name": "GeodeGhost",
    }));
    live.write_rows("whitelist.json", &behind_its_back).await;
    assert!(names_in(&live.file_rows("whitelist.json").await)
        .iter()
        .any(|n| n == "GeodeGhost"));

    // Geode's own change, which goes the other way: down the console, and the
    // server writes its own file.
    let through = live
        .manager()
        .add(ID, PlayerList::Whitelist, "Dinnerbone", None, None)
        .await
        .expect("the console route");
    assert_eq!(through.via, Via::Console);
    assert!(
        !through.said.is_empty(),
        "the server's own answer is what is reported"
    );
    println!("== the server said: {:?}", through.said);

    let after = names_in(&live.file_rows("whitelist.json").await);
    assert!(
        after.iter().any(|n| n == "Dinnerbone"),
        "the server did not write its own file: {after:?}"
    );
    assert!(
        after.iter().any(|n| n == "Notch"),
        "the row that was there before the server started is gone: {after:?}"
    );
    assert!(
        !after.iter().any(|n| n == "GeodeGhost"),
        "an edit made behind a running server's back survived, which it must not: {after:?}"
    );
    println!("== the behind-its-back row was overwritten, exactly as documented");

    // And off again the same way.
    let off = live
        .manager()
        .remove(ID, PlayerList::Whitelist, "Notch", Some(NOTCH_UUID))
        .await
        .expect("the console route");
    assert_eq!(off.via, Via::Console);
    assert!(!off.said.is_empty());
    assert!(!names_in(&live.file_rows("whitelist.json").await)
        .iter()
        .any(|n| n == "Notch"));

    live.stop().await;

    // And the operator level written while it was stopped survived the whole
    // run, which is what makes the file route worth having: `/op` cannot set
    // one, so if a run could lose it there would be no way to set it at all.
    let ops_after = live.file_rows("ops.json").await;
    let notch_op = ops_after
        .iter()
        .find(|r| r.get("name").and_then(|n| n.as_str()) == Some("Notch"))
        .unwrap_or_else(|| panic!("the server dropped the operator: {ops_after:?}"));
    assert_eq!(
        notch_op.get("level").and_then(|l| l.as_u64()),
        Some(3),
        "the level set while it was stopped did not survive the run: {notch_op}"
    );
}

/// A name that cannot be resolved is reported, never guessed.
///
/// These files are keyed by UUID, and which UUID is correct is a property of
/// the server. Online mode: only Mojang can say, and a name nobody owns has to
/// be refused — writing a derived UUID for it would whitelist somebody who
/// does not exist and never whitelist the person meant. Authentication off:
/// the UUID vanilla itself derives is not a guess, and no network is involved.
///
/// Guards `players.rs`'s `resolve` — the `if !online` branch reaching
/// `auth::offline_uuid`, and `from_mojang`'s 404 arm — and the fact that
/// neither one writes a row it could not key.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs a real Minecraft server and asks Mojang"]
async fn a_name_that_cannot_be_resolved_is_reported_rather_than_guessed() {
    let live = live().await;
    // 16 characters, so it is a name Minecraft would allow and the refusal is
    // about the account rather than about the spelling.
    let nobody = "GeodeNoSuchName9";

    // The server is stopped throughout: this is the file route, which is the
    // only one that has to resolve a name for itself.
    assert!(live.manager().lists(ID).await.expect("lists").online_mode);

    let refused = live
        .manager()
        .add(ID, PlayerList::Whitelist, nobody, None, None)
        .await
        .expect_err("Mojang has no such account");
    let text = refused.to_string();
    assert!(
        text.contains(nobody) && text.contains("no Minecraft account"),
        "the refusal should name who could not be found: {text}"
    );
    assert!(
        text.contains("keyed by"),
        "and say why that is fatal here: {text}"
    );
    assert!(
        live.file_rows("whitelist.json").await.is_empty(),
        "a name that could not be resolved was written anyway"
    );
    println!("== {text}");

    // With authentication off the answer is derived rather than looked up, and
    // the derivation is vanilla's own — so this is the UUID that server would
    // write for itself.
    let _ = tokio::fs::remove_file(live.run_dir().join("usercache.json")).await;
    live.set_property("online-mode", "false").await;
    assert!(!live.manager().lists(ID).await.expect("lists").online_mode);

    let derived = live
        .manager()
        .add(ID, PlayerList::Whitelist, nobody, None, None)
        .await
        .expect("an offline-mode server derives its own UUIDs");
    assert_eq!(derived.via, Via::File);
    let rows = live.file_rows("whitelist.json").await;
    assert_eq!(
        rows[0].get("uuid").and_then(|u| u.as_str()),
        Some(geode_lib::auth::offline_uuid(nobody).as_str()),
        "the row should carry the UUID vanilla derives: {:?}",
        rows[0]
    );
    println!(
        "== offline mode keyed {nobody} by {}",
        geode_lib::auth::offline_uuid(nobody)
    );

    // And the same name is still refused the moment authentication is back on,
    // so the derived UUID is a property of the server rather than a fallback.
    live.set_property("online-mode", "true").await;
    live.write_rows("whitelist.json", &[]).await;
    let again = live
        .manager()
        .add(ID, PlayerList::Whitelist, nobody, None, None)
        .await
        .expect_err("still nobody");
    assert!(again.to_string().contains("no Minecraft account"), "{again}");
}

// ---------------------------------------------------------------------------
// backups
// ---------------------------------------------------------------------------

/// A backup holds only what exists nowhere else — and nothing beside the
/// server's own data.
///
/// Two halves. What is left out: the jar, `libraries/` and `versions/` are
/// 156 MB of a 165 MB Paper server and all of it is something Geode can ask
/// for again. What cannot possibly be swept in: `backups/`, `media/` and
/// `published/` sit *beside* `run/`, so a backup is never a backup of a
/// previous backup or of the server's decoration.
///
/// Guards `backup.rs`'s `fetchable_again` and `excluded`, and `paths.rs`'s
/// `server_backups`/`server_media`/`server_published` sitting outside
/// `server_run`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs a real Minecraft server"]
async fn a_backup_is_the_servers_own_data_and_nothing_beside_it() {
    let live = live().await;
    let server = live.server().await;
    let run = live.run_dir();

    // The layout claim, which is what makes the rest possible at all.
    for beside in [
        live.paths.server_backups(ID),
        live.paths.server_media(ID),
        live.paths.server_published(ID),
    ] {
        assert!(
            !beside.starts_with(&run),
            "{} is inside the run directory",
            beside.display()
        );
    }

    // Something irreplaceable, something regenerable, and something that is
    // not the server's at all.
    tokio::fs::create_dir_all(run.join("plugins"))
        .await
        .expect("plugins");
    tokio::fs::write(
        run.join("plugins").join("geode-irreplaceable.yml"),
        "this exists nowhere else\n",
    )
    .await
    .expect("plugin config");
    tokio::fs::create_dir_all(live.paths.server_media(ID))
        .await
        .expect("media");
    tokio::fs::write(live.paths.server_media(ID).join("icon.png"), b"not a png")
        .await
        .expect("icon");
    tokio::fs::create_dir_all(live.paths.server_published(ID))
        .await
        .expect("published");
    tokio::fs::write(
        live.paths.server_published(ID).join("server.properties"),
        "motd=as published\n",
    )
    .await
    .expect("published copy");

    // Two, so the second has a first to sweep up if it could — and back to
    // back, which is the shape that found the collision this test used to
    // step around. `backup::new_name` was `%Y-%m-%dT%H-%M-%S`, so two backups
    // taken inside one second were handed one name and the second
    // `File::create` truncated the first; this test waited the second out to
    // get two files to reason about. That is fixed: the name carries
    // milliseconds and `backup::claim` takes it with `create_new`, so
    // whichever backup loses a name is handed the next one rather than an
    // archive to write over. So the wait is gone, and two backups in one
    // second is the case worth arranging rather than the one to avoid.
    // `backup.rs`'s `two_backups_in_the_same_moment_cannot_take_each_others_name`
    // pins the same claim offline, at a rate no pair of real backups can
    // reach.
    let first = live.backup().await;
    let second = live.backup().await;
    assert_ne!(
        first.name, second.name,
        "two backups in the same second were handed one name, and the second \
         archive is written over the first"
    );
    assert!(
        live.backup_path(&first.name).is_file(),
        "the first backup is not where it was reported"
    );

    let names = zip_names(&live.backup_path(&second.name));
    println!(
        "== {} holds {} entries, {} bytes",
        second.name,
        names.len(),
        second.size
    );

    // What must be in it.
    for kept in [
        "world/level.dat",
        "server.properties",
        "whitelist.json",
        "ops.json",
        "eula.txt",
        "plugins/geode-irreplaceable.yml",
    ] {
        assert!(
            names.contains(kept),
            "{kept} exists nowhere else and is not in the backup"
        );
    }

    // What must not. The jar by its own name, derived from this server rather
    // than from a list.
    assert!(
        !names.contains(&server.jar),
        "{} is 50 MB Geode can ask for again",
        server.jar
    );
    for prefix in [
        "libraries/",
        "versions/",
        "logs/",
        "cache/",
        "crash-reports/",
    ] {
        let swept: Vec<&String> = names.iter().filter(|n| n.starts_with(prefix)).collect();
        assert!(swept.is_empty(), "{prefix} was carried: {swept:?}");
    }

    // The three things that are not under `run/` at all, which is the whole
    // point of where they live.
    for stray in ["backups/", "media/", "published/", "server.json"] {
        let found: Vec<&String> = names.iter().filter(|n| n.starts_with(stray)).collect();
        assert!(found.is_empty(), "{stray} reached the backup: {found:?}");
    }
    let zips: Vec<&String> = names.iter().filter(|n| n.ends_with(".zip")).collect();
    assert!(zips.is_empty(), "a backup swept up a backup: {zips:?}");

    // A nested cache, only where this server actually made one — Paper's
    // remap cache is the single biggest regenerable pile on a modern server,
    // and it lives inside a folder that is otherwise entirely the admin's.
    let remapped = run.join("plugins").join(".paper-remapped");
    if tokio::fs::metadata(&remapped).await.is_ok() {
        let carried: Vec<&String> = names
            .iter()
            .filter(|n| n.starts_with("plugins/.paper-remapped"))
            .collect();
        assert!(
            carried.is_empty(),
            "Paper's remap cache was carried: {carried:?}"
        );
        println!("== plugins/.paper-remapped exists on disk and stayed out");
    }

    // Backing up a *running* server asks it to write its world out first, and
    // that ask is a real command this server answers rather than a string
    // nobody checked.
    live.start().await;
    let said = live
        .registry
        .ask(ID, "save-all flush")
        .await
        .expect("running");
    assert!(
        !said.is_empty()
            && !said
                .iter()
                .any(|l| l.contains("Unknown or incomplete command")),
        "`save-all flush` is what backup::create types at a running server: {said:?}"
    );
    println!("== save-all flush said: {said:?}");

    let hot = live.backup().await;
    let hot_names = zip_names(&live.backup_path(&hot.name));
    assert!(
        hot_names.contains("world/level.dat"),
        "a backup of a running server has no level.dat, which restores as a new world"
    );
    live.stop().await;
}

/// A world comes back from a backup exactly as it went in.
///
/// The inverse of the thinning rule: there is no upstream to re-fetch a region
/// file from, so a world backup carries the whole world and a restore has to
/// put every byte back.
///
/// Guards `backup.rs`'s `create` walk and `restore`'s unpack, including
/// `is_safe_relative` letting ordinary nested paths through.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs a real Minecraft server"]
async fn a_world_comes_back_from_a_backup_byte_for_byte() {
    let live = live().await;
    let world = live.run_dir().join("world");

    // Every file in the real world the server generated, by digest.
    let before = digests_under(&world).await;
    assert!(
        before.len() > 3,
        "a generated world is more than {} files",
        before.len()
    );
    assert!(
        before.contains_key("level.dat"),
        "no level.dat in {}",
        world.display()
    );
    println!("== {} files in the world before", before.len());

    let info = live.backup().await;
    tokio::fs::remove_dir_all(&world).await.expect("delete");
    assert!(tokio::fs::metadata(&world).await.is_err());

    backup::restore(&live.paths, ID, &info.name)
        .await
        .expect("restore");

    let after = digests_under(&world).await;
    let mut missing: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    missing.sort();
    assert!(missing.is_empty(), "the restore left out {missing:?}");

    let mut changed: Vec<&String> = before
        .iter()
        .filter(|(rel, digest)| after.get(*rel) != Some(*digest))
        .map(|(rel, _)| rel)
        .collect();
    changed.sort();
    assert!(
        changed.is_empty(),
        "the restore changed {} files, e.g. {:?}",
        changed.len(),
        changed.iter().take(5).collect::<Vec<_>>()
    );
    println!("== all {} world files came back identical", after.len());
}

/// Every file under `dir`, relative to it, by sha256.
async fn digests_under(dir: &Path) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&current).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(rel) = path.strip_prefix(dir) else {
                continue;
            };
            out.insert(
                rel.to_string_lossy().replace('\\', "/"),
                sha256_of(&path).await,
            );
        }
    }
    out
}

/// The pairing CLAUDE.md says must stay a pair.
///
/// A backup leaves the server's own software out; a start that finds it gone
/// fetches it again. Thinning the backup without that reinstall is a backup
/// that restores to a server which will not boot — and nothing had ever run
/// that round trip against a real server, which is the only way to find out
/// whether the server really does come up afterwards.
///
/// Guards `run.rs`'s `install_if_the_software_is_gone`: its `java_major.is_none()`
/// guard, its `announce`, and the `install::install_jar` call behind it. Change
/// any of the three and this test is left with a server that cannot start.
///
/// It also records what this suite cannot reach: `backup::restore` has no
/// "stop the server first" guard of its own. Both callers have one —
/// `commands::restore_server_backup` and `daemon::api::restore` — and neither
/// is reachable from here, the first being a `#[tauri::command]` taking a
/// `State` and the second needing a `Host`. So the refusal is proved nowhere
/// in this file, and a third caller would have to remember it for itself.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs and runs a real Minecraft server, and downloads it twice"]
async fn a_thinned_backup_restores_to_a_server_that_still_boots() {
    let live = live().await;
    let server = live.server().await;
    let run = live.run_dir();

    let info = live.backup().await;
    let names = zip_names(&live.backup_path(&info.name));
    assert!(
        !names.contains(&server.jar),
        "this test is about a thinned backup and this one carries the jar"
    );
    println!(
        "== backup {} is {:.1} MB without the software",
        info.name,
        info.size as f64 / 1_048_576.0
    );

    // Exactly what a machine that lost its server folder has: the world and
    // the configuration, and none of the software.
    for gone in [run.join(&server.jar), run.join("libraries"), run.join("versions")] {
        if gone.is_dir() {
            tokio::fs::remove_dir_all(&gone).await.expect("remove");
        } else {
            let _ = tokio::fs::remove_file(&gone).await;
        }
        assert!(
            tokio::fs::metadata(&gone).await.is_err(),
            "{} is still there",
            gone.display()
        );
    }
    tokio::fs::remove_dir_all(run.join("world"))
        .await
        .expect("world");

    backup::restore(&live.paths, ID, &info.name)
        .await
        .expect("restore");
    assert!(
        tokio::fs::metadata(run.join("world").join("level.dat"))
            .await
            .is_ok(),
        "the world did not come back"
    );
    assert!(
        tokio::fs::metadata(run.join(&server.jar)).await.is_err(),
        "the restore put the jar back, so the reinstall is not what is being tested"
    );

    // The half that has never been run: this start has to notice, say so, and
    // fetch the software again before it can boot.
    live.start().await;
    assert!(
        live.log
            .has("is not here, so Geode is fetching this server's software again"),
        "the start never said it was reinstalling:\n{}",
        live.log.tail(40)
    );
    assert!(
        tokio::fs::metadata(run.join(&live.server().await.jar))
            .await
            .is_ok(),
        "the jar was not put back"
    );
    println!("== the restored server reinstalled its own software and booted");

    // And it is a working server, not merely a process: it answers.
    let said = live.registry.ask(ID, "list").await.expect("running");
    assert!(!said.is_empty(), "the restored server never answered");

    live.stop().await;
}

// ---------------------------------------------------------------------------
// scheduled work
// ---------------------------------------------------------------------------

/// Both scheduled actions, against a real server.
///
/// A scheduled backup takes a real archive of a real world and says so in the
/// console. A scheduled command reaches the real JVM's stdin, which a `send`
/// returning `true` does not prove — so the command picked is one whose effect
/// lands on disk, written by the server itself, and the disabled one beside it
/// is picked the same way. And the run is stamped without anything else about
/// the server being written back over the minutes a backup takes.
///
/// Guards `schedule.rs`'s `run_due` — both arms of the `match`, the `is_due`
/// filter in front of them, `run_backup`, and `stamp`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs and runs a real Minecraft server"]
async fn a_scheduled_backup_and_a_scheduled_command_reach_a_real_server() {
    let live = live().await;

    let mut server = live.server().await;
    server.notes = "written before the schedules ran".into();
    server.schedules = vec![
        Schedule {
            id: "nightly".into(),
            action: ScheduleAction::Backup,
            every_minutes: 1,
            enabled: true,
            last_run: None,
        },
        // Not `say`: a broadcast with nobody to broadcast to proves nothing on
        // disk, and this one makes the server write its own file.
        Schedule {
            id: "letmein".into(),
            action: ScheduleAction::Command("whitelist add Dinnerbone".into()),
            every_minutes: 1,
            enabled: true,
            last_run: None,
        },
        Schedule {
            id: "off".into(),
            action: ScheduleAction::Command("whitelist add Notch".into()),
            every_minutes: 1,
            enabled: false,
            last_run: None,
        },
    ];
    server.save(&live.paths).await.expect("save schedules");

    let before = backup::list(&live.paths, ID).await.expect("backups").len();
    assert!(live.file_rows("whitelist.json").await.is_empty());

    live.start().await;
    live.log.clear();

    let now = chrono::Utc::now().timestamp();
    schedule::run_due(&live.sink, &live.paths, &live.registry, now).await;

    // The backup really happened, and holds a real world.
    let backups = backup::list(&live.paths, ID).await.expect("backups");
    assert_eq!(backups.len(), before + 1, "the scheduled backup did not run");
    assert!(
        zip_names(&live.backup_path(&backups[0].name)).contains("world/level.dat"),
        "the scheduled backup holds no world"
    );
    assert!(
        live.log.has("scheduled backup taken"),
        "the console was not told:\n{}",
        live.log.tail(20)
    );

    // The command reached the JVM: the server answered it in its own voice,
    // and then wrote its own file. Geode's `[Geode] scheduled:` line proves
    // neither — it is written whether the pipe worked or not.
    let echoed = live
        .wait_for_echo("Dinnerbone", Duration::from_secs(30))
        .await;
    println!("== the server answered the scheduled command: {echoed:?}");

    let names = names_in(&live.file_rows("whitelist.json").await);
    assert!(
        names.iter().any(|n| n == "Dinnerbone"),
        "the scheduled command did not reach the server: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "Notch"),
        "a disabled schedule ran: {names:?}"
    );

    // Stamped, and nothing else written back.
    let after = live.server().await;
    for id in ["nightly", "letmein"] {
        let stamped = after
            .schedules
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("{id} is gone"));
        assert_eq!(stamped.last_run, Some(now), "{id} was not stamped");
    }
    assert_eq!(
        after
            .schedules
            .iter()
            .find(|s| s.id == "off")
            .expect("off")
            .last_run,
        None,
        "a schedule that did not run was stamped anyway"
    );
    assert_eq!(after.notes, "written before the schedules ran");

    live.stop().await;
}

// ---------------------------------------------------------------------------
// server.properties and the EULA
// ---------------------------------------------------------------------------

/// `server.properties` is the server's file, and Geode edits it line by line.
///
/// Stopped, a write is ours and the comments and ordering Mojang put there
/// survive it. Running, the write still lands — the server reads the file at
/// start-up only — and this proves the real server does not write it back out
/// from memory at shutdown and lose the edit.
///
/// And `eula.txt`: nothing starts a server whose EULA has not been accepted,
/// and accepting it writes the file Mojang asks for.
///
/// Guards `properties.rs`'s `apply` against a real Mojang-written file,
/// `mod.rs`'s `accept_eula`, and `run.rs`'s `spawn` refusing on
/// `!server.eula_accepted`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs and runs a real Minecraft server"]
async fn server_properties_and_the_eula_survive_the_real_files() {
    let live = live().await;
    let file = live.run_dir().join("server.properties");

    // --- stopped ---

    let original = tokio::fs::read_to_string(&file).await.expect("properties");
    assert!(
        original.lines().any(|l| l.starts_with('#')),
        "the real file should carry Mojang's own comments:\n{original}"
    );
    let comments_before = original.lines().filter(|l| l.starts_with('#')).count();
    let keys_before: Vec<String> = properties::parse(&original)
        .into_iter()
        .map(|p| p.key)
        .collect();

    live.set_property("motd", "geode-live-stopped").await;
    let edited = tokio::fs::read_to_string(&file).await.expect("properties");
    assert_eq!(
        comments_before,
        edited.lines().filter(|l| l.starts_with('#')).count(),
        "an edit lost Mojang's comments"
    );
    let keys_after: Vec<String> = properties::parse(&edited)
        .into_iter()
        .map(|p| p.key)
        .collect();
    assert_eq!(
        keys_before, keys_after,
        "an edit reordered or dropped settings"
    );
    let motd = properties::read(&live.paths, ID)
        .await
        .expect("read")
        .into_iter()
        .find(|p| p.key == "motd")
        .expect("motd");
    assert_eq!(motd.value, "geode-live-stopped");

    // A line break cannot be held by this format, and is refused rather than
    // written as a setting nobody asked for.
    let broken = properties::write(
        &live.paths,
        ID,
        &[Property {
            key: "motd".into(),
            value: "one\ntwo".into(),
        }],
    )
    .await
    .expect_err("two lines is two settings");
    assert!(broken.to_string().contains("line break"), "{broken}");

    // --- the EULA ---

    let mut server = live.server().await;
    server.eula_accepted = false;
    server.save(&live.paths).await.expect("save");
    let refused = run::start(
        &live.sink,
        &live.paths,
        &live.http,
        &live.registry,
        ID,
        None,
    )
    .await
    .expect_err("nothing starts a server whose EULA nobody accepted");
    assert!(
        refused.to_string().contains("EULA has not been accepted"),
        "{refused}"
    );
    assert!(!live.registry.is_running(ID), "it started anyway");

    let _ = tokio::fs::remove_file(live.run_dir().join("eula.txt")).await;
    let mut server = live.server().await;
    server::accept_eula(&live.paths, &mut server)
        .await
        .expect("accept");
    assert!(server.eula_accepted);
    let eula = tokio::fs::read_to_string(live.run_dir().join("eula.txt"))
        .await
        .expect("eula.txt");
    assert!(eula.contains("eula=true"), "{eula}");
    assert!(
        eula.contains("aka.ms/MinecraftEULA"),
        "the accepted text should point at what was accepted: {eula}"
    );

    // --- running ---

    live.start().await;
    live.set_property("motd", "geode-live-running").await;
    let while_running = properties::read(&live.paths, ID)
        .await
        .expect("read")
        .into_iter()
        .find(|p| p.key == "motd")
        .expect("motd");
    assert_eq!(while_running.value, "geode-live-running");

    live.stop().await;

    // The real question: the server owns this file too, and a clean shutdown
    // must not put the old value back.
    let survived = properties::read(&live.paths, ID)
        .await
        .expect("read")
        .into_iter()
        .find(|p| p.key == "motd")
        .expect("motd");
    assert_eq!(
        survived.value, "geode-live-running",
        "a clean shutdown overwrote an edit made while the server ran"
    );
    println!("== the edit made while it ran survived its shutdown");
}

// ---------------------------------------------------------------------------
// the heap a host allows
// ---------------------------------------------------------------------------

/// A host's heap ceiling lowers the JVM this start puts up, and does not
/// become the server's own setting.
///
/// The second half can only be asserted here. `geoded` passes
/// `GEODE_MAX_HEAP_MB` into every `run::start`; the clamp used to be applied to
/// the record in place, and the save at the end of a successful start writes
/// the whole struct in order to record `last_started` — so the host's ceiling
/// went to disk in place of the figure the owner had asked for, every start on
/// a constrained host lowered it again, nothing ever raised it, and a server
/// moved back to a roomier host went on asking for the smaller heap. It was
/// measured in a container, which is what this test replaces. `run.rs`'s own
/// tests cannot reach it: a start with no jar fails before that save, so
/// nothing is written and the figure they read back is the one they created a
/// moment earlier.
///
/// Guards two lines of `run::start_again`. The `&launching` handed to `spawn`
/// is where the `-Xmx` below comes from — change it back to `&server` and the
/// JVM ignores the ceiling altogether. The absence of any
/// `server.settings.memory_mb = ceiling` before the save is where the figure
/// on disk comes from — put one back and that is the original defect exactly.
///
/// `last_started` is asserted beside the heap, and not for its own sake: it is
/// the proof that this start reached its save at all. Without it, a start that
/// stopped earlier would leave the owner's figure on disk because this test
/// wrote it there, which is how the offline version of this claim passed for
/// as long as it did.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "installs and runs a real Minecraft server"]
async fn a_hosts_ceiling_is_not_saved_as_the_servers_own_setting() {
    let live = live().await;

    // The ceiling is the heap the whole suite runs this server at, so the JVM
    // below is one this machine is known to be able to start. The owner's
    // figure is twice that and is never meant to be launched with.
    let mut server = live.server().await;
    let ceiling = server.settings.memory_mb;
    let owner = ceiling * 2;
    server.settings.memory_mb = owner;
    server.last_started = None;
    server
        .save(&live.paths)
        .await
        .expect("save the owner's figure");

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs() as i64;
    live.start_under(Some(ceiling)).await;

    // Said in the console, because a server quietly given less memory than it
    // asked for is a server nobody can explain the lag of.
    assert!(
        live.log.has(&format!("{ceiling} MB rather than {owner} MB")),
        "the console never said the heap had been lowered:\n{}",
        live.log.tail(30)
    );

    // What the JVM was really started with, read off the process rather than
    // off anything Geode said about it. `/proc` is the whole of why this is a
    // Linux-only assertion, which this file is anyway.
    let pid = guard(&live.pid).expect("a started server has a pid");
    let raw = std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_else(|e| panic!("/proc/{pid}/cmdline: {e} — this assertion needs Linux"));
    let argv: Vec<String> = raw
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    println!("== {}", argv.join(" "));
    // One assertion rather than two: `build_args` pushes exactly one `-Xmx`,
    // so "the ceiling's is there" and "the owner's is not" are the same claim,
    // and the second would only ever fail where the first already had.
    assert!(
        argv.iter().any(|arg| arg == &format!("-Xmx{ceiling}M")),
        "the JVM was not started under the host's ceiling: {argv:?}"
    );

    // And the record that start wrote. The ceiling is a fact about the
    // machine, so what is on disk has to be the owner's figure.
    let saved = live.server().await;
    assert!(
        saved.last_started.unwrap_or(0) >= before,
        "this start never reached its save, so the figure below is only the one \
         this test wrote: last_started is {:?}",
        saved.last_started
    );
    assert_eq!(
        saved.settings.memory_mb, owner,
        "the host's ceiling was written into the server's own settings"
    );
    println!("== started under {ceiling} MB with {owner} MB still on disk");

    live.stop().await;

    // Again after the stop, which is the other moment anything writes
    // `server.json` on this path.
    assert_eq!(
        live.server().await.settings.memory_mb,
        owner,
        "the host's ceiling reached the record as the server shut down"
    );
}

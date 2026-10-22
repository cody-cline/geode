//! `geoded`: Geode's servers, on a machine with nobody sitting at it.
//!
//! The same library the desktop app drives, driven by a process with no window.
//! It exists so a server can keep running when Geode is closed, on a box in a
//! cupboard or a rented host, and still be managed from the app. The container
//! files under `container/` are what it was written for.
//!
//! This is the shell only — the equivalent of `lib.rs` and `desktop.rs` for the
//! app. Every bit of work it does belongs to the module that owns it:
//! `server::` installs and starts servers, `paths::` knows where things live,
//! `events::` is where anything that happens goes. Nothing here reimplements a
//! command.
//!
//! # What it does
//!
//! Reads [`Settings`] from the environment, prepares `GEODE_HOME`, runs the
//! scheduled tasks every server has, serves [`api`]'s routes and [`console`]'s
//! sockets through [`serve`]'s listener, and on SIGTERM stops every running
//! server the way the app stops one before exiting ([`shutdown`]).
//!
//! # What it does not do
//!
//! * **No TLS of its own.** The management port is plain HTTP and expects a
//!   loopback publish, a reverse proxy, a tunnel or a VPN in front of it. See
//!   [`serve`], which says why that is a different answer from
//!   [`crate::share::host`]'s.
//! * **No users.** One token, held by whoever administers the host. There are
//!   no accounts, no roles and no audit of who did what; a person you give the
//!   token to can do everything.
//! * **No instances.** The app launches the game; a host runs servers. Nothing
//!   here exposes `instance::` or `launch::`, and a machine in a cupboard has
//!   no screen to render one on.
//! * **No local-to-remote transfer yet.** Moving a server from the desktop app
//!   to a host is the feature this is ultimately for, and it is not here.
//!
//! # What transfer would need
//!
//! Written down because the shape is a decision, not a detail, and it should
//! be the owner's:
//!
//! * **What moves.** A server is `server.json`, `run/` (the world,
//!   `server.properties`, `eula.txt`, mods and plugins), `media/` and
//!   `share.json`. `backups/` is the one part nobody wants to send over a
//!   domestic uplink, and `share.json` is the one part that must *not* arrive
//!   unchanged — it names a code and an address belonging to the machine that
//!   had the server, and see `server/share.rs` for why it is a separate file.
//! * **References, not bytes, where there is a reference.** The pack format
//!   already carries provider content by project and version and fetches it
//!   with the recipient's credentials — a transfer that zipped `mods/` would
//!   throw that away, and docs/sharing-and-hosting.md says why that matters
//!   beyond size. A world has no reference and must travel as bytes.
//! * **Which direction drives it.** Either the app uploads to the host, or the
//!   host fetches from the app. Uploading needs a route here that accepts a
//!   large body — [`api::MAX_BODY`] is sized for settings, not worlds, so it
//!   would be a route of its own with a ceiling of its own — a resume story
//!   for a connection that drops partway through a 10 GB world, and somewhere
//!   to put it that is not the live server directory. Fetching needs the host
//!   to reach the app, which is a listener on a desktop machine and the thing
//!   `share/host.rs` already is.
//! * **What happens to the original.** Moved or copied, and if moved, when the
//!   local copy goes — before the first successful start on the host, or
//!   after. A server that exists in two places and is started in both will
//!   have two divergent worlds within a minute.
//! * **What the player sees.** A moved server is at a new address. Players who
//!   joined it have the old one pinned in their server list, and the listing in
//!   `share/manifest.rs` is what keeps that row in step.
//!
//! None of the above is built. What is built does not foreclose it: the list
//! route reads `servers/` from disk every time, so a server that arrives needs
//! no cache invalidated, and every route is shaped per server rather than as
//! one document a transfer would have to merge into.

pub mod api;
pub mod console;
pub mod serve;
pub mod settings;
pub mod shutdown;
pub mod token;

use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::error::{Context, Result};
use crate::events::EventSink;
use crate::paths::Paths;
use crate::state::AppState;

pub use console::Console;
pub use settings::Settings;
pub use token::Token;

/// Everything a request needs, and the only thing passed to a route.
///
/// [`AppState`] is the app's own state, used unchanged: it holds the paths, the
/// HTTP client, the jar metadata cache and the registry of running servers, and
/// it knows nothing about Tauri. Sharing it is what lets a route call exactly
/// the function the matching Tauri command calls, with exactly the same
/// arguments.
pub struct Host {
    pub state: Arc<AppState>,
    /// Where the library's events go: standard output, and every open console.
    pub sink: EventSink,
    pub console: Arc<Console>,
    pub token: Token,
    /// The ceiling on one server's heap, if the admin set one.
    pub max_heap_mb: Option<u32>,
    /// Console sockets left, of [`console::MAX_SOCKETS`].
    pub sockets: Arc<Semaphore>,
}

impl Host {
    pub fn new(state: Arc<AppState>, token: Token, max_heap_mb: Option<u32>) -> Self {
        let console = Arc::new(Console::default());
        Self {
            state,
            sink: console.clone(),
            console,
            token,
            max_heap_mb,
            sockets: Arc::new(Semaphore::new(console::MAX_SOCKETS)),
        }
    }
}

/// Start the host and run until it is asked to stop.
pub async fn run() -> Result<()> {
    let settings = Settings::from_env()?;
    let paths = Paths::new();
    paths
        .ensure_all()
        .await
        .ctx(format!("preparing {}", paths.root.display()))?;

    let config = crate::config::Config::load(&paths).await.unwrap_or_default();
    let host = Arc::new(Host::new(
        Arc::new(AppState::new(paths.clone(), config)),
        settings.token,
        settings.max_heap_mb,
    ));

    // The scheduled backups and commands each server carries. The app runs
    // this same loop; a host that did not would be a host where a server's
    // nightly backup silently stopped happening the moment it was moved here.
    let ticker = tokio::spawn(crate::server::schedule::ticker(
        host.sink.clone(),
        paths.clone(),
        host.state.servers.clone(),
    ));

    let listener = serve::Listener::start(settings.listen, host.clone()).await?;
    notice(
        &host.sink,
        format!(
            "listening on {}, data in {}",
            listener.local_addr,
            paths.root.display()
        ),
    );

    shutdown::asked_to_stop().await;
    // The ticker first, so nothing new starts while we are stopping — a
    // scheduled backup begun now would be racing the shutdown below for the
    // same files. It does not stop one already running: `backup::create` zips
    // inside a blocking task that no abort reaches, and it writes under the
    // backup's final name, so a host killed during one leaves a short archive.
    // That is true of the app too, and is the reason the grace period below is
    // generous rather than tight.
    ticker.abort();
    notice(
        &host.sink,
        "stopping: asking every running server to shut down",
    );
    listener.stop().await;

    let killed = shutdown::stop_running(&host.state.servers, shutdown::STOP_LIMIT).await;
    if !killed.is_empty() {
        notice(
            &host.sink,
            format!(
                "killed after {} seconds without shutting down: {} — check these worlds",
                shutdown::STOP_LIMIT.as_secs(),
                killed.join(", ")
            ),
        );
    }
    notice(&host.sink, "stopped");
    Ok(())
}

/// Something the person running the host should see, said through the sink
/// rather than printed — so it goes wherever events go.
fn notice(sink: &EventSink, message: impl Into<String>) {
    sink.emit(
        "geode://daemon",
        serde_json::json!({ "message": message.into() }),
    );
}

#[cfg(test)]
mod tests {
    /// Nothing that compiles the desktop app compiles this module.
    ///
    /// Tauri is behind the `desktop` feature and hands the default build
    /// several Cargo features for free, so a dependency the host needs spelled
    /// out can go missing while `cargo test --lib` and `cargo clippy
    /// --all-targets` stay green — `toml`'s `serde` and `std` did exactly
    /// that. CLAUDE.md therefore asks for `--no-default-features` as well, and
    /// a rule nothing enforces is a rule a tired afternoon skips: without this
    /// a green run would mean the app builds, and the container image that
    /// nobody here builds locally would fail on whoever pulled it.
    ///
    /// Read rather than asserted in prose, in the same spirit as
    /// `commands::tests::every_command_in_this_file_is_reachable_from_the_handler_list`:
    /// the check lives in a file this crate does not otherwise touch, so the
    /// only place the two can be held to each other is here.
    #[test]
    fn ci_compiles_the_headless_build_and_not_only_the_app() {
        let workflow = include_str!("../../../.github/workflows/build.yml");
        // The checks, not the release bundles: a bundle is a desktop app by
        // definition, so a `--no-default-features` down there would prove
        // nothing about a push.
        let checks = workflow.split("\n  bundle:").next().unwrap_or(workflow);

        // A step that runs it, not a comment that mentions it — including the
        // comment beside the step itself, which explains why it is there.
        assert!(
            checks.lines().any(|line| {
                line.contains("run:")
                    && line.contains("cargo")
                    && line.contains("--no-default-features")
            }),
            "no step in build.yml's check job compiles the headless build, so \
             nothing in CI builds `geoded` or `daemon/` at all"
        );
    }
}

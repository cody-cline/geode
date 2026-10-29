//! Running a server and talking to its console.
//!
//! The important difference from launching the game: a server's stdin stays
//! open. That pipe *is* the console — it is how `stop` gets sent, how an
//! operator runs a command, and why stopping can be graceful instead of a kill
//! that loses whatever had not been saved.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use crate::events::{self, EventSink};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use crate::error::{Error, Result};
use crate::net::Progress;
use crate::paths::Paths;

use super::installer::LaunchKind;
use super::{Server, ServerKind};

/// How long a server gets to shut down cleanly before it is killed.
///
/// Generous on purpose: saving a large world takes a while, and killing it
/// halfway is how a world gets corrupted.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a console command gets to say anything at all before Geode calls
/// it silence. Generous enough for a server mid-chunk-save, short enough that a
/// screen waiting on it does not look hung.
const REPLY_WINDOW: std::time::Duration = std::time::Duration::from_millis(3000);

/// Once it has started answering, how long to keep listening for more.
///
/// `banlist` answers over several lines, so stopping at the first would read
/// half a reply as the whole one.
const REPLY_SETTLE: std::time::Duration = std::time::Duration::from_millis(400);

/// How many lines of one reply are worth keeping.
///
/// The window catches whatever the server logged while it answered, which on a
/// busy server is not only the answer. A ceiling keeps that from arriving as a
/// wall of text; what it costs is the tail of a very long `banlist`.
const REPLY_LINES: usize = 24;

/// How many console lines a listener may fall behind before it loses some.
const CONSOLE_BACKLOG: usize = 256;

/// How many times in a row Geode starts a server again that will not stay up.
///
/// A ceiling rather than forever, because a server that dies while booting — a
/// mod that will not load, a port already taken, a world it cannot read — dies
/// the same way every time, and the fifth identical crash is not news. Pressing
/// Start clears the count, so this limits an unattended loop rather than a day.
const RESTART_CEILING: u32 = 5;

/// The wait before the first restart. Doubled for each one after it, so a
/// server that keeps failing is tried less and less often instead of pinning a
/// core while it fails.
const RESTART_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The longest that wait ever gets, which makes the five waits 5, 10, 20, 40
/// and 60 seconds.
const RESTART_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a server has to stay up before the restarts behind it stop
/// counting.
///
/// Ten minutes is past loading the world and letting players in, so a server
/// that falls over once an evening is started again every time, while one that
/// cannot boot runs out of tries.
const RESTART_HEALTHY: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLine {
    pub server_id: String,
    pub line: String,
    /// `out` or `err`. Kept apart because a JVM crash arrives on stderr and
    /// should not be coloured like an ordinary log line.
    pub stream: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerExit {
    pub server_id: String,
    pub code: Option<i32>,
    pub success: bool,
    /// Whether somebody asked for this exit — Stop, Kill, `stop` typed into
    /// the console, or Geode closing.
    ///
    /// An exit code cannot stand in for it. Paper exits 0 after `Failed to
    /// bind to port`, so a server that never came up at all arrived on screen
    /// as a clean "stopped. Ran for 7 seconds." The wait already has this
    /// answer — it is what decides a restart — and it was simply not passed
    /// on to the thing that tells the person.
    pub asked_for: bool,
    pub ran_seconds: u64,
    /// Seconds until Geode starts it again, or `None` when it is not going to
    /// — because somebody asked for this exit, because automatic restarts are
    /// off, or because Geode has given up on it.
    pub restart_in_seconds: Option<u64>,
    /// Which restart this exit causes, or how many were tried before Geode
    /// gave up. Zero when automatic restarting is not in play at all.
    pub restarts: u32,
    /// Set when automatic restarts are on and Geode has stopped trying, so
    /// nobody has to work out for themselves why a server went quiet.
    pub gave_up: bool,
}

impl ServerExit {
    /// The exit as everything watching hears it, including what Geode is about
    /// to do about it.
    fn new(
        server_id: String,
        code: Option<i32>,
        success: bool,
        asked_for: bool,
        ran: std::time::Duration,
        after: AfterExit,
    ) -> Self {
        Self {
            server_id,
            code,
            success,
            asked_for,
            ran_seconds: ran.as_secs(),
            restart_in_seconds: match after {
                AfterExit::Restart { wait, .. } => Some(wait.as_secs()),
                _ => None,
            },
            restarts: match after {
                AfterExit::Nothing => 0,
                AfterExit::Restart { attempt, .. } => attempt,
                AfterExit::GaveUp { attempts } => attempts,
            },
            gave_up: matches!(after, AfterExit::GaveUp { .. }),
        }
    }
}

/// Why a server is no longer running, as far as anything outside the process
/// can tell.
#[derive(Debug, Clone, Copy)]
struct Exit {
    /// Somebody asked for this: Stop, Kill, a scheduled command, or the stop
    /// word typed into the console. Never guessed from the exit code — a
    /// server told to stop and a server that fell over both exit zero as
    /// often as not.
    asked_for: bool,
    /// Geode itself is going away, and took its servers with it.
    closing: bool,
    /// The setting as it is *now*, not as it was when the server started.
    auto_restart: bool,
    /// How long it ran for.
    ran: std::time::Duration,
    /// Restarts since it last stayed up, not counting this exit.
    restarts: u32,
}

/// What Geode does about a server that is no longer running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterExit {
    /// Leave it stopped.
    Nothing,
    /// Start it again after `wait`. `attempt` is which restart that will be.
    Restart {
        attempt: u32,
        wait: std::time::Duration,
    },
    /// It will not stay up, and Geode has stopped trying.
    GaveUp { attempts: u32 },
}

/// Decide what an exit deserves.
///
/// Pure, because this is the whole of the judgement and the part worth testing
/// without a JVM: everything it needs is already known by the time a process
/// has ended.
fn after_exit(exit: Exit) -> AfterExit {
    if exit.asked_for || exit.closing || !exit.auto_restart {
        return AfterExit::Nothing;
    }
    // Stayed up long enough to count as working, so whatever happened now is a
    // fresh problem and gets a fresh set of tries.
    let attempt = if exit.ran >= RESTART_HEALTHY {
        1
    } else {
        exit.restarts + 1
    };
    if attempt > RESTART_CEILING {
        return AfterExit::GaveUp {
            attempts: exit.restarts,
        };
    }
    AfterExit::Restart {
        attempt,
        wait: restart_wait(attempt),
    }
}

/// The wait before restart number `attempt`: the first wait doubled once per
/// try before it, up to the ceiling.
fn restart_wait(attempt: u32) -> std::time::Duration {
    RESTART_WAIT
        .saturating_mul(1u32 << attempt.clamp(1, RESTART_CEILING).saturating_sub(1))
        .min(RESTART_WAIT_MAX)
}

/// The console command that asks this software to shut down.
pub fn stop_command(kind: ServerKind) -> &'static str {
    match kind {
        // BungeeCord and its forks use `end`; Velocity takes `shutdown`.
        ServerKind::Waterfall => "end",
        ServerKind::Velocity => "shutdown",
        _ => "stop",
    }
}

/// Whether this console line is the word that asks this software to shut down.
///
/// A leading slash is allowed because people type one out of habit and a server
/// console takes it either way. Asked of the kind rather than matched against
/// every shutdown word there is: `stop` means nothing to Velocity, and a
/// plugin on a Paper server is free to register `end`.
fn is_stop(command: &str, kind: ServerKind) -> bool {
    command
        .trim()
        .trim_start_matches('/')
        .eq_ignore_ascii_case(stop_command(kind))
}

pub struct RunningServer {
    pub pid: u32,
    pub started_at: std::time::Instant,
    /// Which run of this server this is, counted by the registry.
    ///
    /// An id is not an identity: stop a server, change a setting and start it
    /// again, and the second process answers to the same id as the first. A
    /// stop's grace timer has to outlive the process it is about, so without
    /// this it would find the id still taken two minutes later and kill a
    /// healthy server nobody asked it to touch. Set by [`ServerRegistry::insert`],
    /// so no caller can get it wrong.
    generation: u64,
    /// Which software this is, so the registry knows the word that asks it to
    /// shut down without being told.
    kind: ServerKind,
    /// Whether this process is ending because somebody wanted it to.
    ///
    /// Shared with the task waiting on the process, which is the only place it
    /// is read: an exit nobody asked for is the one worth restarting, and from
    /// the wait there is nothing else to tell them apart by.
    asked_to_stop: Arc<AtomicBool>,
    commands: mpsc::UnboundedSender<String>,
    kill: tokio::sync::watch::Sender<bool>,
    /// Every console line this server has produced, for whoever is waiting on
    /// an answer to something they just sent. The console pane gets its copy
    /// through the event sink; this is the same text, taken a second time by
    /// code that has to read it rather than show it.
    console: broadcast::Sender<String>,
}

impl RunningServer {
    /// Write a line to the server's stdin, exactly as typing it would.
    pub fn send(&self, command: &str) -> bool {
        let line = command.trim_end_matches(['\r', '\n']);
        // The stop word typed into the console ends the process exactly as
        // pressing Stop does, and from the wait it looks like any other exit.
        // This is where the two are told apart, whoever did the typing — an
        // operator, or a scheduled command.
        if is_stop(line, self.kind) {
            self.asked_to_stop.store(true, Ordering::Relaxed);
        }
        self.commands.send(format!("{line}\n")).is_ok()
    }

    /// Ask it to shut down the way an operator would, in the word this
    /// software understands.
    fn ask_to_stop(&self) -> bool {
        self.asked_to_stop.store(true, Ordering::Relaxed);
        self.send(stop_command(self.kind))
    }

    pub fn force_kill(&self) {
        // Every kill is Geode's own doing — the Kill button, a stop that ran
        // out of grace, or the app closing — so none of them is a crash.
        self.asked_to_stop.store(true, Ordering::Relaxed);
        let _ = self.kill.send(true);
    }
}

/// Which servers have a process attached.
///
/// A synchronous mutex, like the game registry: every operation is a map
/// lookup, and the exit task must be able to deregister without an async
/// context.
#[derive(Clone, Default)]
pub struct ServerRegistry {
    inner: Arc<Mutex<HashMap<String, RunningServer>>>,
    /// Servers whose start has begun and has not yet spawned a process.
    ///
    /// The map above only knows a server once there is a process, and a start
    /// spends the time before that moving mods, pruning logs and fetching a
    /// JDK — so without this a second start passes [`Self::is_running`] and
    /// spawns a second JVM on the same world. See [`Self::claim_start`].
    starting: Arc<Mutex<HashSet<String>>>,
    /// Set once Geode is going away, and never cleared.
    ///
    /// A server Geode stopped on its way out asked for nothing and must stay
    /// stopped: without this, closing the app would race a restart that then
    /// has nobody left to watch it.
    closing: Arc<AtomicBool>,
    /// Counts every process this registry has ever held, so each one can be
    /// told from the next under the same id. See [`RunningServer::generation`].
    generations: Arc<std::sync::atomic::AtomicU64>,
}

impl ServerRegistry {
    /// Take the start of `id`, or `None` when one is already under way.
    ///
    /// The claim lasts as long as the returned guard, which the start holds
    /// until the process is in the registry or it has given up. The same shape
    /// as [`crate::state::AppState::claim_launch`], and for the same reason:
    /// two of these at once rewrite the very files the other is reading, and
    /// then both spawn.
    pub fn claim_start(&self, id: &str) -> Option<StartClaim> {
        let mut starting = self.starting.lock().ok()?;
        if !starting.insert(id.to_string()) {
            return None;
        }
        Some(StartClaim {
            starting: self.starting.clone(),
            id: id.to_string(),
        })
    }

    pub fn insert(&self, id: String, mut running: RunningServer) {
        if let Ok(mut map) = self.inner.lock() {
            running.generation = self.generations.fetch_add(1, Ordering::Relaxed);
            map.insert(id, running);
        }
    }

    pub fn remove(&self, id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(id);
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.inner
            .lock()
            .map(|m| m.contains_key(id))
            .unwrap_or(false)
    }

    pub fn running_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Send one console command. False when nothing is running.
    pub fn send(&self, id: &str, command: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|s| s.send(command)))
            .unwrap_or(false)
    }

    /// Send one console command and collect what the server says back.
    ///
    /// `None` means nothing is running, which is a different answer from
    /// `Some(vec![])` — that one means the command went down the pipe and the
    /// server said nothing at all. Both matter: a screen that reports a ban as
    /// done when the server never acknowledged it has lied to somebody about
    /// who can get in.
    ///
    /// The listener is subscribed before the command is written, so a reply
    /// that arrives in the same instant is still caught. What comes back is
    /// everything the console produced during the wait — on a busy server that
    /// can include lines of its own, which is why callers show the reply rather
    /// than claiming to have understood it.
    pub async fn ask(&self, id: &str, command: &str) -> Option<Vec<String>> {
        let mut lines = {
            // The lock is a synchronous one and must not be held across an
            // await, so subscribing and sending both happen inside this block.
            let map = self.inner.lock().ok()?;
            let running = map.get(id)?;
            let rx = running.console.subscribe();
            if !running.send(command) {
                return None;
            }
            rx
        };

        let mut said = Vec::new();
        let silence_ends = tokio::time::Instant::now() + REPLY_WINDOW;
        while said.len() < REPLY_LINES {
            let wait = if said.is_empty() {
                silence_ends.saturating_duration_since(tokio::time::Instant::now())
            } else {
                REPLY_SETTLE
            };
            match tokio::time::timeout(wait, lines.recv()).await {
                Ok(Ok(line)) => said.push(line),
                // Behind by more than the backlog. Whatever was missed is
                // gone, so keep listening rather than reporting silence.
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                // The server's pipes closed: it has exited mid-command.
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_) => break,
            }
        }
        Some(said)
    }

    /// Ask a server to stop the way an operator would, and kill it only if it
    /// will not. Returns false when nothing is running.
    ///
    /// The kind is not a parameter because the registry knows it: a caller
    /// that had to look it up could pass one the running process disagrees
    /// with, and the stop would go out in a word it does not understand.
    pub fn stop(&self, id: &str) -> bool {
        // Taken before the word goes out, because the grace timer below runs
        // long after this process may have exited and the id may have been
        // taken by the next one. Stop, change a setting, start again is an
        // ordinary afternoon, and a timer that knew only the id would kill
        // that healthy second server two minutes later with nothing said but
        // "stopped".
        let Some(generation) = self.generation(id) else {
            return false;
        };
        if !self.ask_to_stop(id) {
            return false;
        }
        let registry = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(STOP_GRACE).await;
            registry.kill_run(&id, generation);
        });
        true
    }

    /// Which run of `id` is in the registry now, if any.
    fn generation(&self, id: &str) -> Option<u64> {
        self.inner.lock().ok()?.get(id).map(|s| s.generation)
    }

    pub fn kill(&self, id: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|s| s.force_kill()))
            .is_some()
    }

    /// Kill `id` only while it is still the run that `generation` names.
    ///
    /// False when that run has already gone — which is the ordinary case for
    /// a server that stopped when it was asked to, and the whole reason this
    /// exists rather than [`Self::kill`].
    fn kill_run(&self, id: &str, generation: u64) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|m| {
                m.get(id)
                    .filter(|s| s.generation == generation)
                    .map(|s| s.force_kill())
            })
            .is_some()
    }

    /// Whether Geode is shutting down, and so should start nothing again.
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
    }

    /// Record that we are going away, so nothing is started again from here on.
    ///
    /// [`Self::stop_all`] is the app's whole closing sequence; the host stops
    /// its servers with a grace period of its own ([`crate::daemon::shutdown`])
    /// and needs this half by itself. Never cleared: a process that has begun
    /// exiting does not change its mind.
    pub fn close(&self) {
        self.closing.store(true, Ordering::Relaxed);
    }

    /// Ask one server to shut down the way an operator would, and leave the
    /// killing to the caller. Returns false when nothing is running.
    ///
    /// [`Self::stop`] with the 120-second kill attached is what a Stop button
    /// wants; a host stopping every server at once waits on all of them
    /// together and kills what is left itself, so it asks through this.
    pub fn ask_to_stop(&self, id: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|s| s.ask_to_stop()))
            .unwrap_or(false)
    }

    /// A registry holding one server whose console a test drives instead of a
    /// JVM: what Geode writes to stdin arrives on the returned receiver, and
    /// what the test pushes into the sender is what the server "said".
    ///
    /// Here rather than in a test module because the console direction is the
    /// half of `server::players` that cannot be tested against files, and
    /// `RunningServer`'s channels are this module's own.
    #[cfg(test)]
    pub(crate) fn fake_running(
        id: &str,
    ) -> (
        Self,
        mpsc::UnboundedReceiver<String>,
        broadcast::Sender<String>,
    ) {
        let registry = Self::default();
        let (typed, console) = registry.fake_start(id, ServerKind::Vanilla);
        (registry, typed, console)
    }

    /// The same, into a registry a test already holds: a server that starts
    /// part-way through something, the way a scheduled task or a click on Start
    /// does while a stopped server's files are being edited.
    #[cfg(test)]
    pub(crate) fn fake_start(
        &self,
        id: &str,
        kind: ServerKind,
    ) -> (
        mpsc::UnboundedReceiver<String>,
        broadcast::Sender<String>,
    ) {
        let (commands, typed) = mpsc::unbounded_channel();
        let (kill, _killed) = tokio::sync::watch::channel(false);
        let (console, _) = broadcast::channel(CONSOLE_BACKLOG);
        self.insert(
            id.to_string(),
            RunningServer {
                pid: 0,
                started_at: std::time::Instant::now(),
                generation: 0,
                kind,
                asked_to_stop: Arc::new(AtomicBool::new(false)),
                commands,
                kill,
                console: console.clone(),
            },
        );
        (typed, console)
    }

    pub fn stop_all(&self) {
        // First, so an exit that lands while this runs is never read as a
        // crash worth restarting.
        self.close();
        if let Ok(map) = self.inner.lock() {
            for running in map.values() {
                // No grace here: the app is going away, and a half-written
                // world is worse than a stopped one.
                running.ask_to_stop();
                running.force_kill();
            }
        }
    }
}

/// One server's start, held from [`ServerRegistry::claim_start`] until dropped.
///
/// A guard rather than a pair of calls, so a start that returns early — no
/// jar, no Java, an EULA nobody accepted — still frees the server.
pub struct StartClaim {
    starting: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Drop for StartClaim {
    fn drop(&mut self) {
        if let Ok(mut starting) = self.starting.lock() {
            starting.remove(&self.id);
        }
    }
}

/// Assemble the command line for a server.
///
/// Split out so it can be tested without starting a JVM, which is the only
/// part of this file a unit test can reach.
pub fn build_args(server: &Server, agent: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    let memory = server.settings.memory_mb.max(512);
    args.push(format!("-Xmx{memory}M"));
    args.push(format!("-Xms{}M", (memory / 2).max(512)));

    if let Some(extra) = &server.settings.java_args {
        args.extend(
            extra
                .split_whitespace()
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }

    // Before the launch kind, and that is not cosmetic: an argument file names
    // the main class, so the JVM stops reading its own options there. A
    // `-javaagent:` after `@argfile` is handed to the game as a program
    // argument, which it rejects, and the server does not start.
    if let Some(agent) = agent {
        args.push(agent.to_string());
    }

    match &server.launch {
        // Forge and NeoForge from 1.17 on: the argument file carries the module
        // path, the JVM flags and the main class, and there is no jar to name.
        // Our heap flags go before it, which is why they are pushed first.
        LaunchKind::ArgFile(file) => args.push(format!("@{file}")),
        LaunchKind::Jar => {
            args.push("-jar".into());
            args.push(server.jar.clone());
        }
    }

    // `nogui` suppresses the little Swing window a vanilla-derived server
    // opens. A proxy has none and rejects the argument.
    if !server.kind.is_proxy() {
        args.push("nogui".into());
    }
    args
}

/// A server that started, and the mods moved out of its Mods first.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStart {
    pub pid: u32,
    /// File names moved to Client mods, so the app can say so on whichever
    /// tab the admin pressed Start from.
    pub moved: Vec<String>,
}

/// A server that is running again, whoever or whatever started it.
///
/// The counterpart of `geode://server-exit`: without it, a server Geode
/// started again on its own would still look stopped to every screen watching,
/// and its console, its players list and its Start button would all be talking
/// about a process that is long gone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStarted {
    pub server_id: String,
    pub pid: u32,
    /// Which restart this was, or zero when a person pressed Start. What makes
    /// an uptime that went back to nothing explainable rather than a mystery.
    pub restarts: u32,
}

/// Start a server, exactly as pressing Start does.
///
/// The one way in, and deliberately the only one: everything a start owes the
/// rest of the app happens here — client-only mods moved out of the server's
/// way, the oldest log pruned, the Java runtime found, the console, the command
/// probe and the scheduled tasks wired to a live process — so a restart Geode
/// decides on for itself is the same act as a click rather than a second,
/// thinner one. The desktop app is not the only caller: `daemon::api` presses
/// the same Start, and a second copy of this would be a second set of answers
/// to drift apart.
///
/// `heap_ceiling_mb` is the host's limit on one server, not the server's own:
/// `None` starts it with whatever it asks for, which is what the app does.
pub async fn start(
    sink: &EventSink,
    paths: &Paths,
    http: &reqwest::Client,
    registry: &ServerRegistry,
    id: &str,
    heap_ceiling_mb: Option<u32>,
) -> Result<ServerStart> {
    start_again(sink, paths, http, registry, id, heap_ceiling_mb, 0).await
}

/// The one file a start cannot do without, by the way this server launches.
///
/// Modern Forge and NeoForge produce no runnable jar at all and are started
/// from an argument file, so the jar name alone cannot answer this.
fn launch_target(server: &super::Server) -> String {
    match &server.launch {
        LaunchKind::ArgFile(file) => file.clone(),
        LaunchKind::Jar => server.jar.clone(),
    }
}

/// Fetch the server's software again when it is not on disk.
///
/// A backup carries what only this machine has — the world, the
/// configuration, the player lists — and not the bytes Geode can ask for
/// again. So a restored server is one whose jar, libraries and extracted
/// version are all absent, and before this the start simply refused and told
/// the admin to reinstall by hand. Nothing else about the server is touched:
/// this is the same install that created it, run a second time.
async fn install_if_the_software_is_gone(
    sink: &EventSink,
    paths: &Paths,
    http: &reqwest::Client,
    server: &mut super::Server,
) -> Result<()> {
    // Only for a server Geode has installed before, which is what
    // `java_major` records — it is written by that install and by nothing
    // else. A server that was never installed has no software to put back
    // and is refused later in the start's own words; without this check,
    // every such server would reach for the network to find that out.
    let needed = launch_target(server);
    if needed.is_empty()
        || server.java_major.is_none()
        || tokio::fs::metadata(paths.server_run(&server.id).join(&needed))
            .await
            .is_ok()
    {
        return Ok(());
    }

    super::schedule::announce(
        sink,
        &server.id,
        format!("{needed} is not here, so Geode is fetching this server's software again"),
    );
    let progress = Progress::new(sink.clone(), format!("server-install:{}", server.id));
    let outcome = super::install::install_jar(http, paths, server, &progress).await;
    match outcome {
        Ok(()) => {
            server.save_if_present(paths).await?;
            progress.finish().await;
            Ok(())
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Refuse a start whose game port something already holds.
///
/// Minecraft's own answer to a taken port is to log `Failed to bind to port`
/// and exit — zero, on Paper — within a few seconds, which is why this is
/// worth asking first: an exit code cannot say what happened, and the admin
/// is left with a server that was running a moment ago and is not now.
///
/// [`properties::clashes`] answers a neighbouring question and cannot stand
/// in for this one: it compares this server's port against the other Geode
/// servers' `server.properties` files, so it is silent about another
/// launcher, a container, a `geoded` on the same box, or a Geode server whose
/// port was changed outside that file. Binding the socket asks the machine
/// instead, which is the only thing that knows.
///
/// This is a better error, not a reservation: the port can be taken in the
/// moment between here and the JVM. A proxy is skipped, because its port
/// lives in its own config rather than in `server.properties` and reading
/// 25565 for it would be a guess.
async fn refuse_if_the_port_is_taken(paths: &Paths, server: &super::Server) -> Result<()> {
    // A server with nothing installed is not going to start whatever the
    // port says, and "install one first" is the more useful of the two
    // sentences — so the cheaper refusal keeps its place in the queue.
    if server.kind.is_proxy() || launch_target(server).is_empty() {
        return Ok(());
    }
    let Some(address) = super::properties::game_socket(paths, &server.id).await else {
        return Ok(());
    };
    // `TcpListener::bind` sets `SO_REUSEADDR`, so a socket merely sitting in
    // TIME_WAIT from this server's own last run does not read as taken —
    // which matters, because stopping and starting again is when that is most
    // likely.
    if std::net::TcpListener::bind(address).is_ok() {
        return Ok(());
    }

    // Name the neighbour where Geode can, because "another Geode server" is
    // something the admin can act on and "something on this machine" is not.
    let port = address.port();
    let mine = super::properties::clashes(paths, &server.id, Some(port))
        .await
        .unwrap_or_default();
    Err(Error::msg(match mine.first() {
        Some(name) => format!(
            "port {port} is already in use, and {name} is set to the same port. \
             Give one of them a different port, or stop that server first"
        ),
        None => format!(
            "port {port} is already in use by something else on this machine. \
             Stop whatever is holding it, or give this server a different port"
        ),
    }))
}

/// The same start, carrying how many restarts it took to get here so the exit
/// after it knows whether to keep trying, and the ceiling it was started
/// under so a restart does not quietly ask for more memory than the host
/// allows.
async fn start_again(
    sink: &EventSink,
    paths: &Paths,
    http: &reqwest::Client,
    registry: &ServerRegistry,
    id: &str,
    heap_ceiling_mb: Option<u32>,
    restarts: u32,
) -> Result<ServerStart> {
    // Asked at the one place every start goes through, rather than only of the
    // restarter: whether it is a click, a route or a restart, a JVM that comes
    // up after Geode has begun going away has nothing left watching it and a
    // kill coming. Both closing paths set this before they stop anything.
    if registry.is_closing() {
        return Err(Error::msg(
            "Geode is stopping, so it is not starting anything new",
        ));
    }
    // One start of a server at a time, whoever asked for it: a click, a route
    // on the host, or the restarter below. Everything after this happens
    // before there is anything in the registry, so without it a second start
    // passes the check beneath and spawns a second JVM on the same world —
    // whose registry entry then replaces the first's, dropping the kill
    // channel the first's exit task is watching and so killing it outright,
    // and that task then deregisters the second, leaving a live server nothing
    // can stop.
    let _claim = registry
        .claim_start(id)
        .ok_or_else(|| Error::msg("that server is already starting"))?;
    if registry.is_running(id) {
        return Err(Error::msg("that server is already running"));
    }
    let mut server = super::load(paths, id).await?;

    // Before anything is moved or pruned, because a start that is going to be
    // refused should leave the server exactly as it found it.
    refuse_if_the_port_is_taken(paths, &server).await?;

    // And before the Java it needs is looked up, because installing is what
    // records which Java that is.
    install_if_the_software_is_gone(sink, paths, http, &mut server).await?;

    // While nothing is running, so the server never loads a mod that only
    // runs in players' games. Saved straight away, so the moves stay recorded
    // even if the start fails, and said in the console too, which is the only
    // place a start that fails later still says it.
    let moved = super::move_client_only_mods(paths, &mut server).await;
    if !moved.is_empty() {
        server.save_if_present(paths).await?;
        for name in &moved {
            super::schedule::announce(
                sink,
                id,
                format!("Moved {name} to Client mods: it only runs in players' games"),
            );
        }
    }

    // The server rotates its own log as it starts, so this is the moment the
    // oldest archive becomes one too many.
    if let Some(keep) = server.settings.keep_logs {
        let _ = super::logs::prune(paths, id, keep as usize).await;
    }

    // Lowered for this start only, and never saved: the ceiling is a fact
    // about the machine, so a server moved back to a roomier one should still
    // ask for what its owner configured.
    if let Some(ceiling) = heap_ceiling_mb {
        if server.settings.memory_mb > ceiling {
            super::schedule::announce(
                sink,
                id,
                format!(
                    "Starting with {ceiling} MB rather than {} MB: this host allows no more",
                    server.settings.memory_mb
                ),
            );
            server.settings.memory_mb = ceiling;
        }
    }

    let progress = Progress::new(sink.clone(), format!("server:{id}"));
    let java = match server.settings.java_path.clone() {
        Some(path) => PathBuf::from(path),
        None => match server.java_major {
            Some(major) => crate::java::ensure(http, paths, major, None, &progress).await?,
            // Nothing told us which Java this wants — a proxy, or a jar
            // installed before we started asking. The newest one is the best
            // guess available.
            None => crate::java::detect(paths)
                .await
                .into_iter()
                .next()
                .map(|j| j.path)
                .ok_or_else(|| Error::msg("no Java runtime was found on this system"))?,
        },
    };

    let pid = spawn(
        sink,
        paths,
        http,
        &server,
        java,
        registry,
        heap_ceiling_mb,
        restarts,
    )
    .await?;

    // Before the save, and not for tidiness: from here on there is a process,
    // and anything that went wrong afterwards must not leave a screen showing
    // a stopped server with a live JVM behind it.
    events::emit(
        &Some(sink.clone()),
        "geode://server-started",
        &ServerStarted {
            server_id: id.to_string(),
            pid,
            restarts,
        },
    );

    server.last_started = Some(chrono::Utc::now().timestamp());
    server.save_if_present(paths).await?;
    Ok(ServerStart { pid, moved })
}

/// Put a JVM behind a server and watch for it ending.
///
/// Private on purpose: reached only through [`start`], so nothing can put a
/// process behind a server by a path that skips what a start owes everything
/// else. Returns once the process exists; how it ended arrives as
/// `geode://server-exit`.
// The ceiling and the restart count travel together with everything else a
// start needs, because the exit this watches for has to start the server again
// on the same terms.
#[allow(clippy::too_many_arguments)]
async fn spawn(
    sink: &EventSink,
    paths: &Paths,
    http: &reqwest::Client,
    server: &Server,
    java: PathBuf,
    registry: &ServerRegistry,
    heap_ceiling_mb: Option<u32>,
    restarts: u32,
) -> Result<u32> {
    if matches!(server.launch, LaunchKind::Jar) && server.jar.is_empty() {
        return Err(Error::msg(
            "this server has no jar yet — install one before starting it",
        ));
    }
    if server.kind.needs_eula() && !server.eula_accepted {
        // Belt and braces: the UI asks first, but nothing should be able to
        // start a server that has not had the EULA accepted for it.
        return Err(Error::msg(
            "Minecraft's EULA has not been accepted for this server yet",
        ));
    }

    let run = paths.server_run(&server.id);
    tokio::fs::create_dir_all(&run).await?;
    // Whatever the launch actually depends on has to be there. By here the
    // start has already offered to fetch it again, so anything still missing
    // is a folder somebody emptied underneath a running Geode, and says so
    // rather than arriving as a wall of Java output.
    let needed = launch_target(server);
    if tokio::fs::metadata(run.join(&needed)).await.is_err() {
        return Err(Error::msg(format!(
            "{needed} is missing from the server folder — reinstall it"
        )));
    }

    // Best effort by design: `None` starts the server exactly as it would have
    // started before this existed — whether that is because the user turned it
    // off or because the agent could not be made ready.
    let agent = if server.settings.read_commands {
        super::probe::agent_arg(paths, &server.id).await
    } else {
        None
    };

    let mut child = tokio::process::Command::new(&java)
        .args(build_args(server, agent.as_deref()))
        .current_dir(&run)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The console lives here.
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("could not start Java at {}: {e}", java.display())))?;

    let pid = child.id().unwrap_or(0);
    let started_at = std::time::Instant::now();
    let (kill_tx, mut kill_rx) = tokio::sync::watch::channel(false);
    let (command_tx, mut command_rx) = mpsc::unbounded_channel::<String>();

    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            while let Some(line) = command_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });
    }

    let (console_tx, _) = broadcast::channel(CONSOLE_BACKLOG);

    for (pipe, stream) in [
        (child.stdout.take().map(Pipe::Out), "out"),
        (child.stderr.take().map(Pipe::Err), "err"),
    ] {
        let Some(pipe) = pipe else { continue };
        let sink = sink.clone();
        let id = server.id.clone();
        let stream = stream.to_string();
        let console = console_tx.clone();
        tokio::spawn(async move {
            match pipe {
                Pipe::Out(out) => emit_lines(sink, console, id, stream, BufReader::new(out)).await,
                Pipe::Err(err) => emit_lines(sink, console, id, stream, BufReader::new(err)).await,
            }
        });
    }

    let asked_to_stop = Arc::new(AtomicBool::new(false));
    registry.insert(
        server.id.clone(),
        RunningServer {
            pid,
            started_at,
            generation: 0,
            kind: server.kind,
            asked_to_stop: asked_to_stop.clone(),
            commands: command_tx,
            kill: kill_tx,
            console: console_tx,
        },
    );

    let sink_for_wait = sink.clone();
    let paths_for_wait = paths.clone();
    let http_for_wait = http.clone();
    let registry = registry.clone();
    let id = server.id.clone();
    tokio::spawn(async move {
        let status = tokio::select! {
            status = child.wait() => status,
            _ = kill_rx.changed() => {
                let _ = child.start_kill();
                child.wait().await
            }
        };

        registry.remove(&id);
        let (code, success) = match status {
            Ok(s) => (s.code(), s.success()),
            Err(_) => (None, false),
        };
        let ran = started_at.elapsed();

        // Read now rather than remembered from the start: somebody who turned
        // automatic restarts off while this was running meant it.
        let auto_restart = super::load(&paths_for_wait, &id)
            .await
            .map(|server| server.settings.auto_restart)
            .unwrap_or(false);

        let asked_for = asked_to_stop.load(Ordering::Relaxed);
        let after = after_exit(Exit {
            asked_for,
            closing: registry.is_closing(),
            auto_restart,
            ran,
            restarts,
        });

        events::emit(
            &Some(sink_for_wait.clone()),
            "geode://server-exit",
            &ServerExit::new(id.clone(), code, success, asked_for, ran, after),
        );

        carry_out(
            after,
            &sink_for_wait,
            &paths_for_wait,
            &http_for_wait,
            &registry,
            &id,
            heap_ceiling_mb,
        )
        .await;
    });

    Ok(pid)
}

/// Do what an exit was judged to deserve: say so in the console, wait out the
/// backoff, and start the server again through the one start path.
///
/// Returns a boxed future rather than being an `async fn`, which is what breaks
/// the cycle here: a start watches for the exit, and an exit starts again, so
/// the two futures' types each contain the other and neither can be worked out.
/// The box is the one point where the chain stops being a type.
fn carry_out<'a>(
    after: AfterExit,
    sink: &'a EventSink,
    paths: &'a Paths,
    http: &'a reqwest::Client,
    registry: &'a ServerRegistry,
    id: &'a str,
    heap_ceiling_mb: Option<u32>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let (attempt, wait) = match after {
            AfterExit::Nothing => return,
            AfterExit::GaveUp { attempts } => {
                super::schedule::announce(
                    sink,
                    id,
                    format!(
                        "it has been started again {attempts} times and has not stayed \
                         up for {} minutes, so Geode has stopped restarting it. Fix \
                         what is stopping it, then press Start.",
                        RESTART_HEALTHY.as_secs() / 60
                    ),
                );
                return;
            }
            AfterExit::Restart { attempt, wait } => (attempt, wait),
        };

        super::schedule::announce(
            sink,
            id,
            format!(
                "it exited without being asked to — starting it again in {} seconds \
                 (restart {attempt} of {RESTART_CEILING})",
                wait.as_secs()
            ),
        );
        tokio::time::sleep(wait).await;

        // Geode may have closed while we waited, or somebody may have pressed
        // Start themselves. Either way this restart is no longer wanted.
        if registry.is_closing() || registry.is_running(id) {
            return;
        }

        if let Err(e) =
            start_again(sink, paths, http, registry, id, heap_ceiling_mb, attempt).await
        {
            super::schedule::announce(sink, id, format!("could not start it again: {e}"));
        }
    })
}

/// The two pipe types differ only in their concrete reader, and a generic
/// function is cheaper than duplicating the loop.
enum Pipe {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

async fn emit_lines<R>(
    sink: EventSink,
    console: broadcast::Sender<String>,
    id: String,
    stream: String,
    reader: BufReader<R>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // No receivers is the ordinary case — nobody is waiting on a reply —
        // and an error here means exactly that, not a failure.
        let _ = console.send(line.clone());
        events::emit(
            &Some(sink.clone()),
            "geode://server-log",
            &ServerLine {
                server_id: id.clone(),
                line,
                stream: stream.clone(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::content::ContentKind;
    use crate::instance::jar::tests::fabric_jar;
    use crate::paths::Scratch;

    fn server(kind: ServerKind) -> Server {
        let mut s = Server::new("Test".into(), kind, "1.21.1".into());
        s.jar = "server.jar".into();
        s
    }

    /// The reinstall is only for software that is actually gone.
    ///
    /// It reaches the network, so firing it when the jar is sitting right
    /// there would turn every start into a download — and a start that is
    /// offline would stop working for a server that was perfectly installed.
    #[tokio::test]
    async fn a_server_whose_software_is_there_is_not_fetched_again() {
        let scratch = Scratch::new("server-present-jar");
        let paths = scratch.paths.clone();
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: EventSink = recorder.clone();
        let mut server = server(ServerKind::Paper);

        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(&run).await.unwrap();
        tokio::fs::write(run.join(&server.jar), "a jar").await.unwrap();

        install_if_the_software_is_gone(&sink, &paths, &crate::net::client(), &mut server)
            .await
            .expect("nothing to do");
        assert!(console_lines(&recorder).is_empty(), "it said something");
    }

    /// And a server that was never installed at all is left alone, so the
    /// start refuses it in its own words rather than trying to guess what it
    /// should have been.
    #[tokio::test]
    async fn a_server_that_was_never_installed_is_left_to_the_refusal() {
        let scratch = Scratch::new("server-no-jar");
        let paths = scratch.paths.clone();
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: EventSink = recorder.clone();
        let mut server = server(ServerKind::Paper);
        server.jar = String::new();

        install_if_the_software_is_gone(&sink, &paths, &crate::net::client(), &mut server)
            .await
            .expect("nothing to do");
        assert!(console_lines(&recorder).is_empty());
    }

    /// What a start cannot do without depends on how the server launches:
    /// modern Forge and NeoForge produce no runnable jar at all.
    #[test]
    fn what_a_start_needs_follows_how_the_server_launches() {
        let mut jar = server(ServerKind::Paper);
        jar.jar = "paper-26.3.jar".into();
        assert_eq!(launch_target(&jar), "paper-26.3.jar");

        let mut argfile = server(ServerKind::NeoForge);
        argfile.jar = "leftover.jar".into();
        argfile.launch = LaunchKind::ArgFile("libraries/neoforge/unix_args.txt".into());
        assert_eq!(launch_target(&argfile), "libraries/neoforge/unix_args.txt");
    }

    /// A port something else holds is refused before the JVM is spawned.
    ///
    /// Minecraft's own answer is `Failed to bind to port` and an exit — zero,
    /// on Paper — a few seconds later, which the app reported as a clean
    /// stop. Port-in-use is the most ordinary first-run failure on a real
    /// host, and it was the one Geode could say least about.
    #[tokio::test]
    async fn a_start_is_refused_when_something_already_holds_the_port() {
        let scratch = Scratch::new("server-port-probe");
        let paths = scratch.paths.clone();
        let server = server(ServerKind::Paper);

        // A real socket on a real free port, held for the length of the test.
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();

        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(&run).await.unwrap();
        tokio::fs::write(
            run.join("server.properties"),
            format!("server-ip=127.0.0.1\nserver-port={port}\n"),
        )
        .await
        .unwrap();

        let refused = refuse_if_the_port_is_taken(&paths, &server)
            .await
            .expect_err("a port something else holds must not reach the JVM");
        let said = refused.to_string();
        assert!(said.contains(&port.to_string()), "{said}");
        assert!(said.contains("already in use"), "{said}");

        // And a port nothing holds is no obstacle. A free port can only be
        // found by binding one and letting it go, so any other test in this
        // binary can take it in the gap — which is a steal rather than a
        // regression, and asserting on the first try fails about one run in
        // three. Trying a few ports makes a steal vanishingly unlikely
        // without hiding a refusal that is real, because every attempt has
        // to be refused before the test is.
        drop(held);
        let mut asked = None;
        for _ in 0..5 {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let free = probe.local_addr().unwrap().port();
            drop(probe);
            tokio::fs::write(
                run.join("server.properties"),
                format!("server-ip=127.0.0.1\nserver-port={free}\n"),
            )
            .await
            .unwrap();
            asked = Some(refuse_if_the_port_is_taken(&paths, &server).await);
            if matches!(asked, Some(Ok(()))) {
                break;
            }
        }
        asked
            .expect("the loop asks at least once")
            .expect("a free port is not an obstacle");
    }

    /// A proxy keeps its port in its own config, so `server.properties` says
    /// nothing about it and 25565 would be a guess — and a guess that refuses
    /// a start is worse than not asking.
    #[tokio::test]
    async fn a_proxy_is_not_probed() {
        let scratch = Scratch::new("server-port-proxy");
        let paths = scratch.paths.clone();
        let server = server(ServerKind::Velocity);

        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(&run).await.unwrap();
        tokio::fs::write(
            run.join("server.properties"),
            format!("server-ip=127.0.0.1\nserver-port={port}\n"),
        )
        .await
        .unwrap();

        refuse_if_the_port_is_taken(&paths, &server)
            .await
            .expect("a proxy is left alone");
    }

    #[test]
    fn a_normal_server_is_launched_without_its_gui() {
        let args = build_args(&server(ServerKind::Paper), None);
        // The tail is the whole point: `-jar <jar> nogui`, in that order.
        assert_eq!(args[args.len() - 3], "-jar");
        assert_eq!(args[args.len() - 2], "server.jar");
        assert_eq!(args[args.len() - 1], "nogui");
    }

    #[test]
    fn a_proxy_has_no_gui_to_suppress() {
        // Velocity rejects the argument rather than ignoring it.
        let args = build_args(&server(ServerKind::Velocity), None);
        assert!(!args.contains(&"nogui".to_string()));
        assert_eq!(args.last().unwrap(), "server.jar");
    }

    /// The ordering that stops a Forge server starting if it is wrong.
    ///
    /// An argument file names the main class, so the JVM stops reading its own
    /// options at `@file`. A `-javaagent:` after that is handed to the game
    /// instead, which rejects it — verified by breaking a real NeoForge server
    /// exactly that way.
    #[test]
    fn the_agent_goes_before_an_argument_file_not_after_it() {
        let mut s = server(ServerKind::NeoForge);
        s.launch = LaunchKind::ArgFile("libraries/neoforge/unix_args.txt".into());
        let args = build_args(&s, Some("-javaagent:/x/probe.jar=/y/tree.json"));

        let agent = args
            .iter()
            .position(|a| a.starts_with("-javaagent:"))
            .expect("the agent should be on the command line");
        let argfile = args
            .iter()
            .position(|a| a.starts_with('@'))
            .expect("an argfile launch should name one");
        assert!(
            agent < argfile,
            "the agent must precede the argument file: {args:?}"
        );
    }

    #[test]
    fn a_jar_launch_takes_the_agent_before_the_jar_too() {
        let args = build_args(&server(ServerKind::Paper), Some("-javaagent:/x/probe.jar"));
        let agent = args.iter().position(|a| a.starts_with("-javaagent:")).unwrap();
        let jar = args.iter().position(|a| a == "-jar").unwrap();
        assert!(agent < jar, "{args:?}");
    }

    /// Turning the setting off has to leave the command line as it was.
    #[test]
    fn no_agent_means_no_trace_of_one() {
        let args = build_args(&server(ServerKind::Paper), None);
        assert!(
            !args.iter().any(|a| a.contains("javaagent")),
            "{args:?}"
        );
    }

    #[test]
    fn memory_becomes_both_bounds() {
        let mut s = server(ServerKind::Paper);
        s.settings.memory_mb = 8192;
        let args = build_args(&s, None);
        assert!(args.contains(&"-Xmx8192M".to_string()));
        assert!(args.contains(&"-Xms4096M".to_string()));
    }

    #[test]
    fn a_tiny_heap_is_raised_to_something_that_starts() {
        let mut s = server(ServerKind::Paper);
        s.settings.memory_mb = 64;
        let args = build_args(&s, None);
        assert!(args.contains(&"-Xmx512M".to_string()));
        assert!(args.contains(&"-Xms512M".to_string()));
    }

    #[test]
    fn extra_java_args_land_before_the_jar() {
        let mut s = server(ServerKind::Paper);
        s.settings.java_args = Some("-XX:+UseG1GC -Dfoo=bar".into());
        let args = build_args(&s, None);
        let jar_at = args.iter().position(|a| a == "-jar").unwrap();
        let flag_at = args.iter().position(|a| a == "-XX:+UseG1GC").unwrap();
        assert!(flag_at < jar_at);
        assert!(args.contains(&"-Dfoo=bar".to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_console_command_is_typed_in_and_the_answer_comes_back() {
        let (registry, mut typed, console) = ServerRegistry::fake_running("survival");

        let answering = tokio::spawn(async move {
            let line = typed.recv().await.expect("the command should be written");
            // Exactly what typing it into the console would send.
            assert_eq!(line, "whitelist list\n");
            let _ = console.send("[12:00:00] [Server thread/INFO]: There are 2 whitelisted players: alice, bob".into());
        });

        let said = registry.ask("survival", "whitelist list").await;
        answering.await.unwrap();

        let said = said.expect("something is running");
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("alice, bob"), "{said:?}");
    }

    /// The distinction the whole screen rests on: a server that said nothing is
    /// not a server that did the thing.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_answers_nothing_reports_silence_rather_than_success() {
        let (registry, mut typed, _console) = ServerRegistry::fake_running("survival");

        let said = registry.ask("survival", "whitelist add ghost").await;

        assert_eq!(
            said,
            Some(Vec::new()),
            "a server that says nothing must come back as nothing said"
        );
        assert_eq!(
            typed.recv().await.as_deref(),
            Some("whitelist add ghost\n"),
            "the command should still have been sent"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn asking_a_server_that_is_not_running_is_its_own_answer() {
        let (registry, _typed, _console) = ServerRegistry::fake_running("survival");
        assert_eq!(registry.ask("creative", "list").await, None);
    }

    /// A reply arriving line by line is collected whole, not cut off at the
    /// first one — `banlist` answers with a count and then a line per ban.
    #[tokio::test(start_paused = true)]
    async fn a_reply_that_arrives_over_several_lines_is_collected_whole() {
        let (registry, mut typed, console) = ServerRegistry::fake_running("survival");

        let answering = tokio::spawn(async move {
            typed.recv().await.expect("the command should be written");
            for line in ["There are 2 bans:", "alice was banned by Server", "bob was banned by Server"] {
                let _ = console.send(line.to_string());
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });

        let said = registry.ask("survival", "banlist players").await.unwrap();
        answering.await.unwrap();

        assert_eq!(said.len(), 3, "{said:?}");
    }

    #[test]
    fn each_family_is_stopped_the_way_it_understands() {
        assert_eq!(stop_command(ServerKind::Paper), "stop");
        assert_eq!(stop_command(ServerKind::Vanilla), "stop");
        assert_eq!(stop_command(ServerKind::Mohist), "stop");
        // These two would not recognise `stop`.
        assert_eq!(stop_command(ServerKind::Velocity), "shutdown");
        assert_eq!(stop_command(ServerKind::Waterfall), "end");
    }

    /// Two starts of one server would move its mods and fetch its Java twice
    /// over each other and then both spawn a JVM on the same world. The second
    /// is turned away until the first has a process, or has given up.
    #[test]
    fn a_second_start_of_one_server_waits_for_the_first() {
        let registry = ServerRegistry::default();

        let first = registry.claim_start("survival").expect("nothing holds it");
        assert!(registry.claim_start("survival").is_none());

        // Another server is nobody else's business.
        let other = registry.claim_start("creative").expect("a different server");
        drop(other);
        assert!(registry.claim_start("creative").is_some());

        // And a start that gave up frees the server rather than wedging it.
        drop(first);
        assert!(registry.claim_start("survival").is_some());
    }

    /// The same guard from where it matters: [`start`] refuses while a start
    /// is under way, and refuses it *before* it has touched anything —
    /// a server with no jar otherwise gets as far as saying so, which is what
    /// this asserts once the claim is let go.
    #[tokio::test]
    async fn a_start_while_one_is_under_way_is_refused_before_anything_is_touched() {
        let scratch = crate::paths::Scratch::new("run-two-starts");
        let server = super::super::create(
            &scratch.paths,
            "Survival".into(),
            ServerKind::Paper,
            "1.21.1".into(),
        )
        .await
        .unwrap();

        let registry = ServerRegistry::default();
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: EventSink = recorder.clone();
        let http = crate::net::client();
        let start = || start(&sink, &scratch.paths, &http, &registry, &server.id, None);

        let claim = registry.claim_start(&server.id).expect("nothing holds it");
        let refused = start().await.expect_err("one start is already under way");
        assert!(
            refused.to_string().contains("already starting"),
            "it should say a start is under way: {refused}"
        );
        assert!(
            recorder.events().is_empty(),
            "nothing should have happened to the server: {:?}",
            recorder.topics()
        );

        drop(claim);
        let refused = start().await.expect_err("a server with no jar cannot start");
        assert!(
            refused.to_string().contains("install one before starting it"),
            "the claim should have been let go: {refused}"
        );
    }

    /// Nothing starts once Geode has begun going away, by any path: both
    /// [`ServerRegistry::stop_all`] and the host's own shutdown close the
    /// registry before they stop a thing, and a start that slipped past that
    /// would leave a JVM with nobody watching it and a kill on the way.
    #[tokio::test]
    async fn nothing_is_started_once_geode_has_begun_closing() {
        let scratch = crate::paths::Scratch::new("run-closing");
        let server = super::super::create(
            &scratch.paths,
            "Survival".into(),
            ServerKind::Paper,
            "1.21.1".into(),
        )
        .await
        .unwrap();

        let registry = ServerRegistry::default();
        registry.close();

        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: EventSink = recorder.clone();
        let refused = start(
            &sink,
            &scratch.paths,
            &crate::net::client(),
            &registry,
            &server.id,
            None,
        )
        .await
        .expect_err("Geode is on its way out");
        assert!(refused.to_string().contains("stopping"), "{refused}");
        assert!(
            recorder.events().is_empty(),
            "it should refuse before touching the server: {:?}",
            recorder.topics()
        );
    }

    /// [`start`] against a server with no software, which is how this is
    /// offline: the heap is decided before any Java is looked for, and
    /// the start then refuses at the missing jar without a process ever
    /// existing. What is asserted is what it did on the way there.
    async fn heap_announcements(asked_mb: u32, ceiling: Option<u32>) -> (Vec<String>, u32) {
        let scratch = crate::paths::Scratch::new("run-heap");
        let mut server = super::super::create(
            &scratch.paths,
            "Survival".into(),
            ServerKind::Paper,
            "1.21.1".into(),
        )
        .await
        .unwrap();
        server.settings.memory_mb = asked_mb;
        server.save(&scratch.paths).await.unwrap();

        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: EventSink = recorder.clone();
        let refused = start(
            &sink,
            &scratch.paths,
            &crate::net::client(),
            &ServerRegistry::default(),
            &server.id,
            ceiling,
        )
        .await;
        assert!(refused.is_err(), "a server with no jar must not start");

        let said = recorder
            .events()
            .into_iter()
            .filter_map(|(_, payload)| {
                payload
                    .get("line")
                    .and_then(|l| l.as_str())
                    .map(str::to_string)
            })
            .collect();
        let on_disk = super::super::load(&scratch.paths, &server.id)
            .await
            .unwrap()
            .settings
            .memory_mb;
        (said, on_disk)
    }

    /// A modpack configured for 16 GB on somebody's desktop must not take down
    /// a host with 8. Said in the console, because a server quietly given less
    /// memory than it asked for is a server nobody can explain the lag of.
    #[tokio::test]
    async fn a_hosts_heap_ceiling_lowers_what_a_server_asks_for_and_says_so() {
        let (said, on_disk) = heap_announcements(16384, Some(8192)).await;
        assert!(
            said.iter()
                .any(|line| line.contains("8192 MB rather than 16384 MB")),
            "it should say what it did and why: {said:?}"
        );
        // The ceiling is the machine's, not the server's: moved to a roomier
        // host, this server should still ask for what its owner configured.
        assert_eq!(on_disk, 16384, "the server's own setting was overwritten");
    }

    /// And a server content with less than the ceiling is left alone — no
    /// console line, because nothing happened to it.
    #[tokio::test]
    async fn a_server_asking_for_less_than_the_ceiling_is_not_touched() {
        let (said, on_disk) = heap_announcements(2048, Some(8192)).await;
        assert!(
            !said.iter().any(|line| line.contains("rather than")),
            "nothing was lowered, so nothing should be announced: {said:?}"
        );
        assert_eq!(on_disk, 2048);

        // And the app's case, which passes no ceiling at all.
        let (said, _) = heap_announcements(16384, None).await;
        assert!(!said.iter().any(|line| line.contains("rather than")), "{said:?}");
    }

    // -----------------------------------------------------------------
    // Restarting a server that exited on its own
    // -----------------------------------------------------------------

    /// A server that fell over with automatic restarts on: the case every
    /// other one here is a variation of.
    fn crashed() -> Exit {
        Exit {
            asked_for: false,
            closing: false,
            auto_restart: true,
            ran: std::time::Duration::from_secs(4),
            restarts: 0,
        }
    }

    /// A stop's grace timer must not reach past the process it was about.
    ///
    /// Stop a server, change a setting, start it again — an ordinary
    /// afternoon. The first stop's timer is still out there, and it wakes two
    /// minutes later to find the id taken. Keyed on the id alone it killed
    /// whatever held it, which is a live server with players on it, and
    /// `force_kill` marks the exit as asked for, so the auto-restarter
    /// correctly leaves it down. The admin's only clue was a toast reading
    /// "stopped".
    #[test]
    fn an_old_stops_grace_does_not_kill_the_server_that_replaced_it() {
        let registry = ServerRegistry::default();
        let _first = registry.fake_start("survival", ServerKind::Paper);
        let stopping = registry
            .generation("survival")
            .expect("the first run is registered");

        // It stopped when it was asked to, so its process deregisters...
        registry.remove("survival");
        // ...and the admin starts it again well inside the grace period.
        let _second = registry.fake_start("survival", ServerKind::Paper);

        // Now the first stop's timer fires.
        assert!(
            !registry.kill_run("survival", stopping),
            "the old stop killed the run that replaced it"
        );
        assert!(
            !asked_to_stop(&registry, "survival"),
            "the new run was marked as ending on purpose, so nothing would restart it"
        );

        // And the guard is not simply refusing everything: its own run dies.
        let current = registry.generation("survival").expect("still running");
        assert!(registry.kill_run("survival", current));
        assert!(asked_to_stop(&registry, "survival"));
    }

    /// Whether the registry has this server down as ending on purpose.
    fn asked_to_stop(registry: &ServerRegistry, id: &str) -> bool {
        registry
            .inner
            .lock()
            .unwrap()
            .get(id)
            .expect("something should be running")
            .asked_to_stop
            .load(Ordering::Relaxed)
    }

    /// Everything Geode said in a server's console during a test.
    fn console_lines(recorder: &crate::events::Recorder) -> Vec<String> {
        recorder
            .events()
            .into_iter()
            .filter(|(topic, _)| topic == "geode://server-log")
            .filter_map(|(_, payload)| payload["line"].as_str().map(str::to_string))
            .collect()
    }

    /// The distinction the whole feature rests on. A Stop, a Kill and a `stop`
    /// typed into the console all end with the process gone, and from the wait
    /// they are indistinguishable from a crash — so the asking is recorded
    /// rather than inferred from an exit code, which a server that was told to
    /// stop and a server that fell over both leave at zero as often as not.
    #[test]
    fn an_exit_somebody_asked_for_is_not_restarted() {
        assert_eq!(
            after_exit(Exit {
                asked_for: true,
                ..crashed()
            }),
            AfterExit::Nothing
        );
    }

    #[test]
    fn an_exit_nobody_asked_for_starts_the_server_again() {
        assert_eq!(
            after_exit(crashed()),
            AfterExit::Restart {
                attempt: 1,
                wait: RESTART_WAIT
            }
        );
    }

    #[test]
    fn a_server_with_automatic_restarts_off_is_left_where_it_fell() {
        assert_eq!(
            after_exit(Exit {
                auto_restart: false,
                ..crashed()
            }),
            AfterExit::Nothing
        );
    }

    /// Geode on its way out stops its servers itself, and must not then fight
    /// its own restarter for them.
    #[test]
    fn geode_closing_restarts_nothing() {
        assert_eq!(
            after_exit(Exit {
                closing: true,
                ..crashed()
            }),
            AfterExit::Nothing
        );
    }

    #[test]
    fn a_server_that_will_not_stay_up_waits_longer_each_try_and_is_then_given_up_on() {
        let waits: Vec<u64> = (0..RESTART_CEILING)
            .map(|done| match after_exit(Exit {
                restarts: done,
                ..crashed()
            }) {
                AfterExit::Restart { attempt, wait } => {
                    assert_eq!(attempt, done + 1);
                    wait.as_secs()
                }
                other => panic!("try {} should still be taken: {other:?}", done + 1),
            })
            .collect();
        assert_eq!(waits, vec![5, 10, 20, 40, 60]);

        assert_eq!(
            after_exit(Exit {
                restarts: RESTART_CEILING,
                ..crashed()
            }),
            AfterExit::GaveUp {
                attempts: RESTART_CEILING
            }
        );
    }

    /// One crash an evening is not a crash loop, however many evenings it has
    /// been happening for.
    #[test]
    fn a_server_that_stayed_up_gets_a_fresh_set_of_tries() {
        assert_eq!(
            after_exit(Exit {
                restarts: RESTART_CEILING,
                ran: RESTART_HEALTHY,
                ..crashed()
            }),
            AfterExit::Restart {
                attempt: 1,
                wait: RESTART_WAIT
            }
        );
    }

    #[tokio::test]
    async fn pressing_stop_marks_the_exit_as_one_somebody_asked_for() {
        let (registry, mut typed, _console) = ServerRegistry::fake_running("survival");
        assert!(!asked_to_stop(&registry, "survival"));

        assert!(registry.stop("survival"));

        assert!(asked_to_stop(&registry, "survival"));
        assert_eq!(typed.recv().await.as_deref(), Some("stop\n"));
    }

    #[test]
    fn killing_a_server_marks_the_exit_as_one_somebody_asked_for() {
        let (registry, _typed, _console) = ServerRegistry::fake_running("survival");
        assert!(registry.kill("survival"));
        assert!(asked_to_stop(&registry, "survival"));
    }

    #[test]
    fn the_stop_word_typed_into_the_console_is_an_exit_somebody_asked_for() {
        let (registry, _typed, _console) = ServerRegistry::fake_running("survival");

        assert!(registry.send("survival", "say back in five"));
        assert!(
            !asked_to_stop(&registry, "survival"),
            "an ordinary command is not a request to shut down"
        );

        // A slash because that is how people type it.
        assert!(registry.send("survival", "/stop"));
        assert!(asked_to_stop(&registry, "survival"));
    }

    /// The stop word is a fact about the software, not a list of every word
    /// that has ever stopped something: Velocity ignores `stop`, so a proxy
    /// that exits after being handed one did so for some other reason.
    #[test]
    fn a_proxy_is_only_asked_to_stop_in_the_word_it_knows() {
        let registry = ServerRegistry::default();
        let (_typed, _console) = registry.fake_start("lobby", ServerKind::Velocity);

        assert!(registry.send("lobby", "stop"));
        assert!(!asked_to_stop(&registry, "lobby"));

        assert!(registry.send("lobby", "shutdown"));
        assert!(asked_to_stop(&registry, "lobby"));
    }

    #[tokio::test]
    async fn closing_geode_asks_every_server_to_stop_in_its_own_word() {
        let registry = ServerRegistry::default();
        let (mut paper, _paper_console) = registry.fake_start("survival", ServerKind::Paper);
        let (mut proxy, _proxy_console) = registry.fake_start("lobby", ServerKind::Velocity);

        registry.stop_all();

        assert!(registry.is_closing());
        assert_eq!(paper.recv().await.as_deref(), Some("stop\n"));
        assert_eq!(proxy.recv().await.as_deref(), Some("shutdown\n"));
        assert!(asked_to_stop(&registry, "survival"));
        assert!(asked_to_stop(&registry, "lobby"));
    }

    /// What the frontend is told, because a screen that cannot tell a restart
    /// from a giving-up cannot explain either one.
    #[test]
    fn an_exit_says_what_geode_is_about_to_do_about_it() {
        let ran = std::time::Duration::from_secs(90);

        let quiet =
            ServerExit::new("survival".into(), Some(0), true, true, ran, AfterExit::Nothing);
        assert_eq!(quiet.restart_in_seconds, None);
        assert_eq!(quiet.restarts, 0);
        assert!(!quiet.gave_up);
        assert_eq!(quiet.ran_seconds, 90);

        let coming = ServerExit::new(
            "survival".into(),
            Some(1),
            false,
            false,
            ran,
            AfterExit::Restart {
                attempt: 3,
                wait: std::time::Duration::from_secs(20),
            },
        );
        assert_eq!(coming.restart_in_seconds, Some(20));
        assert_eq!(coming.restarts, 3);
        assert!(!coming.gave_up);

        let done = ServerExit::new(
            "survival".into(),
            None,
            false,
            false,
            ran,
            AfterExit::GaveUp { attempts: 5 },
        );
        assert_eq!(done.restart_in_seconds, None);
        assert_eq!(done.restarts, 5);
        assert!(done.gave_up);
    }

    #[tokio::test(start_paused = true)]
    async fn giving_up_on_a_server_says_so_in_its_own_console() {
        let scratch = Scratch::new("server-gave-up");
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: EventSink = recorder.clone();

        carry_out(
            AfterExit::GaveUp { attempts: 5 },
            &sink,
            &scratch.paths,
            &crate::net::client(),
            &ServerRegistry::default(),
            "survival",
            None,
        )
        .await;

        let said = console_lines(&recorder);
        assert!(
            said.iter().any(|line| line.contains("started again 5 times")
                && line.contains("stopped restarting it")),
            "a server nobody was told about is a server that went quiet: {said:?}"
        );
    }

    /// The restart is the same act as pressing Start, not a thinner one that
    /// only puts a process back.
    ///
    /// Proved by the client-only mod: moving it out of the server's way is
    /// something only the start path does, so finding it moved means the
    /// restart went through that path rather than round it. The start then
    /// fails on the missing jar, and says so where the admin is looking.
    #[tokio::test(start_paused = true)]
    async fn a_restart_goes_through_the_same_start_path_as_pressing_start() {
        let scratch = Scratch::new("server-restarts");
        let paths = scratch.paths.clone();

        let mut server = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
        // Named rather than found, so what this machine has installed cannot
        // change the answer. Nothing runs it: the start stops at the jar.
        server.settings.java_path = Some("/nonexistent/java".into());
        server.settings.auto_restart = true;
        server.save(&paths).await.unwrap();

        let mods = super::super::content_dir(&paths, &server.id, ContentKind::Mod).await;
        tokio::fs::create_dir_all(&mods).await.unwrap();
        tokio::fs::write(mods.join("minimap.jar"), fabric_jar("minimap", "client"))
            .await
            .unwrap();

        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: EventSink = recorder.clone();

        carry_out(
            AfterExit::Restart {
                attempt: 2,
                wait: RESTART_WAIT,
            },
            &sink,
            &paths,
            &crate::net::client(),
            &ServerRegistry::default(),
            &server.id,
            None,
        )
        .await;

        let said = console_lines(&recorder);
        assert!(
            said.iter()
                .any(|line| line.contains("starting it again in 5 seconds (restart 2 of 5)")),
            "{said:?}"
        );

        let client_mods =
            super::super::content_dir(&paths, &server.id, ContentKind::ClientMod).await;
        assert!(
            client_mods.join("minimap.jar").is_file(),
            "a restart has to move client-only mods out of the way, as a start does"
        );
        assert!(
            !mods.join("minimap.jar").exists(),
            "the mod should have moved, not been copied"
        );
        assert!(
            said.iter()
                .any(|line| line.contains("could not start it again") && line.contains("no jar")),
            "a restart that failed must say so in the console: {said:?}"
        );
    }

    /// And it is a start on the same terms: a host that lowers a server's heap
    /// to what the machine allows must go on doing it after the server has
    /// restarted itself, or the ceiling holds only until the first crash.
    #[tokio::test(start_paused = true)]
    async fn a_restart_is_started_under_the_hosts_heap_ceiling_too() {
        let scratch = Scratch::new("server-restart-ceiling");

        let mut server = Server::new("Survival".into(), ServerKind::Paper, "1.21.1".into());
        server.settings.java_path = Some("/nonexistent/java".into());
        server.settings.memory_mb = 16384;
        server.settings.auto_restart = true;
        server.save(&scratch.paths).await.unwrap();

        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: EventSink = recorder.clone();

        carry_out(
            AfterExit::Restart {
                attempt: 1,
                wait: RESTART_WAIT,
            },
            &sink,
            &scratch.paths,
            &crate::net::client(),
            &ServerRegistry::default(),
            &server.id,
            Some(8192),
        )
        .await;

        let said = console_lines(&recorder);
        assert!(
            said.iter()
                .any(|line| line.contains("8192 MB rather than 16384 MB")),
            "the restart should be held to the host's ceiling as a click is: {said:?}"
        );
    }
}

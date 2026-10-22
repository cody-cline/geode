//! Running a server and talking to its console.
//!
//! The important difference from launching the game: a server's stdin stays
//! open. That pipe *is* the console — it is how `stop` gets sent, how an
//! operator runs a command, and why stopping can be graceful instead of a kill
//! that loses whatever had not been saved.

use std::collections::HashMap;
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
        ran: std::time::Duration,
        after: AfterExit,
    ) -> Self {
        Self {
            server_id,
            code,
            success,
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
    /// Set once Geode is going away, and never cleared.
    ///
    /// A server Geode stopped on its way out asked for nothing and must stay
    /// stopped: without this, closing the app would race a restart that then
    /// has nobody left to watch it.
    closing: Arc<AtomicBool>,
}

impl ServerRegistry {
    pub fn insert(&self, id: String, running: RunningServer) {
        if let Ok(mut map) = self.inner.lock() {
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
        let asked = self
            .inner
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|s| s.ask_to_stop()))
            .unwrap_or(false);
        if !asked {
            return false;
        }
        let registry = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(STOP_GRACE).await;
            registry.kill(&id);
        });
        true
    }

    pub fn kill(&self, id: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|s| s.force_kill()))
            .is_some()
    }

    /// Whether Geode is shutting down, and so should start nothing again.
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
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
        self.closing.store(true, Ordering::Relaxed);
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
/// thinner one.
pub async fn start(
    sink: &EventSink,
    paths: &Paths,
    http: &reqwest::Client,
    registry: &ServerRegistry,
    id: &str,
) -> Result<ServerStart> {
    start_again(sink, paths, http, registry, id, 0).await
}

/// The same start, carrying how many restarts it took to get here so the exit
/// after it knows whether to keep trying.
async fn start_again(
    sink: &EventSink,
    paths: &Paths,
    http: &reqwest::Client,
    registry: &ServerRegistry,
    id: &str,
    restarts: u32,
) -> Result<ServerStart> {
    if registry.is_running(id) {
        return Err(Error::msg("that server is already running"));
    }
    let mut server = super::load(paths, id).await?;

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

    let pid = spawn(sink, paths, http, &server, java, registry, restarts).await?;

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
async fn spawn(
    sink: &EventSink,
    paths: &Paths,
    http: &reqwest::Client,
    server: &Server,
    java: PathBuf,
    registry: &ServerRegistry,
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
    // Whatever the launch actually depends on has to be there. An installed
    // server whose files were deleted underneath it should say so here rather
    // than as a wall of Java output.
    let needed = match &server.launch {
        LaunchKind::ArgFile(file) => file.clone(),
        LaunchKind::Jar => server.jar.clone(),
    };
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

        let after = after_exit(Exit {
            asked_for: asked_to_stop.load(Ordering::Relaxed),
            closing: registry.is_closing(),
            auto_restart,
            ran,
            restarts,
        });

        events::emit(
            &Some(sink_for_wait.clone()),
            "geode://server-exit",
            &ServerExit::new(id.clone(), code, success, ran, after),
        );

        carry_out(
            after,
            &sink_for_wait,
            &paths_for_wait,
            &http_for_wait,
            &registry,
            &id,
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

        if let Err(e) = start_again(sink, paths, http, registry, id, attempt).await {
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

        let quiet = ServerExit::new("survival".into(), Some(0), true, ran, AfterExit::Nothing);
        assert_eq!(quiet.restart_in_seconds, None);
        assert_eq!(quiet.restarts, 0);
        assert!(!quiet.gave_up);
        assert_eq!(quiet.ran_seconds, 90);

        let coming = ServerExit::new(
            "survival".into(),
            Some(1),
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
}

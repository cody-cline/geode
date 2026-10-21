//! Running a server and talking to its console.
//!
//! The important difference from launching the game: a server's stdin stays
//! open. That pipe *is* the console — it is how `stop` gets sent, how an
//! operator runs a command, and why stopping can be graceful instead of a kill
//! that loses whatever had not been saved.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use crate::events::{self, EventSink};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use crate::error::{Error, Result};
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

pub struct RunningServer {
    pub pid: u32,
    pub started_at: std::time::Instant,
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
        self.commands.send(format!("{line}\n")).is_ok()
    }

    pub fn force_kill(&self) {
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
    pub fn stop(&self, id: &str, kind: ServerKind) -> bool {
        if !self.send(id, stop_command(kind)) {
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
        let (typed, console) = registry.fake_start(id);
        (registry, typed, console)
    }

    /// The same, into a registry a test already holds: a server that starts
    /// part-way through something, the way a scheduled task or a click on Start
    /// does while a stopped server's files are being edited.
    #[cfg(test)]
    pub(crate) fn fake_start(
        &self,
        id: &str,
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
                commands,
                kill,
                console: console.clone(),
            },
        );
        (typed, console)
    }

    pub fn stop_all(&self) {
        if let Ok(map) = self.inner.lock() {
            for running in map.values() {
                // No grace here: the app is going away, and a half-written
                // world is worse than a stopped one.
                let _ = running.send("stop");
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

/// Start the server. Returns once the process exists; the caller learns how it
/// ended from the `geode://server-exit` event.
pub async fn start(
    sink: &EventSink,
    paths: &Paths,
    server: &Server,
    java: PathBuf,
    registry: ServerRegistry,
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

    registry.insert(
        server.id.clone(),
        RunningServer {
            pid,
            started_at,
            commands: command_tx,
            kill: kill_tx,
            console: console_tx,
        },
    );

    let sink_for_wait = sink.clone();
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

        events::emit(
            &Some(sink_for_wait.clone()),
            "geode://server-exit",
            &ServerExit {
                server_id: id,
                code,
                success,
                ran_seconds: started_at.elapsed().as_secs(),
            },
        );
    });

    Ok(pid)
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
}

//! Stopping, without losing a world.
//!
//! `docker stop` sends SIGTERM and then, after its own grace period, SIGKILL.
//! A SIGKILL reaches the JVMs as well, and a Minecraft server killed partway
//! through saving a chunk is how a world is corrupted — so the host's answer to
//! SIGTERM is the same thing the app does when a server is stopped from the
//! console: send that software its own shutdown command, and wait for the
//! process to go.
//!
//! The waiting is the point. [`STOP_LIMIT`] is longer than Docker's default
//! ten seconds on purpose, because saving a large world takes longer than ten
//! seconds; the container files set a matching `stop_grace_period`, and without
//! one Docker kills the host before this can finish. A server that will not
//! stop within the limit is killed here, where the exit is at least recorded,
//! rather than left for the SIGKILL.
//!
//! Only Linux is tested, like the rest of Geode. The signal this waits for on
//! other platforms is Ctrl-C alone.

use std::time::Duration;

use crate::server::run::ServerRegistry;

/// How long every running server together gets to stop before it is killed.
///
/// The same 120 seconds [`crate::server::run`] gives one server stopped from
/// the console, for the same reason: a world of any size takes a while to
/// write, and killing it halfway is the thing being avoided.
pub const STOP_LIMIT: Duration = Duration::from_secs(120);

/// How often the processes are counted while waiting.
const POLL: Duration = Duration::from_millis(250);

/// Resolve when the host is asked to stop.
///
/// SIGTERM is what a container stop sends; Ctrl-C is what a person running it
/// in a terminal sends. A SIGTERM handler that cannot be installed is not
/// worth refusing to run over, so that case waits for Ctrl-C alone.
pub async fn asked_to_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = term.recv() => return,
                _ = tokio::signal::ctrl_c() => return,
            }
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Ask every running server to stop, wait for the processes to go, and kill
/// whatever is still there when `limit` runs out.
///
/// Returns the servers that had to be killed, so the host can say so: that is
/// the one outcome an admin needs to know about, because it is the one where a
/// world may have been written mid-save.
pub async fn stop_running(registry: &ServerRegistry, limit: Duration) -> Vec<String> {
    // First, and before anything is asked to stop: a server that exits without
    // being asked to is started again (`server::run`), and a restart that
    // raced this would bring a server back while the host is trying to exit —
    // with the listener already stopped, nothing left watching it, and
    // Docker's SIGKILL due. Every exit from here on is one somebody asked for.
    registry.close();

    for id in registry.running_ids() {
        // Each software has its own word for it — Velocity takes `shutdown`,
        // BungeeCord's forks take `end` — and sending the wrong one stops
        // nothing at all. The registry knows which software each running
        // server is, so none of that is read back off disk, where it could
        // disagree with the process.
        registry.ask_to_stop(&id);
    }

    drain(
        limit,
        POLL,
        || registry.running_ids(),
        |id| {
            registry.kill(id);
        },
    )
    .await
}

/// Wait for `running` to empty, and `kill` whatever is left when `limit` runs
/// out. Returns what had to be killed.
///
/// Takes the registry as two closures rather than the registry itself, because
/// a `RunningServer` only exists behind a real JVM: no offline test can put one
/// in a registry, and this — how long it waits, and what it does when the wait
/// is over — is the part worth proving.
async fn drain(
    limit: Duration,
    poll: Duration,
    running: impl Fn() -> Vec<String>,
    kill: impl Fn(&str),
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let left = running();
        if left.is_empty() {
            return Vec::new();
        }
        if tokio::time::Instant::now() >= deadline {
            for id in &left {
                kill(id);
            }
            return left;
        }
        tokio::time::sleep(poll).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::server::ServerKind;

    const LIMIT: Duration = Duration::from_secs(120);
    const POLL: Duration = Duration::from_millis(250);

    /// Nothing is running, so the host must not sit out the grace period
    /// before it exits — a container stop that always took two minutes would
    /// be a bug of its own.
    #[tokio::test(start_paused = true)]
    async fn an_idle_host_stops_at_once() {
        let started = tokio::time::Instant::now();
        let killed = drain(LIMIT, POLL, Vec::new, |_| panic!("nothing to kill")).await;
        assert!(killed.is_empty());
        assert_eq!(started.elapsed(), Duration::ZERO, "it should not have waited");
    }

    /// The ordinary stop: the server takes a while to save and then goes, and
    /// nothing is killed.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_saves_and_exits_is_never_killed() {
        let started = tokio::time::Instant::now();
        let polls = AtomicUsize::new(0);
        let killed = drain(
            LIMIT,
            POLL,
            || {
                if polls.fetch_add(1, Ordering::Relaxed) < 20 {
                    vec!["survival".to_string()]
                } else {
                    Vec::new()
                }
            },
            |id| panic!("{id} was killed while it was still saving"),
        )
        .await;
        assert!(killed.is_empty());
        assert!(
            started.elapsed() < LIMIT,
            "it should have stopped waiting as soon as the process went, not after {:?}",
            started.elapsed()
        );
    }

    /// The host is going away, so an exit from here on is one it asked for.
    ///
    /// Without this, `server::run` would read every server the host just
    /// stopped as having fallen over and start it again — a JVM coming back
    /// with the listener already gone, nothing watching it, and Docker's
    /// SIGKILL moments away. Asked in the word its own software understands,
    /// which the registry knows and this no longer reads off disk.
    #[tokio::test(start_paused = true)]
    async fn stopping_the_host_leaves_nothing_for_the_restarter_to_bring_back() {
        let registry = ServerRegistry::default();
        let (mut paper, _paper_console) = registry.fake_start("survival", ServerKind::Paper);
        let (mut proxy, _proxy_console) = registry.fake_start("lobby", ServerKind::Velocity);

        // Nothing deregisters a server with no process behind it, so both are
        // still there when the grace period runs out and both are killed.
        let killed = stop_running(&registry, LIMIT).await;

        assert!(
            registry.is_closing(),
            "an exit during a shutdown must not be read as a crash"
        );
        assert_eq!(paper.recv().await.as_deref(), Some("stop\n"));
        assert_eq!(proxy.recv().await.as_deref(), Some("shutdown\n"));
        assert_eq!(killed.len(), 2, "{killed:?}");
    }

    /// A server that ignores its shutdown command is killed by the host when
    /// the grace period runs out — and not before, and not left for Docker's
    /// SIGKILL.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_will_not_stop_is_killed_when_the_grace_runs_out() {
        let started = tokio::time::Instant::now();
        let killed_ids: Mutex<Vec<String>> = Mutex::default();
        let left = drain(
            LIMIT,
            POLL,
            || vec!["survival".to_string(), "creative".to_string()],
            |id| killed_ids.lock().unwrap().push(id.to_string()),
        )
        .await;

        assert_eq!(left, vec!["survival", "creative"]);
        assert_eq!(*killed_ids.lock().unwrap(), vec!["survival", "creative"]);
        assert!(
            started.elapsed() >= LIMIT,
            "it was killed after {:?}, before the grace period was up",
            started.elapsed()
        );
    }
}

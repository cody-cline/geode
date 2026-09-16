//! Things a server does on its own: backups on a timer, and commands on one.
//!
//! One ticker for every server rather than a task each, because the work is a
//! map lookup a minute and a thousand idle tasks would be a thousand ways to
//! leak one.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use crate::events::{self, EventSink};

use crate::net::Progress;
use crate::paths::Paths;

use super::run::{ServerLine, ServerRegistry};

/// How often the ticker looks for work. A minute is the smallest interval
/// worth offering for this kind of job, so it is also the resolution.
const TICK: Duration = Duration::from_secs(60);

/// Long enough for a busy server to finish flushing chunks before the archive
/// starts reading them.
const FLUSH_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ScheduleAction {
    /// Archive the server's files.
    Backup,
    /// Type a line at the console, as an operator would.
    Command(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub action: ScheduleAction,
    /// How often it runs. Zero means never, which is how a half-filled form
    /// stays harmless.
    pub every_minutes: u32,
    pub enabled: bool,
    /// When it last ran, so restarting the launcher does not fire everything
    /// that was ever scheduled.
    #[serde(default)]
    pub last_run: Option<i64>,
}

/// Whether this schedule wants to run now.
///
/// Pure, because "is it time yet" is the whole of the logic and the only part
/// worth testing without waiting a minute to find out.
pub fn is_due(schedule: &Schedule, now: i64) -> bool {
    if !schedule.enabled || schedule.every_minutes == 0 {
        return false;
    }
    match schedule.last_run {
        // Never run. New schedules are stamped when they are created, so this
        // is a hand-edited file, and running it once is the forgiving answer.
        None => true,
        Some(last) => now.saturating_sub(last) >= i64::from(schedule.every_minutes) * 60,
    }
}

/// Say something in the server's own console, so scheduled work, and what
/// Geode does to a server as it starts, is visible where the user is already
/// looking rather than only in a log. Marked as Geode's, so it reads apart
/// from the server's own output.
pub(crate) fn announce(sink: &EventSink, server_id: &str, line: String) {
    events::emit(
        &Some(sink.clone()),
        "geode://server-log",
        &ServerLine {
            server_id: server_id.to_string(),
            line: format!("[Geode] {line}"),
            stream: "out".into(),
        },
    );
}

async fn run_backup(
    sink: &EventSink,
    paths: &Paths,
    servers: &ServerRegistry,
    server: &super::Server,
) {
    if servers.is_running(&server.id) {
        // Flush the world before archiving it. `save-off` would give a cleaner
        // snapshot, but if we die mid-backup the server is left never saving —
        // a slightly torn archive is the better failure.
        servers.send(&server.id, "save-all flush");
        tokio::time::sleep(FLUSH_GRACE).await;
    }

    let progress = Progress::silent(format!("schedule-backup:{}", server.id));
    match super::backup::create(paths, &server.id, &progress).await {
        Ok(info) => {
            announce(
                sink,
                &server.id,
                format!("scheduled backup taken: {}", info.name),
            );
            if let Some(keep) = server.settings.keep_backups {
                let _ = super::backup::prune(paths, &server.id, keep as usize).await;
            }
        }
        Err(e) => announce(sink, &server.id, format!("scheduled backup failed: {e}")),
    }
}

/// Run everything that is due, once.
pub async fn run_due(sink: &EventSink, paths: &Paths, servers: &ServerRegistry, now: i64) {
    let Ok(all) = super::list(paths).await else {
        return;
    };

    for server in all {
        let due: Vec<Schedule> = server
            .schedules
            .iter()
            .filter(|s| is_due(s, now))
            .cloned()
            .collect();
        if due.is_empty() {
            continue;
        }

        // Reloaded rather than reused: the listing may be a minute old by the
        // time we get here, and a backup should prune by the settings as they
        // are now.
        let Ok(current) = super::load(paths, &server.id).await else {
            continue;
        };

        for schedule in &due {
            match &schedule.action {
                ScheduleAction::Backup => run_backup(sink, paths, servers, &current).await,
                ScheduleAction::Command(command) => {
                    if servers.send(&current.id, command) {
                        announce(sink, &current.id, format!("scheduled: {command}"));
                    }
                    // A command for a stopped server is skipped rather than
                    // queued: it would arrive at a console nobody is at, and
                    // its moment has passed either way.
                }
            }
        }

        stamp(paths, &current.id, &due, now).await;
    }
}

/// Record that these schedules ran, and change nothing else.
///
/// The server is read again here rather than `current` being saved, because
/// `current` is as old as the backup was long. Saving it would undo whatever
/// changed in those minutes — a mod installed, a setting, the schedules
/// themselves.
async fn stamp(paths: &Paths, id: &str, ran: &[Schedule], now: i64) {
    // Gone while the work ran: saving would bring back a deleted server.
    let Ok(mut fresh) = super::load(paths, id).await else {
        return;
    };
    for schedule in ran {
        if let Some(stored) = fresh.schedules.iter_mut().find(|s| s.id == schedule.id) {
            stored.last_run = Some(now);
        }
    }
    let _ = fresh.save(paths).await;
}

/// The ticker that runs scheduled work, for the life of the app.
///
/// Returns the loop rather than spawning it. Whoever owns a runtime decides how
/// to run it — which is the point of this module not knowing whether it is
/// inside a desktop app or a daemon, and also avoids the trap that `tokio::spawn`
/// requires an active runtime context. Tauri's `setup` hook is not one, so
/// spawning here panicked with "there is no reactor running".
pub async fn ticker(sink: EventSink, paths: Paths, servers: ServerRegistry) {
    let mut ticker = tokio::time::interval(TICK);
    loop {
        ticker.tick().await;
        run_due(&sink, &paths, &servers, chrono::Utc::now().timestamp()).await;
    }
}

#[cfg(test)]
mod tests {

    /// Creating the ticker does no work and needs no runtime.
    ///
    /// The real guard against the bug this replaced — `tokio::spawn` inside the
    /// library, which panics with "there is no reactor running" when called
    /// from Tauri's `setup` hook — is the signature: `lib.rs` spawns what this
    /// returns, so a version that spawns for itself returns `()` and does not
    /// compile. This pins the other half, that merely building it is inert.
    #[test]
    fn the_ticker_can_be_built_without_a_runtime() {
        let sink: crate::events::EventSink =
            std::sync::Arc::new(crate::events::Recorder::default());
        let paths = Paths {
            root: std::env::temp_dir().join("geode-ticker-test"),
        };
        // Never awaited: creating it is the whole assertion. If this function
        // spawns rather than returns, this line panics.
        let _loop = ticker(sink, paths, ServerRegistry::default());
    }
    use super::*;
    use crate::paths::Scratch;

    fn every(minutes: u32, last_run: Option<i64>) -> Schedule {
        Schedule {
            id: "s1".into(),
            action: ScheduleAction::Backup,
            every_minutes: minutes,
            enabled: true,
            last_run,
        }
    }

    #[test]
    fn a_schedule_runs_once_its_interval_has_passed() {
        let now = 10_000;
        assert!(!is_due(&every(60, Some(now - 3599)), now));
        assert!(is_due(&every(60, Some(now - 3600)), now));
        assert!(is_due(&every(60, Some(now - 7200)), now));
    }

    #[test]
    fn a_disabled_schedule_never_runs() {
        let mut schedule = every(1, Some(0));
        schedule.enabled = false;
        assert!(!is_due(&schedule, 10_000));
    }

    #[test]
    fn an_interval_of_zero_never_runs() {
        // How a half-filled form stays harmless rather than firing every tick.
        assert!(!is_due(&every(0, Some(0)), 10_000));
        assert!(!is_due(&every(0, None), 10_000));
    }

    #[test]
    fn a_schedule_that_has_never_run_is_due() {
        assert!(is_due(&every(60, None), 10_000));
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_fire_everything() {
        // last_run in the future would underflow a plain subtraction.
        assert!(!is_due(&every(60, Some(20_000)), 10_000));
    }

    #[test]
    fn an_action_round_trips_through_json() {
        let command = ScheduleAction::Command("say hello".into());
        let text = serde_json::to_string(&command).unwrap();
        assert_eq!(
            serde_json::from_str::<ScheduleAction>(&text).unwrap(),
            command
        );

        let backup = serde_json::to_string(&ScheduleAction::Backup).unwrap();
        assert!(backup.contains("backup"), "{backup}");
    }

    /// Open a pipe's write end once something is reading it, without blocking
    /// forever if nothing ever does.
    #[cfg(unix)]
    async fn open_once_read(pipe: &std::path::Path) -> std::fs::File {
        use std::os::unix::fs::OpenOptionsExt;

        for _ in 0..2000 {
            // Refused with ENXIO while nobody has the pipe open to read.
            if let Ok(file) = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(pipe)
            {
                return file;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the backup never opened {}", pipe.display());
    }

    /// A scheduled backup lasts as long as the world is big, and the ticker
    /// used to save the copy of the server it loaded before starting one. That
    /// undid whatever changed in between, such as a note. A pipe in the run
    /// folder holds the backup at a known point, so the change lands inside it
    /// every time.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_backup_saves_that_it_ran_and_nothing_else() {
        use std::os::unix::ffi::OsStrExt;

        use crate::server::{Server, ServerKind};

        let scratch = Scratch::new("schedule-stale");
        let paths = scratch.paths.clone();

        let mut server = Server::new("Nightly".into(), ServerKind::Fabric, "1.21.1".into());
        server.schedules = vec![every(60, None)];
        server.save(&paths).await.unwrap();

        let run = paths.server_run(&server.id);
        std::fs::create_dir_all(&run).unwrap();
        let pipe = run.join("level.dat");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid, NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "no pipe was made");

        let now = 1_000_000;
        let job = tokio::spawn({
            let paths = paths.clone();
            async move {
                let sink: EventSink = std::sync::Arc::new(crate::events::Recorder::default());
                run_due(&sink, &paths, &ServerRegistry::default(), now).await;
            }
        });

        // The backup is reading the world now, so the ticker loaded the server
        // before this.
        let writer = open_once_read(&pipe).await;
        let mut admin = crate::server::load(&paths, &server.id).await.unwrap();
        admin.notes = "written during the backup".into();
        admin.save(&paths).await.unwrap();
        assert!(!job.is_finished(), "the backup finished before the change it should race");

        // Closing the pipe lets the backup finish.
        drop(writer);
        job.await.unwrap();

        let after = crate::server::load(&paths, &server.id).await.unwrap();
        assert_eq!(after.schedules[0].last_run, Some(now), "the run was not recorded");
        assert_eq!(after.notes, "written during the backup");
        assert_eq!(
            crate::server::backup::list(&paths, &server.id).await.unwrap().len(),
            1,
            "no backup was taken, so nothing was raced"
        );
    }
}

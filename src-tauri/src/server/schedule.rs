//! Things a server does on its own: backups on a timer, and commands on one.
//!
//! One ticker for every server rather than a task each, because the work is a
//! map lookup a minute and a thousand idle tasks would be a thousand ways to
//! leak one.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

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

/// Say something in the server's own console, so scheduled work is visible
/// where the user is already looking rather than only in a log.
fn announce(app: &AppHandle, server_id: &str, line: String) {
    let _ = app.emit(
        "geode://server-log",
        ServerLine {
            server_id: server_id.to_string(),
            line: format!("[Geode] {line}"),
            stream: "out".into(),
        },
    );
}

async fn run_backup(
    app: &AppHandle,
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
                app,
                &server.id,
                format!("scheduled backup taken: {}", info.name),
            );
            if let Some(keep) = server.settings.keep_backups {
                let _ = super::backup::prune(paths, &server.id, keep as usize).await;
            }
        }
        Err(e) => announce(app, &server.id, format!("scheduled backup failed: {e}")),
    }
}

/// Run everything that is due, once.
pub async fn run_due(app: &AppHandle, paths: &Paths, servers: &ServerRegistry, now: i64) {
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
        // time we get here, and saving a stale copy would undo an edit made
        // while a backup was running.
        let Ok(mut current) = super::load(paths, &server.id).await else {
            continue;
        };

        for schedule in due {
            match &schedule.action {
                ScheduleAction::Backup => run_backup(app, paths, servers, &current).await,
                ScheduleAction::Command(command) => {
                    if servers.send(&current.id, command) {
                        announce(app, &current.id, format!("scheduled: {command}"));
                    }
                    // A command for a stopped server is skipped rather than
                    // queued: it would arrive at a console nobody is at, and
                    // its moment has passed either way.
                }
            }
            if let Some(stored) = current.schedules.iter_mut().find(|s| s.id == schedule.id) {
                stored.last_run = Some(now);
            }
        }

        let _ = current.save(paths).await;
    }
}

/// Start the ticker for the life of the app.
pub fn spawn(app: AppHandle, paths: Paths, servers: ServerRegistry) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            run_due(&app, &paths, &servers, chrono::Utc::now().timestamp()).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

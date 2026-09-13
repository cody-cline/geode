//! An instance's log files.
//!
//! The game client writes the same `logs/latest.log` and rotated archives a
//! server does, just inside its own `.minecraft` folder — so the work is in
//! [`crate::logs`] and this only says where to look.
//!
//! Worth having on top of the live console: the console only holds what was
//! captured since the window opened, and the log that matters is usually the
//! one from the run that crashed, which nobody was watching.

use std::path::PathBuf;

use crate::error::Result;
use crate::logs;
pub use crate::logs::LogFile;
use crate::paths::Paths;

fn dir(paths: &Paths, id: &str) -> PathBuf {
    paths.game_dir(id).join("logs")
}

/// Where the game writes crash reports, for a diagnostic report to pick up.
pub fn crash_dir(paths: &Paths, id: &str) -> PathBuf {
    paths.game_dir(id).join("crash-reports")
}

pub async fn list(paths: &Paths, id: &str) -> Result<Vec<LogFile>> {
    logs::list(&dir(paths, id)).await
}

pub async fn read(paths: &Paths, id: &str, name: &str) -> Result<String> {
    logs::read(&dir(paths, id), name).await
}

pub async fn delete(paths: &Paths, id: &str, name: &str) -> Result<()> {
    logs::delete(&dir(paths, id), name).await
}

/// The path of one log, for reading a tail without loading the whole file.
pub fn path(paths: &Paths, id: &str, name: &str) -> Result<PathBuf> {
    logs::resolve(&dir(paths, id), name)
}

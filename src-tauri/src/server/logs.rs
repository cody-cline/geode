//! A server's log files.
//!
//! All of the work is in [`crate::logs`], which knows nothing about servers —
//! a server's logs and an instance's are the same files in different folders.
//! This says which folder.

use std::path::PathBuf;

use crate::error::Result;
use crate::logs;
pub use crate::logs::LogFile;
use crate::paths::Paths;

fn dir(paths: &Paths, id: &str) -> PathBuf {
    paths.server_run(id).join("logs")
}

/// Where this server writes crash reports, for a diagnostic report to pick up.
pub fn crash_dir(paths: &Paths, id: &str) -> PathBuf {
    paths.server_run(id).join("crash-reports")
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

pub async fn prune(paths: &Paths, id: &str, keep: usize) -> Result<usize> {
    logs::prune(&dir(paths, id), keep).await
}

/// The path of one log, for reading a tail without loading the whole file.
pub fn path(paths: &Paths, id: &str, name: &str) -> Result<PathBuf> {
    logs::resolve(&dir(paths, id), name)
}

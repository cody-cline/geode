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

/// The newest crash report, read for which mod broke.
///
/// The mods are passed to the reader rather than looked up by it: what a
/// report names is only worth offering a button for when there is a file here
/// answering to that name, and the file is also what the button turns off.
pub async fn crash(
    paths: &Paths,
    instance: &super::Instance,
    jars: &super::jar::MetaCache,
) -> Result<Option<crate::crash::Report>> {
    let Some(found) = crate::crash::newest(&crash_dir(paths, &instance.id)).await else {
        return Ok(None);
    };
    let known: Vec<crate::crash::Known> =
        super::content::list(paths, instance, super::content::ContentKind::Mod, jars)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|item| crate::crash::Known {
                file: item.file_name,
                name: item.name,
                enabled: item.enabled,
                mod_ids: item.mod_ids,
            })
            .collect();
    Ok(Some(crate::crash::analyse(found, &known)))
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

//! Backing a server up, and putting it back.
//!
//! A backup is a zip of the server's run directory — the world, the configs,
//! the plugins, everything it needs to be itself — kept beside the server
//! rather than inside it, so a backup is never swept into the next backup.
//!
//! Logs and crash reports are left out. They are written afresh every run,
//! they are the bulk of a long-lived server's folder, and nobody has ever
//! restored one.
//!
//! A running server is asked to write its world out first. Minecraft holds a
//! world open and only puts `level.dat` on disk at an autosave or a clean
//! shutdown, so a zip taken without that ask holds every region file and no
//! `level.dat` for any dimension — and restoring it hands the game a world it
//! cannot identify, which it answers by generating a new one with a new seed
//! beside the terrain that is already there. That ask therefore lives in
//! [`create`] rather than beside each caller, because a caller that forgets
//! it produces a backup that reads as fine until the day it is needed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::net::Progress;
use crate::paths::Paths;
use crate::server::run::ServerRegistry;

/// Written every run and never worth restoring.
const SKIP: &[&str] = &["logs", "crash-reports", "cache"];

/// How long to let a running server write its world out before archiving it.
const FLUSH_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    /// The file name, which is also its timestamp.
    pub name: String,
    pub size: u64,
    pub created_at: i64,
}

/// Timestamped so the name sorts chronologically and survives any filesystem —
/// colons are fine on Linux and forbidden on Windows.
fn new_name() -> String {
    format!("{}.zip", chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S"))
}

/// Reject anything that is not a plain backup file name.
fn resolve(paths: &Paths, id: &str, name: &str) -> Result<PathBuf> {
    let rel = Path::new(name);
    if !crate::paths::is_safe_relative(rel)
        || rel.components().count() != 1
        || !name.ends_with(".zip")
    {
        return Err(Error::msg("invalid backup name"));
    }
    Ok(paths.server_backups(id).join(rel))
}

/// Which backups to delete to keep only the newest `keep`.
///
/// Names are timestamps, so sorting them is sorting by age. Pure, because
/// deciding what to throw away is the part worth testing.
pub fn to_prune(names: &[String], keep: usize) -> Vec<String> {
    let mut sorted: Vec<String> = names.to_vec();
    sorted.sort();
    if sorted.len() <= keep {
        return Vec::new();
    }
    sorted.truncate(sorted.len() - keep);
    sorted
}

/// Ask a running server to put its world on disk, and give it a moment to.
///
/// `save-off` would give a cleaner snapshot, but a backup that died halfway
/// would leave the server never saving again — a slightly torn archive is the
/// better failure. Nothing happens for a stopped server: its world is already
/// written.
async fn flush_world(servers: &ServerRegistry, id: &str) {
    if servers.is_running(id) {
        servers.send(id, "save-all flush");
        tokio::time::sleep(FLUSH_GRACE).await;
    }
}

/// Zip the run directory into a new backup.
pub async fn create(
    paths: &Paths,
    servers: &ServerRegistry,
    id: &str,
    progress: &Progress,
) -> Result<BackupInfo> {
    let run = paths.server_run(id);
    if tokio::fs::metadata(&run).await.is_err() {
        return Err(Error::msg("this server has no files to back up yet"));
    }

    flush_world(servers, id).await;

    let dir = paths.server_backups(id);
    tokio::fs::create_dir_all(&dir).await?;
    let name = new_name();
    let dest = dir.join(&name);

    progress.stage("Backing up", 1).await;
    let dest_for_write = dest.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::create(&dest_for_write)?;
        let mut zip = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let mut stack = vec![run.clone()];
        while let Some(current) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(rel) = path.strip_prefix(&run) else {
                    continue;
                };
                let top = rel.components().next().and_then(|c| c.as_os_str().to_str());
                if top.is_some_and(|t| SKIP.contains(&t)) {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let name = rel.to_string_lossy().replace('\\', "/");
                zip.start_file(name, options)?;
                // A world file being written as we read it is a torn read, not
                // a failure: skip it rather than abandoning the whole backup.
                if let Ok(mut source) = std::fs::File::open(&path) {
                    let _ = std::io::copy(&mut source, &mut zip);
                }
            }
        }
        zip.finish()?;
        Ok(())
    })
    .await
    .map_err(|e| Error::msg(format!("backup panicked: {e}")))??;

    progress.advance(1, &name).await;
    let size = tokio::fs::metadata(&dest).await.map(|m| m.len()).unwrap_or(0);
    Ok(BackupInfo {
        name,
        size,
        created_at: chrono::Utc::now().timestamp(),
    })
}

pub async fn list(paths: &Paths, id: &str) -> Result<Vec<BackupInfo>> {
    let dir = paths.server_backups(id);
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".zip") {
            continue;
        }
        let meta = entry.metadata().await?;
        out.push(BackupInfo {
            name,
            size: meta.len(),
            created_at: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default(),
        });
    }

    // Newest first: the one you want is nearly always the last one taken.
    out.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(out)
}

pub async fn delete(paths: &Paths, id: &str, name: &str) -> Result<()> {
    tokio::fs::remove_file(resolve(paths, id, name)?).await?;
    Ok(())
}

/// Keep only the newest `keep` backups. Returns how many were removed.
pub async fn prune(paths: &Paths, id: &str, keep: usize) -> Result<usize> {
    let names: Vec<String> = list(paths, id).await?.into_iter().map(|b| b.name).collect();
    let doomed = to_prune(&names, keep);
    let count = doomed.len();
    for name in doomed {
        // One failure should not stop the rest being cleaned up.
        let _ = delete(paths, id, &name).await;
    }
    Ok(count)
}

/// Unpack a backup over the run directory.
///
/// Files the backup does not contain are left alone rather than the directory
/// being wiped first: a restore should not also be a delete of whatever was
/// added since, and anything genuinely unwanted is one folder away.
pub async fn restore(paths: &Paths, id: &str, name: &str) -> Result<()> {
    let archive = resolve(paths, id, name)?;
    if tokio::fs::metadata(&archive).await.is_err() {
        return Err(Error::msg(format!("no backup called {name}")));
    }
    let run = paths.server_run(id);
    tokio::fs::create_dir_all(&run).await?;

    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&archive)?;
        let mut zip = zip::ZipArchive::new(file)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let Some(rel) = entry.enclosed_name() else {
                continue;
            };
            if !crate::paths::is_safe_relative(&rel) {
                continue;
            }
            let dest = run.join(&rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&dest)?;
                continue;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&dest)?;
            std::io::copy(&mut entry, &mut out)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| Error::msg(format!("restore panicked: {e}")))??;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The reason the flush lives in `create` and not beside each caller.
    ///
    /// Minecraft keeps the world open and writes `level.dat` only at an
    /// autosave or a clean shutdown. A backup of a running server taken
    /// without asking it to save holds the region files and no `level.dat`,
    /// and restoring that gives the game a world it cannot identify: it makes
    /// a new one, with a new seed, beside terrain generated from the old. The
    /// scheduled backup asked; both Backup buttons called straight past it.
    #[tokio::test(start_paused = true)]
    async fn backing_up_a_running_server_asks_it_to_save_first() {
        use crate::server::ServerKind;

        let scratch = crate::paths::Scratch::new("backup-flush");
        let paths = scratch.paths.clone();
        tokio::fs::create_dir_all(paths.server_run("survival"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(paths.server_backups("survival"))
            .await
            .unwrap();

        let servers = ServerRegistry::default();
        let (mut typed, _console) = servers.fake_start("survival", ServerKind::Paper);

        create(&paths, &servers, "survival", &Progress::silent("test"))
            .await
            .expect("a backup of a server with a run directory");

        // `try_recv` rather than `recv`: the registry holds the sending half,
        // so an await here would hang for ever if the ask were dropped again,
        // and a regression test that hangs is one nobody reads the output of.
        assert_eq!(
            typed.try_recv().ok().as_deref(),
            Some("save-all flush\n"),
            "the world was archived without being written out first"
        );
    }

    /// And a stopped server is not sent anything: its world is already on
    /// disk, and there is no console to type at.
    #[tokio::test(start_paused = true)]
    async fn backing_up_a_stopped_server_sends_it_nothing() {
        let scratch = crate::paths::Scratch::new("backup-noflush");
        let paths = scratch.paths.clone();
        tokio::fs::create_dir_all(paths.server_run("survival"))
            .await
            .unwrap();

        let servers = ServerRegistry::default();
        create(&paths, &servers, "survival", &Progress::silent("test"))
            .await
            .expect("a backup of a stopped server");
    }

    #[test]
    fn a_name_is_a_sortable_timestamp() {
        let name = new_name();
        assert!(name.ends_with(".zip"));
        // Nothing a filesystem objects to, on any platform.
        assert!(!name.contains(':'));
        assert!(!name.contains('/'));
        assert_eq!(name.len(), "2026-09-12T04-15-00.zip".len());
    }

    #[test]
    fn pruning_keeps_the_newest() {
        let all = names(&[
            "2026-09-10T00-00-00.zip",
            "2026-09-12T00-00-00.zip",
            "2026-09-11T00-00-00.zip",
        ]);
        let doomed = to_prune(&all, 2);
        assert_eq!(doomed, vec!["2026-09-10T00-00-00.zip"]);
    }

    #[test]
    fn pruning_below_the_limit_deletes_nothing() {
        let all = names(&["2026-09-11T00-00-00.zip", "2026-09-12T00-00-00.zip"]);
        assert!(to_prune(&all, 2).is_empty());
        assert!(to_prune(&all, 5).is_empty());
        assert!(to_prune(&[], 3).is_empty());
    }

    #[test]
    fn keeping_none_deletes_everything() {
        let all = names(&["2026-09-11T00-00-00.zip", "2026-09-12T00-00-00.zip"]);
        assert_eq!(to_prune(&all, 0).len(), 2);
    }

    #[test]
    fn a_backup_name_cannot_escape_its_folder() {
        let paths = Paths {
            root: PathBuf::from("/tmp/geode-test"),
        };
        assert!(resolve(&paths, "s", "../../etc/passwd.zip").is_err());
        assert!(resolve(&paths, "s", "nested/name.zip").is_err());
        assert!(resolve(&paths, "s", "notazip.txt").is_err());
        assert!(resolve(&paths, "s", "2026-09-12T04-15-00.zip").is_ok());
    }

    #[test]
    fn the_noise_a_server_regenerates_is_not_backed_up() {
        // Logs are the bulk of a long-lived server and nobody restores them.
        assert!(SKIP.contains(&"logs"));
        assert!(SKIP.contains(&"crash-reports"));
        assert!(!SKIP.contains(&"world"));
        assert!(!SKIP.contains(&"plugins"));
    }
}

//! Backing a server up, and putting it back.
//!
//! A backup is a zip of what only this machine has — the world, the configs,
//! the plugin data, the player lists — kept beside the server rather than
//! inside it, so a backup is never swept into the next backup.
//!
//! Everything else is left out. Logs and crash reports are written afresh
//! every run and nobody has ever restored one; the server's own software is
//! something Geode can ask for again, and a start that finds it gone does
//! exactly that. See [`fetchable_again`]. A few caches the server rebuilds for
//! itself sit *inside* `plugins/`, which is otherwise entirely the admin's, and
//! are named one at a time in [`NESTED_CACHES`].
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

/// Directories that are only ever the installer's output.
///
/// `libraries/` is what the loader or the bundler downloaded, and `versions/`
/// is the game the bundler extracted out of the server jar sitting beside it.
/// Neither is ever the admin's own work, and both come back by asking the same
/// place they came from the first time — which [`crate::server::run`] does by
/// itself when a start finds the software gone.
const FETCHABLE: &[&str] = &["libraries", "versions"];

/// Caches a server rebuilds for itself, nested inside a directory that is
/// otherwise entirely the admin's.
///
/// `plugins/` cannot be judged by its top-level name in either direction: it
/// holds every plugin's own config and data, which has no upstream and is half
/// the reason a backup exists, and it also holds the single largest pile of
/// regenerable bytes on a modern Paper server. So this is a list of exact
/// paths, not a rule — each one is a tree some server software owns outright
/// and writes again when it is missing, and the only test applied is
/// CLAUDE.md's: does it exist nowhere else? A path nobody can answer that for
/// stays in the backup, because one that is too big is a nuisance and one
/// missing a plugin's data is a disaster.
///
/// A list rather than a pattern because every candidate name here is also a
/// plausible name for something irreplaceable. `tmp` is scratch in spark and
/// a work queue somewhere else; a dot-directory is Paper's cache here and
/// could be a plugin's own store next door. Only knowing the software tells
/// the two apart, so each entry carries the reasoning that got it added:
///
/// * `plugins/.paper-remapped` — Paper (1.20.5 and up) remaps every
///   Mojang-mapped plugin against the server jar on boot and caches the work
///   here: the remap classpath it builds and the reversed mappings it builds
///   it from. Both are derived from the server jar and the plugin jars, and
///   Paper writes them again on the next start — including after a restore,
///   where the jar itself came back via [`crate::server::run`]. Measured on a
///   real Paper 1.21.1 server this was 30.2 MB of a 31.8 MB backup whose
///   irreplaceable part was 1.1 MB. It is the whole reason this list exists.
/// * `plugins/spark/tmp-client` — spark's scratch directory: the
///   async-profiler native library it unpacks out of its own jar, and the
///   part-written `.jfr` of a profile that was running when the backup was
///   taken. Its sibling `plugins/spark/config.json` is the admin's and is
///   deliberately *not* excluded, which is why this entry names the
///   subdirectory and not the plugin.
///
/// Unconditional, unlike [`FETCHABLE`]: these come back from the server's own
/// software rather than from anything Geode fetches, so there is no record
/// Geode has to be holding for them to be safe to drop.
const NESTED_CACHES: &[&str] = &["plugins/.paper-remapped", "plugins/spark/tmp-client"];

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
///
/// Milliseconds, and not for precision. At one-second resolution two backups
/// taken in the same second were handed the same name, and [`create`] opened
/// it with `File::create`, which truncates — so the second quietly took the
/// first's place and reported a new backup. Two clicks of Backup did it, and
/// so did a scheduled backup landing in the same second as a manual one.
/// Milliseconds make that rare; [`claim`] makes what is left of it safe
/// rather than silent.
fn new_name() -> String {
    format!("{}.zip", chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S-%3f"))
}

/// Take a name nothing holds yet, and the open file to write the archive into.
///
/// `create_new` is the whole point: it is the only way to learn that a name
/// was taken that has no window between the looking and the writing. Whoever
/// wins the name gets the file; whoever loses tries the next millisecond.
///
/// One note for anyone reading [`to_prune`] next: a sub-second name sorts
/// *before* a bare-second one from that same second, because `-` precedes
/// `.`. That can only matter across the one second in which this format
/// changed, and the cost there is prune order rather than a lost archive.
fn claim(dir: &Path) -> Result<(String, PathBuf, std::fs::File)> {
    for _ in 0..100 {
        let name = new_name();
        let dest = dir.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
        {
            Ok(file) => return Ok((name, dest, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::msg(
        "Try this backup again in a moment. Every name for the last tenth of a \
         second is already taken, which means something is taking backups in a loop.",
    ))
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

/// Everything under this server's run directory that a backup leaves out.
///
/// A backup is for what only this machine has: the world, the configuration,
/// the plugin data, the player lists. The bytes Geode can ask for again are
/// not worth carrying — on a fresh Paper server they are 156 MB of 165 MB,
/// and at ten kept backups that is the same jar ten times over.
///
/// Derived from the server rather than listed by name, because what is
/// fetchable is a fact about how this one launches: its own jar or argument
/// file is the thing a start reinstalls, and `libraries/` and `versions/` are
/// what that install writes.
fn fetchable_again(server: Option<&crate::server::Server>) -> Vec<String> {
    let mut out: Vec<String> = SKIP.iter().map(|s| s.to_string()).collect();
    // Only once there is a record saying how to fetch it again. A folder with
    // no `server.json` beside it is backed up whole, because nothing can
    // promise to put any of it back.
    let Some(server) = server else {
        return out;
    };
    out.extend(FETCHABLE.iter().map(|s| s.to_string()));
    if !server.jar.is_empty() {
        out.push(server.jar.clone());
    }
    out
}

/// Whether this path, relative to the run directory, stays out of the backup.
///
/// Two kinds of match, and the difference between them is load-bearing.
/// `skip` — everything [`fetchable_again`] gathered — is matched against the
/// **top-level component only**, because every name in it is something the
/// server owns at the root of its run directory and the same name deeper down
/// belongs to whoever made it: a plugin's own `cache/` may be the only copy of
/// something. [`NESTED_CACHES`] is matched as a **whole path prefix**, by
/// component, so an entry excludes exactly the tree it names and nothing
/// beside it — `plugins/spark/tmp-client` takes the scratch and leaves
/// `plugins/spark/config.json` alone.
///
/// Matching directories too, not just files: excluding a directory here prunes
/// the walk, so a cache is never descended into.
fn excluded(rel: &Path, skip: &[String]) -> bool {
    let top = rel.components().next().and_then(|c| c.as_os_str().to_str());
    if top.is_some_and(|t| skip.iter().any(|s| s == t)) {
        return true;
    }
    NESTED_CACHES
        .iter()
        .any(|cache| rel.starts_with(Path::new(cache)))
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

    // Read rather than passed in: every caller already has the id and none of
    // them should have to know what makes a file worth keeping.
    let skip = fetchable_again(crate::server::load(paths, id).await.ok().as_ref());

    let dir = paths.server_backups(id);
    tokio::fs::create_dir_all(&dir).await?;
    // Claimed before any of the work, so a second backup racing this one is
    // given its own name instead of writing over this archive — see [`claim`].
    let (name, dest, file) = tokio::task::spawn_blocking(move || claim(&dir))
        .await
        .map_err(|e| Error::msg(format!("claiming a name for the backup panicked: {e}")))??;

    progress.stage("Backing up", 1).await;
    tokio::task::spawn_blocking(move || -> Result<()> {
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
                if excluded(rel, &skip) {
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
    let archive = resolve(paths, id, name)?;
    // Said the way `restore` says it. A second Delete — a double click, a
    // list another window took a minute ago, a retried DELETE against the
    // host — otherwise reached the screen as `No such file or directory (os
    // error 2)`, and an Error here serialises straight into the frontend's
    // copy.
    if tokio::fs::metadata(&archive).await.is_err() {
        return Err(Error::msg(format!("no backup called {name}")));
    }
    tokio::fs::remove_file(&archive).await?;
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

/// How many bytes a backup may unpack to, all together.
///
/// Mostly a backup is one Geode wrote, and then the bound is bookkeeping — but
/// not always: a backup can have arrived inside a transfer bundle from another
/// machine ([`crate::server::transfer`]), so the zip is somebody else's file
/// and `std::io::copy` with no ceiling is a stranger choosing how much of this
/// disk to use.
///
/// A ratio rather than a flat limit, for the reason `worlds::unpack_budget`
/// gives: what a backup holds is the world and the plugin data, there is no
/// honest absolute size for those, and a limit low enough to stop a bomb would
/// refuse a real server's. A region file is already-compressed chunk data, so
/// a real backup deflates a few times over; a zip of zeros deflates about a
/// thousand. The floor is so a small server's backup is not refused for being
/// small.
fn unpack_budget(archive_bytes: u64) -> u64 {
    const RATIO: u64 = 64;
    const FLOOR: u64 = 16 * 1024 * 1024;
    archive_bytes.saturating_mul(RATIO).max(FLOOR)
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
        use std::io::Read;

        let file = std::fs::File::open(&archive)?;
        let archive_bytes = file.metadata()?.len();
        let budget = unpack_budget(archive_bytes);
        let mut unpacked: u64 = 0;
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
            // Counted on the bytes as they inflate. The entry's header states
            // a size too, and a crafted zip can understate it.
            let left = budget - unpacked;
            let copied = std::io::copy(&mut (&mut entry).take(left + 1), &mut out)?;
            if copied > left {
                return Err(Error::msg(format!(
                    "that backup unpacks to more than {} MB from a {} MB file, which no real \
                     server's does — it was stopped before it could fill the disk",
                    budget / (1024 * 1024),
                    archive_bytes.div_ceil(1024 * 1024)
                )));
            }
            unpacked += copied;
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

    /// Error text is the frontend's copy, so a second Delete must read like a
    /// sentence rather than like errno.
    #[tokio::test]
    async fn deleting_a_backup_that_is_not_there_says_so_in_words() {
        let scratch = crate::paths::Scratch::new("backup-delete-twice");
        let paths = scratch.paths.clone();
        tokio::fs::create_dir_all(paths.server_backups("survival"))
            .await
            .unwrap();
        tokio::fs::write(
            paths.server_backups("survival").join("2026-01-01T00-00-00.zip"),
            b"not really a zip",
        )
        .await
        .unwrap();

        delete(&paths, "survival", "2026-01-01T00-00-00.zip")
            .await
            .expect("the first delete");
        let said = delete(&paths, "survival", "2026-01-01T00-00-00.zip")
            .await
            .expect_err("the second")
            .to_string();
        assert_eq!(said, "no backup called 2026-01-01T00-00-00.zip");
        assert!(!said.contains("os error"), "{said}");
    }

    /// A backup carries what only this machine has, and nothing Geode can ask
    /// for again.
    ///
    /// On a fresh Paper server the jar, `libraries/` and `versions/` are
    /// 156 MB of 165 MB, and the irreplaceable part — world, plugins,
    /// configs, player lists — is under 9 MB. At ten kept backups the old
    /// behaviour stored the same server jar ten times.
    #[tokio::test(start_paused = true)]
    async fn a_backup_leaves_out_what_geode_can_fetch_again() {
        use crate::server::ServerKind;

        let scratch = crate::paths::Scratch::new("backup-thin");
        let paths = scratch.paths.clone();
        let mut server =
            crate::server::Server::new("Survival".into(), ServerKind::Paper, "26.3".into());
        server.jar = "paper-26.3.jar".into();
        server.save(&paths).await.unwrap();

        let run = paths.server_run(&server.id);
        for dir in ["world", "plugins/MyPlugin", "libraries/net/x", "versions/26.3"] {
            tokio::fs::create_dir_all(run.join(dir)).await.unwrap();
        }
        for (rel, body) in [
            ("world/level.dat", "the world"),
            ("plugins/MyPlugin/config.yml", "the admin's own"),
            ("server.properties", "settings"),
            ("ops.json", "[]"),
            ("paper-26.3.jar", "fetchable"),
            ("libraries/net/x/x.jar", "fetchable"),
            ("versions/26.3/26.3.jar", "fetchable"),
        ] {
            tokio::fs::write(run.join(rel), body).await.unwrap();
        }

        let info = create(
            &paths,
            &crate::server::run::ServerRegistry::default(),
            &server.id,
            &Progress::silent("t"),
        )
        .await
        .expect("a backup");

        let held = entries(&paths.server_backups(&server.id).join(&info.name));
        for kept in [
            "world/level.dat",
            "plugins/MyPlugin/config.yml",
            "server.properties",
            "ops.json",
        ] {
            assert!(held.contains(&kept.to_string()), "{kept} was dropped: {held:?}");
        }
        for dropped in [
            "paper-26.3.jar",
            "libraries/net/x/x.jar",
            "versions/26.3/26.3.jar",
        ] {
            assert!(
                !held.contains(&dropped.to_string()),
                "{dropped} was carried: {held:?}"
            );
        }
    }

    /// With no record beside it there is nothing that could promise to fetch
    /// any of it again, so the folder is carried whole.
    #[tokio::test(start_paused = true)]
    async fn a_folder_with_no_server_record_is_backed_up_whole() {
        let scratch = crate::paths::Scratch::new("backup-orphan");
        let paths = scratch.paths.clone();
        let run = paths.server_run("stray");
        tokio::fs::create_dir_all(run.join("libraries")).await.unwrap();
        tokio::fs::write(run.join("libraries/x.jar"), "bytes").await.unwrap();

        let info = create(
            &paths,
            &crate::server::run::ServerRegistry::default(),
            "stray",
            &Progress::silent("t"),
        )
        .await
        .expect("a backup");
        let held = entries(&paths.server_backups("stray").join(&info.name));
        assert!(held.contains(&"libraries/x.jar".to_string()), "{held:?}");
    }

    /// A cache nested under `plugins/` is left out — and its neighbours are
    /// not, which is the half that is hard.
    ///
    /// Measured on a real Paper 1.21.1 server: a 22.9 MB zip holding about
    /// 1.1 MB of irreplaceable data. 30.7 MB of the 31.8 MB uncompressed was
    /// Paper's remap cache and spark's scratch — both under `plugins/`, where
    /// a filter that only ever looked at `rel.components().next()` could not
    /// see them. `plugins/` is also the thing a backup exists for, so the
    /// assertions that matter are the ones below the dropped list.
    #[tokio::test(start_paused = true)]
    async fn a_cache_nested_under_plugins_goes_and_its_neighbours_stay() {
        use crate::server::ServerKind;

        let scratch = crate::paths::Scratch::new("backup-nested-cache");
        let paths = scratch.paths.clone();
        let mut server =
            crate::server::Server::new("Survival".into(), ServerKind::Paper, "1.21.1".into());
        server.jar = "paper-1.21.1.jar".into();
        server.save(&paths).await.unwrap();

        let run = paths.server_run(&server.id);
        let files = [
            // Regenerable, and 96% of the bytes.
            "plugins/.paper-remapped/remap-classpath/a1b2.jar",
            "plugins/.paper-remapped/mappings/reversed/a1b2.tiny",
            "plugins/spark/tmp-client/libasyncProfiler.so.tmp",
            "plugins/spark/tmp-client/profile-data.jfr.tmp",
            // Exists nowhere else, and sits in the same tree.
            "plugins/spark/config.json",
            "plugins/MyPlugin/config.yml",
            "plugins/MyPlugin/data/players.db",
            // `cache` is in SKIP, but only as a top-level name: this one is
            // some plugin's, and nothing promises to put it back.
            "plugins/MyPlugin/cache/tiles.dat",
            "world/level.dat",
            "server.properties",
        ];
        for rel in files {
            let path = run.join(rel);
            tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
            tokio::fs::write(&path, b"bytes").await.unwrap();
        }

        let info = create(
            &paths,
            &crate::server::run::ServerRegistry::default(),
            &server.id,
            &Progress::silent("t"),
        )
        .await
        .expect("a backup");

        let held = entries(&paths.server_backups(&server.id).join(&info.name));
        for dropped in [
            "plugins/.paper-remapped/remap-classpath/a1b2.jar",
            "plugins/.paper-remapped/mappings/reversed/a1b2.tiny",
            "plugins/spark/tmp-client/libasyncProfiler.so.tmp",
            "plugins/spark/tmp-client/profile-data.jfr.tmp",
        ] {
            assert!(
                !held.contains(&dropped.to_string()),
                "{dropped} was carried: {held:?}"
            );
        }
        for kept in [
            "plugins/spark/config.json",
            "plugins/MyPlugin/config.yml",
            "plugins/MyPlugin/data/players.db",
            "plugins/MyPlugin/cache/tiles.dat",
            "world/level.dat",
            "server.properties",
        ] {
            assert!(held.contains(&kept.to_string()), "{kept} was dropped: {held:?}");
        }
    }

    /// What each of the two filters may and may not match.
    ///
    /// The top-level names and the nested paths are matched differently on
    /// purpose, and both directions have teeth: matching the top-level names
    /// at any depth would throw away a plugin's own `cache/`, and matching the
    /// nested paths by string rather than by component would catch a sibling
    /// whose name merely begins the same way.
    #[test]
    fn a_nested_cache_is_matched_by_whole_path_and_a_top_level_name_is_not() {
        let skip = names(&["logs", "cache", "libraries", "paper-1.21.1.jar"]);
        let out = |p: &str| excluded(Path::new(p), &skip);

        assert!(out("logs/latest.log"));
        assert!(out("cache/mojang_1.21.1.jar"));
        assert!(out("libraries/net/x/x.jar"));
        assert!(out("paper-1.21.1.jar"));

        // The same names deeper down belong to whoever made them.
        assert!(!out("plugins/MyPlugin/cache/tiles.dat"));
        assert!(!out("plugins/MyPlugin/logs/audit.log"));
        assert!(!out("world/datapacks/libraries/pack.mcmeta"));

        // A nested cache, and the tree above it that must survive.
        assert!(out("plugins/.paper-remapped"));
        assert!(out("plugins/.paper-remapped/mappings/reversed/a.tiny"));
        assert!(out("plugins/spark/tmp-client/libasyncProfiler.so.tmp"));
        assert!(!out("plugins"));
        assert!(!out("plugins/spark"));
        assert!(!out("plugins/spark/config.json"));

        // By component, not by string, and anchored at the top rather than
        // found anywhere.
        assert!(!out("plugins/.paper-remapped-notes.txt"));
        assert!(!out("world/plugins/.paper-remapped/a1b2.jar"));
    }

    /// A restore is held to what the archive unpacks to, measured against its
    /// own size.
    ///
    /// Mostly these are archives Geode wrote and this is bookkeeping, but not
    /// always: a backup can arrive inside a transfer bundle from another
    /// machine, and `std::io::copy` with no ceiling let whoever built it choose
    /// how much of this disk to use. A ratio rather than a flat limit, because
    /// what a backup holds is a world and there is no honest absolute size for
    /// one — see [`unpack_budget`].
    #[tokio::test]
    async fn restoring_a_backup_that_unpacks_past_its_budget_is_stopped() {
        use std::io::Write;

        let scratch = crate::paths::Scratch::new("backup-bomb");
        let paths = scratch.paths.clone();
        let backups = paths.server_backups("survival");
        tokio::fs::create_dir_all(&backups).await.unwrap();

        let archive = backups.join("2026-01-01T00-00-00.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("world/region/r.0.0.mca", options).unwrap();
            // Over the floor, which is what a near-empty archive is held to.
            zip.write_all(&vec![0u8; unpack_budget(0) as usize + 1]).unwrap();
            zip.finish().unwrap();
        }
        let on_disk = tokio::fs::metadata(&archive).await.unwrap().len();
        assert!(
            on_disk * 64 < unpack_budget(0),
            "the control is wrong: the archive is {on_disk} bytes, so the ratio alone \
             would have allowed what it unpacks to"
        );

        let err = restore(&paths, "survival", "2026-01-01T00-00-00.zip")
            .await
            .expect_err("a backup that unpacks past its budget is refused");
        assert!(
            err.to_string().contains("it could fill the disk"),
            "refused for some other reason: {err}"
        );
    }

    /// Everything inside a backup, by entry name.
    fn entries(archive: &std::path::Path) -> Vec<String> {
        let file = std::fs::File::open(archive).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect()
    }

    #[test]
    fn a_name_is_a_sortable_timestamp() {
        let name = new_name();
        assert!(name.ends_with(".zip"));
        // Nothing a filesystem objects to, on any platform.
        assert!(!name.contains(':'));
        assert!(!name.contains('/'));
        assert_eq!(name.len(), "2026-09-12T04-15-00-000.zip".len());
        // Fixed width, so sorting the names is still sorting by age — which is
        // the whole of how `to_prune` decides what to throw away.
        assert_eq!(new_name().len(), name.len());
    }

    /// Two backups a moment apart must be two backups.
    ///
    /// At one-second resolution they were given one name and `File::create`
    /// truncated, so the second overwrote the first and returned a
    /// `BackupInfo` as though a new archive existed. A live run of two
    /// back-to-back backups is what caught it.
    ///
    /// Claiming the file rather than checking for it first is the part worth
    /// pinning: the names here are generated as fast as the machine can, which
    /// is the only way to actually land two in one millisecond, and whichever
    /// loses has to be handed a different name rather than the same one.
    #[test]
    fn two_backups_in_the_same_moment_cannot_take_each_others_name() {
        let scratch = crate::paths::Scratch::new("backup-claim");
        let dir = scratch.paths.server_backups("survival");
        std::fs::create_dir_all(&dir).unwrap();

        let mut seen = std::collections::BTreeSet::new();
        let mut handed_out = Vec::new();
        for _ in 0..24 {
            let (name, dest, file) = claim(&dir).expect("a name was available");
            drop(file);
            assert!(
                seen.insert(name.clone()),
                "{name} was handed out twice, so one archive would overwrite the other"
            );
            assert!(dest.exists(), "the claim should have created {name}");
            handed_out.push(name);
        }

        // Kept in the order they were taken, then sorted: the two have to
        // agree, or `to_prune` would throw away the wrong archive.
        let mut sorted = handed_out.clone();
        sorted.sort();
        assert_eq!(
            handed_out, sorted,
            "names handed out in time order must also sort in that order"
        );
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

        // And no nested entry may swallow a whole directory the admin's own
        // data lives in. Every one names a tree *inside* one, which is the
        // difference between dropping Paper's remap cache and dropping every
        // plugin's config with it.
        for cache in NESTED_CACHES {
            assert!(
                cache.contains('/'),
                "{cache} excludes a whole top-level directory"
            );
        }
    }
}

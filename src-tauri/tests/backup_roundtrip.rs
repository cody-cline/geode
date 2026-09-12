//! A backup has to actually contain the server, and actually put it back.
//!
//! The unit tests cover the decisions — what to prune, what names are legal,
//! what to leave out. None of them writes a zip. This does, against a real
//! directory tree, because the failure that matters is a backup that restores
//! to something other than what was backed up.
//!
//! No network, so these run in a normal `cargo test`.

use std::path::PathBuf;

use geode_lib::net::Progress;
use geode_lib::paths::Paths;
use geode_lib::server::backup;

/// A throwaway data directory per test; they run in parallel in one process.
fn scratch(name: &str) -> Paths {
    let dir = std::env::temp_dir().join(format!(
        "geode-backup-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    Paths { root: dir }
}

async fn write(path: PathBuf, contents: &[u8]) {
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .expect("parent");
    tokio::fs::write(&path, contents).await.expect("write");
}

/// A server folder with the shapes a real one has: a world, configs, a plugin,
/// and logs that should not travel.
async fn populate(paths: &Paths, id: &str) {
    let run = paths.server_run(id);
    write(run.join("server.properties"), b"motd=Hello\n").await;
    write(run.join("world/level.dat"), b"LEVEL").await;
    write(run.join("world/region/r.0.0.mca"), b"REGION").await;
    write(run.join("plugins/thing.jar"), b"JAR").await;
    write(run.join("logs/latest.log"), b"noise").await;
    write(run.join("crash-reports/crash.txt"), b"boom").await;
}

#[tokio::test]
async fn a_backup_restores_what_it_backed_up() {
    let paths = scratch("roundtrip");
    populate(&paths, "srv").await;

    let info = backup::create(&paths, "srv", &Progress::silent("test"))
        .await
        .expect("backup should be taken");
    assert!(info.size > 0, "an empty archive is not a backup");
    assert!(info.name.ends_with(".zip"));

    // Break the server the way a bad afternoon would.
    let run = paths.server_run("srv");
    tokio::fs::remove_file(run.join("world/level.dat"))
        .await
        .expect("delete");
    tokio::fs::write(run.join("server.properties"), b"motd=Ruined\n")
        .await
        .expect("overwrite");

    backup::restore(&paths, "srv", &info.name)
        .await
        .expect("restore should succeed");

    assert_eq!(
        tokio::fs::read(run.join("world/level.dat")).await.unwrap(),
        b"LEVEL",
        "a deleted world file should come back"
    );
    assert_eq!(
        tokio::fs::read(run.join("server.properties")).await.unwrap(),
        b"motd=Hello\n",
        "an overwritten config should be put back"
    );
    assert_eq!(
        tokio::fs::read(run.join("world/region/r.0.0.mca"))
            .await
            .unwrap(),
        b"REGION",
        "nested files should survive the round trip"
    );

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

#[tokio::test]
async fn logs_are_not_carried_along() {
    let paths = scratch("excludes");
    populate(&paths, "srv").await;

    let info = backup::create(&paths, "srv", &Progress::silent("test"))
        .await
        .expect("backup");

    // Delete them, restore, and see whether they come back. If the archive
    // held them, they would.
    let run = paths.server_run("srv");
    tokio::fs::remove_dir_all(run.join("logs")).await.expect("rm");
    tokio::fs::remove_dir_all(run.join("crash-reports"))
        .await
        .expect("rm");

    backup::restore(&paths, "srv", &info.name)
        .await
        .expect("restore");

    assert!(
        tokio::fs::metadata(run.join("logs/latest.log")).await.is_err(),
        "logs should not have been in the archive"
    );
    assert!(
        tokio::fs::metadata(run.join("crash-reports/crash.txt"))
            .await
            .is_err(),
        "crash reports should not have been in the archive"
    );
    // The things that matter still came back.
    assert!(tokio::fs::metadata(run.join("plugins/thing.jar")).await.is_ok());

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

#[tokio::test]
async fn restoring_puts_back_rather_than_rewinds() {
    let paths = scratch("additive");
    populate(&paths, "srv").await;

    let info = backup::create(&paths, "srv", &Progress::silent("test"))
        .await
        .expect("backup");

    // Something added after the backup was taken.
    let run = paths.server_run("srv");
    write(run.join("plugins/installed-today.jar"), b"NEW").await;

    backup::restore(&paths, "srv", &info.name)
        .await
        .expect("restore");

    assert!(
        tokio::fs::metadata(run.join("plugins/installed-today.jar"))
            .await
            .is_ok(),
        "a restore should not delete what it does not know about"
    );

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

#[tokio::test]
async fn backups_are_listed_newest_first_and_pruned_to_the_limit() {
    let paths = scratch("retention");
    populate(&paths, "srv").await;

    // Names carry a one-second resolution timestamp, so they are written
    // directly rather than raced for.
    let dir = paths.server_backups("srv");
    for name in [
        "2026-09-10T00-00-00.zip",
        "2026-09-11T00-00-00.zip",
        "2026-09-12T00-00-00.zip",
    ] {
        write(dir.join(name), b"PK").await;
    }

    let listed = backup::list(&paths, "srv").await.expect("list");
    assert_eq!(listed.len(), 3);
    assert_eq!(
        listed[0].name, "2026-09-12T00-00-00.zip",
        "the newest should be first, since it is the one you reach for"
    );

    let removed = backup::prune(&paths, "srv", 2).await.expect("prune");
    assert_eq!(removed, 1);

    let left: Vec<String> = backup::list(&paths, "srv")
        .await
        .expect("list")
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(
        left,
        vec!["2026-09-12T00-00-00.zip", "2026-09-11T00-00-00.zip"]
    );

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

#[tokio::test]
async fn a_server_with_no_files_cannot_be_backed_up() {
    let paths = scratch("empty");
    let error = backup::create(&paths, "never-started", &Progress::silent("test"))
        .await
        .expect_err("there is nothing to archive");
    assert!(error.to_string().contains("no files"), "{error}");

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

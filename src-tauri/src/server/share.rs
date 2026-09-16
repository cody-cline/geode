//! A server's share state, kept in its own file beside `server.json`.
//!
//! Not in `server.json`, because that file is rewritten whole by jobs that load
//! it, work for minutes — a backup, an install, a Java download — and save the
//! copy they loaded. The server's password is the secret that decides who can
//! download its pack; if it lived there, replacing a leaked password during a
//! backup would be quietly undone when the backup saved, and the leaked
//! password would work again. Nothing but sharing ever writes this file, and it
//! is written by rename, so a reader never sees half of it.
//!
//! The share outlives sharing. A server gets its password the first time it is
//! shared, and keeps it through stopping, sharing again and restarts, until the
//! admin replaces it. Stopping renames `share.json` to `share.stopped.json`
//! rather than marking the file stopped: a Geode from before passwords were
//! kept reads every `share.json` as shared, and one opened after this stopped a
//! server would share it again at startup — asking the router to open the port
//! and sending `config/` — without a word to the admin.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

/// A server's side of sharing its pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerShare {
    /// The server's password: the secret in the link, and what a player types
    /// beside the address. It is what picks this server out of the ones this
    /// machine serves, and replacing it cuts off everyone holding the old.
    pub code: crate::share::code::Code,
    /// Send the server's `config/` folder too. Off unless asked for, because a
    /// modded server's configs hold bot tokens, database passwords and proxy
    /// secrets as readily as they hold anything a player needs.
    #[serde(default)]
    pub include_config: bool,
    /// Whether players can download the pack now. Stop sharing turns this off
    /// and keeps the password, so it is the same when the server is shared
    /// again. Not written: which of the two files the share is in says it, so a
    /// `share.json` from before this was kept is shared.
    #[serde(skip, default = "shared_by_default")]
    pub shared: bool,
}

fn shared_by_default() -> bool {
    true
}

/// What sharing looks like across every server.
#[derive(Debug, Default)]
pub struct Shared {
    /// Servers that are shared right now, by id. A server whose sharing is
    /// stopped is not here.
    pub servers: Vec<(String, ServerShare)>,
    /// Servers whose `share.json` exists but could not be read, by id. Reported
    /// rather than skipped, so a caller deciding whether to stop serving can
    /// tell "nothing is shared" from "something is, and I could not read it"
    /// — and which server it could not read.
    pub unreadable: Vec<String>,
}

/// `servers/<id>/share.json`, the share of a server shared right now.
pub fn path(paths: &Paths, id: &str) -> std::path::PathBuf {
    paths.server(id).join("share.json")
}

/// `servers/<id>/share.stopped.json`, the share of a server whose sharing is
/// stopped.
pub(crate) fn stopped_path(paths: &Paths, id: &str) -> std::path::PathBuf {
    paths.server(id).join("share.stopped.json")
}

/// This server's share, if it has ever been shared — stopped or not. An
/// unreadable file is an error, not "not shared".
///
/// `share.json` comes first. Both are there only when an older Geode shared
/// the server again after this one stopped it, and what that one made is the
/// share players are using.
pub async fn load(paths: &Paths, id: &str) -> Result<Option<ServerShare>> {
    if let Some(share) = read(&path(paths, id)).await? {
        return Ok(Some(share));
    }
    Ok(read(&stopped_path(paths, id))
        .await?
        .map(|share| ServerShare {
            shared: false,
            ..share
        }))
}

async fn read(file: &std::path::Path) -> Result<Option<ServerShare>> {
    let bytes = match tokio::fs::read(file).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::json(name, e))
}

/// Write atomically: to a temporary file in the same directory, then rename.
/// A shared share goes to `share.json`, a stopped one to `share.stopped.json`.
///
/// The server's folder is not created. Sharing a server that is not there, or
/// one deleted a moment ago, would leave a password with nothing behind it.
pub async fn save(paths: &Paths, id: &str, share: &ServerShare) -> Result<()> {
    if tokio::fs::metadata(paths.server(id)).await.is_err() {
        return Err(Error::msg(format!("server '{id}' was not found")));
    }
    let file = if share.shared {
        path(paths, id)
    } else {
        stopped_path(paths, id)
    };
    super::write_atomic(&file, &serde_json::to_vec_pretty(share)?).await
}

/// Share a server: again with the password it has, or with a new one the first
/// time.
pub async fn start(paths: &Paths, id: &str) -> Result<ServerShare> {
    match load(paths, id).await? {
        Some(share) if share.shared => Ok(share),
        Some(stopped) => {
            tokio::fs::rename(stopped_path(paths, id), path(paths, id)).await?;
            Ok(ServerShare {
                shared: true,
                ..stopped
            })
        }
        None => {
            let share = ServerShare {
                code: crate::share::code::Code::generate(),
                include_config: false,
                shared: true,
            };
            save(paths, id, &share).await?;
            Ok(share)
        }
    }
}

/// Stop sharing, keeping the password for when the server is shared again. A
/// server that was never shared, or is stopped already, has nothing to stop.
///
/// One rename, so no reader sees the server both shared and stopped, or
/// neither.
pub async fn stop(paths: &Paths, id: &str) -> Result<()> {
    match tokio::fs::rename(path(paths, id), stopped_path(paths, id)).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Forget a server's sharing entirely, password and all, for a server that is
/// going away. Unlike [`stop`] there is nothing to come back to.
///
/// Removes both files: a delete that stops part-way must not leave a password
/// behind for a folder nobody can see any more.
pub async fn clear(paths: &Paths, id: &str) -> Result<()> {
    for file in [path(paths, id), stopped_path(paths, id)] {
        match tokio::fs::remove_file(file).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

/// Give a server a new password, whether it is shared now or stopped, and leave
/// that as it is.
pub async fn replace_password(paths: &Paths, id: &str) -> Result<ServerShare> {
    let share = load(paths, id)
        .await?
        .ok_or_else(|| Error::msg("this server has no password yet — share it first"))?;
    let share = ServerShare {
        code: crate::share::code::Code::generate(),
        ..share
    };
    save(paths, id, &share).await?;
    Ok(share)
}

/// Every server shared right now, reading every `servers/*/share.json`.
pub async fn list(paths: &Paths) -> Result<Shared> {
    let mut shared = Shared::default();
    let mut entries = match tokio::fs::read_dir(paths.servers()).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(shared),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        match read(&path(paths, &id)).await {
            Ok(Some(share)) => shared.servers.push((id, share)),
            Ok(None) => {}
            Err(_) => shared.unreadable.push(id),
        }
    }

    // Directory order is whatever the filesystem likes.
    shared.servers.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::share::code::Code;

    fn scratch(tag: &str) -> Paths {
        let root = std::env::temp_dir().join(format!("geode-server-share-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Paths { root }
    }

    fn files_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_share_is_saved_read_back_and_stopped() {
        let paths = scratch("round-trip");
        std::fs::create_dir_all(paths.server("survival")).unwrap();

        assert_eq!(load(&paths, "survival").await.unwrap(), None);
        // A server never shared has nothing to stop, and that is not an error.
        stop(&paths, "survival").await.unwrap();
        assert_eq!(
            load(&paths, "survival").await.unwrap(),
            None,
            "stopping made a share"
        );

        let share = ServerShare {
            code: Code::generate(),
            include_config: true,
            shared: true,
        };
        save(&paths, "survival", &share).await.unwrap();
        assert_eq!(load(&paths, "survival").await.unwrap(), Some(share.clone()));
        // Written by rename, with nothing left over beside it.
        assert_eq!(files_in(&paths.server("survival")), vec!["share.json"]);

        // A new password replaces the old outright.
        let rotated = ServerShare {
            code: Code::generate(),
            ..share
        };
        save(&paths, "survival", &rotated).await.unwrap();
        assert_eq!(
            load(&paths, "survival").await.unwrap(),
            Some(rotated.clone())
        );

        let stopped = ServerShare {
            shared: false,
            ..rotated
        };
        stop(&paths, "survival").await.unwrap();
        assert_eq!(
            load(&paths, "survival").await.unwrap(),
            Some(stopped.clone())
        );
        // Stopping twice is not an error, and changes nothing.
        stop(&paths, "survival").await.unwrap();
        assert_eq!(load(&paths, "survival").await.unwrap(), Some(stopped));

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// The password is the server's, not one sharing's. Links already sent
    /// stop working while sharing is stopped, and work again when it starts.
    #[tokio::test]
    async fn stopping_keeps_the_password_and_sharing_again_brings_it_back() {
        let paths = scratch("keep-password");
        std::fs::create_dir_all(paths.server("survival")).unwrap();
        let listed = |shared: &Shared| {
            shared
                .servers
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        };

        let first = start(&paths, "survival").await.unwrap();
        assert!(first.shared);
        assert_eq!(listed(&list(&paths).await.unwrap()), vec!["survival"]);

        stop(&paths, "survival").await.unwrap();
        assert!(
            listed(&list(&paths).await.unwrap()).is_empty(),
            "a stopped server is still listed"
        );
        let stopped = load(&paths, "survival")
            .await
            .unwrap()
            .expect("stopping removed the password");
        assert_eq!(stopped.code, first.code);
        assert!(!stopped.shared);

        let again = start(&paths, "survival").await.unwrap();
        assert_eq!(again.code, first.code, "sharing again made a new password");
        assert!(again.shared);
        assert_eq!(load(&paths, "survival").await.unwrap(), Some(again));
        assert_eq!(listed(&list(&paths).await.unwrap()), vec!["survival"]);

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A Geode from before passwords were kept reads `share.json` and nothing
    /// else, and takes whatever it finds there for shared. A server stopped
    /// here must leave nothing there, or opening that Geode shares the server
    /// again — router, `config/` and all — with nobody asking.
    #[tokio::test]
    async fn a_stopped_share_is_not_where_an_older_geode_looks() {
        let paths = scratch("older-geode");
        let dir = paths.server("survival");
        std::fs::create_dir_all(&dir).unwrap();

        start(&paths, "survival").await.unwrap();
        assert_eq!(files_in(&dir), vec!["share.json"]);
        stop(&paths, "survival").await.unwrap();
        assert_eq!(files_in(&dir), vec!["share.stopped.json"]);
        // Nor does replacing the password while stopped put it back.
        replace_password(&paths, "survival").await.unwrap();
        assert_eq!(files_in(&dir), vec!["share.stopped.json"]);
        start(&paths, "survival").await.unwrap();
        assert_eq!(files_in(&dir), vec!["share.json"]);

        // Stopped here, then shared again by that older Geode, which makes a
        // password of its own. That is the one players hold now, and stopping
        // here again stops that one.
        stop(&paths, "survival").await.unwrap();
        let older = Code::generate();
        let json = format!(r#"{{"code":"{}","includeConfig":false}}"#, older.raw());
        std::fs::write(path(&paths, "survival"), json).unwrap();
        let share = load(&paths, "survival").await.unwrap().unwrap();
        assert!(share.shared);
        assert_eq!(share.code, older);
        stop(&paths, "survival").await.unwrap();
        assert_eq!(files_in(&dir), vec!["share.stopped.json"]);
        let stopped = load(&paths, "survival").await.unwrap().unwrap();
        assert_eq!((stopped.code, stopped.shared), (older, false));

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// Every share file written before stopping kept the file was for a server
    /// that was shared, since stopping removed it. Its code is the password,
    /// so every link already sent keeps working.
    #[tokio::test]
    async fn a_share_file_from_before_passwords_were_kept_reads_as_shared() {
        let paths = scratch("older-file");
        std::fs::create_dir_all(paths.server("survival")).unwrap();
        let code = Code::generate();
        let json = format!(r#"{{"code":"{}","includeConfig":false}}"#, code.raw());
        std::fs::write(path(&paths, "survival"), json).unwrap();

        let share = load(&paths, "survival").await.unwrap().unwrap();
        assert!(share.shared);
        assert_eq!(share.code, code);
        let shared = list(&paths).await.unwrap();
        assert_eq!(shared.servers, vec![("survival".to_string(), share)]);

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// The reason this file exists. A job that loaded the server before the
    /// admin replaced a leaked password saves that copy afterwards, and the old
    /// password must stay dead.
    #[tokio::test]
    async fn a_stale_server_save_leaves_the_share_alone() {
        use crate::server::{Server, ServerKind};

        let paths = scratch("stale-server");
        let server = Server::new("Leaky".into(), ServerKind::Fabric, "1.21.1".into());
        server.save(&paths).await.unwrap();
        let leaked = ServerShare {
            code: Code::generate(),
            include_config: true,
            shared: true,
        };
        save(&paths, &server.id, &leaked).await.unwrap();

        let mut job = crate::server::load(&paths, &server.id).await.unwrap();

        let replaced = ServerShare {
            code: Code::generate(),
            include_config: false,
            shared: true,
        };
        save(&paths, &server.id, &replaced).await.unwrap();

        job.last_started = Some(42);
        job.save(&paths).await.unwrap();
        assert_eq!(
            load(&paths, &server.id).await.unwrap(),
            Some(replaced.clone())
        );

        // Nor does it share again a server whose sharing was stopped, or take
        // its password away.
        stop(&paths, &server.id).await.unwrap();
        job.save(&paths).await.unwrap();
        assert_eq!(
            load(&paths, &server.id).await.unwrap(),
            Some(ServerShare {
                shared: false,
                ..replaced
            })
        );

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A password that reached the wrong person is replaced before sharing
    /// starts again, so it must not take sharing again to replace it.
    #[tokio::test]
    async fn replacing_the_password_works_while_stopped_and_keeps_it_stopped() {
        let paths = scratch("replace-stopped");
        std::fs::create_dir_all(paths.server("survival")).unwrap();

        let err = replace_password(&paths, "survival")
            .await
            .expect_err("a server never shared");
        assert!(err.to_string().contains("no password yet"), "{err}");
        assert_eq!(load(&paths, "survival").await.unwrap(), None);

        let first = start(&paths, "survival").await.unwrap();
        stop(&paths, "survival").await.unwrap();
        let replaced = replace_password(&paths, "survival").await.unwrap();
        assert_ne!(replaced.code, first.code);
        assert!(
            !replaced.shared,
            "replacing the password shared the server again"
        );
        assert_eq!(load(&paths, "survival").await.unwrap(), Some(replaced));

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A share with no server behind it would be a password that opens nothing.
    #[tokio::test]
    async fn a_server_that_is_not_there_cannot_be_shared() {
        let paths = scratch("missing");
        let share = ServerShare {
            code: Code::generate(),
            include_config: false,
            shared: true,
        };
        save(&paths, "nowhere", &share).await.unwrap_err();
        start(&paths, "nowhere").await.unwrap_err();
        assert!(!paths.server("nowhere").exists());
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    /// Something that cannot read a share file must not take it for "not
    /// shared": that is how a server still shared gets its listener stopped.
    #[tokio::test]
    async fn a_share_file_that_cannot_be_read_is_counted_not_skipped() {
        let paths = scratch("list");
        assert_eq!(list(&paths).await.unwrap().servers, vec![], "no servers folder at all");

        for id in ["b-shared", "a-shared", "not-shared", "torn"] {
            std::fs::create_dir_all(paths.server(id)).unwrap();
        }
        let b = ServerShare {
            code: Code::generate(),
            include_config: false,
            shared: true,
        };
        let a = ServerShare {
            code: Code::generate(),
            include_config: true,
            shared: true,
        };
        save(&paths, "b-shared", &b).await.unwrap();
        save(&paths, "a-shared", &a).await.unwrap();
        std::fs::write(path(&paths, "torn"), b"{\"code\": \"GEO").unwrap();
        // Not a server's folder.
        std::fs::write(paths.servers().join("share.json"), b"{}").unwrap();

        assert!(load(&paths, "torn").await.is_err());

        let shared = list(&paths).await.unwrap();
        assert_eq!(
            shared.servers,
            vec![("a-shared".to_string(), a), ("b-shared".to_string(), b)]
        );
        assert_eq!(shared.unreadable, vec!["torn".to_string()]);

        std::fs::remove_dir_all(&paths.root).unwrap();
    }
}

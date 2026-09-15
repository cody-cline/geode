//! A server's share state, kept in its own file beside `server.json`.
//!
//! Not in `server.json`, because that file is rewritten whole by jobs that load
//! it, work for minutes — a backup, an install, a Java download — and save the
//! copy they loaded. The share code is the secret that decides who can download
//! a server's pack; if it lived there, rotating a leaked code during a backup
//! would be quietly undone when the backup saved, and the leaked code would
//! work again. Nothing but sharing ever writes this file, and it is written by
//! rename, so a reader never sees half of it.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

/// A server's side of sharing its pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerShare {
    /// The secret in the link. It is what picks this server out of the ones
    /// this machine serves, and replacing it cuts off everyone holding the old.
    pub code: crate::share::code::Code,
    /// Send the server's `config/` folder too. Off unless asked for, because a
    /// modded server's configs hold bot tokens, database passwords and proxy
    /// secrets as readily as they hold anything a player needs.
    #[serde(default)]
    pub include_config: bool,
}

/// What sharing looks like across every server.
#[derive(Debug, Default)]
pub struct Shared {
    /// Servers that are shared, by id.
    pub servers: Vec<(String, ServerShare)>,
    /// Servers whose share files exist but could not be read, by id. Reported
    /// rather than skipped, so a caller deciding whether to stop serving can
    /// tell "nothing is shared" from "something is, and I could not read it"
    /// — and which server it could not read.
    pub unreadable: Vec<String>,
}

/// `servers/<id>/share.json`.
pub fn path(paths: &Paths, id: &str) -> std::path::PathBuf {
    paths.server(id).join("share.json")
}

/// This server's share, if it is shared. An unreadable file is an error, not
/// "not shared".
pub async fn load(paths: &Paths, id: &str) -> Result<Option<ServerShare>> {
    let bytes = match tokio::fs::read(path(paths, id)).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::json("share.json", e))
}

/// Write atomically: to a temporary file in the same directory, then rename.
///
/// The server's folder is not created. Sharing a server that is not there, or
/// one deleted a moment ago, would leave a code with nothing behind it.
pub async fn save(paths: &Paths, id: &str, share: &ServerShare) -> Result<()> {
    if tokio::fs::metadata(paths.server(id)).await.is_err() {
        return Err(Error::msg(format!("server '{id}' was not found")));
    }
    super::write_atomic(&path(paths, id), &serde_json::to_vec_pretty(share)?).await
}

/// Stop sharing. Removing a file that is already gone is not an error.
pub async fn clear(paths: &Paths, id: &str) -> Result<()> {
    match tokio::fs::remove_file(path(paths, id)).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Every shared server, reading every `servers/*/share.json`.
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
        match load(paths, &id).await {
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
    async fn a_share_is_saved_read_back_and_cleared() {
        let paths = scratch("round-trip");
        std::fs::create_dir_all(paths.server("survival")).unwrap();

        assert_eq!(load(&paths, "survival").await.unwrap(), None);

        let share = ServerShare {
            code: Code::generate(),
            include_config: true,
        };
        save(&paths, "survival", &share).await.unwrap();
        assert_eq!(load(&paths, "survival").await.unwrap(), Some(share.clone()));
        // Written by rename, with nothing left over beside it.
        assert_eq!(files_in(&paths.server("survival")), vec!["share.json"]);

        // A new code replaces the old outright.
        let rotated = ServerShare {
            code: Code::generate(),
            ..share
        };
        save(&paths, "survival", &rotated).await.unwrap();
        assert_eq!(load(&paths, "survival").await.unwrap(), Some(rotated));

        clear(&paths, "survival").await.unwrap();
        assert_eq!(load(&paths, "survival").await.unwrap(), None);
        // Unsharing twice is not an error.
        clear(&paths, "survival").await.unwrap();

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// The reason this file exists. A job that loaded the server before the
    /// admin replaced a leaked code saves that copy afterwards, and the old
    /// code must stay dead.
    #[tokio::test]
    async fn a_stale_server_save_leaves_the_share_alone() {
        use crate::server::{Server, ServerKind};

        let paths = scratch("stale-server");
        let server = Server::new("Leaky".into(), ServerKind::Fabric, "1.21.1".into());
        server.save(&paths).await.unwrap();
        let leaked = ServerShare {
            code: Code::generate(),
            include_config: true,
        };
        save(&paths, &server.id, &leaked).await.unwrap();

        let mut job = crate::server::load(&paths, &server.id).await.unwrap();

        let replaced = ServerShare {
            code: Code::generate(),
            include_config: false,
        };
        save(&paths, &server.id, &replaced).await.unwrap();

        job.last_started = Some(42);
        job.save(&paths).await.unwrap();
        assert_eq!(load(&paths, &server.id).await.unwrap(), Some(replaced));

        // Nor does it bring back a share that was stopped.
        clear(&paths, &server.id).await.unwrap();
        job.save(&paths).await.unwrap();
        assert_eq!(load(&paths, &server.id).await.unwrap(), None);

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A share with no server behind it would be a code that opens nothing.
    #[tokio::test]
    async fn a_server_that_is_not_there_cannot_be_shared() {
        let paths = scratch("missing");
        let share = ServerShare {
            code: Code::generate(),
            include_config: false,
        };
        save(&paths, "nowhere", &share).await.unwrap_err();
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
        };
        let a = ServerShare {
            code: Code::generate(),
            include_config: true,
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

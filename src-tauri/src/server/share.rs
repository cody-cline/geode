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
    /// What happens to a player's own changes to the files this server sends,
    /// per path, and the push counter that dates every change to that.
    ///
    /// Empty — which is every share from before this, and every one whose
    /// admin has chosen nothing — means every file it sends is replaced on the
    /// player's every Play, which is what sending a config folder has always
    /// done. [`crate::share::manifest::Settings::policy_for`] is that rule.
    ///
    /// Here rather than in `server.json` for the reason the password is: the
    /// counter must never go backwards, and nothing but sharing writes this
    /// file. A backup covers `run/` alone, so it cannot sweep this up and hand
    /// an older count back.
    #[serde(default)]
    pub settings: crate::share::manifest::Settings,
    /// How this server should appear in its players' multiplayer list: the
    /// name they see, and the address the *game* connects to. `None` until the
    /// admin sets it, and then no row is written for anybody.
    ///
    /// Here rather than in `server.json` for the same reason the password is:
    /// nothing but sharing writes this file, and a long job that loaded the
    /// server minutes ago cannot undo an address the admin corrected since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<crate::share::manifest::Listing>,
    /// Mods this server does not allow its players to run, with the admin's
    /// reason for each. Empty unless they have said.
    ///
    /// Here rather than in `server.json` for the reason the password is: a
    /// long job that loaded the server minutes ago must not put back a rule
    /// the admin has since changed. It rides the pack, so adding one reaches
    /// every player on their next Play — and a mod turned back on after that
    /// goes off again on the next update, because it is a standing rule and
    /// not a one-time change. The next *update*, not the next Play: a Play
    /// finding the revision where it left it downloads no pack.
    ///
    /// Client-side only, and the admin is told so where they set it: a patched
    /// Geode, another launcher, or a jar dropped in after an update all ignore
    /// it. Real enforcement is the server software's job.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden: Vec<crate::share::manifest::Forbidden>,
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

impl ServerShare {
    /// Send this server's `config/` folder or stop sending it, and choose what
    /// happens to a player's own changes to it when the caller says so.
    ///
    /// `policy` is `None` for "leave the choice where it is", which is what
    /// the switch on the Share tab sends: turning the folder off and on again
    /// is the behaviour the admin already picked, not a fresh guess at one.
    /// The switch has no choice to send back anyway — a folder nobody is sent
    /// reports none — and a guess would be
    /// [`Settings::choose`](crate::share::manifest::Settings::choose) with a
    /// behaviour the admin never asked for, which moves the push counter and
    /// so sends every player a pack that writes their settings files whole.
    pub fn send_config(&mut self, include: bool, policy: Option<crate::share::manifest::Policy>) {
        self.include_config = include;
        if let Some(policy) = policy {
            self.settings.choose("config", policy);
        }
    }
}

/// What to put in the admin's two fields before they have said anything.
///
/// A suggestion, not a setting: nothing is written until the admin saves it,
/// and an address Geode cannot work out is left blank and asked for rather
/// than guessed at. Guessing here would hand every player an address that
/// does not answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    /// The server's own name, which is what players would expect to see.
    pub name: String,
    /// The game address, worked out from the share address's host and the
    /// server's own `server-port`. `None` when there is nothing to work it
    /// out from — see [`suggest`].
    pub address: Option<String>,
}

/// The pair to offer the admin for this server.
///
/// The address comes from two things Geode already knows, and from neither of
/// them alone:
///
/// - the **host** of the share address, because that is the machine players
///   reach — but never its port, which is Geode's own and not the game's;
/// - the server's own **`server-port`**, read from its `server.properties`,
///   because that is the port the game listens on.
///
/// Nothing is suggested when the share address is an `https://` tunnel: a
/// tunnel in front of the pack port says nothing whatever about where
/// Minecraft's TCP port is, and the admin is asked instead. Nor when there is
/// no share address at all, nor when `server-port` is `0`, which means "any
/// free port" and so names no address to give anybody.
pub async fn suggest(
    paths: &Paths,
    server: &crate::server::Server,
    share_address: Option<&crate::share::link::Address>,
) -> Suggestion {
    Suggestion {
        name: server.name.clone(),
        address: suggest_address(paths, &server.id, share_address).await,
    }
}

async fn suggest_address(
    paths: &Paths,
    id: &str,
    share_address: Option<&crate::share::link::Address>,
) -> Option<String> {
    let crate::share::link::Address::Direct { host, .. } = share_address? else {
        return None;
    };
    let port = game_port(paths, id).await?;
    let text = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    // Through the same validator the admin's own typing goes through, so a
    // router reporting something unusable produces no suggestion rather than
    // a bad one — and Minecraft's own port comes back off the end.
    crate::share::link::game_address(&text).ok()
}

/// The port this server listens on for the game, from its own
/// `server.properties`.
///
/// A server that has never started has no such file yet, and 25565 is what it
/// will write there on its first boot — so that, rather than nothing, is the
/// answer for one not yet run.
async fn game_port(paths: &Paths, id: &str) -> Option<u16> {
    let properties = crate::server::properties::read(paths, id)
        .await
        .unwrap_or_default();
    match properties.iter().find(|p| p.key == "server-port") {
        Some(property) => property
            .value
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0),
        None => Some(crate::share::link::GAME_PORT),
    }
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
                settings: Default::default(),
                listing: None,
                forbidden: Vec::new(),
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

/// `servers/<id>/share.link.json`, the address this server's links were last
/// built from. Its own file beside the others for the same reason they are
/// apart: nothing that writes a server's password has any business rewriting
/// this, or the other way about.
fn link_path(paths: &Paths, id: &str) -> std::path::PathBuf {
    paths.server(id).join("share.link.json")
}

/// Where this server's links point, so the Share tab can say when the address
/// has moved on since and the links already sent lead nowhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkAddress {
    address: String,
}

/// The address this server's links were last built from, or `None` when no
/// link has been taken yet.
///
/// A file that cannot be read or parsed is no address either. What it feeds is
/// a note about addresses; failing the whole Share tab over it would cost the
/// admin the link as well, and the next link taken writes it afresh.
pub async fn link_address(paths: &Paths, id: &str) -> Option<String> {
    let bytes = tokio::fs::read(link_path(paths, id)).await.ok()?;
    let stored: LinkAddress = serde_json::from_slice(&bytes).ok()?;
    Some(stored.address)
}

/// The address this server's links were last taken at, for a server list that
/// has not loaded the share — `None` as soon as sharing stops.
///
/// Stopping answers `None` because the warning this feeds is about links in
/// other people's hands: a stopped share serves nobody, so there is nothing to
/// tell the admin to re-send. The Share tab asks `link_address` instead; it has
/// the share in hand already and says which case it is in for itself.
pub async fn shared_link_address(paths: &Paths, id: &str) -> Option<String> {
    let share = load(paths, id).await.ok().flatten()?;
    if !share.shared {
        return None;
    }
    link_address(paths, id).await
}

/// Remember the address this server's links are being handed out with.
///
/// Written by rename, like the share itself, so a reader never finds half an
/// address. Deleting the server takes its whole folder, this with it.
pub async fn save_link_address(paths: &Paths, id: &str, address: &str) -> Result<()> {
    let stored = LinkAddress {
        address: address.to_string(),
    };
    super::write_atomic(&link_path(paths, id), &serde_json::to_vec_pretty(&stored)?).await
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
    use crate::paths::Scratch;
    use crate::share::code::Code;

    fn scratch(tag: &str) -> Scratch {
        Scratch::new(&format!("server-share-{tag}"))
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
        let scratch = scratch("round-trip");
        let paths = scratch.paths.clone();
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
            settings: Default::default(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
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
    }

    /// What the admin chose happens to a player's own changes is kept like the
    /// password is: through saving, through stopping, and through sharing
    /// again. Sharing again the way it was shared before is the whole reason
    /// stopping renames the file instead of deleting it, and the choice is now
    /// part of "the way it was".
    #[tokio::test]
    async fn a_stopped_share_keeps_what_the_admin_chose_for_its_config() {
        use crate::share::manifest::Policy;

        let scratch = scratch("settings");
        let paths = scratch.paths.clone();
        std::fs::create_dir_all(paths.server("survival")).unwrap();

        let mut share = ServerShare {
            code: Code::generate(),
            include_config: true,
            settings: Default::default(),
            listing: None,
            forbidden: Vec::new(),
            shared: true,
        };
        share.settings.choose("config", Policy::ShipOnce);
        save(&paths, "survival", &share).await.unwrap();
        assert_eq!(load(&paths, "survival").await.unwrap(), Some(share.clone()));

        stop(&paths, "survival").await.unwrap();
        let back = start(&paths, "survival").await.unwrap();
        assert_eq!(back, share, "sharing again lost the choice");
        assert_eq!(back.settings.paths[0].policy, Policy::ShipOnce);
        assert_eq!(back.settings.at, 1, "the push counter went backwards");
    }

    /// The switch that sends the config folder must not be able to replace the
    /// behaviour the admin chose for it. Turning the folder off and on again is
    /// the choice they already made: a guess sent back in its place dates a new
    /// choice, which moves the revision, which hands every player a pack that
    /// writes their own settings files whole — and the Share tab has nothing to
    /// send back anyway, since a folder nobody is sent reports no behaviour.
    #[test]
    fn turning_the_config_folder_off_and_on_again_keeps_the_choice() {
        use crate::share::manifest::Policy;

        let mut share = ServerShare {
            code: Code::generate(),
            include_config: false,
            settings: Default::default(),
            listing: None,
            forbidden: Vec::new(),
            shared: true,
        };

        // Opting in says nothing about behaviour, so what answers is the one
        // already in force — which for a share that has chosen nothing is what
        // sending a config folder has always done.
        share.send_config(true, None);
        assert!(share.include_config);
        assert_eq!(share.settings, Default::default(), "the switch chose for the admin");
        assert_eq!(
            share.settings.policy_for(std::path::Path::new("config")),
            Policy::Enforce
        );

        share.send_config(true, Some(Policy::ShipOnce));
        let chosen = share.settings.clone();
        assert_eq!(chosen.at, 1);

        share.send_config(false, None);
        assert!(!share.include_config);
        assert_eq!(share.settings, chosen, "turning the folder off forgot the choice");

        share.send_config(true, None);
        assert!(share.include_config);
        assert_eq!(
            share.settings, chosen,
            "turning the folder back on was a different behaviour from the one chosen"
        );
        assert_eq!(
            share.settings.at, 1,
            "the push counter moved, so every player is sent their settings files again"
        );
    }

    /// The password is the server's, not one sharing's. Links already sent
    /// stop working while sharing is stopped, and work again when it starts.
    #[tokio::test]
    async fn stopping_keeps_the_password_and_sharing_again_brings_it_back() {
        let scratch = scratch("keep-password");
        let paths = scratch.paths.clone();
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
    }

    /// A Geode from before passwords were kept reads `share.json` and nothing
    /// else, and takes whatever it finds there for shared. A server stopped
    /// here must leave nothing there, or opening that Geode shares the server
    /// again — router, `config/` and all — with nobody asking.
    #[tokio::test]
    async fn a_stopped_share_is_not_where_an_older_geode_looks() {
        let scratch = scratch("older-geode");
        let paths = scratch.paths.clone();
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
    }

    /// Every share file written before stopping kept the file was for a server
    /// that was shared, since stopping removed it. Its code is the password,
    /// so every link already sent keeps working.
    #[tokio::test]
    async fn a_share_file_from_before_passwords_were_kept_reads_as_shared() {
        let scratch = scratch("older-file");
        let paths = scratch.paths.clone();
        std::fs::create_dir_all(paths.server("survival")).unwrap();
        let code = Code::generate();
        let json = format!(r#"{{"code":"{}","includeConfig":false}}"#, code.raw());
        std::fs::write(path(&paths, "survival"), json).unwrap();

        let share = load(&paths, "survival").await.unwrap().unwrap();
        assert!(share.shared);
        assert_eq!(share.code, code);
        let shared = list(&paths).await.unwrap();
        assert_eq!(shared.servers, vec![("survival".to_string(), share)]);
    }

    /// The reason this file exists. A job that loaded the server before the
    /// admin replaced a leaked password saves that copy afterwards, and the old
    /// password must stay dead.
    #[tokio::test]
    async fn a_stale_server_save_leaves_the_share_alone() {
        use crate::server::{Server, ServerKind};

        let scratch = scratch("stale-server");
        let paths = scratch.paths.clone();
        let server = Server::new("Leaky".into(), ServerKind::Fabric, "1.21.1".into());
        server.save(&paths).await.unwrap();
        let leaked = ServerShare {
            code: Code::generate(),
            include_config: true,
            settings: Default::default(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
        };
        save(&paths, &server.id, &leaked).await.unwrap();

        let mut job = crate::server::load(&paths, &server.id).await.unwrap();

        let replaced = ServerShare {
            code: Code::generate(),
            include_config: false,
            settings: Default::default(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
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
    }

    /// A password that reached the wrong person is replaced before sharing
    /// starts again, so it must not take sharing again to replace it.
    #[tokio::test]
    async fn replacing_the_password_works_while_stopped_and_keeps_it_stopped() {
        let scratch = scratch("replace-stopped");
        let paths = scratch.paths.clone();
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
    }

    /// The address a server's links were built from outlives the Geode that
    /// wrote it — a restart has to find the links already sent still pointing
    /// where they did — and nothing else that writes the share disturbs it.
    #[tokio::test]
    async fn the_address_links_were_built_from_is_read_back_from_disk() {
        let scratch = scratch("link-address");
        let paths = scratch.paths.clone();
        std::fs::create_dir_all(paths.server("survival")).unwrap();

        assert_eq!(link_address(&paths, "survival").await, None);
        start(&paths, "survival").await.unwrap();
        assert_eq!(
            link_address(&paths, "survival").await,
            None,
            "sharing a server invented a link nobody has taken"
        );

        save_link_address(&paths, "survival", "203.0.113.7:25590")
            .await
            .unwrap();
        // A second Paths over the same directory is all a restart has.
        let restarted = Paths {
            root: paths.root.clone(),
        };
        assert_eq!(
            link_address(&restarted, "survival").await,
            Some("203.0.113.7:25590".to_string())
        );

        // The share's own file is written by stopping, sharing again and a new
        // password; none of them is news about where links point.
        stop(&paths, "survival").await.unwrap();
        replace_password(&paths, "survival").await.unwrap();
        start(&paths, "survival").await.unwrap();
        assert_eq!(
            link_address(&paths, "survival").await,
            Some("203.0.113.7:25590".to_string())
        );

        // Taking a link at a new address replaces it outright.
        save_link_address(&paths, "survival", "198.51.100.4:25590")
            .await
            .unwrap();
        assert_eq!(
            link_address(&paths, "survival").await,
            Some("198.51.100.4:25590".to_string())
        );

        // A file no reader can make sense of is no address, rather than a
        // Share tab that will not open.
        std::fs::write(link_path(&paths, "survival"), b"{\"addr").unwrap();
        assert_eq!(link_address(&paths, "survival").await, None);
    }

    /// A share with no server behind it would be a password that opens nothing.
    #[tokio::test]
    async fn a_server_that_is_not_there_cannot_be_shared() {
        let scratch = scratch("missing");
        let paths = scratch.paths.clone();
        let share = ServerShare {
            code: Code::generate(),
            include_config: false,
            settings: Default::default(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
        };
        save(&paths, "nowhere", &share).await.unwrap_err();
        start(&paths, "nowhere").await.unwrap_err();
        assert!(!paths.server("nowhere").exists());
    }

    /// Something that cannot read a share file must not take it for "not
    /// shared": that is how a server still shared gets its listener stopped.
    #[tokio::test]
    async fn a_share_file_that_cannot_be_read_is_counted_not_skipped() {
        let scratch = scratch("list");
        let paths = scratch.paths.clone();
        assert_eq!(list(&paths).await.unwrap().servers, vec![], "no servers folder at all");

        for id in ["b-shared", "a-shared", "not-shared", "torn"] {
            std::fs::create_dir_all(paths.server(id)).unwrap();
        }
        let b = ServerShare {
            code: Code::generate(),
            include_config: false,
            settings: Default::default(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
        };
        let a = ServerShare {
            code: Code::generate(),
            include_config: true,
            settings: Default::default(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
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
    }

    /// The listing is the server's, like its password: it survives stopping,
    /// sharing again and a replaced password, because each of those is about
    /// who may download and not about where the server is.
    #[tokio::test]
    async fn a_listing_survives_stopping_sharing_again_and_a_new_password() {
        let scratch = scratch("listing");
        let paths = scratch.paths.clone();
        std::fs::create_dir_all(paths.server("survival")).unwrap();

        let listing = crate::share::manifest::Listing {
            name: "§aSurvival".into(),
            address: "play.example.com:25566".into(),
        };
        let first = start(&paths, "survival").await.unwrap();
        assert_eq!(first.listing, None, "sharing invented a listing");

        let shared = ServerShare {
            listing: Some(listing.clone()),
            ..first
        };
        save(&paths, "survival", &shared).await.unwrap();
        assert_eq!(
            load(&paths, "survival").await.unwrap().unwrap().listing,
            Some(listing.clone())
        );

        stop(&paths, "survival").await.unwrap();
        assert_eq!(
            load(&paths, "survival").await.unwrap().unwrap().listing,
            Some(listing.clone()),
            "stopping lost the listing"
        );
        let replaced = replace_password(&paths, "survival").await.unwrap();
        assert_eq!(replaced.listing, Some(listing.clone()));
        let again = start(&paths, "survival").await.unwrap();
        assert_eq!(again.listing, Some(listing));
    }

    /// The game address is suggested from the share address's **host** and the
    /// server's **own** `server-port` — never from the share port, which is
    /// Geode's and would hand players a port nothing is listening on.
    ///
    /// Use `config.share.port` here instead and 25590 appears, which is what
    /// the first assertion is looking for.
    #[tokio::test]
    async fn the_game_address_is_suggested_from_the_share_host_and_the_servers_own_port() {
        use crate::share::link::Address;
        use crate::server::{Server, ServerKind};

        let scratch = scratch("suggest");
        let paths = scratch.paths.clone();
        let server = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
        server.save(&paths).await.unwrap();

        // The share address, on Geode's own port.
        let share_address = Address::parse("203.0.113.7:25590", 25590).unwrap();
        let at = |address: Option<&Address>| {
            let (paths, server) = (paths.clone(), server.clone());
            let address = address.cloned();
            async move { suggest(&paths, &server, address.as_ref()).await }
        };

        // Never started, so no server.properties: 25565 is what it will write
        // there on its first boot, and Minecraft's own port is left off.
        let fresh = at(Some(&share_address)).await;
        assert_eq!(fresh.name, "Survival");
        assert_eq!(fresh.address.as_deref(), Some("203.0.113.7"));

        // Once it has a port of its own, that is the port.
        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(&run).await.unwrap();
        let properties = |text: &str| std::fs::write(run.join("server.properties"), text).unwrap();
        properties("#Minecraft server properties\nserver-port=25566\nmotd=Hi\n");
        let moved = at(Some(&share_address)).await;
        assert_eq!(moved.address.as_deref(), Some("203.0.113.7:25566"));
        assert!(
            !moved.address.as_deref().unwrap().contains("25590"),
            "the share port reached the game address"
        );

        // `0` is "any free port", which is no address to give anybody.
        properties("server-port=0\n");
        assert_eq!(at(Some(&share_address)).await.address, None);

        // An https tunnel in front of the pack port says nothing about where
        // Minecraft's own port is, so the admin is asked rather than guessed
        // at — and so they are with no share address at all.
        properties("server-port=25566\n");
        let tunnel = Address::parse("https://pack.example.com", 25590).unwrap();
        assert_eq!(at(Some(&tunnel)).await.address, None);
        assert_eq!(at(None).await.address, None);

        // An address a router reported that nobody could connect to produces
        // no suggestion rather than a bad one.
        let unusable = Address::parse("0.0.0.0:25590", 25590).unwrap();
        assert_eq!(at(Some(&unusable)).await.address, None);

        // IPv6 keeps its brackets, so the game can tell address from port.
        let v6 = Address::parse("[2001:db8::5]:25590", 25590).unwrap();
        assert_eq!(
            at(Some(&v6)).await.address.as_deref(),
            Some("[2001:db8::5]:25566")
        );
    }
}

//! Who may play: the whitelist, the operators, the bans, and who is on now.
//!
//! Four of these lists are files Minecraft itself owns — `whitelist.json`,
//! `ops.json`, `banned-players.json` and `banned-ips.json`, all in the run
//! directory — and that ownership is the whole design problem. A running
//! server holds them in memory and writes them out whenever it changes one, so
//! an edit made behind its back is overwritten the next time somebody is
//! whitelisted in game. So every change here goes one of exactly two ways, and
//! which one is never a guess:
//!
//! - **The server is running** — the change goes through the console, the way
//!   an operator at the terminal would make it, and the server writes its own
//!   file. Geode reads the file afterwards to show the result, and reports what
//!   the server said. If it said nothing, that is what is reported: a screen
//!   that shows a ban as done when the server never acknowledged it has lied to
//!   somebody about who can get in.
//! - **The server is stopped** — the file is nobody's but ours, so it is
//!   edited directly. This is the safer direction and the only one that can set
//!   an operator's level, which `/op` has no argument for.
//!
//! Two more rules this module exists to hold:
//!
//! **A name is not an identity.** These files are keyed by UUID because a
//! Minecraft name can change hands, so nothing is written under a name that has
//! not been resolved to one. Which resolution is correct is a property of the
//! server: one in online mode keys players by the UUID Mojang issued, and only
//! Mojang can say what that is; one with `online-mode=false` keys them by the
//! UUID vanilla derives from the name itself, which needs no network and is not
//! a guess. A lookup that fails is reported, never worked around.
//!
//! **A command this server does not have is not offered.** The 14 server kinds
//! are not all vanilla: a proxy has no whitelist at all, and a mod or plugin can
//! add or replace commands. The only complete account of what a server can be
//! asked is the Brigadier tree its own probe wrote ([`super::probe`]), so that
//! is what is consulted — and a server with no tree yet offers nothing rather
//! than being guessed at.

use std::net::IpAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Context, Error, Result};
use crate::paths::Paths;

use super::probe::CommandNode;
use super::properties::{self, Property};
use super::run::ServerRegistry;

/// Mojang's name lookup. Unauthenticated, and the only way to learn the UUID an
/// online-mode server will key a player by.
const LOOKUP: &str = "https://api.minecraftservices.com/minecraft/profile/lookup/name/";

/// What `white-list` is called in `server.properties`.
const WHITELIST_KEY: &str = "white-list";

/// One of the four lists Minecraft keeps beside a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerList {
    Whitelist,
    Ops,
    Bans,
    IpBans,
}

impl PlayerList {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "whitelist" => PlayerList::Whitelist,
            "ops" => PlayerList::Ops,
            "bans" => PlayerList::Bans,
            "ip-bans" => PlayerList::IpBans,
            _ => return None,
        })
    }

    /// The file Minecraft keeps this list in, named by the game rather than by
    /// us — these are its files.
    fn file_name(self) -> &'static str {
        match self {
            PlayerList::Whitelist => "whitelist.json",
            PlayerList::Ops => "ops.json",
            PlayerList::Bans => "banned-players.json",
            PlayerList::IpBans => "banned-ips.json",
        }
    }

    /// Whether entries are accounts (keyed by UUID) or addresses.
    fn is_addresses(self) -> bool {
        matches!(self, PlayerList::IpBans)
    }
}

/// One row of one of those files, with everything we do not read carried
/// through untouched.
///
/// The files are the server's: a fork or a later Minecraft version may add a
/// field, and rewriting one while the server is stopped must not be how that
/// field gets dropped. `bypassesPlayerLimit` on an operator and `source` on a
/// ban both ride along this way.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// One entry as a screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEntry {
    /// The name **as the server last recorded it**, which is not necessarily
    /// the name that account answers to today. Shown beside the UUID for that
    /// reason.
    pub name: String,
    /// What the file is keyed by. Empty only for an IP ban, which has no
    /// account behind it.
    pub uuid: String,
    pub ip: Option<String>,
    /// An operator's permission level, 1 to 4.
    pub level: Option<u8>,
    pub reason: Option<String>,
    pub created: Option<String>,
}

impl ListEntry {
    /// What a console command or a file edit names this entry by.
    pub fn target(&self) -> &str {
        self.ip.as_deref().unwrap_or(&self.name)
    }
}

/// A name and the UUID that name resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub uuid: String,
}

/// What this server's own command tree says it can be asked to do.
///
/// Every field is read off the tree the probe wrote, never off the server kind:
/// a Velocity proxy has no whitelist, a plugin can replace `/ban`, and nothing
/// but the server's own dispatcher knows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Console {
    /// Whether a command tree was read at all. Without one nothing below is
    /// true, and the screen says so rather than offering commands that may not
    /// exist.
    pub known: bool,
    pub who_is_online: bool,
    pub whitelist_add: bool,
    pub whitelist_remove: bool,
    pub whitelist_toggle: bool,
    pub op: bool,
    pub deop: bool,
    pub ban: bool,
    pub pardon: bool,
    pub ban_ip: bool,
    pub pardon_ip: bool,
    pub kick: bool,
}

impl Console {
    /// Read the capabilities off a command tree.
    pub fn from_tree(tree: &[CommandNode]) -> Self {
        let root = |name: &str| tree.iter().any(|n| n.name == name);
        let under = |parent: &str, child: &str| {
            tree.iter()
                .find(|n| n.name == parent)
                .is_some_and(|n| n.children.iter().any(|c| c.name == child))
        };
        Self {
            known: !tree.is_empty(),
            who_is_online: root("list"),
            whitelist_add: under("whitelist", "add"),
            whitelist_remove: under("whitelist", "remove"),
            whitelist_toggle: under("whitelist", "on") && under("whitelist", "off"),
            op: root("op"),
            deop: root("deop"),
            ban: root("ban"),
            pardon: root("pardon"),
            ban_ip: root("ban-ip"),
            pardon_ip: root("pardon-ip"),
            kick: root("kick"),
        }
    }

    fn can_add(&self, list: PlayerList) -> bool {
        match list {
            PlayerList::Whitelist => self.whitelist_add,
            PlayerList::Ops => self.op,
            PlayerList::Bans => self.ban,
            PlayerList::IpBans => self.ban_ip,
        }
    }

    fn can_remove(&self, list: PlayerList) -> bool {
        match list {
            PlayerList::Whitelist => self.whitelist_remove,
            PlayerList::Ops => self.deop,
            PlayerList::Bans => self.pardon,
            PlayerList::IpBans => self.pardon_ip,
        }
    }
}

/// Everything the screen shows, in one read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerLists {
    pub whitelist: Vec<ListEntry>,
    pub ops: Vec<ListEntry>,
    pub bans: Vec<ListEntry>,
    pub ip_bans: Vec<ListEntry>,
    /// `white-list` in `server.properties` — the toggle that decides whether
    /// the whitelist is consulted at all.
    pub whitelist_on: bool,
    /// Whether this server authenticates players with Mojang. It decides how a
    /// name becomes a UUID, so the screen can say which it will be.
    pub online_mode: bool,
    pub running: bool,
    pub console: Console,
}

/// Which way a change went. The screen says this out loud, because "the server
/// did it" and "Geode wrote the file" fail in different ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Console,
    File,
}

/// A change that happened, and the state it left behind.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub via: Via,
    /// What the server said, when it went through the console — as it said it.
    /// Not classified into success or failure: "Added alice to the whitelist"
    /// is the server's own answer, and every fork and every language spells it
    /// differently, so deciding what it means here would be inventing.
    pub said: Vec<String>,
    /// The lists as they stand after the change.
    pub lists: PlayerLists,
}

/// Who is on the server now, as its own `/list` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Online {
    /// The names, when the reply was in the shape `/list` produces. `None`
    /// means the reply could not be read — a plugin has replaced `/list`, or
    /// the server answers in another language — and then `said` is all there
    /// is to go on.
    pub players: Option<Vec<String>>,
    pub said: Vec<String>,
}

/// What a change to these lists has to be able to reach: the files, the running
/// server, and Mojang.
pub struct Manager<'a> {
    pub paths: &'a Paths,
    pub servers: &'a ServerRegistry,
    pub http: &'a reqwest::Client,
}

impl Manager<'_> {
    /// Every list, the whitelist toggle, and what the console can be asked.
    ///
    /// The files are read even while the server is running: it keeps them
    /// current as it changes them, so they are the honest account of the
    /// lists either way. Only *writing* them depends on who is holding them.
    pub async fn lists(&self, id: &str) -> Result<PlayerLists> {
        let props = properties::read(self.paths, id).await?;
        let find = |key: &str| {
            props
                .iter()
                .find(|p| p.key == key)
                .map(|p| p.value.trim().to_ascii_lowercase())
        };
        Ok(PlayerLists {
            whitelist: self.entries(id, PlayerList::Whitelist).await?,
            ops: self.entries(id, PlayerList::Ops).await?,
            bans: self.entries(id, PlayerList::Bans).await?,
            ip_bans: self.entries(id, PlayerList::IpBans).await?,
            whitelist_on: find(WHITELIST_KEY).as_deref() == Some("true"),
            // Vanilla's own default, and the safe way round: assuming online
            // mode sends a name to Mojang rather than deriving a UUID that
            // would be wrong.
            online_mode: find("online-mode").as_deref() != Some("false"),
            running: self.servers.is_running(id),
            console: Console::from_tree(&super::probe::read(self.paths, id).await),
        })
    }

    async fn entries(&self, id: &str, list: PlayerList) -> Result<Vec<ListEntry>> {
        Ok(read_file(self.paths, id, list)
            .await?
            .into_iter()
            .map(|row| ListEntry {
                name: row.name.unwrap_or_default(),
                uuid: row.uuid.unwrap_or_default(),
                ip: row.ip,
                level: row.level,
                reason: row.reason,
                created: row.created,
            })
            .collect())
    }

    /// Who is online, asked of the running server.
    pub async fn online(&self, id: &str) -> Result<Online> {
        let tree = super::probe::read(self.paths, id).await;
        if !Console::from_tree(&tree).who_is_online {
            return Err(Error::msg(
                "Geode has not seen a `list` command on this server, so it will not \
                 guess at one. The list of commands is read while the server runs — \
                 start it once with \"Read commands\" on in Settings.",
            ));
        }
        let said = self.say(id, "list").await?;
        Ok(Online {
            players: read_online(&said),
            said,
        })
    }

    /// Put somebody on one of these lists.
    ///
    /// `level` is an operator's permission level and is only accepted while the
    /// server is stopped: `/op` takes no level argument, so while it runs the
    /// server decides from its own `op-permission-level`.
    pub async fn add(
        &self,
        id: &str,
        list: PlayerList,
        target: &str,
        reason: Option<&str>,
        level: Option<u8>,
    ) -> Result<Change> {
        let reason = check_reason(reason)?;
        if let Some(level) = level {
            if !(1..=4).contains(&level) {
                return Err(Error::msg(
                    "an operator's level is 1 to 4 — 4 is full access",
                ));
            }
            if list != PlayerList::Ops {
                return Err(Error::msg("only an operator has a level"));
            }
        }

        if self.servers.is_running(id) {
            if level.is_some() {
                return Err(Error::msg(
                    "an operator's level can only be set while the server is stopped — \
                     while it runs, /op uses the server's own op-permission-level.",
                ));
            }
            // Held to what a console line can carry rather than to what a
            // Minecraft name may be: the names the screen offers to ban or op
            // come off this server's own `/list` reply, and a Bedrock player
            // bridged in by Floodgate answers to `.Steve`.
            let target = self.check_console_target(list, target)?;
            let verb = match list {
                PlayerList::Whitelist => "whitelist add",
                PlayerList::Ops => "op",
                PlayerList::Bans => "ban",
                PlayerList::IpBans => "ban-ip",
            };
            return self.through_console(id, list, true, &with_reason(verb, &target, reason)).await;
        }

        // A name going into the file is resolved first, at Mojang or against
        // this server's own cache, so this is the one an admin typed and it is
        // held to what a Minecraft name may be.
        let target = self.check_target(list, target)?;
        let mut rows = read_file(self.paths, id, list).await?;
        let mut row = Stored::default();
        if list.is_addresses() {
            if rows.iter().any(|r| same_address(r.ip.as_deref(), &target)) {
                return Err(Error::msg(format!("{target} is already on that list")));
            }
            row.ip = Some(target.clone());
        } else {
            let who = self.resolve(id, &target).await?;
            if rows.iter().any(|r| r.uuid.as_deref() == Some(who.uuid.as_str())) {
                return Err(Error::msg(format!(
                    "{} is already on that list",
                    who.name
                )));
            }
            row.uuid = Some(who.uuid);
            row.name = Some(who.name);
            row.level = level;
        }
        // The direction was chosen before any of this, and resolving a name is
        // a round trip to Mojang — a second in which a scheduled task or a
        // click on Start can bring the server up. It is holding this file in
        // memory now, and would write Geode's row straight back out the next
        // time it changed the list itself. So the direction is checked again
        // here, where the write actually happens, and nothing is written behind
        // a server's back or reported as though it had been.
        if self.servers.is_running(id) {
            return Err(Error::msg(format!(
                "this server started while “{target}” was being added, so nothing was \
                 written — {} is the running server's again now. Ask again and the \
                 change goes through its console.",
                list.file_name()
            )));
        }

        if matches!(list, PlayerList::Bans | PlayerList::IpBans) {
            row.created = Some(stamp());
            row.reason = Some(reason.unwrap_or("Banned by an operator.").to_string());
            // What vanilla writes, and what it shows the banned player. Geode
            // rather than Server, because Geode made this edit and the server
            // was not even running.
            row.rest
                .insert("source".into(), serde_json::Value::String("Geode".into()));
            row.rest
                .insert("expires".into(), serde_json::Value::String("forever".into()));
        }
        rows.push(row);
        write_file(self.paths, id, list, &rows).await?;
        Ok(Change {
            via: Via::File,
            said: Vec::new(),
            lists: self.lists(id).await?,
        })
    }

    /// Take somebody off one of these lists.
    ///
    /// `uuid` is the key the row carries on screen, and while the server is
    /// stopped it is what the row is found by. Three reasons, all of them the
    /// file's: it is keyed by UUID, so that is the only thing that picks out one
    /// row; two accounts can be recorded under one name, and taking the stale
    /// one off must not take the current owner with it; and a name the server
    /// itself wrote is not something an admin typed, so it is not something to
    /// hold to what an admin may type — a Bedrock player bridged in by
    /// Floodgate is legitimately recorded as `.Steve`.
    ///
    /// Without a UUID — an address ban has none, and nor does a row written
    /// without one — the name is the only identity the file offers, and then
    /// every row under it goes.
    pub async fn remove(
        &self,
        id: &str,
        list: PlayerList,
        target: &str,
        uuid: Option<&str>,
    ) -> Result<Change> {
        if self.servers.is_running(id) {
            // A console command takes a name, and a row recorded without one
            // can only be named by the key it is keyed by — which is a thing
            // the file direction can do and this one cannot.
            if !list.is_addresses() && target.trim().is_empty() {
                return Err(Error::msg(format!(
                    "this row records no name, only a UUID, and a console command takes \
                     a name. Stop the server and Geode can take it out of {} by that \
                     UUID.",
                    list.file_name()
                )));
            }
            // Held to what a console line can carry, not to what an admin may
            // type: this name came out of the file, and the file's names are
            // the server's own.
            let target = self.check_console_target(list, target)?;
            let verb = match list {
                PlayerList::Whitelist => "whitelist remove",
                PlayerList::Ops => "deop",
                PlayerList::Bans => "pardon",
                PlayerList::IpBans => "pardon-ip",
            };
            return self
                .through_console(id, list, false, &format!("{verb} {target}"))
                .await;
        }

        let uuid = uuid.map(str::trim).filter(|u| !u.is_empty());
        let rows = read_file(self.paths, id, list).await?;
        let before = rows.len();
        // What a refusal calls the row, alongside the rows that survive it: the
        // name it showed, or the UUID when the file recorded no name for it.
        let (shown, keep): (String, Vec<Stored>) = if list.is_addresses() {
            let target = check_ip(target)?;
            let keep = rows
                .into_iter()
                .filter(|r| !same_address(r.ip.as_deref(), &target))
                .collect();
            (target, keep)
        } else if let Some(uuid) = uuid {
            let keep = rows
                .into_iter()
                .filter(|r| !r.uuid.as_deref().is_some_and(|u| u.eq_ignore_ascii_case(uuid)))
                .collect();
            let shown = match target.trim() {
                "" => uuid.to_string(),
                name => name.to_string(),
            };
            (shown, keep)
        } else {
            let target = check_name(target)?;
            let keep = rows
                .into_iter()
                .filter(|r| {
                    !r.name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(&target))
                })
                .collect();
            (target, keep)
        };
        if keep.len() == before {
            return Err(Error::msg(format!("{shown} is not on that list")));
        }
        write_file(self.paths, id, list, &keep).await?;
        Ok(Change {
            via: Via::File,
            said: Vec::new(),
            lists: self.lists(id).await?,
        })
    }

    /// Put a player off the server now. Nothing is written anywhere: a kick
    /// lasts until they reconnect, so there is no list for it and no file to
    /// edit when the server is down.
    pub async fn kick(&self, id: &str, name: &str, reason: Option<&str>) -> Result<Change> {
        // Whoever is kicked is on the server now, so this name came out of its
        // own `/list` reply rather than out of a field: it is held to what a
        // console line can carry, not to what an admin may type.
        let name = check_console_name(name)?;
        let reason = check_reason(reason)?;
        if !self.servers.is_running(id) {
            return Err(Error::msg(
                "nobody can be kicked from a server that is not running",
            ));
        }
        let tree = super::probe::read(self.paths, id).await;
        if !Console::from_tree(&tree).kick {
            return Err(unknown_command("kick"));
        }
        let said = self.say(id, &with_reason("kick", &name, reason)).await?;
        Ok(Change {
            via: Via::Console,
            said,
            lists: self.lists(id).await?,
        })
    }

    /// Turn the whitelist on or off.
    ///
    /// The setting lives in `server.properties`, so a stopped server is simply
    /// a write. A running one is told as well, because the file is only read at
    /// start-up and an admin who turns the whitelist on means now.
    pub async fn set_whitelist(&self, id: &str, on: bool) -> Result<Change> {
        let mut via = Via::File;
        let mut said = Vec::new();

        if self.servers.is_running(id) {
            let tree = super::probe::read(self.paths, id).await;
            if !Console::from_tree(&tree).whitelist_toggle {
                return Err(unknown_command("whitelist on"));
            }
            said = self
                .say(id, if on { "whitelist on" } else { "whitelist off" })
                .await?;
            via = Via::Console;
        }

        // Written either way: the console applies it now, the file is what the
        // server reads next time it starts.
        properties::write(
            self.paths,
            id,
            &[Property {
                key: WHITELIST_KEY.to_string(),
                value: on.to_string(),
            }],
        )
        .await?;

        Ok(Change {
            via,
            said,
            lists: self.lists(id).await?,
        })
    }

    /// Send a command, having already established that the server has it, and
    /// refuse to call silence a result.
    async fn say(&self, id: &str, command: &str) -> Result<Vec<String>> {
        let said = self
            .servers
            .ask(id, command)
            .await
            .ok_or_else(|| Error::msg("this server is not running any more"))?;
        if said.is_empty() {
            return Err(Error::msg(format!(
                "“{command}” was sent to the console and the server said nothing back. \
                 Geode cannot tell whether it worked — check the console."
            )));
        }
        Ok(said.iter().map(|line| strip_log_prefix(line).to_string()).collect())
    }

    async fn through_console(
        &self,
        id: &str,
        list: PlayerList,
        adding: bool,
        command: &str,
    ) -> Result<Change> {
        let console = Console::from_tree(&super::probe::read(self.paths, id).await);
        let allowed = if adding {
            console.can_add(list)
        } else {
            console.can_remove(list)
        };
        if !allowed {
            return Err(unknown_command(command.split(' ').next().unwrap_or(command)));
        }
        let said = self.say(id, command).await?;
        Ok(Change {
            via: Via::Console,
            said,
            lists: self.lists(id).await?,
        })
    }

    fn check_target(&self, list: PlayerList, target: &str) -> Result<String> {
        if list.is_addresses() {
            return check_ip(target);
        }
        check_name(target)
    }

    /// The same, for a target the server itself produced rather than one an
    /// admin typed. An address is still an address either way.
    fn check_console_target(&self, list: PlayerList, target: &str) -> Result<String> {
        if list.is_addresses() {
            return check_ip(target);
        }
        check_console_name(target)
    }

    /// Turn a name into the UUID this server's files are keyed by.
    async fn resolve(&self, id: &str, name: &str) -> Result<Identity> {
        let props = properties::read(self.paths, id).await?;
        let online = props
            .iter()
            .find(|p| p.key == "online-mode")
            .map(|p| p.value.trim().to_ascii_lowercase())
            .as_deref()
            != Some("false");
        if !online {
            // `online-mode=false` does not mean the UUIDs are derived from
            // names: the usual reason a backend server has it off is a Velocity
            // or BungeeCord proxy in front, which authenticates the player and
            // forwards the Mojang UUID it got. So what this server keys a
            // player by is asked of this server — its own profile cache is the
            // record of what it used, whichever of the two that was.
            if let Some(who) = cached_profile(self.paths, id, name).await {
                return Ok(who);
            }
            // Nobody this server has ever seen. Then it is not a lookup and not
            // a guess: this is the UUID vanilla itself derives for a player on
            // a server with authentication off, and the one that server will
            // write into its own files.
            return Ok(Identity {
                uuid: crate::auth::offline_uuid(name),
                name: name.to_string(),
            });
        }
        resolve_online(self.http, name).await
    }
}

/// What this server's own profile cache recorded for a name it has seen.
///
/// `usercache.json` is vanilla's `GameProfileCache` on disk, in the same run
/// directory as the four lists, and it is the only local account of how this
/// server actually keyed a player: the UUID in it is the one the server used
/// the last time that player connected, under whatever authentication and
/// behind whatever proxy. Vanilla's own `/whitelist add` resolves through this
/// cache, which is why the two agree about a name the server has seen.
///
/// The casing comes with it, and matters: the offline derivation is
/// case-sensitive, and the client spells the name the way the player does
/// rather than the way an admin types it.
///
/// `expiresOn` is not read. A stale row still records the key this server used
/// for that player, which is the only question being asked of it.
///
/// `None` for a cache that has never seen the name, and for one that cannot be
/// read — a truncated or hand-edited cache is a reason to fall back to the
/// derived UUID, not to refuse a whitelist entry.
async fn cached_profile(paths: &Paths, id: &str, name: &str) -> Option<Identity> {
    #[derive(Deserialize)]
    struct Cached {
        name: String,
        uuid: String,
    }

    let path = paths.server_run(id).join("usercache.json");
    let text = tokio::fs::read_to_string(&path).await.ok()?;
    let cached: Vec<Cached> = serde_json::from_str(&text).ok()?;
    cached.into_iter().find_map(|row| {
        if !row.name.eq_ignore_ascii_case(name) {
            return None;
        }
        // A hand-edited cache can hold anything, and what goes into a file
        // keyed by UUID has to be one, spelled the way these files spell it.
        let uuid = uuid::Uuid::parse_str(&row.uuid).ok()?;
        Some(Identity {
            name: row.name,
            uuid: uuid.to_string(),
        })
    })
}

/// Copy for a command this server never reported having.
fn unknown_command(command: &str) -> Error {
    Error::msg(format!(
        "Geode has not seen a `{command}` command on this server, so it will not guess \
         at one. Stop the server and change the list directly, or start it once with \
         \"Read commands\" on in Settings so Geode learns what it has."
    ))
}

/// Ask Mojang who owns a name, for a server that authenticates its players.
///
/// The one place in this module that needs the network, and the one that cannot
/// be unit-tested offline — see `tests/live_server.rs`.
pub async fn resolve_online(client: &reqwest::Client, name: &str) -> Result<Identity> {
    let name = check_name(name)?;
    from_mojang(client, &name).await
}

/// Ask Mojang who owns a name.
async fn from_mojang(client: &reqwest::Client, name: &str) -> Result<Identity> {
    #[derive(Deserialize)]
    struct Profile {
        id: String,
        name: String,
    }

    let url = format!("{LOOKUP}{name}");
    let resp = client
        .get(&url)
        .send()
        .await
        .ctx(format!("asking Mojang who “{name}” is"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(Error::msg(format!(
            "Mojang has no Minecraft account called “{name}”. These files are keyed by \
             UUID, so a name has to be resolved before it can go in one — check the \
             spelling."
        )));
    }
    let status = resp.status();
    if !status.is_success() {
        return Err(Error::msg(format!(
            "Mojang could not be asked who “{name}” is (HTTP {status}). Nothing was changed."
        )));
    }
    let profile: Profile = resp
        .json()
        .await
        .ctx(format!("reading Mojang's answer about “{name}”"))?;
    Ok(Identity {
        // Mojang's spelling, not the admin's: this is the name the file should
        // record, and it is the one the server would have written itself.
        name: profile.name,
        uuid: crate::auth::store::dash_uuid(&profile.id),
    })
}

/// A Minecraft name, or a refusal saying what one looks like.
///
/// Also the gate on what a typed name reaches a URL path as: one with a slash
/// in it would land somewhere else entirely at Mojang. A name the *server*
/// produced is a different question — see [`check_console_name`].
fn check_name(name: &str) -> Result<String> {
    let name = name.trim();
    let ok = (3..=16).contains(&name.chars().count())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        return Err(Error::msg(format!(
            "“{name}” is not a Minecraft name — those are 3 to 16 letters, numbers or \
             underscores."
        )));
    }
    Ok(name.to_string())
}

/// A name the server itself produced, held only to what a console line can
/// carry.
///
/// Not every name on these screens was typed. A name off a `/list` reply or out
/// of one of the four files is the server's own, and the server's vocabulary is
/// wider than Mojang's: a Bedrock player bridged in by Floodgate is recorded
/// and reported as `.Steve` — the dot is Floodgate's default `username-prefix`
/// — and a gamertag can be longer than a Minecraft name may be. Holding those
/// to [`check_name`] refuses to kick, ban or unlist somebody the screen is
/// showing, when the command the server itself takes would have worked.
///
/// What is refused is what the console could not read as one name: the
/// characters outside a Brigadier unquoted word, a space among them, which
/// would arrive as a second argument or a second command.
fn check_console_name(name: &str) -> Result<String> {
    let name = name.trim();
    let word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+');
    if name.is_empty() || !name.chars().all(word) {
        return Err(Error::msg(format!(
            "“{name}” cannot be sent to this server's console as one name — it would be \
             read as more than one argument."
        )));
    }
    Ok(name.to_string())
}

/// An IP address, spelled the way the server spells it.
fn check_ip(ip: &str) -> Result<String> {
    let ip = ip.trim();
    ip.parse::<IpAddr>()
        .map(java_address)
        .map_err(|_| Error::msg(format!("“{ip}” is not an IP address")))
}

/// An address as Java writes it, because `banned-ips.json` is Java's file.
///
/// `Inet6Address.getHostAddress()` is what puts an address in that file and
/// what the server looks one up by, and it never compresses a run of zero
/// groups the way Rust's `Display` does. A ban Geode wrote as `2001:db8::1` is
/// one the server would never match against `2001:db8:0:0:0:0:0:1`, which is
/// the string it computes for the connection.
fn java_address(ip: IpAddr) -> String {
    let v6 = match ip {
        IpAddr::V4(v4) => return v4.to_string(),
        IpAddr::V6(v6) => v6,
    };
    match v6.to_ipv4_mapped() {
        // Java hands a v4-mapped address back as an `Inet4Address`, so this is
        // the dotted form in its file too.
        Some(v4) => v4.to_string(),
        None => v6
            .segments()
            .iter()
            .map(|group| format!("{group:x}"))
            .collect::<Vec<_>>()
            .join(":"),
    }
}

/// Whether a row's recorded address is this one, compared as addresses rather
/// than as text.
///
/// The two spellings of one IPv6 address are both real: the server writes it
/// uncompressed, and anything that has been through Rust writes it compressed.
/// Comparing the strings leaves a ban on screen with no way to lift it.
fn same_address(recorded: Option<&str>, target: &str) -> bool {
    let parse = |s: &str| s.trim().parse::<IpAddr>().ok();
    match (recorded.and_then(parse), parse(target)) {
        (Some(recorded), Some(target)) => recorded == target,
        _ => false,
    }
}

/// A reason an admin typed, or nothing.
///
/// A newline here would end the console line and start a second command, so a
/// reason that carries one is refused rather than quietly cut.
fn check_reason(reason: Option<&str>) -> Result<Option<&str>> {
    let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if reason.chars().any(char::is_control) {
        return Err(Error::msg(
            "a reason has to be one line of plain text — it is sent to the server's \
             console as typed.",
        ));
    }
    Ok(Some(reason))
}

fn with_reason(verb: &str, target: &str, reason: Option<&str>) -> String {
    match reason {
        Some(reason) => format!("{verb} {target} {reason}"),
        None => format!("{verb} {target}"),
    }
}

/// The timestamp format Minecraft writes into these files.
fn stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S %z").to_string()
}

fn file(paths: &Paths, id: &str, list: PlayerList) -> PathBuf {
    paths.server_run(id).join(list.file_name())
}

/// One of the four files, or nothing when the server has never written it.
async fn read_file(paths: &Paths, id: &str, list: PlayerList) -> Result<Vec<Stored>> {
    let path = file(paths, id, list);
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    // An empty file is what a server that has never had an entry leaves.
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|e| Error::json(list.file_name(), e))
}

/// Replace one of the four files, whole.
///
/// Written through [`super::write_atomic`] because the server reads these at
/// start-up: a truncated `whitelist.json` found half-written would read as a
/// whitelist with nobody on it, which on a whitelisted server locks everybody
/// out.
async fn write_file(paths: &Paths, id: &str, list: PlayerList, rows: &[Stored]) -> Result<()> {
    let path = file(paths, id, list);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    super::write_atomic(&path, &serde_json::to_vec_pretty(rows)?).await
}

/// Drop a log line's `[12:00:00] [Server thread/INFO]:` front, so what is left
/// is what the server actually said.
fn strip_log_prefix(line: &str) -> &str {
    let mut rest = line.trim();
    while rest.starts_with('[') {
        let Some(end) = rest.find(']') else { break };
        rest = rest[end + 1..].trim_start();
    }
    rest.strip_prefix(':').unwrap_or(rest).trim_start()
}

/// Pull the names out of a `/list` reply.
///
/// Vanilla answers on one line — "There are 2 of a max of 20 players online:
/// alice, bob" — and so do its forks. Anything else comes back `None`, and the
/// screen shows the reply instead of a list it invented.
pub fn read_online(said: &[String]) -> Option<Vec<String>> {
    const MARKER: &str = "players online:";
    said.iter().find_map(|line| {
        // Lowercased only to find the marker, and `to_ascii_lowercase` keeps
        // byte lengths, so the index is still valid in the original.
        let at = line.to_ascii_lowercase().find(MARKER)? + MARKER.len();
        Some(
            line[at..]
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect(),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::Poll;

    use tokio::sync::{broadcast, mpsc};

    use super::*;
    use crate::paths::Scratch;

    /// A command tree shaped like a vanilla-derived server's: the real names,
    /// trimmed to what this module reads.
    fn vanilla_tree() -> Vec<CommandNode> {
        let node = |name: &str, children: &[&str]| CommandNode {
            name: name.to_string(),
            children: children
                .iter()
                .map(|c| CommandNode {
                    name: c.to_string(),
                    children: Vec::new(),
                    args: Vec::new(),
                })
                .collect(),
            args: vec!["targets".to_string()],
        };
        vec![
            node("ban", &[]),
            node("ban-ip", &[]),
            node("banlist", &["ips", "players"]),
            node("deop", &[]),
            node("kick", &[]),
            node("list", &["uuids"]),
            node("op", &[]),
            node("pardon", &[]),
            node("pardon-ip", &[]),
            node("whitelist", &["add", "list", "off", "on", "reload", "remove"]),
        ]
    }

    fn manager<'a>(paths: &'a Paths, servers: &'a ServerRegistry, http: &'a reqwest::Client) -> Manager<'a> {
        Manager { paths, servers, http }
    }

    async fn write_run_file(paths: &Paths, id: &str, name: &str, body: &str) {
        let run = paths.server_run(id);
        tokio::fs::create_dir_all(&run).await.unwrap();
        tokio::fs::write(run.join(name), body).await.unwrap();
    }

    // -- what the console can be asked ------------------------------------

    #[test]
    fn a_vanilla_tree_offers_every_list() {
        let console = Console::from_tree(&vanilla_tree());
        assert!(console.known);
        assert!(console.whitelist_add && console.whitelist_remove && console.whitelist_toggle);
        assert!(console.op && console.deop);
        assert!(console.ban && console.pardon && console.ban_ip && console.pardon_ip);
        assert!(console.kick && console.who_is_online);
    }

    /// A proxy hosts no world, so it has no whitelist and no ops — and its tree
    /// says so. Nothing may be offered that the server would not understand.
    #[test]
    fn a_proxy_that_has_no_whitelist_is_not_offered_one() {
        let tree = vec![CommandNode {
            name: "end".into(),
            children: Vec::new(),
            args: Vec::new(),
        }];
        let console = Console::from_tree(&tree);
        assert!(console.known, "a tree was read, it just has no whitelist");
        assert!(!console.whitelist_add);
        assert!(!console.whitelist_toggle);
        assert!(!console.op);
        assert!(!console.who_is_online);
    }

    /// A server that has never run has no tree, and the rule is to offer
    /// nothing rather than assume it is vanilla.
    #[test]
    fn no_tree_means_nothing_is_offered() {
        assert_eq!(Console::from_tree(&[]), Console::default());
        assert!(!Console::from_tree(&[]).known);
    }

    /// `whitelist` with no `on` is Velocity-shaped: addable but not toggleable.
    /// Each subcommand is read for itself rather than inferred from the parent.
    #[test]
    fn each_whitelist_subcommand_is_asked_for_separately() {
        let tree = vec![CommandNode {
            name: "whitelist".into(),
            children: vec![CommandNode {
                name: "add".into(),
                children: Vec::new(),
                args: Vec::new(),
            }],
            args: Vec::new(),
        }];
        let console = Console::from_tree(&tree);
        assert!(console.whitelist_add);
        assert!(!console.whitelist_remove);
        assert!(!console.whitelist_toggle);
    }

    // -- reading the files -------------------------------------------------

    #[tokio::test]
    async fn the_lists_are_read_with_both_the_name_and_the_uuid_the_file_is_keyed_by() {
        let scratch = Scratch::new("players-read");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();

        write_run_file(
            &paths,
            "survival",
            "whitelist.json",
            r#"[{"uuid":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Notch"}]"#,
        )
        .await;
        write_run_file(
            &paths,
            "survival",
            "ops.json",
            r#"[{"uuid":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Notch","level":4,"bypassesPlayerLimit":false}]"#,
        )
        .await;
        write_run_file(
            &paths,
            "survival",
            "banned-players.json",
            r#"[{"uuid":"853c80ef-3c37-49fd-aa49-938b674adae6","name":"jeb_","created":"2026-10-01 10:00:00 +0000","source":"Server","expires":"forever","reason":"griefing"}]"#,
        )
        .await;
        write_run_file(
            &paths,
            "survival",
            "banned-ips.json",
            r#"[{"ip":"203.0.113.9","created":"2026-10-01 10:00:00 +0000","source":"Server","expires":"forever","reason":"Banned by an operator."}]"#,
        )
        .await;
        write_run_file(&paths, "survival", "server.properties", "white-list=true\n").await;

        let lists = manager(&paths, &servers, &http)
            .lists("survival")
            .await
            .unwrap();

        assert_eq!(lists.whitelist[0].name, "Notch");
        assert_eq!(
            lists.whitelist[0].uuid,
            "069a79f4-44e9-4726-a5be-fca90e38aaf5",
            "the key the file actually uses has to reach the screen"
        );
        assert_eq!(lists.ops[0].level, Some(4));
        assert_eq!(lists.bans[0].reason.as_deref(), Some("griefing"));
        assert_eq!(lists.ip_bans[0].ip.as_deref(), Some("203.0.113.9"));
        assert!(lists.whitelist_on);
        assert!(lists.online_mode, "no online-mode line means online mode");
        assert!(!lists.running);
    }

    #[tokio::test]
    async fn a_server_that_has_never_run_has_empty_lists_rather_than_an_error() {
        let scratch = Scratch::new("players-fresh");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();

        let lists = manager(&paths, &servers, &http)
            .lists("survival")
            .await
            .unwrap();

        assert!(lists.whitelist.is_empty() && lists.ops.is_empty());
        assert!(lists.bans.is_empty() && lists.ip_bans.is_empty());
        assert!(!lists.whitelist_on);
        assert!(!lists.console.known, "nothing is offered without a tree");
    }

    // -- changing the files while the server is stopped --------------------

    #[tokio::test]
    async fn an_offline_mode_server_resolves_a_name_without_asking_mojang() {
        let scratch = Scratch::new("players-offline");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();
        write_run_file(&paths, "survival", "server.properties", "online-mode=false\n").await;

        let change = manager(&paths, &servers, &http)
            .add("survival", PlayerList::Whitelist, "Notch", None, None)
            .await
            .expect("no network should be needed for an offline-mode server");

        assert_eq!(change.via, Via::File);
        assert!(change.said.is_empty());
        // The UUID vanilla itself derives, which is what that server will key
        // the player by.
        assert_eq!(
            change.lists.whitelist[0].uuid,
            crate::auth::offline_uuid("Notch")
        );
        assert_eq!(change.lists.whitelist[0].name, "Notch");
    }

    /// `online-mode=false` is not the same question as "derive the UUID from
    /// the name". The usual reason a backend server has authentication off is a
    /// Velocity or BungeeCord proxy in front of it, which authenticates the
    /// player itself and forwards the Mojang UUID — and the server records what
    /// it used in its own profile cache. Keying a row by a UUID derived from
    /// the name instead is a whitelist entry that never matches anybody, an op
    /// with no powers, and a ban that keeps nobody out.
    #[tokio::test]
    async fn an_offline_mode_server_keys_a_player_by_what_its_own_profile_cache_recorded() {
        let scratch = Scratch::new("players-usercache");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();
        write_run_file(&paths, "survival", "server.properties", "online-mode=false\n").await;
        // What a proxied backend's cache holds: a v4 UUID Mojang issued, next
        // to the name as the player spells it.
        write_run_file(
            &paths,
            "survival",
            "usercache.json",
            r#"[{"name":"littlesquirt1","uuid":"5de852d7-a45b-499a-9f7a-56dd8a149238",
                 "expiresOn":"2022-04-01 19:57:48 -0400"}]"#,
        )
        .await;

        let manager = manager(&paths, &servers, &http);
        let change = manager
            .add("survival", PlayerList::Whitelist, "LittleSquirt1", None, None)
            .await
            .unwrap();

        let row = &change.lists.whitelist[0];
        assert_eq!(
            row.uuid, "5de852d7-a45b-499a-9f7a-56dd8a149238",
            "the key this server itself used is the key the row has to carry"
        );
        assert_ne!(
            row.uuid,
            crate::auth::offline_uuid("littlesquirt1"),
            "deriving it from the name is exactly the wrong answer here"
        );
        assert_eq!(
            row.name, "littlesquirt1",
            "the casing the server saw, not the casing the admin typed"
        );

        // Nobody the server has ever seen. Then there is nothing to read, and
        // the derived UUID is what that server would write itself.
        let change = manager
            .add("survival", PlayerList::Whitelist, "jeb_", None, None)
            .await
            .unwrap();
        assert_eq!(
            change.lists.whitelist[1].uuid,
            crate::auth::offline_uuid("jeb_")
        );
    }

    /// The file is the server's, so the fields this module does not read have
    /// to survive a rewrite of it.
    #[tokio::test]
    async fn rewriting_ops_keeps_the_fields_geode_does_not_read() {
        let scratch = Scratch::new("players-keep");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();
        write_run_file(&paths, "survival", "server.properties", "online-mode=false\n").await;
        write_run_file(
            &paths,
            "survival",
            "ops.json",
            r#"[{"uuid":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Notch","level":4,"bypassesPlayerLimit":true}]"#,
        )
        .await;

        manager(&paths, &servers, &http)
            .add("survival", PlayerList::Ops, "jeb_", None, Some(2))
            .await
            .unwrap();

        let text = tokio::fs::read_to_string(paths.server_run("survival").join("ops.json"))
            .await
            .unwrap();
        assert!(
            text.contains("\"bypassesPlayerLimit\": true"),
            "a field Geode does not model must still be there: {text}"
        );
        let rows = read_file(&paths, "survival", PlayerList::Ops).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].level, Some(2), "the level asked for is the level written");
    }

    #[tokio::test]
    async fn a_ban_written_to_the_file_carries_a_reason_and_a_date() {
        let scratch = Scratch::new("players-ban");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();
        write_run_file(&paths, "survival", "server.properties", "online-mode=false\n").await;

        let change = manager(&paths, &servers, &http)
            .add(
                "survival",
                PlayerList::Bans,
                "Notch",
                Some("griefing spawn"),
                None,
            )
            .await
            .unwrap();

        let ban = &change.lists.bans[0];
        assert_eq!(ban.reason.as_deref(), Some("griefing spawn"));
        assert!(ban.created.is_some(), "a ban records when it was given");
        assert!(!ban.uuid.is_empty(), "a ban is keyed by UUID like the rest");
    }

    #[tokio::test]
    async fn an_ip_ban_takes_an_address_and_refuses_anything_else() {
        let scratch = Scratch::new("players-ip");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();

        let manager = manager(&paths, &servers, &http);
        let change = manager
            .add("survival", PlayerList::IpBans, "203.0.113.9", None, None)
            .await
            .unwrap();
        assert_eq!(change.lists.ip_bans[0].ip.as_deref(), Some("203.0.113.9"));

        let refused = manager
            .add("survival", PlayerList::IpBans, "Notch", None, None)
            .await
            .expect_err("a name is not an address");
        assert!(refused.to_string().contains("not an IP address"));
    }

    #[tokio::test]
    async fn removing_somebody_who_is_not_on_the_list_says_so() {
        let scratch = Scratch::new("players-missing");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();

        let refused = manager(&paths, &servers, &http)
            .remove("survival", PlayerList::Whitelist, "Notch", None)
            .await
            .expect_err("nothing was removed, so nothing may be reported as removed");
        assert!(refused.to_string().contains("not on that list"));
    }

    #[tokio::test]
    async fn removing_from_the_whitelist_leaves_everybody_else() {
        let scratch = Scratch::new("players-remove");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();
        write_run_file(
            &paths,
            "survival",
            "whitelist.json",
            r#"[{"uuid":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Notch"},
                {"uuid":"853c80ef-3c37-49fd-aa49-938b674adae6","name":"jeb_"}]"#,
        )
        .await;

        let change = manager(&paths, &servers, &http)
            .remove("survival", PlayerList::Whitelist, "notch", None)
            .await
            .unwrap();

        assert_eq!(
            change
                .lists
                .whitelist
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["jeb_"],
            "a name is matched however it was capitalised, and only that one goes"
        );
    }

    /// The file is keyed by UUID, and a Minecraft name can change hands: two
    /// rows can stand under one name, and taking the stale one off must not
    /// take the current owner with it. A row the file recorded with no name at
    /// all has nothing but that key to be found by.
    #[tokio::test]
    async fn a_row_is_taken_off_by_the_uuid_the_file_keys_it_by() {
        let scratch = Scratch::new("players-remove-uuid");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();
        write_run_file(
            &paths,
            "survival",
            "whitelist.json",
            r#"[{"uuid":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Notch"},
                {"uuid":"853c80ef-3c37-49fd-aa49-938b674adae6","name":"Notch"},
                {"uuid":"3c7e6a1c-0d20-4a1d-9f9a-5c1a1b2c3d4e"}]"#,
        )
        .await;

        let manager = manager(&paths, &servers, &http);
        let change = manager
            .remove(
                "survival",
                PlayerList::Whitelist,
                "Notch",
                Some("853c80ef-3c37-49fd-aa49-938b674adae6"),
            )
            .await
            .unwrap();
        assert_eq!(
            change
                .lists
                .whitelist
                .iter()
                .map(|e| e.uuid.as_str())
                .collect::<Vec<_>>(),
            vec![
                "069a79f4-44e9-4726-a5be-fca90e38aaf5",
                "3c7e6a1c-0d20-4a1d-9f9a-5c1a1b2c3d4e"
            ],
            "only the row that key belongs to goes, not the other account under that name"
        );

        // Nothing on screen but the key, so the key is what comes back.
        let change = manager
            .remove(
                "survival",
                PlayerList::Whitelist,
                "",
                Some("3c7e6a1c-0d20-4a1d-9f9a-5c1a1b2c3d4e"),
            )
            .await
            .unwrap();
        assert_eq!(change.lists.whitelist.len(), 1);
        let refused = manager
            .remove(
                "survival",
                PlayerList::Whitelist,
                "",
                Some("3c7e6a1c-0d20-4a1d-9f9a-5c1a1b2c3d4e"),
            )
            .await
            .expect_err("it has already gone");
        assert!(
            refused
                .to_string()
                .starts_with("3c7e6a1c-0d20-4a1d-9f9a-5c1a1b2c3d4e"),
            "a row with no name is named by its key, not by a blank: {refused}"
        );
    }

    #[tokio::test]
    async fn the_whitelist_toggle_is_written_to_the_properties_file() {
        let scratch = Scratch::new("players-toggle");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();
        write_run_file(
            &paths,
            "survival",
            "server.properties",
            "#Minecraft server properties\nwhite-list=false\nmotd=Hi\n",
        )
        .await;

        let change = manager(&paths, &servers, &http)
            .set_whitelist("survival", true)
            .await
            .unwrap();

        assert_eq!(change.via, Via::File);
        assert!(change.lists.whitelist_on);
        let text =
            tokio::fs::read_to_string(paths.server_run("survival").join("server.properties"))
                .await
                .unwrap();
        assert!(text.contains("white-list=true"));
        assert!(text.contains("motd=Hi"), "the rest of the file is untouched");
    }

    #[tokio::test]
    async fn nobody_is_kicked_from_a_server_that_is_not_running() {
        let scratch = Scratch::new("players-kick");
        let paths = scratch.paths.clone();
        let servers = ServerRegistry::default();
        let http = crate::net::client();

        let refused = manager(&paths, &servers, &http)
            .kick("survival", "Notch", None)
            .await
            .expect_err("there is nobody on a stopped server");
        assert!(refused.to_string().contains("not running"));
    }

    /// A server can come up after the direction has been chosen and before the
    /// file is written: a scheduled task, or somebody clicking Start while a
    /// name is being resolved. From that moment the file is the running
    /// server's — it is holding the list in memory and would write Geode's row
    /// straight back out — so the direction is checked again where the write
    /// happens, and the admin is told rather than told it worked.
    #[test]
    fn a_server_that_starts_mid_change_has_its_file_left_alone() {
        // One blocking thread, held by this test, so the first file read inside
        // `add` parks at a known point rather than at a lucky one: the server
        // is started while that read is still queued, which is after the
        // direction was chosen and before anything has been written.
        let rt = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let scratch = Scratch::new("players-mid-change");
            let paths = scratch.paths.clone();
            let servers = ServerRegistry::default();
            let http = crate::net::client();
            write_run_file(&paths, "survival", "server.properties", "online-mode=false\n").await;

            let (release, held) = std::sync::mpsc::channel::<()>();
            let holding = tokio::task::spawn_blocking(move || {
                let _ = held.recv();
            });

            let manager = manager(&paths, &servers, &http);
            let mut adding =
                Box::pin(manager.add("survival", PlayerList::Whitelist, "Notch", None, None));
            let parked =
                std::future::poll_fn(|cx| Poll::Ready(adding.as_mut().poll(cx))).await;
            assert!(
                parked.is_pending(),
                "the change should be waiting on the file read"
            );

            let (_typed, _console) = servers.fake_start("survival");
            release.send(()).unwrap();
            holding.await.unwrap();

            let refused = adding
                .await
                .expect_err("nothing may be written behind a running server's back");
            assert!(
                refused.to_string().contains("started while"),
                "the admin has to be told why: {refused}"
            );
            assert!(
                tokio::fs::metadata(paths.server_run("survival").join("whitelist.json"))
                    .await
                    .is_err(),
                "the file is the running server's now, so Geode must not have written it"
            );
        });
    }

    // -- changing them through the console while the server runs -----------

    /// A Brigadier tree in the shape the probe writes one, so
    /// [`super::probe::read`] is the thing under test alongside the rest.
    async fn write_command_tree(paths: &Paths, id: &str) {
        let dir = paths.server(id);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            super::super::probe::tree_file(paths, id),
            r#"{"type":"root","children":{
                 "ban":{"type":"literal"},
                 "ban-ip":{"type":"literal"},
                 "deop":{"type":"literal"},
                 "kick":{"type":"literal"},
                 "list":{"type":"literal"},
                 "op":{"type":"literal"},
                 "pardon":{"type":"literal"},
                 "pardon-ip":{"type":"literal"},
                 "whitelist":{"type":"literal","children":{
                    "add":{"type":"literal"},
                    "remove":{"type":"literal"},
                    "on":{"type":"literal"},
                    "off":{"type":"literal"}}}}}"#,
        )
        .await
        .unwrap();
    }

    /// Answer one console command the way a server does: take the line Geode
    /// wrote, say something back, and hand the line over to be checked.
    ///
    /// Under a timeout so that a change refused before anything was sent fails
    /// the test rather than waiting for a line that is never coming. Time is
    /// paused in these tests, so it is spent instantly.
    async fn answer(
        typed: &mut mpsc::UnboundedReceiver<String>,
        console: &broadcast::Sender<String>,
        reply: &str,
    ) -> String {
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), typed.recv())
            .await
            .expect("a command should have been written to the console")
            .expect("the console should still be open");
        let _ = console.send(format!("[12:00:00] [Server thread/INFO]: {reply}"));
        line
    }

    /// Floodgate's default `username-prefix` is a dot, so a Bedrock player
    /// bridged into a Paper server is `.Steve` in its `/list` reply and
    /// `.Steve` in its `whitelist.json`. That is a name no admin could have
    /// typed and the server's own nonetheless, and the console takes it
    /// happily: a dot is a legal Brigadier word. Holding it to what an admin
    /// may type left a griefing Bedrock player with no button on this screen
    /// that worked.
    #[tokio::test(start_paused = true)]
    async fn a_name_the_server_itself_reported_is_not_held_to_what_an_admin_may_type() {
        let scratch = Scratch::new("players-bedrock");
        let paths = scratch.paths.clone();
        let (servers, mut typed, console) = ServerRegistry::fake_running("survival");
        let http = crate::net::client();
        write_command_tree(&paths, "survival").await;
        let manager = manager(&paths, &servers, &http);

        let (kicked, line) = tokio::join!(
            manager.kick("survival", ".Steve", None),
            answer(&mut typed, &console, "Kicked .Steve: Kicked by an operator"),
        );
        assert_eq!(line, "kick .Steve\n");
        kicked.expect("the server reported this name, so it can be sent this name");

        let (banned, line) = tokio::join!(
            manager.add("survival", PlayerList::Bans, ".Steve", None, None),
            answer(&mut typed, &console, "Banned .Steve: Banned by an operator"),
        );
        assert_eq!(line, "ban .Steve\n");
        banned.expect("the same name, offered to ban beside the kick");

        let (removed, line) = tokio::join!(
            manager.remove("survival", PlayerList::Whitelist, ".Steve", None),
            answer(&mut typed, &console, "Removed .Steve from the whitelist"),
        );
        assert_eq!(line, "whitelist remove .Steve\n");
        removed.expect("and the row the whitelist pane shows for them");
    }

    /// The other half of that: a row recorded with no name cannot be named to a
    /// console that takes names, and saying so beats sending a blank.
    #[tokio::test(start_paused = true)]
    async fn a_row_with_no_name_cannot_be_removed_through_the_console() {
        let scratch = Scratch::new("players-nameless");
        let paths = scratch.paths.clone();
        let (servers, mut typed, _console) = ServerRegistry::fake_running("survival");
        let http = crate::net::client();
        write_command_tree(&paths, "survival").await;

        let refused = manager(&paths, &servers, &http)
            .remove(
                "survival",
                PlayerList::Whitelist,
                "",
                Some("3c7e6a1c-0d20-4a1d-9f9a-5c1a1b2c3d4e"),
            )
            .await
            .expect_err("there is no name to send");

        let text = refused.to_string();
        assert!(text.contains("records no name"), "{text}");
        assert!(text.contains("whitelist.json"), "{text}");
        assert!(typed.try_recv().is_err(), "nothing should have been sent");
    }

    /// A running server owns these files, so the change is typed into its
    /// console and the file is left for the server to write.
    #[tokio::test(start_paused = true)]
    async fn a_change_to_a_running_server_is_typed_into_its_console() {
        let scratch = Scratch::new("players-console");
        let paths = scratch.paths.clone();
        let (servers, mut typed, console) = ServerRegistry::fake_running("survival");
        let http = crate::net::client();
        write_command_tree(&paths, "survival").await;

        let answering = tokio::spawn(async move {
            let line = typed.recv().await.expect("a command should be written");
            assert_eq!(line, "whitelist add Notch\n");
            let _ = console.send(
                "[12:00:00] [Server thread/INFO]: Added Notch to the whitelist".to_string(),
            );
            typed
        });

        let change = manager(&paths, &servers, &http)
            .add("survival", PlayerList::Whitelist, "Notch", None, None)
            .await
            .unwrap();
        let mut typed = answering.await.unwrap();

        assert_eq!(change.via, Via::Console);
        assert_eq!(
            change.said,
            vec!["Added Notch to the whitelist".to_string()],
            "the server's own words, with the log furniture off"
        );
        assert!(
            tokio::fs::metadata(paths.server_run("survival").join("whitelist.json"))
                .await
                .is_err(),
            "the file is the running server's to write, not Geode's"
        );
        assert!(typed.try_recv().is_err(), "exactly one command was sent");
    }

    /// The rule the whole screen rests on: sent is not done.
    #[tokio::test(start_paused = true)]
    async fn a_console_command_the_server_never_answers_is_not_reported_as_done() {
        let scratch = Scratch::new("players-silent");
        let paths = scratch.paths.clone();
        let (servers, mut typed, _console) = ServerRegistry::fake_running("survival");
        let http = crate::net::client();
        write_command_tree(&paths, "survival").await;

        let refused = manager(&paths, &servers, &http)
            .add("survival", PlayerList::Bans, "Notch", Some("griefing"), None)
            .await
            .expect_err("silence is not a result");

        let text = refused.to_string();
        assert!(text.contains("said nothing"), "{text}");
        assert_eq!(
            typed.recv().await.as_deref(),
            Some("ban Notch griefing\n"),
            "it was sent — that is exactly why the silence has to be reported"
        );
    }

    /// No command tree means Geode does not know what this server has, and the
    /// rule is to offer nothing rather than guess it is vanilla.
    #[tokio::test(start_paused = true)]
    async fn a_running_server_whose_commands_are_unknown_is_sent_nothing() {
        let scratch = Scratch::new("players-unknown");
        let paths = scratch.paths.clone();
        let (servers, mut typed, _console) = ServerRegistry::fake_running("survival");
        let http = crate::net::client();

        let refused = manager(&paths, &servers, &http)
            .add("survival", PlayerList::Whitelist, "Notch", None, None)
            .await
            .expect_err("nothing may be sent to a server whose commands are unknown");

        assert!(refused.to_string().contains("has not seen"), "{refused}");
        assert!(
            typed.try_recv().is_err(),
            "no command should have reached the console"
        );
    }

    /// `/op` has no level argument, so a level asked for while the server runs
    /// would be silently dropped. It is refused instead.
    #[tokio::test(start_paused = true)]
    async fn an_operator_level_is_refused_while_the_server_is_running() {
        let scratch = Scratch::new("players-level");
        let paths = scratch.paths.clone();
        let (servers, mut typed, _console) = ServerRegistry::fake_running("survival");
        let http = crate::net::client();
        write_command_tree(&paths, "survival").await;

        let refused = manager(&paths, &servers, &http)
            .add("survival", PlayerList::Ops, "Notch", None, Some(2))
            .await
            .expect_err("a level cannot be set through the console");

        assert!(refused.to_string().contains("while the server is stopped"), "{refused}");
        assert!(typed.try_recv().is_err(), "nothing should have been sent");
    }

    /// The toggle has to reach both places: the console so it is true now, and
    /// the file so it is still true after a restart.
    #[tokio::test(start_paused = true)]
    async fn the_whitelist_toggle_on_a_running_server_is_sent_and_also_written() {
        let scratch = Scratch::new("players-toggle-live");
        let paths = scratch.paths.clone();
        let (servers, mut typed, console) = ServerRegistry::fake_running("survival");
        let http = crate::net::client();
        write_command_tree(&paths, "survival").await;
        write_run_file(&paths, "survival", "server.properties", "white-list=false\n").await;

        let answering = tokio::spawn(async move {
            let line = typed.recv().await.expect("a command should be written");
            assert_eq!(line, "whitelist on\n");
            let _ = console.send("[12:00:00] [Server thread/INFO]: Whitelist is now turned on".to_string());
        });

        let change = manager(&paths, &servers, &http)
            .set_whitelist("survival", true)
            .await
            .unwrap();
        answering.await.unwrap();

        assert_eq!(change.via, Via::Console);
        assert!(change.lists.whitelist_on);
        let text =
            tokio::fs::read_to_string(paths.server_run("survival").join("server.properties"))
                .await
                .unwrap();
        assert!(
            text.contains("white-list=true"),
            "the file is what the server reads next time it starts: {text}"
        );
    }

    // -- validation --------------------------------------------------------

    #[test]
    fn a_name_that_could_not_be_a_minecraft_name_is_refused_before_anything_happens() {
        for bad in ["", "ab", "a".repeat(17).as_str(), "has space", "../etc", "sl/ash"] {
            assert!(check_name(bad).is_err(), "{bad:?} should be refused");
        }
        assert_eq!(check_name("  Notch  ").unwrap(), "Notch");
        assert_eq!(check_name("jeb_").unwrap(), "jeb_");
    }

    /// `banned-ips.json` is Java's file, and `Inet6Address.getHostAddress()`
    /// never compresses a run of zero groups the way Rust's `Display` does. The
    /// two spellings of one address have to be written and matched as one, or a
    /// ban sits on the screen with no way to lift it.
    #[test]
    fn an_ipv6_address_is_spelled_and_matched_the_way_java_spells_it() {
        assert_eq!(check_ip("2001:db8::1").unwrap(), "2001:db8:0:0:0:0:0:1");
        assert_eq!(
            check_ip("::ffff:203.0.113.9").unwrap(),
            "203.0.113.9",
            "Java hands a v4-mapped address back as an Inet4Address"
        );
        assert!(same_address(Some("2001:db8:0:0:0:0:0:1"), "2001:db8::1"));
        assert!(!same_address(Some("2001:db8::2"), "2001:db8::1"));
        assert!(!same_address(None, "2001:db8::1"));
    }

    /// The looser rule for a name the server produced is still a rule: it is
    /// what a console line can carry as one argument, which is wider than a
    /// Minecraft name and narrower than any text at all.
    #[test]
    fn a_reported_name_a_console_line_could_not_carry_is_still_refused() {
        assert_eq!(check_console_name(".Steve").unwrap(), ".Steve");
        assert_eq!(
            check_console_name("  .A_Bedrock-Gamertag  ").unwrap(),
            ".A_Bedrock-Gamertag",
            "longer than a Minecraft name, and the server's own"
        );
        for bad in ["", "two words", "quote\"d", "semi;colon", "ends\nhere", "sl/ash"] {
            assert!(check_console_name(bad).is_err(), "{bad:?} should be refused");
        }
    }

    /// A reason goes onto a console line as typed, so a newline in it would end
    /// that line and start a command of its own.
    #[test]
    fn a_reason_carrying_a_newline_is_refused_rather_than_cut() {
        assert!(check_reason(Some("griefing\nop attacker")).is_err());
        assert_eq!(check_reason(Some("  griefing  ")).unwrap(), Some("griefing"));
        assert_eq!(check_reason(Some("   ")).unwrap(), None);
        assert_eq!(check_reason(None).unwrap(), None);
    }

    // -- reading what the server said --------------------------------------

    #[test]
    fn the_online_players_are_read_out_of_the_servers_own_answer() {
        let said = vec![
            "There are 2 of a max of 20 players online: alice, Bob_99".to_string(),
        ];
        assert_eq!(
            read_online(&said),
            Some(vec!["alice".to_string(), "Bob_99".to_string()])
        );
    }

    #[test]
    fn an_empty_server_answers_with_nobody_rather_than_unreadably() {
        let said = vec!["There are 0 of a max of 20 players online:".to_string()];
        assert_eq!(read_online(&said), Some(Vec::new()));
    }

    /// A plugin can replace `/list` entirely. Then there is no list to show,
    /// and inventing one would be worse than saying so.
    #[test]
    fn a_reply_that_is_not_a_player_list_is_not_turned_into_one() {
        let said = vec!["Unknown or incomplete command".to_string()];
        assert_eq!(read_online(&said), None);
    }

    #[test]
    fn the_log_furniture_comes_off_what_the_server_said() {
        assert_eq!(
            strip_log_prefix("[12:00:00] [Server thread/INFO]: Added alice to the whitelist"),
            "Added alice to the whitelist"
        );
        assert_eq!(
            strip_log_prefix("[12:00:00 INFO]: Made alice a server operator"),
            "Made alice a server operator"
        );
        assert_eq!(strip_log_prefix("plain text"), "plain text");
    }

    #[test]
    fn a_console_command_is_spelled_with_its_reason_only_when_there_is_one() {
        assert_eq!(with_reason("ban", "alice", Some("griefing")), "ban alice griefing");
        assert_eq!(with_reason("ban", "alice", None), "ban alice");
    }

    #[test]
    fn each_list_is_named_the_way_the_frontend_asks_for_it() {
        assert_eq!(PlayerList::parse("whitelist"), Some(PlayerList::Whitelist));
        assert_eq!(PlayerList::parse("ip-bans"), Some(PlayerList::IpBans));
        assert_eq!(PlayerList::parse("ops"), Some(PlayerList::Ops));
        assert_eq!(PlayerList::parse("bans"), Some(PlayerList::Bans));
        assert_eq!(PlayerList::parse("banned"), None);
    }
}

//! Minecraft servers Geode runs and manages.
//!
//! A server is close enough to an instance to feel familiar — a Minecraft
//! version, a piece of server software, a folder of content — and different
//! enough to deserve its own model. It has no account and no window; it has a
//! console, a port, a EULA that must be accepted before it will start, and
//! content that may be plugins, mods, or both at once.
//!
//! The "both at once" case is not an edge: Mohist, Arclight and Banner exist
//! precisely to load Bukkit plugins alongside Forge or Fabric mods, and a manager that
//! assumes one or the other cannot describe them.

pub mod backup;
pub mod from_pack;
pub mod from_plan;
pub mod git;
pub mod install;
pub mod installer;
pub mod logs;
pub mod meta;
pub mod probe;
pub mod properties;
pub mod run;
pub mod schedule;
pub mod share;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

/// The server software, which decides both where the jar comes from and what
/// kind of content the folder takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    /// Mojang's own server jar. No plugins, no mods.
    #[default]
    Vanilla,
    Paper,
    Folia,
    Purpur,
    Spigot,
    /// Proxies. They sit in front of other servers and take plugins of their
    /// own, but host no world themselves.
    Velocity,
    Waterfall,
    Fabric,
    Quilt,
    NeoForge,
    Forge,
    /// Hybrids: a modded server that also implements the Bukkit API.
    Mohist,
    Arclight,
    Banner,
}

impl ServerKind {
    /// Every kind, in the order a picker should offer them: what most people
    /// want first, the modded loaders next, hybrids and proxies last.
    pub const ALL: &'static [ServerKind] = &[
        ServerKind::Paper,
        ServerKind::Purpur,
        ServerKind::Folia,
        ServerKind::Spigot,
        ServerKind::Vanilla,
        ServerKind::Fabric,
        ServerKind::Quilt,
        ServerKind::NeoForge,
        ServerKind::Forge,
        ServerKind::Mohist,
        ServerKind::Arclight,
        ServerKind::Banner,
        ServerKind::Velocity,
        ServerKind::Waterfall,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ServerKind::Vanilla => "vanilla",
            ServerKind::Paper => "paper",
            ServerKind::Folia => "folia",
            ServerKind::Purpur => "purpur",
            ServerKind::Spigot => "spigot",
            ServerKind::Velocity => "velocity",
            ServerKind::Waterfall => "waterfall",
            ServerKind::Fabric => "fabric",
            ServerKind::Quilt => "quilt",
            ServerKind::NeoForge => "neoforge",
            ServerKind::Forge => "forge",
            ServerKind::Mohist => "mohist",
            ServerKind::Arclight => "arclight",
            ServerKind::Banner => "banner",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ServerKind::Vanilla => "Vanilla",
            ServerKind::Paper => "Paper",
            ServerKind::Folia => "Folia",
            ServerKind::Purpur => "Purpur",
            ServerKind::Spigot => "Spigot",
            ServerKind::Velocity => "Velocity",
            ServerKind::Waterfall => "Waterfall",
            ServerKind::Fabric => "Fabric",
            ServerKind::Quilt => "Quilt",
            ServerKind::NeoForge => "NeoForge",
            ServerKind::Forge => "Forge",
            ServerKind::Mohist => "Mohist",
            ServerKind::Arclight => "Arclight",
            ServerKind::Banner => "Banner",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "vanilla" => ServerKind::Vanilla,
            "paper" => ServerKind::Paper,
            "folia" => ServerKind::Folia,
            "purpur" => ServerKind::Purpur,
            "spigot" => ServerKind::Spigot,
            "velocity" => ServerKind::Velocity,
            "waterfall" => ServerKind::Waterfall,
            "fabric" => ServerKind::Fabric,
            "quilt" => ServerKind::Quilt,
            "neoforge" => ServerKind::NeoForge,
            "forge" => ServerKind::Forge,
            "mohist" => ServerKind::Mohist,
            "arclight" => ServerKind::Arclight,
            "banner" => ServerKind::Banner,
            _ => return None,
        })
    }

    /// Whether it loads Bukkit-style plugins from `plugins/`.
    pub fn takes_plugins(self) -> bool {
        matches!(
            self,
            ServerKind::Paper
                | ServerKind::Folia
                | ServerKind::Purpur
                | ServerKind::Spigot
                | ServerKind::Velocity
                | ServerKind::Waterfall
                | ServerKind::Mohist
                | ServerKind::Arclight
                | ServerKind::Banner
        )
    }

    /// Whether it loads mods from `mods/`.
    pub fn takes_mods(self) -> bool {
        matches!(
            self,
            ServerKind::Fabric
                | ServerKind::Quilt
                | ServerKind::NeoForge
                | ServerKind::Forge
                | ServerKind::Mohist
                | ServerKind::Arclight
                | ServerKind::Banner
        )
    }

    /// The loader name to ask a content provider for. Hybrids are asked as the
    /// mod loader they are built on, since that is what their mods are built
    /// for; their plugin side is searched separately as Bukkit.
    pub fn mod_loader(self) -> Option<&'static str> {
        Some(match self {
            ServerKind::Fabric | ServerKind::Banner => "fabric",
            ServerKind::Quilt => "quilt",
            ServerKind::NeoForge => "neoforge",
            ServerKind::Forge | ServerKind::Mohist | ServerKind::Arclight => "forge",
            _ => return None,
        })
    }

    /// The plugin platform to ask a provider for, most specific first: a Paper
    /// server runs Paper, Spigot and Bukkit plugins alike.
    pub fn plugin_loaders(self) -> &'static [&'static str] {
        match self {
            ServerKind::Paper | ServerKind::Mohist | ServerKind::Arclight | ServerKind::Banner => {
                &["paper", "spigot", "bukkit"]
            }
            ServerKind::Purpur => &["purpur", "paper", "spigot", "bukkit"],
            ServerKind::Folia => &["folia", "paper", "spigot", "bukkit"],
            ServerKind::Spigot => &["spigot", "bukkit"],
            ServerKind::Velocity => &["velocity"],
            ServerKind::Waterfall => &["waterfall", "bungeecord"],
            _ => &[],
        }
    }

    /// A proxy hosts no world of its own, so it has no level or difficulty to
    /// configure and nothing to back up but its configuration.
    pub fn is_proxy(self) -> bool {
        matches!(self, ServerKind::Velocity | ServerKind::Waterfall)
    }

    /// Proxies read their own config format and never write `eula.txt`.
    pub fn needs_eula(self) -> bool {
        !self.is_proxy()
    }
}

/// Per-server overrides and settings that are ours rather than Minecraft's.
/// Anything the game itself owns lives in `server.properties`, which is edited
/// as a file rather than mirrored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerSettings {
    pub memory_mb: u32,
    pub java_path: Option<String>,
    pub java_args: Option<String>,
    /// Restart automatically when the process exits without being asked to.
    pub auto_restart: bool,
    /// Keep this many rotated logs. `None` keeps everything.
    pub keep_logs: Option<u32>,
    /// Keep this many backups, deleting the oldest as new ones are taken.
    /// `None` keeps every backup ever made.
    pub keep_backups: Option<u32>,
    /// Let the command probe ride along on this server's JVM.
    ///
    /// On by default, because without it the console cannot know what commands
    /// a mod or plugin registered. Off is offered because this is the one place
    /// Geode puts its own code inside somebody else's server process, and
    /// "don't" is a reasonable thing to want to be able to say.
    pub read_commands: bool,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            memory_mb: 4096,
            java_path: None,
            java_args: None,
            auto_restart: false,
            keep_logs: Some(20),
            keep_backups: Some(10),
            read_commands: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub name: String,
    /// An uploaded image, named relative to the server's `media` folder.
    #[serde(default)]
    pub icon: Option<String>,
    /// The artwork across the top of the server's page.
    #[serde(default)]
    pub banner: Option<String>,
    pub kind: ServerKind,
    pub mc_version: String,
    /// The modpack this server was built from, when it was built from one.
    ///
    /// Exactly what an instance records, and the same type: a server made from
    /// a pack is as much that pack as an instance made from it, and should be
    /// able to say so — its name, its artwork, and a way back to its page.
    #[serde(default)]
    pub pack: Option<crate::instance::PackSource>,
    /// The build or loader version of the jar in place, for update checks.
    #[serde(default)]
    pub build: String,
    /// File name of the jar to launch, relative to the run directory.
    #[serde(default)]
    pub jar: String,
    /// How this server is started. Modern Forge and NeoForge produce no jar at
    /// all — they leave an argument file and are launched from that — so the
    /// jar name by itself cannot describe every server.
    #[serde(default)]
    pub launch: installer::LaunchKind,
    /// The Java major version this server needs, worked out when the jar was
    /// installed. Recorded rather than derived at start-up so starting a
    /// server does not depend on being online.
    #[serde(default)]
    pub java_major: Option<u32>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub last_started: Option<i64>,
    /// Accepting Mojang's EULA is the user's act, not ours. Recorded here so
    /// the question is asked once, before the first start rather than after a
    /// failed one.
    #[serde(default)]
    pub eula_accepted: bool,
    #[serde(default)]
    pub settings: ServerSettings,
    /// Provenance for installed plugins and mods, exactly as an instance keeps
    /// it — disk stays the source of truth.
    #[serde(default)]
    pub content: Vec<crate::instance::content::ContentRecord>,
    /// Backups and commands this server runs on a timer.
    #[serde(default)]
    pub schedules: Vec<schedule::Schedule>,
    #[serde(default)]
    pub notes: String,
    // Whether and how this server is shared is deliberately not here: see
    // `share.rs` for why it lives in a file of its own.
    /// `content` as it was when this copy was read or last saved, so a late
    /// save can tell a record its own job dropped from one another job saved
    /// while it waited. Never written.
    #[serde(skip)]
    content_as_loaded: Vec<crate::instance::content::ContentRecord>,
}

/// Cardboard implements the Bukkit API as a Fabric mod, which makes plugin
/// support a property of what a server has installed rather than only of what
/// software it runs.
pub const CARDBOARD_SLUG: &str = "cardboard";

impl Server {
    /// Whether this server loads Bukkit-style plugins.
    ///
    /// Asked of the server rather than the kind, because Cardboard turns a
    /// Fabric server into one that takes plugins without changing what it is.
    pub fn takes_plugins(&self) -> bool {
        self.kind.takes_plugins() || self.has_cardboard()
    }

    /// Whether this server loads mods. Nothing installable changes this, so it
    /// is still the kind's own answer.
    pub fn takes_mods(&self) -> bool {
        self.kind.takes_mods()
    }

    /// Whether the software is actually in place and this server can start.
    ///
    /// Not the same question as "does it have a jar". From 1.17 a Forge or
    /// NeoForge server has no jar whatsoever — its installer leaves an argument
    /// file instead — so asking about the jar alone calls a perfectly good
    /// server uninstalled and refuses to start it.
    pub fn is_installed(&self) -> bool {
        match &self.launch {
            installer::LaunchKind::Jar => !self.jar.is_empty(),
            installer::LaunchKind::ArgFile(file) => !file.is_empty(),
        }
    }

    /// Cardboard sitting in this server's mods folder.
    pub fn has_cardboard(&self) -> bool {
        self.content.iter().any(|c| {
            c.kind == crate::instance::content::ContentKind::Mod
                && c.slug.as_deref() == Some(CARDBOARD_SLUG)
        })
    }

    /// Whether offering to add Cardboard makes sense here: it is a Fabric mod,
    /// and pointless on a server that already speaks Bukkit.
    pub fn can_add_cardboard(&self) -> bool {
        matches!(self.kind, ServerKind::Fabric | ServerKind::Quilt)
            && !self.kind.takes_plugins()
            && !self.has_cardboard()
    }

    /// The plugin platforms to search for this server, most specific first.
    pub fn plugin_loaders(&self) -> Vec<&'static str> {
        if self.kind.takes_plugins() {
            return self.kind.plugin_loaders().to_vec();
        }
        if self.has_cardboard() {
            // Cardboard reimplements Bukkit and Spigot. Paper's own extensions
            // are not there, so Paper-only plugins are not offered as if they
            // would work.
            return vec!["spigot", "bukkit"];
        }
        Vec::new()
    }

    pub fn new(name: String, kind: ServerKind, mc_version: String) -> Self {
        Self {
            id: crate::instance::slugify(&name),
            name,
            icon: None,
            banner: None,
            pack: None,
            kind,
            mc_version,
            build: String::new(),
            jar: String::new(),
            launch: installer::LaunchKind::default(),
            java_major: None,
            created_at: chrono::Utc::now().timestamp(),
            last_started: None,
            eula_accepted: false,
            settings: ServerSettings::default(),
            content: Vec::new(),
            schedules: Vec::new(),
            notes: String::new(),
            content_as_loaded: Vec::new(),
        }
    }

    /// Write the whole server over what is on disk.
    ///
    /// For a load, a change and a save with nothing slow between them. A copy
    /// that has waited on the network, a download or a backup is out of date by
    /// the time it is written, and belongs to [`Server::save_if_present`].
    pub async fn save(&self, paths: &Paths) -> Result<()> {
        let dir = paths.server(&self.id);
        tokio::fs::create_dir_all(&dir).await?;
        write_atomic(&dir.join("server.json"), &serde_json::to_vec_pretty(self)?).await
    }

    /// Save a copy loaded before a long wait, unless the server is gone, and
    /// without losing what other jobs recorded while it waited.
    ///
    /// Every command that loads a server and awaits slow work before saving it
    /// saves through this: installing or updating content, identifying
    /// untracked files, adding Cardboard, and starting a server, which may
    /// download Java first. A server deleted while the job ran stays deleted,
    /// rather than being written back from nothing.
    ///
    /// Installs clicked one after another run side by side, each from its own
    /// copy, and the last to save would drop every mod the others recorded:
    /// their jars would read as untracked, with no update checks, and one from
    /// CurseForge would go to players as a copy rather than a reference. So the
    /// server is read again just before writing, and a record on disk for a
    /// file this copy neither has nor started with is kept. One for a file it
    /// did start with is a record its own job dropped, and stays dropped.
    ///
    /// A mod moved between Mods and Client mods meanwhile is the same file
    /// under the other kind. This copy's record of the kind it started with
    /// gives way to the moved one rather than staying beside it, where an
    /// update would put the mod back in the folder it was moved out of.
    ///
    /// Only records are kept, and only across the wait: a change landing
    /// between this read and its write is still lost, and any other field saved
    /// meanwhile is overwritten by this copy. The password is safe from that
    /// because it lives in a file of its own.
    pub async fn save_if_present(&mut self, paths: &Paths) -> Result<()> {
        use crate::instance::content::{ContentKind, ContentRecord};

        let fresh = load(paths, &self.id).await?;
        let same_file =
            |r: &ContentRecord, d: &ContentRecord| r.kind == d.kind && r.file_name == d.file_name;
        let has = |records: &[ContentRecord], kind: ContentKind, name: &str| {
            records
                .iter()
                .any(|r| r.kind == kind && r.file_name == name)
        };
        let as_loaded = &self.content_as_loaded;
        self.content.retain(|r| {
            let other = match r.kind {
                ContentKind::Mod => ContentKind::ClientMod,
                ContentKind::ClientMod => ContentKind::Mod,
                _ => return true,
            };
            let moved = has(as_loaded, r.kind, &r.file_name)
                && !has(as_loaded, other, &r.file_name)
                && !has(&fresh.content, r.kind, &r.file_name)
                && has(&fresh.content, other, &r.file_name);
            !moved
        });
        for current in fresh.content {
            if !self
                .content
                .iter()
                .chain(&self.content_as_loaded)
                .any(|r| same_file(r, &current))
            {
                self.content.push(current);
            }
        }
        self.save(paths).await?;
        self.content_as_loaded = self.content.clone();
        Ok(())
    }
}

/// Replace a file so that anyone reading it sees the old contents or the new,
/// never a mix.
///
/// A plain write truncates first and fills in after, and a reader in between
/// finds an empty or half-written file. Things that read server state to decide
/// what to do — whether anything is still shared, which code opens a pack —
/// would take that for "nothing here" and act on it. The new contents go to a
/// file beside the target, so the rename stays on one filesystem, and reach
/// the disk before the rename, so a crash leaves the old file rather than an
/// empty new one.
async fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::AsyncWriteExt;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    // Named per write, so two saves of one file at once each rename a whole
    // file of their own rather than truncating each other's.
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));

    let written = async {
        let mut file = tokio::fs::File::create(&tmp).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&tmp, path).await
    }
    .await;

    if let Err(e) = written {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    Ok(())
}

pub async fn load(paths: &Paths, id: &str) -> Result<Server> {
    let bytes = tokio::fs::read(paths.server(id).join("server.json"))
        .await
        .map_err(|_| Error::msg(format!("server '{id}' was not found")))?;
    let mut server: Server = serde_json::from_slice(&bytes).map_err(|e| Error::json("server.json", e))?;
    server.content.retain(|r| !left_by_dont_share(r));
    server.content_as_loaded = server.content.clone();
    Ok(server)
}

/// A record that says nothing but that a mod was once kept from players.
///
/// Only the removed "don't share" choice ever wrote this shape: no provider,
/// the admin's own, not a dependency and no requirements looked up. Its choice
/// no longer means anything, and left in place it would stop a jar added by
/// hand from reading as untracked, so it could not be identified.
fn left_by_dont_share(r: &crate::instance::content::ContentRecord) -> bool {
    r.provider.is_empty()
        && r.origin == crate::instance::content::Origin::User
        && !r.dependency
        && r.requires.is_none()
}

pub async fn list(paths: &Paths) -> Result<Vec<Server>> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(paths.servers()).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if let Ok(server) = load(paths, &id).await {
            out.push(server);
        }
    }

    out.sort_by(|a, b| {
        b.last_started
            .unwrap_or(0)
            .cmp(&a.last_started.unwrap_or(0))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// Append a numeric suffix until the id is free on disk.
async fn unique_id(paths: &Paths, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while tokio::fs::metadata(paths.server(&candidate)).await.is_ok() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    candidate
}

/// Create the server's folders. The jar arrives separately, because resolving
/// and downloading it can fail and a half-made folder is easier to explain
/// than a half-made record.
pub async fn create(
    paths: &Paths,
    name: String,
    kind: ServerKind,
    mc_version: String,
) -> Result<Server> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::msg("give the server a name"));
    }

    let mut server = Server::new(name, kind, mc_version);
    server.id = unique_id(paths, &server.id).await;

    let run = paths.server_run(&server.id);
    tokio::fs::create_dir_all(&run).await?;
    if kind.takes_plugins() {
        tokio::fs::create_dir_all(run.join("plugins")).await?;
    }
    if kind.takes_mods() {
        tokio::fs::create_dir_all(run.join("mods")).await?;
    }

    server.save(paths).await?;
    Ok(server)
}

pub async fn delete(paths: &Paths, id: &str) -> Result<()> {
    let dir = paths.server(id);
    // Guard against a malformed id deleting something outside the tree.
    if !dir.starts_with(paths.servers()) || dir == paths.servers() {
        return Err(Error::msg("refusing to delete an unexpected path"));
    }
    tokio::fs::remove_dir_all(&dir)
        .await
        .map_err(|_| Error::msg(format!("server '{id}' was not found")))?;
    Ok(())
}

/// The world folder's name, as `server.properties` declares it.
///
/// Pure so it can be tested without a server on disk. Blank counts as absent:
/// a half-edited properties file should not send datapacks to the run root.
fn level_name(declared: Option<&str>) -> String {
    declared
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("world")
        .to_string()
}

/// Where a server keeps one kind of content.
///
/// Everything lives in the run directory under its own name, with one
/// exception that matters: a server reads datapacks from *inside its world*,
/// not from the run root. Putting them in `run/datapacks` would create a folder
/// the server never looks at — files that appear installed and do nothing.
pub async fn content_dir(
    paths: &Paths,
    id: &str,
    kind: crate::instance::content::ContentKind,
) -> std::path::PathBuf {
    let run = paths.server_run(id);
    if kind != crate::instance::content::ContentKind::DataPack {
        return run.join(kind.folder());
    }

    let declared = properties::read(paths, id)
        .await
        .ok()
        .and_then(|props| {
            props
                .into_iter()
                .find(|p| p.key == "level-name")
                .map(|p| p.value)
        });
    run.join(level_name(declared.as_deref())).join(kind.folder())
}

/// Where a modpack's or plan's mod goes when a server is built from it.
///
/// Half of a big pack exists to make the game look better, and none of that
/// runs on a server. It is still part of what the pack is to a player, so it is
/// kept for them in Client mods rather than left out.
pub(crate) fn folder_for(
    project: &crate::providers::Project,
) -> crate::instance::content::ContentKind {
    use crate::instance::content::ContentKind;
    if project.runs_on_a_server() {
        ContentKind::Mod
    } else {
        ContentKind::ClientMod
    }
}

/// Install one mod into a server, into whichever folder [`folder_for`] gives it.
///
/// No dependency walk. A pack already lists its whole closure, so chasing each
/// mod's dependencies would re-resolve what is about to be installed anyway, at
/// the cost of a network round trip per mod. A plan does not list one — the
/// same plan built as an instance walks them in [`crate::plan::build`] — so a
/// server built from a plan gets the mods the plan names and nothing else.
#[allow(clippy::too_many_arguments)] // one mod's coordinates, travelling together
pub(crate) async fn install_one(
    client: &reqwest::Client,
    paths: &Paths,
    server: &mut Server,
    provider: &str,
    project_id: &str,
    loader: &str,
    game_version: &str,
    curseforge_key: Option<&str>,
) -> Result<String> {
    use crate::providers::{self, Provider};

    let provider = Provider::parse(provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let project = providers::project(client, provider, project_id, curseforge_key).await?;
    let kind = folder_for(&project);

    let versions = providers::versions(
        client,
        provider,
        project_id,
        Some(game_version),
        Some(loader),
        curseforge_key,
    )
    .await?;
    let best = crate::install::content::pick_best(&versions)
        .ok_or_else(|| Error::msg(format!("no build for {loader} {game_version}")))?;

    crate::install::content::install_into(
        client,
        &content_dir(paths, &server.id, kind).await,
        &mut server.content,
        best,
        kind,
        &project.name,
        project.icon_url.clone(),
        Some(project.slug.clone()),
        false,
        project.listed_side(),
    )
    .await
}

/// Move one mod between Mods and Client mods, turned off or not.
///
/// The file keeps its name, `.disabled` included, so it arrives in the state it
/// left, and its record moves with it, so it is still tracked for updates and
/// still travels as a reference. Refused, with nothing changed, when the other
/// folder already has that mod, on or off, under the same name or as the same
/// project under another.
pub async fn move_mod(
    paths: &Paths,
    server: &mut Server,
    file: &str,
    to_client: bool,
) -> Result<()> {
    use crate::instance::content::{disabled_name, is_content_file, strip_disabled, ContentKind};

    let rel = std::path::Path::new(file);
    if !crate::paths::is_safe_relative(rel)
        || rel.components().count() != 1
        || !is_content_file(file, ContentKind::Mod)
    {
        return Err(Error::msg("invalid filename"));
    }
    let (from, to) = if to_client {
        (ContentKind::Mod, ContentKind::ClientMod)
    } else {
        (ContentKind::ClientMod, ContentKind::Mod)
    };
    let from_dir = content_dir(paths, &server.id, from).await;
    let to_dir = content_dir(paths, &server.id, to).await;

    // The other folder may have this mod under its name, or under another
    // name as the same project, such as a library a mod there brought in at a
    // newer version. Either way the loader, or a player's game, would find the
    // mod twice. Only a file on disk counts, since a record can outlive it.
    let base = strip_disabled(file).0.to_string();
    let project = server
        .content
        .iter()
        .find(|r| r.kind == from && r.file_name == base)
        .filter(|r| !r.provider.is_empty() && !r.project_id.is_empty());
    let mut taken = vec![base.clone()];
    taken.extend(
        server
            .content
            .iter()
            .filter(|r| {
                r.kind == to
                    && project
                        .is_some_and(|p| r.provider == p.provider && r.project_id == p.project_id)
            })
            .map(|r| r.file_name.clone()),
    );
    for name in taken {
        let disabled = disabled_name(&name);
        for on_disk in [&name, &disabled] {
            if tokio::fs::symlink_metadata(to_dir.join(on_disk))
                .await
                .is_ok()
            {
                return Err(Error::msg(format!(
                    "{name} is already in {}",
                    if to_client { "Client mods" } else { "Mods" }
                )));
            }
        }
    }

    tokio::fs::create_dir_all(&to_dir).await?;
    tokio::fs::rename(from_dir.join(file), to_dir.join(file)).await?;

    // A record of the destination's kind under this name belongs to no file,
    // since the folder had none, and would otherwise sit beside the one moving.
    server
        .content
        .retain(|r| !(r.kind == to && r.file_name == base));
    for record in &mut server.content {
        if record.kind == from && record.file_name == base {
            record.kind = to;
        }
    }
    Ok(())
}

/// Move every mod in Mods whose own jar says it only runs in a player's game
/// into Client mods, and name the files moved.
///
/// Run as a server starts, before its process exists, so the server never
/// loads a renderer or a minimap — at best dead weight, at worst a crash on
/// boot — while its players still get it with the pack.
///
/// Only the jar's own declaration counts. Modrinth's listing is an author's
/// tick-box and can be wrong, and a mod taken out of a server on its word would
/// simply be missing there; those are badged in the Content tab for the admin
/// to move. A move [`move_mod`] refuses — Client mods already has that mod —
/// is skipped, and the rest still move. A `mods/` or `client-mods/` that is a
/// link is somewhere else, perhaps shared with other servers, and nothing is
/// moved out of or into it: a pack sends nothing from a linked folder, so a mod
/// moved into one would stop reaching players.
pub async fn move_client_only_mods(paths: &Paths, server: &mut Server) -> Vec<String> {
    use crate::instance::content::{is_content_file, ContentKind};
    use crate::instance::jar::Side;

    if !server.takes_mods() {
        return Vec::new();
    }
    let dir = content_dir(paths, &server.id, ContentKind::Mod).await;
    let client_dir = content_dir(paths, &server.id, ContentKind::ClientMod).await;
    let client_only = tokio::task::spawn_blocking(move || {
        if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
            return Vec::new();
        }
        // Missing is fine, since the first move makes it.
        match std::fs::symlink_metadata(&client_dir) {
            Ok(meta) if meta.is_dir() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Vec::new(),
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            // The entry's own type, so what a link points at is never read.
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| is_content_file(name, ContentKind::Mod))
            .filter(|name| {
                crate::instance::jar::read(&dir.join(name)).and_then(|meta| meta.side)
                    == Some(Side::Client)
            })
            .collect();
        names.sort();
        names
    })
    .await
    .unwrap_or_default();

    let mut moved = Vec::new();
    for name in client_only {
        if move_mod(paths, server, &name, true).await.is_ok() {
            moved.push(name);
        }
    }
    moved
}

/// Write `eula.txt`, which Mojang requires before a server will start.
///
/// Only ever called after the user has actually agreed in the UI. Writing it
/// on their behalf unasked would be forging a legal acceptance.
pub async fn accept_eula(paths: &Paths, server: &mut Server) -> Result<()> {
    let run = paths.server_run(&server.id);
    tokio::fs::create_dir_all(&run).await?;
    tokio::fs::write(
        run.join("eula.txt"),
        "# Accepted through Geode.\n\
         # https://aka.ms/MinecraftEULA\n\
         eula=true\n",
    )
    .await?;
    server.eula_accepted = true;
    server.save(paths).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_name() {
        for kind in [
            ServerKind::Vanilla,
            ServerKind::Paper,
            ServerKind::Folia,
            ServerKind::Purpur,
            ServerKind::Spigot,
            ServerKind::Velocity,
            ServerKind::Waterfall,
            ServerKind::Fabric,
            ServerKind::Quilt,
            ServerKind::NeoForge,
            ServerKind::Forge,
            ServerKind::Mohist,
            ServerKind::Arclight,
            ServerKind::Banner,
        ] {
            assert_eq!(ServerKind::parse(kind.as_str()), Some(kind), "{kind:?}");
        }
        assert_eq!(ServerKind::parse("not-a-server"), None);
    }

    #[test]
    fn a_hybrid_takes_both_plugins_and_mods() {
        // The whole reason these exist.
        for kind in [ServerKind::Mohist, ServerKind::Arclight, ServerKind::Banner] {
            assert!(kind.takes_plugins(), "{kind:?}");
            assert!(kind.takes_mods(), "{kind:?}");
            assert!(kind.mod_loader().is_some(), "{kind:?}");
            assert!(!kind.plugin_loaders().is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn paper_takes_plugins_and_fabric_takes_mods_and_neither_takes_both() {
        assert!(ServerKind::Paper.takes_plugins());
        assert!(!ServerKind::Paper.takes_mods());
        assert!(ServerKind::Fabric.takes_mods());
        assert!(!ServerKind::Fabric.takes_plugins());
    }

    #[test]
    fn vanilla_takes_no_content_at_all() {
        assert!(!ServerKind::Vanilla.takes_plugins());
        assert!(!ServerKind::Vanilla.takes_mods());
        assert!(ServerKind::Vanilla.mod_loader().is_none());
        assert!(ServerKind::Vanilla.plugin_loaders().is_empty());
    }

    #[test]
    fn a_paper_server_is_offered_spigot_and_bukkit_plugins_too() {
        // Plugins written for the older APIs still run, and refusing to list
        // them would hide most of what exists.
        assert_eq!(
            ServerKind::Paper.plugin_loaders(),
            &["paper", "spigot", "bukkit"]
        );
        // Most specific first, so a Purpur-only plugin is reachable.
        assert_eq!(ServerKind::Purpur.plugin_loaders()[0], "purpur");
    }

    #[test]
    fn proxies_host_no_world_and_sign_no_eula() {
        for kind in [ServerKind::Velocity, ServerKind::Waterfall] {
            assert!(kind.is_proxy(), "{kind:?}");
            assert!(!kind.needs_eula(), "{kind:?}");
            assert!(kind.takes_plugins(), "{kind:?}");
            assert!(!kind.takes_mods(), "{kind:?}");
        }
        assert!(ServerKind::Paper.needs_eula());
        assert!(!ServerKind::Paper.is_proxy());
    }

    fn installed(
        slug: &str,
        kind: crate::instance::content::ContentKind,
    ) -> crate::instance::content::ContentRecord {
        crate::instance::content::ContentRecord {
            file_name: format!("{slug}.jar"),
            kind,
            provider: "modrinth".into(),
            project_id: "MLYQ9VGP".into(),
            version_id: "abc".into(),
            name: slug.into(),
            version_number: "1.0.0".into(),
            slug: Some(slug.into()),
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: None,
            side: None,
            server_off: false,
        }
    }

    #[test]
    fn a_fabric_server_takes_plugins_once_cardboard_is_installed() {
        use crate::instance::content::ContentKind;

        let mut server = Server::new("Test".into(), ServerKind::Fabric, "1.21.1".into());
        assert!(!server.takes_plugins());
        assert!(server.plugin_loaders().is_empty());
        assert!(server.can_add_cardboard());

        server.content.push(installed(CARDBOARD_SLUG, ContentKind::Mod));

        assert!(server.has_cardboard());
        assert!(server.takes_plugins());
        // Bukkit and Spigot, but not Paper: Cardboard does not implement
        // Paper's own extensions, and offering Paper plugins would promise
        // something that does not work.
        assert_eq!(server.plugin_loaders(), vec!["spigot", "bukkit"]);
        // Nothing left to offer once it is there.
        assert!(!server.can_add_cardboard());
        // It changes nothing about mods.
        assert!(server.takes_mods());
    }

    #[test]
    fn datapacks_go_inside_the_world_the_properties_name() {
        // The server reads them from its world folder. A renamed world moves
        // them with it, and anything in the run root is never looked at.
        assert_eq!(level_name(Some("survival")), "survival");
        // Absent, or present but blank, is Minecraft's own default.
        assert_eq!(level_name(None), "world");
        assert_eq!(level_name(Some("   ")), "world");
        assert_eq!(level_name(Some(" hardcore ")), "hardcore");
    }

    #[test]
    fn a_forge_server_is_installed_even_though_it_has_no_jar() {
        let mut forge = Server::new("F".into(), ServerKind::Forge, "1.20.1".into());
        assert!(!forge.is_installed(), "nothing has been installed yet");

        // What the installer actually leaves: an argument file and no jar.
        forge.launch = installer::LaunchKind::ArgFile(
            "libraries/net/minecraftforge/forge/1.20.1-47.3.0/unix_args.txt".into(),
        );
        assert!(
            forge.is_installed(),
            "a server that launches from an argument file needs no jar, and \
             refusing to start it is the bug this guards"
        );
        assert!(forge.jar.is_empty());

        // The ordinary case still turns on the jar being there.
        let mut paper = Server::new("P".into(), ServerKind::Paper, "1.21.1".into());
        assert!(!paper.is_installed());
        paper.jar = "paper-1.21.1-123.jar".into();
        assert!(paper.is_installed());
    }

    #[test]
    fn cardboard_is_not_offered_where_it_would_do_nothing() {
        // Paper already speaks Bukkit, so adding a reimplementation of it is
        // pointless rather than merely redundant.
        let paper = Server::new("P".into(), ServerKind::Paper, "1.21.1".into());
        assert!(!paper.can_add_cardboard());
        assert!(paper.takes_plugins());
        assert_eq!(paper.plugin_loaders(), vec!["paper", "spigot", "bukkit"]);

        // Vanilla loads no mods at all, so there is nowhere to put it.
        let vanilla = Server::new("V".into(), ServerKind::Vanilla, "1.21.1".into());
        assert!(!vanilla.can_add_cardboard());
        assert!(!vanilla.takes_plugins());
    }

    #[test]
    fn something_merely_named_cardboard_is_not_cardboard() {
        use crate::instance::content::ContentKind;

        let mut server = Server::new("Test".into(), ServerKind::Fabric, "1.21.1".into());
        // The right slug in the wrong folder. A resource pack does not
        // implement the Bukkit API.
        server
            .content
            .push(installed(CARDBOARD_SLUG, ContentKind::ResourcePack));
        assert!(!server.has_cardboard());
        assert!(!server.takes_plugins());
    }

    fn scratch(tag: &str) -> Paths {
        let root = std::env::temp_dir().join(format!("geode-server-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Paths { root }
    }

    /// A reader of `server.json` used to be able to open it between the
    /// truncate and the write, find nothing parseable, and conclude the server
    /// was not there — the sharing service then stopped serving a server that
    /// was still shared. Saved over a large file, so a torn read has a wide
    /// window to land in, while another thread reads as fast as it can.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reader_never_sees_half_a_saved_server() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let paths = scratch("atomic");
        let mut server = Server::new("Torn".into(), ServerKind::Fabric, "1.21.1".into());
        server.notes = "a".repeat(4 << 20);
        server.save(&paths).await.unwrap();

        let file = paths.server(&server.id).join("server.json");
        let stop = Arc::new(AtomicBool::new(false));
        let reader = std::thread::spawn({
            let stop = stop.clone();
            move || {
                let (mut reads, mut torn) = (0u32, 0u32);
                while !stop.load(Ordering::Relaxed) {
                    let bytes = std::fs::read(&file).expect("server.json went missing");
                    reads += 1;
                    if serde_json::from_slice::<Server>(&bytes).is_err() {
                        torn += 1;
                    }
                }
                (reads, torn)
            }
        });

        for round in 0..40 {
            server.notes = if round % 2 == 0 { "b" } else { "a" }.repeat(4 << 20);
            server.save(&paths).await.unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        let (reads, torn) = reader.join().unwrap();

        assert!(reads > 40, "the reader barely ran ({reads} reads), so this proves nothing");
        assert_eq!(torn, 0, "{torn} of {reads} reads found a half-written server.json");

        // Nothing left behind beside it.
        let left: Vec<String> = std::fs::read_dir(paths.server(&server.id))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left, vec!["server.json".to_string()]);

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    fn record(
        file: &str,
        provider: &str,
        project: &str,
    ) -> crate::instance::content::ContentRecord {
        crate::instance::content::ContentRecord {
            file_name: file.into(),
            provider: provider.into(),
            project_id: project.into(),
            ..installed("x", crate::instance::content::ContentKind::Mod)
        }
    }

    /// "Don't share" left a record behind for a jar added by hand, saying
    /// nothing else. Kept, it would stop that jar reading as untracked; every
    /// record that says something more stays.
    #[tokio::test]
    async fn a_record_left_by_dont_share_is_dropped_on_load() {
        use crate::instance::content::Origin;

        let paths = scratch("left-by-dont-share");
        let mut server = Server::new("Old".into(), ServerKind::Fabric, "1.21.1".into());
        let bare = |file: &str| crate::instance::content::ContentRecord {
            project_id: String::new(),
            version_id: String::new(),
            ..record(file, "", "")
        };
        server.content = vec![
            bare("hand.jar"),
            crate::instance::content::ContentRecord {
                origin: Origin::Server,
                ..bare("from-server.jar")
            },
            crate::instance::content::ContentRecord {
                dependency: true,
                ..bare("library.jar")
            },
            record("sodium.jar", "modrinth", "SODIUM"),
        ];
        server.save(&paths).await.unwrap();

        let loaded = load(&paths, &server.id).await.unwrap();
        let files: Vec<&str> = loaded
            .content
            .iter()
            .map(|r| r.file_name.as_str())
            .collect();
        assert_eq!(files, vec!["from-server.jar", "library.jar", "sodium.jar"]);

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// Deleted while the job ran. Writing the copy back would bring the
    /// server back from nothing.
    #[tokio::test]
    async fn a_late_save_does_not_bring_back_a_deleted_server() {
        let paths = scratch("late-save-deleted");
        let server = Server::new("Gone".into(), ServerKind::Fabric, "1.21.1".into());
        server.save(&paths).await.unwrap();

        let mut job = load(&paths, &server.id).await.unwrap();
        delete(&paths, &server.id).await.unwrap();

        job.save_if_present(&paths).await.unwrap_err();
        assert!(!paths.server(&server.id).exists(), "the deleted server was written again");

        let _ = std::fs::remove_dir_all(&paths.root);
    }

    /// Two installs clicked one after the other, or a start waiting on a Java
    /// download while an install finishes. Whichever saves last must not take
    /// the other's record with it, or that mod reads as untracked — and one
    /// from CurseForge goes to players as a copy.
    #[tokio::test]
    async fn a_late_save_keeps_what_another_job_saved_while_it_waited() {
        let paths = scratch("keep-other-job");
        let mut server = Server::new("Overlap".into(), ServerKind::Fabric, "1.21.1".into());
        server.content = vec![
            record("sodium.jar", "modrinth", "SODIUM"),
            record("old.jar", "modrinth", "OLD"),
        ];
        server.save(&paths).await.unwrap();

        let mut waiting = load(&paths, &server.id).await.unwrap();

        let mut other = load(&paths, &server.id).await.unwrap();
        other
            .content
            .push(record("create-cf.jar", "curseforge", "328085"));
        other.save_if_present(&paths).await.unwrap();

        // The waiting job removes a mod it started with, adds its own, and
        // saves last.
        waiting.content.retain(|r| r.file_name != "old.jar");
        waiting
            .content
            .push(record("lithium.jar", "modrinth", "LITHIUM"));
        waiting.last_started = Some(7);
        waiting.save_if_present(&paths).await.unwrap();

        let after = load(&paths, &server.id).await.unwrap();
        let files = |server: &Server| {
            let mut files: Vec<String> =
                server.content.iter().map(|r| r.file_name.clone()).collect();
            files.sort();
            files
        };
        assert_eq!(
            files(&after),
            vec!["create-cf.jar", "lithium.jar", "sodium.jar"],
            "a record was lost, or a removal undone"
        );
        assert_eq!(after.last_started, Some(7));

        // The same copy goes on to remove the mod installed meanwhile, and
        // saves again. What it saved the first time is now what it started
        // from, so that removal is its own, and stays.
        waiting.content.retain(|r| r.file_name != "create-cf.jar");
        waiting.save_if_present(&paths).await.unwrap();
        let again = load(&paths, &server.id).await.unwrap();
        assert_eq!(
            files(&again),
            vec!["lithium.jar", "sodium.jar"],
            "a removal on a second save was undone"
        );

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A Modrinth project with the given server-side listing.
    fn listed(server_side: Option<&str>) -> crate::providers::Project {
        serde_json::from_value(serde_json::json!({
            "provider": "modrinth", "id": "P", "slug": "p", "name": "P",
            "summary": "", "downloads": 0, "follows": 0, "author": "",
            "kind": "mod", "pageUrl": "", "clientSide": "required",
            "serverSide": server_side,
        }))
        .unwrap()
    }

    /// Building a server from a pack keeps the pack's client-only mods for
    /// players instead of leaving them out, and anything that may run on a
    /// server — including a platform that says nothing — goes where it loads.
    #[test]
    fn a_pack_mod_no_server_runs_is_kept_for_players() {
        use crate::instance::content::ContentKind;

        assert_eq!(
            folder_for(&listed(Some("unsupported"))),
            ContentKind::ClientMod
        );
        for server_side in [Some("required"), Some("optional"), None] {
            assert_eq!(
                folder_for(&listed(server_side)),
                ContentKind::Mod,
                "{server_side:?}"
            );
        }
    }

    /// A move takes the mod's record and its on or off state with it, and
    /// refuses to land on a mod the other folder already has, or to reach
    /// outside the folder.
    #[tokio::test]
    async fn moving_a_mod_between_folders_takes_its_record_and_its_state() {
        use crate::instance::content::ContentKind;

        let paths = scratch("move-mod");
        let mut server = Server::new("Mover".into(), ServerKind::Fabric, "1.21.1".into());
        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(run.join("mods")).await.unwrap();
        tokio::fs::write(run.join("mods/x.jar.disabled"), b"x")
            .await
            .unwrap();
        server.content = vec![record("x.jar", "modrinth", "X")];

        move_mod(&paths, &mut server, "x.jar.disabled", true)
            .await
            .unwrap();
        assert!(run.join("client-mods/x.jar.disabled").is_file());
        assert!(!run.join("mods/x.jar.disabled").exists());
        assert_eq!(server.content.len(), 1);
        assert_eq!(server.content[0].kind, ContentKind::ClientMod);

        // Mods has an x.jar of its own now, so moving the other one back would
        // put two copies of the mod side by side.
        tokio::fs::write(run.join("mods/x.jar"), b"another")
            .await
            .unwrap();
        let err = move_mod(&paths, &mut server, "x.jar.disabled", false)
            .await
            .expect_err("a move onto a mod already there must be refused");
        assert!(err.to_string().contains("already in Mods"), "{err}");
        assert!(run.join("client-mods/x.jar.disabled").is_file());
        assert_eq!(std::fs::read(run.join("mods/x.jar")).unwrap(), b"another");
        assert_eq!(server.content[0].kind, ContentKind::ClientMod);

        // Refused for its name, before anything is looked at, and not because
        // the check above happens to find a file where the name leads.
        tokio::fs::write(run.join("x.jar"), b"outside")
            .await
            .unwrap();
        tokio::fs::create_dir_all(run.join("mods/old"))
            .await
            .unwrap();
        tokio::fs::write(run.join("mods/old/y.jar"), b"parked")
            .await
            .unwrap();
        for name in ["../x.jar", "old/y.jar"] {
            let err = move_mod(&paths, &mut server, name, true)
                .await
                .expect_err("a name that is not one file in the folder must be refused");
            assert_eq!(err.to_string(), "invalid filename", "{name}");
        }
        assert!(run.join("x.jar").is_file());
        assert!(run.join("mods/old/y.jar").is_file());
        assert_eq!(server.content.len(), 1);

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A library a client mod brought into Client mods, and the same library
    /// at another version that a mod in Mods brought in later. Moving the
    /// first across would give the loader two copies of one mod, so it is
    /// refused. A record whose file is gone is no copy, and refuses nothing.
    #[tokio::test]
    async fn a_move_onto_the_same_project_under_another_name_is_refused() {
        use crate::instance::content::ContentKind;

        let paths = scratch("move-same-project");
        let mut server = Server::new("Mover".into(), ServerKind::Fabric, "1.21.1".into());
        let run = paths.server_run(&server.id);
        for rel in ["client-mods/cloth-15.0.jar", "mods/cloth-15.1.jar.disabled"] {
            let path = run.join(rel);
            tokio::fs::create_dir_all(path.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(path, b"cloth").await.unwrap();
        }
        server.content = vec![
            crate::instance::content::ContentRecord {
                kind: ContentKind::ClientMod,
                ..record("cloth-15.0.jar", "modrinth", "CLOTH")
            },
            record("cloth-15.1.jar", "modrinth", "CLOTH"),
        ];
        let tracked = |server: &Server| {
            server
                .content
                .iter()
                .map(|r| (r.kind, r.file_name.clone()))
                .collect::<Vec<_>>()
        };
        let before = tracked(&server);

        let err = move_mod(&paths, &mut server, "cloth-15.0.jar", false)
            .await
            .expect_err("a second copy of the project in Mods must be refused");
        assert_eq!(err.to_string(), "cloth-15.1.jar is already in Mods");
        assert!(run.join("client-mods/cloth-15.0.jar").is_file());
        assert!(!run.join("mods/cloth-15.0.jar").exists());
        assert_eq!(tracked(&server), before);

        tokio::fs::remove_file(run.join("mods/cloth-15.1.jar.disabled"))
            .await
            .unwrap();
        move_mod(&paths, &mut server, "cloth-15.0.jar", false)
            .await
            .unwrap();
        assert!(run.join("mods/cloth-15.0.jar").is_file());

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A jar with a real `fabric.mod.json`, so reading it finds its side.
    fn fabric_jar(id: &str, environment: &str) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(
            format!(
                r#"{{"schemaVersion":1,"id":"{id}","version":"1.0","environment":"{environment}"}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap().into_inner()
    }

    /// Starting a server takes out of Mods what its own jar says only a player's
    /// game runs, record and all. A jar for both sides stays, and so does one
    /// only Modrinth lists as client-side, since a listing can be wrong. A mod
    /// Client mods already has is left where it is, and the rest still move.
    #[tokio::test]
    async fn starting_moves_the_mods_that_declare_they_only_run_in_players_games() {
        use crate::instance::content::ContentKind;
        use crate::instance::jar::Side;

        let paths = scratch("start-moves-client-mods");
        let mut server = Server::new("Starter".into(), ServerKind::Fabric, "1.21.1".into());
        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(run.join("mods")).await.unwrap();
        tokio::fs::create_dir_all(run.join("client-mods"))
            .await
            .unwrap();
        for (rel, environment) in [
            ("mods/minimap.jar", "client"),
            ("mods/both.jar", "*"),
            ("mods/listed.jar", "*"),
            ("mods/already.jar", "client"),
            ("client-mods/already.jar", "client"),
        ] {
            tokio::fs::write(run.join(rel), fabric_jar("x", environment))
                .await
                .unwrap();
        }
        server.content = vec![
            record("minimap.jar", "modrinth", "MINIMAP"),
            crate::instance::content::ContentRecord {
                side: Some(Side::Client),
                ..record("listed.jar", "modrinth", "LISTED")
            },
        ];

        let moved = move_client_only_mods(&paths, &mut server).await;

        assert_eq!(moved, vec!["minimap.jar"]);
        assert!(run.join("client-mods/minimap.jar").is_file());
        assert!(!run.join("mods/minimap.jar").exists());
        for stays in ["both.jar", "listed.jar", "already.jar"] {
            assert!(run.join("mods").join(stays).is_file(), "{stays} was moved");
        }
        let kind = |file: &str| {
            server
                .content
                .iter()
                .find(|r| r.file_name == file)
                .map(|r| r.kind)
        };
        assert_eq!(kind("minimap.jar"), Some(ContentKind::ClientMod));
        assert_eq!(kind("listed.jar"), Some(ContentKind::Mod));

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A `client-mods/` that is a link is somewhere else, and a pack sends
    /// nothing from it, so a jar moved through it would stop reaching players
    /// and turn up in a folder the admin never put it in. Starting moves
    /// nothing then. A `client-mods/` that is not there yet is made by the move.
    #[cfg(unix)]
    #[tokio::test]
    async fn starting_moves_nothing_into_a_linked_client_mods_folder() {
        use crate::instance::content::ContentKind;

        let paths = scratch("start-linked-client-mods");
        let mut server = Server::new("Linked".into(), ServerKind::Fabric, "1.21.1".into());
        let run = paths.server_run(&server.id);
        let elsewhere = paths.root.join("elsewhere");
        tokio::fs::create_dir_all(run.join("mods")).await.unwrap();
        tokio::fs::create_dir_all(&elsewhere).await.unwrap();
        tokio::fs::write(
            run.join("mods/minimap.jar"),
            fabric_jar("minimap", "client"),
        )
        .await
        .unwrap();
        std::os::unix::fs::symlink(&elsewhere, run.join("client-mods")).unwrap();
        server.content = vec![record("minimap.jar", "modrinth", "MINIMAP")];

        assert!(move_client_only_mods(&paths, &mut server).await.is_empty());
        assert!(run.join("mods/minimap.jar").is_file());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        assert_eq!(server.content[0].kind, ContentKind::Mod);

        std::fs::remove_file(run.join("client-mods")).unwrap();
        assert_eq!(
            move_client_only_mods(&paths, &mut server).await,
            vec!["minimap.jar"]
        );
        assert!(run.join("client-mods/minimap.jar").is_file());

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    /// A move made while another job waited, in either direction. The waiting
    /// copy still has the mod's record under the folder it left, and saving it
    /// beside the moved one would leave the mod tracked in both — where an
    /// update would put it back in the folder it was just moved out of.
    #[tokio::test]
    async fn a_late_save_keeps_a_move_made_while_it_waited() {
        use crate::instance::content::ContentKind;

        let paths = scratch("late-save-move");
        let mut server = Server::new("Overlap".into(), ServerKind::Fabric, "1.21.1".into());
        server.content = vec![record("x.jar", "modrinth", "X")];
        server.save(&paths).await.unwrap();
        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(run.join("mods")).await.unwrap();
        tokio::fs::write(run.join("mods/x.jar"), b"x")
            .await
            .unwrap();

        for (to_client, moved_to) in [(true, ContentKind::ClientMod), (false, ContentKind::Mod)] {
            let mut waiting = load(&paths, &server.id).await.unwrap();

            let mut other = load(&paths, &server.id).await.unwrap();
            move_mod(&paths, &mut other, "x.jar", to_client)
                .await
                .unwrap();
            other.save(&paths).await.unwrap();

            // The waiting job's own install, which the save keeps as well.
            let own = format!("own-{to_client}.jar");
            waiting.content.push(record(&own, "modrinth", "OWN"));
            waiting.save_if_present(&paths).await.unwrap();

            let after = load(&paths, &server.id).await.unwrap();
            let x: Vec<ContentKind> = after
                .content
                .iter()
                .filter(|r| r.file_name == "x.jar")
                .map(|r| r.kind)
                .collect();
            assert_eq!(x, vec![moved_to], "the move was undone or doubled");
            assert!(after.content.iter().any(|r| r.file_name == own), "{own}");
        }

        std::fs::remove_dir_all(&paths.root).unwrap();
    }

    #[test]
    fn a_new_server_has_not_accepted_anything() {
        let server = Server::new("Test".into(), ServerKind::Paper, "1.21.1".into());
        assert!(!server.eula_accepted);
        assert_eq!(server.id, "test");
        assert_eq!(server.settings.memory_mb, 4096);
    }
}

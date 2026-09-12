//! Minecraft servers Geode runs and manages.
//!
//! A server is close enough to an instance to feel familiar — a Minecraft
//! version, a piece of server software, a folder of content — and different
//! enough to deserve its own model. It has no account and no window; it has a
//! console, a port, a EULA that must be accepted before it will start, and
//! content that may be plugins, mods, or both at once.
//!
//! The "both at once" case is not an edge: Mohist, Arclight and Banner exist
//! precisely to load Bukkit plugins alongside Forge mods, and a manager that
//! assumes one or the other cannot describe them.

pub mod backup;
pub mod from_plan;
pub mod install;
pub mod installer;
pub mod logs;
pub mod meta;
pub mod properties;
pub mod run;
pub mod schedule;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

/// The server software, which decides both where the jar comes from and what
/// kind of content the folder takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
            ServerKind::Fabric => "fabric",
            ServerKind::Quilt => "quilt",
            ServerKind::NeoForge | ServerKind::Banner => "neoforge",
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
        }
    }

    pub async fn save(&self, paths: &Paths) -> Result<()> {
        let dir = paths.server(&self.id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("server.json"), serde_json::to_vec_pretty(self)?).await?;
        Ok(())
    }
}

pub async fn load(paths: &Paths, id: &str) -> Result<Server> {
    let bytes = tokio::fs::read(paths.server(id).join("server.json"))
        .await
        .map_err(|_| Error::msg(format!("server '{id}' was not found")))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::json("server.json", e))
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
            requires: None,
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

    #[test]
    fn a_new_server_has_not_accepted_anything() {
        let server = Server::new("Test".into(), ServerKind::Paper, "1.21.1".into());
        assert!(!server.eula_accepted);
        assert_eq!(server.id, "test");
        assert_eq!(server.settings.memory_mb, 4096);
    }
}

//! Where a server's jar comes from.
//!
//! Every piece of server software publishes differently, and the differences
//! are not cosmetic: Paper hands out a build number and a sha256, Purpur a
//! build id and no hash at all, Mojang a sha1 buried in the version JSON, and
//! Fabric a URL that *is* the jar. What they have in common is only the end
//! result, so that is what this module normalises to.
//!
//! Software we cannot fetch says so plainly rather than failing late. Spigot is
//! the pointed case: it may not legally be redistributed as a jar, which is why
//! every host asks you to run BuildTools yourself, and why Paper — a drop-in
//! superset — is the answer here.

use futures::stream::{FuturesUnordered, StreamExt};
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::net::get_json;

use super::ServerKind;

const PAPER_API: &str = "https://fill.papermc.io/v3";
const PURPUR_API: &str = "https://api.purpurmc.org/v2";
const FABRIC_META: &str = "https://meta.fabricmc.net";
const MOHIST_API: &str = "https://api.mohistmc.com";
const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/net/minecraftforge/forge";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/net/neoforged/neoforge";
const QUILT_MAVEN: &str = "https://maven.quiltmc.org/repository/release/org/quiltmc/quilt-installer";
const QUILT_META: &str = "https://meta.quiltmc.org";
const SPIGOT_HUB: &str = "https://hub.spigotmc.org";
const BUILDTOOLS: &str =
    "https://hub.spigotmc.org/jenkins/job/BuildTools/lastSuccessfulBuild/artifact/target/BuildTools.jar";
const ARCLIGHT_RELEASES: &str = "https://api.github.com/repos/IzzelAliz/Arclight/releases?per_page=100";

/// A tool that has to be run before the server exists.
#[derive(Debug, Clone)]
pub struct Builder {
    pub jar: ServerJar,
    /// Arguments for the tool. `{run}` stands for the server's run directory,
    /// substituted once the path is known.
    pub args: Vec<String>,
    /// Where to run it. `None` is the run directory itself; `Some(name)` is a
    /// shared cache folder, for tools that leave a large build tree behind that
    /// must not end up inside the server — or inside its backups.
    pub workspace: Option<&'static str>,
    /// A JDK this tool insists on. `Some` also means it needs a compiler rather
    /// than just a runtime.
    pub jdk: Option<u32>,
    /// Maximum heap for the tool itself, where it has an opinion. BuildTools
    /// refuses to start with less than about 448MiB, and the JVM default
    /// depends on how much memory the machine has.
    pub heap_mb: Option<u32>,
    /// Conditions to check before spending a download and several minutes.
    pub preflight: Preflight,
    /// What should exist when it finishes.
    pub produces: Produces,
    /// Shown while it runs.
    pub stage: &'static str,
}

/// What has to be true before a tool can run at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preflight {
    /// Nothing beyond a working Java.
    None,
    /// BuildTools shells out to `git` and a POSIX shell, refuses to run in a
    /// path containing certain characters, and self-gates on its own build
    /// number — so a cached copy has to be replaced rather than reused.
    BuildTools,
}

/// What running the tool is expected to leave behind.
#[derive(Debug, Clone)]
pub enum Produces {
    /// Look for whatever an installer assembled — an argument file or a loader
    /// jar, whichever it turned out to be.
    Server,
    /// Exactly this file, because the tool was told to write it.
    Jar(String),
}

/// Where a server's software comes from.
#[derive(Debug, Clone)]
pub enum Source {
    /// The jar *is* the server: download it and it is ready.
    Jar(ServerJar),
    /// The server has to be assembled or compiled first.
    Build(Box<Builder>),
}

/// One downloadable server jar, whatever published it.
#[derive(Debug, Clone)]
pub struct ServerJar {
    pub url: String,
    pub file_name: String,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub size: Option<u64>,
    /// The build or loader version this jar is, so an update check has
    /// something to compare against.
    pub build: String,
}

/// Sortable form of a version string. Local to this module rather than shared
/// with the planner's: that one answers questions about Minecraft releases,
/// this one also has to order Velocity's own 4.1.x line.
fn version_key(v: &str) -> (u32, u32, u32) {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// A plain release, not a candidate or a snapshot. Every publisher here marks
/// those the same way — with a suffix — so one rule covers them all.
fn is_release(v: &str) -> bool {
    !v.contains('-') && v.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn sort_newest_first(mut versions: Vec<String>) -> Vec<String> {
    versions.sort_by_key(|v| std::cmp::Reverse(version_key(v)));
    versions.dedup();
    versions
}

// ---------------------------------------------------------------------------
// PaperMC (Paper, Folia, Velocity, Waterfall)
// ---------------------------------------------------------------------------

/// PaperMC's project id for a kind, where it has one.
fn paper_project(kind: ServerKind) -> Option<&'static str> {
    Some(match kind {
        ServerKind::Paper => "paper",
        ServerKind::Folia => "folia",
        ServerKind::Velocity => "velocity",
        ServerKind::Waterfall => "waterfall",
        _ => return None,
    })
}

#[derive(Debug, Deserialize)]
struct PaperProject {
    /// Version families to the versions in them, e.g. `"1.21" -> ["1.21.11", …]`.
    #[serde(default)]
    versions: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct PaperBuild {
    id: u32,
    #[serde(default)]
    channel: String,
    #[serde(default)]
    downloads: std::collections::HashMap<String, PaperDownload>,
}

#[derive(Debug, Deserialize)]
struct PaperDownload {
    name: String,
    url: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    checksums: PaperChecksums,
}

#[derive(Debug, Default, Deserialize)]
struct PaperChecksums {
    #[serde(default)]
    sha256: Option<String>,
}

async fn paper_versions(client: &reqwest::Client, project: &str) -> Result<Vec<String>> {
    let url = format!("{PAPER_API}/projects/{project}");
    let resp: PaperProject = get_json(client, &url).await?;
    Ok(sort_newest_first(
        resp.versions
            .into_values()
            .flatten()
            .filter(|v| is_release(v))
            .collect(),
    ))
}

async fn paper_jar(client: &reqwest::Client, project: &str, version: &str) -> Result<ServerJar> {
    let url = format!("{PAPER_API}/projects/{project}/versions/{version}/builds");
    let builds: Vec<PaperBuild> = get_json(client, &url).await?;

    // Newest first, and a stable build is worth reaching past a handful of
    // experimental ones for.
    let build = builds
        .iter()
        .find(|b| b.channel.eq_ignore_ascii_case("STABLE"))
        .or_else(|| builds.first())
        .ok_or_else(|| {
            Error::msg(format!("{project} has no builds for Minecraft {version}"))
        })?;

    // The key is `server:default` today; falling back to whatever single
    // download exists keeps a rename from breaking every server.
    let download = build
        .downloads
        .get("server:default")
        .or_else(|| build.downloads.values().next())
        .ok_or_else(|| Error::msg(format!("{project} build {} has no file", build.id)))?;

    Ok(ServerJar {
        url: download.url.clone(),
        file_name: download.name.clone(),
        sha1: None,
        sha256: download.checksums.sha256.clone(),
        size: download.size,
        build: build.id.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Purpur
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PurpurProject {
    #[serde(default)]
    versions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PurpurVersion {
    builds: PurpurBuilds,
}

#[derive(Debug, Deserialize)]
struct PurpurBuilds {
    latest: String,
}

async fn purpur_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let resp: PurpurProject = get_json(client, &format!("{PURPUR_API}/purpur")).await?;
    Ok(sort_newest_first(
        resp.versions.into_iter().filter(|v| is_release(v)).collect(),
    ))
}

async fn purpur_jar(client: &reqwest::Client, version: &str) -> Result<ServerJar> {
    let resp: PurpurVersion = get_json(client, &format!("{PURPUR_API}/purpur/{version}")).await?;
    let build = resp.builds.latest;
    Ok(ServerJar {
        url: format!("{PURPUR_API}/purpur/{version}/{build}/download"),
        file_name: format!("purpur-{version}-{build}.jar"),
        // Purpur publishes no checksum with the download, so there is nothing
        // to verify against. Size is checked instead, which catches a
        // truncated transfer if not a corrupted one.
        sha1: None,
        sha256: None,
        size: None,
        build,
    })
}

// ---------------------------------------------------------------------------
// Fabric
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FabricGameVersion {
    version: String,
    #[serde(default)]
    stable: bool,
}

#[derive(Debug, Deserialize)]
struct FabricLoaderEntry {
    loader: FabricLoaderVersion,
}

#[derive(Debug, Deserialize)]
struct FabricLoaderVersion {
    version: String,
    #[serde(default)]
    stable: bool,
}

#[derive(Debug, Deserialize)]
struct FabricInstaller {
    version: String,
    #[serde(default)]
    stable: bool,
}

async fn fabric_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let url = format!("{FABRIC_META}/v2/versions/game");
    let games: Vec<FabricGameVersion> = get_json(client, &url).await?;
    // Already newest-first from the API; keeping that order preserves Mojang's
    // own idea of recency rather than imposing a numeric one on snapshots.
    Ok(games
        .into_iter()
        .filter(|g| g.stable)
        .map(|g| g.version)
        .collect())
}

async fn fabric_jar(client: &reqwest::Client, game: &str) -> Result<ServerJar> {
    let loaders: Vec<FabricLoaderEntry> =
        get_json(client, &format!("{FABRIC_META}/v2/versions/loader/{game}")).await?;
    let loader = loaders
        .iter()
        .find(|l| l.loader.stable)
        .or_else(|| loaders.first())
        .ok_or_else(|| Error::msg(format!("Fabric has no loader for Minecraft {game}")))?
        .loader
        .version
        .clone();

    let installers: Vec<FabricInstaller> =
        get_json(client, &format!("{FABRIC_META}/v2/versions/installer")).await?;
    let installer = installers
        .iter()
        .find(|i| i.stable)
        .or_else(|| installers.first())
        .ok_or_else(|| Error::msg("Fabric has no installer release"))?
        .version
        .clone();

    Ok(ServerJar {
        // Fabric serves the assembled server launcher directly, so unlike
        // Forge there is no installer to run before the server exists.
        url: format!("{FABRIC_META}/v2/versions/loader/{game}/{loader}/{installer}/server/jar"),
        file_name: format!("fabric-server-mc.{game}-loader.{loader}-launcher.{installer}.jar"),
        sha1: None,
        sha256: None,
        size: None,
        build: loader,
    })
}

// ---------------------------------------------------------------------------
// Mohist (Forge mods and Bukkit plugins in one server)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct MohistVersion {
    name: String,
}

#[derive(Debug, Deserialize)]
struct MohistBuild {
    id: u32,
    #[serde(default)]
    file_sha256: Option<String>,
}

/// Mohist lists Minecraft versions it has never built for — 1.21.4 sits in the
/// list with an empty build array — so the list by itself offers dead ends, and
/// the newest entry is one of them. Nothing says which have builds, so each is
/// asked. Banner is published by the same people through the same API, so both
/// go through one implementation.
async fn mohist_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    mohist_family_versions(client, "mohist").await
}

async fn mohist_jar(client: &reqwest::Client, version: &str) -> Result<ServerJar> {
    mohist_family_jar(client, "mohist", version).await
}

// ---------------------------------------------------------------------------
// Vanilla
// ---------------------------------------------------------------------------

async fn vanilla_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let manifest = crate::meta::piston::fetch_manifest(client).await?;
    Ok(manifest
        .versions
        .into_iter()
        .filter(|v| v.kind == "release")
        .map(|v| v.id)
        .collect())
}

async fn vanilla_jar(client: &reqwest::Client, version: &str) -> Result<ServerJar> {
    let manifest = crate::meta::piston::fetch_manifest(client).await?;
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == version)
        .ok_or_else(|| Error::msg(format!("unknown Minecraft version '{version}'")))?;

    let json: crate::meta::piston::VersionJson = get_json(client, &entry.url).await?;
    let server = json.downloads.get("server").ok_or_else(|| {
        Error::msg(format!(
            "Mojang publishes no server jar for Minecraft {version}"
        ))
    })?;

    Ok(ServerJar {
        url: server.url.clone(),
        file_name: format!("minecraft_server.{version}.jar"),
        sha1: server.sha1.clone(),
        sha256: None,
        size: server.size,
        build: version.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Shared helpers for the sources that publish no JSON
// ---------------------------------------------------------------------------

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(Error::msg(format!("{url} returned HTTP {status}")));
    }
    Ok(text)
}

/// Pull the contents of every `<tag>…</tag>` out of a Maven metadata document.
///
/// These files are small, flat and machine-generated, and the alternative is an
/// XML parser dependency earning its keep on four elements.
fn xml_values(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    body.split(&open)
        .skip(1)
        .filter_map(|rest| rest.split(&close).next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Every Minecraft version Mojang calls a release.
///
/// Used to sanity-check versions derived from someone else's numbering: if a
/// derivation produces something Mojang never shipped, the derivation is wrong
/// and the entry is dropped rather than offered.
async fn mojang_releases(client: &reqwest::Client) -> Result<std::collections::HashSet<String>> {
    let manifest = crate::meta::piston::fetch_manifest(client).await?;
    Ok(manifest
        .versions
        .into_iter()
        .filter(|v| v.kind == "release")
        .map(|v| v.id)
        .collect())
}

// ---------------------------------------------------------------------------
// Forge
// ---------------------------------------------------------------------------

/// Forge names every build `<minecraft>-<forge>`, so its own versions carry the
/// Minecraft version and nothing has to be inferred.
async fn forge_builds(client: &reqwest::Client) -> Result<Vec<String>> {
    let body = get_text(client, &format!("{FORGE_MAVEN}/maven-metadata.xml")).await?;
    Ok(xml_values(&body, "version"))
}

fn forge_mc_of(build: &str) -> Option<&str> {
    build.split_once('-').map(|(mc, _)| mc)
}

async fn forge_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let (builds, releases) = tokio::try_join!(forge_builds(client), mojang_releases(client))?;
    Ok(sort_newest_first(
        builds
            .iter()
            .filter_map(|b| forge_mc_of(b))
            // Forge's Maven goes back far enough to carry builds for Minecraft
            // pre-releases — `1.7.10_pre4` among them. Offering one would mean
            // a server nothing else in Geode can describe.
            .filter(|mc| releases.contains(*mc))
            .map(str::to_string)
            .collect(),
    ))
}

async fn forge_installer(client: &reqwest::Client, game: &str) -> Result<ServerJar> {
    let builds = forge_builds(client).await?;
    let mut matching: Vec<String> = builds
        .into_iter()
        .filter(|b| forge_mc_of(b) == Some(game))
        .collect();
    matching.sort_by_key(|b| {
        version_key(b.split_once('-').map(|(_, f)| f).unwrap_or_default())
    });
    let build = matching
        .pop()
        .ok_or_else(|| Error::msg(format!("Forge has no build for Minecraft {game}")))?;

    Ok(ServerJar {
        url: format!("{FORGE_MAVEN}/{build}/forge-{build}-installer.jar"),
        file_name: format!("forge-{build}-installer.jar"),
        sha1: None,
        sha256: None,
        size: None,
        build: build
            .split_once('-')
            .map(|(_, f)| f.to_string())
            .unwrap_or(build),
    })
}

// ---------------------------------------------------------------------------
// NeoForge
// ---------------------------------------------------------------------------

/// NeoForge's version carries no Minecraft version, only a numbering derived
/// from one — and the derivation changed when Minecraft moved to date-style
/// releases. So candidates are generated and checked against what Mojang
/// actually shipped, instead of trusting a single rule to hold forever.
fn neoforge_mc_candidates(version: &str) -> Vec<String> {
    let core = version.split('-').next().unwrap_or(version);
    let p: Vec<&str> = core.split('.').collect();
    let mut out = Vec::new();
    if p.len() >= 4 {
        // 26.1.2.109 -> 26.1.2, and 26.2.0.87 -> 26.2
        out.push(p[..3].join("."));
        out.push(p[..2].join("."));
    }
    if p.len() >= 3 {
        // 21.8.54 -> 1.21.8
        out.push(format!("1.{}.{}", p[0], p[1]));
        if p[1] == "0" {
            // 21.0.167 -> 1.21
            out.push(format!("1.{}", p[0]));
        }
    }
    out
}

async fn neoforge_builds(client: &reqwest::Client) -> Result<Vec<String>> {
    let body = get_text(client, &format!("{NEOFORGE_MAVEN}/maven-metadata.xml")).await?;
    Ok(xml_values(&body, "version"))
}

async fn neoforge_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let (builds, releases) = tokio::try_join!(neoforge_builds(client), mojang_releases(client))?;
    let mut out: Vec<String> = builds
        .iter()
        .filter_map(|b| {
            neoforge_mc_candidates(b)
                .into_iter()
                .find(|c| releases.contains(c))
        })
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(version_key(v)));
    out.dedup();
    Ok(out)
}

async fn neoforge_installer(client: &reqwest::Client, game: &str) -> Result<ServerJar> {
    let builds = neoforge_builds(client).await?;
    let mut matching: Vec<String> = builds
        .into_iter()
        .filter(|b| !b.contains("-beta"))
        .filter(|b| neoforge_mc_candidates(b).iter().any(|c| c == game))
        .collect();
    matching.sort_by_key(|b| version_key(b));
    let build = matching
        .pop()
        .ok_or_else(|| Error::msg(format!("NeoForge has no build for Minecraft {game}")))?;

    Ok(ServerJar {
        url: format!("{NEOFORGE_MAVEN}/{build}/neoforge-{build}-installer.jar"),
        file_name: format!("neoforge-{build}-installer.jar"),
        sha1: None,
        sha256: None,
        size: None,
        build,
    })
}

// ---------------------------------------------------------------------------
// Quilt
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct QuiltGameVersion {
    version: String,
    #[serde(default)]
    stable: bool,
}

async fn quilt_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let games: Vec<QuiltGameVersion> =
        get_json(client, &format!("{QUILT_META}/v3/versions/game")).await?;
    Ok(games
        .into_iter()
        .filter(|g| g.stable)
        .map(|g| g.version)
        .collect())
}

/// Quilt publishes no assembled server jar, only an installer — unlike Fabric,
/// whose meta server hands one over directly.
async fn quilt_installer(client: &reqwest::Client) -> Result<ServerJar> {
    let body = get_text(client, &format!("{QUILT_MAVEN}/maven-metadata.xml")).await?;
    let version = xml_values(&body, "release")
        .into_iter()
        .next()
        .or_else(|| xml_values(&body, "latest").into_iter().next())
        .or_else(|| xml_values(&body, "version").into_iter().next_back())
        .ok_or_else(|| Error::msg("Quilt publishes no installer release"))?;

    Ok(ServerJar {
        url: format!("{QUILT_MAVEN}/{version}/quilt-installer-{version}.jar"),
        file_name: format!("quilt-installer-{version}.jar"),
        sha1: None,
        sha256: None,
        size: None,
        build: version,
    })
}

// ---------------------------------------------------------------------------
// Spigot (built locally, because it may not be redistributed)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpigotRevision {
    /// Class-file versions, not Java versions: 65 means Java 21.
    ///
    /// The rename above is load-bearing. Without it this never matches the
    /// `javaVersions` key, every revision silently looks like it has no
    /// requirement, and BuildTools is then run under whatever Java the server
    /// wanted — which it refuses outright rather than tolerating.
    #[serde(default)]
    java_versions: Option<Vec<u32>>,
}

/// Java major version from a class-file major version. Class 65 is Java 21, and
/// the offset has been 44 since Java 1.1.
fn java_major_of_class(class_version: u32) -> u32 {
    class_version.saturating_sub(44)
}

/// Revisions BuildTools knows how to build.
///
/// The index also lists thousands of Jenkins build numbers, which are not
/// versions anybody wants to pick from a menu, so it is filtered against
/// Mojang's release list.
async fn spigot_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let index = format!("{SPIGOT_HUB}/versions/");
    let (body, releases) = tokio::try_join!(get_text(client, &index), mojang_releases(client))?;

    let mut out: Vec<String> = body
        .split("href=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .filter_map(|name| name.strip_suffix(".json"))
        .filter(|name| releases.contains(*name))
        .map(str::to_string)
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(version_key(v)));
    out.dedup();
    Ok(out)
}

/// The JDK a given revision must be built with.
///
/// BuildTools refuses outright when the running Java is outside the range the
/// revision declares, so this is not advisory — building 1.21.1 under Java 25
/// stops with an error and produces nothing. The lower bound is taken because
/// it is the one most likely to still be installable.
///
/// Revisions old enough to predate the field — 1.8.8 and its contemporaries —
/// were built under Java 7 or 8, so that is what they get.
async fn spigot_jdk(client: &reqwest::Client, game: &str) -> u32 {
    const LEGACY: u32 = 8;

    let url = format!("{SPIGOT_HUB}/versions/{game}.json");
    let Ok(revision) = get_json::<SpigotRevision>(client, &url).await else {
        return LEGACY;
    };
    revision
        .java_versions
        .as_ref()
        .and_then(|versions| versions.first())
        .map(|class| java_major_of_class(*class))
        .unwrap_or(LEGACY)
}

async fn spigot_builder(client: &reqwest::Client, game: &str) -> Result<Builder> {
    let jdk = spigot_jdk(client, game).await;

    Ok(Builder {
        jar: ServerJar {
            url: BUILDTOOLS.to_string(),
            file_name: "BuildTools.jar".to_string(),
            sha1: None,
            sha256: None,
            size: None,
            build: game.to_string(),
        },
        args: vec![
            "--rev".into(),
            game.to_string(),
            "--compile".into(),
            "spigot".into(),
            // Without this it tries to open a Swing window.
            "--nogui".into(),
            "--output-dir".into(),
            "{run}".into(),
            // Name it ourselves so there is no pattern to guess afterwards.
            "--final-name".into(),
            format!("spigot-{game}.jar"),
        ],
        // BuildTools clones four repositories and an Apache Maven, and leaves
        // roughly 600MB behind. That belongs in the cache, where the next build
        // can reuse it, and never inside a server folder that gets backed up.
        workspace: Some("buildtools"),
        jdk: Some(jdk),
        // It checks its own maximum heap and stops below roughly 448MiB.
        heap_mb: Some(1024),
        preflight: Preflight::BuildTools,
        produces: Produces::Jar(format!("spigot-{game}.jar")),
        stage: "Building Spigot — this takes a few minutes",
    })
}

// ---------------------------------------------------------------------------
// Arclight
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GithubRelease {
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: Option<u64>,
}

/// Arclight publishes per-loader builds — `arclight-forge-1.21.1-1.0.1-<sha>.jar`
/// and fabric and neoforge alongside. The forge variant is the one offered,
/// matching what [`ServerKind::mod_loader`] promises Arclight mods are built for.
fn arclight_parts(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("arclight-")?.strip_suffix(".jar")?;
    let (loader, rest) = rest.split_once('-')?;
    let game = rest.split('-').next()?.to_string();
    // A Minecraft version rather than something else in the same position.
    if !game.contains('.') || !game.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some((loader.to_string(), game))
}

async fn arclight_assets(client: &reqwest::Client) -> Result<Vec<GithubAsset>> {
    // GitHub rejects requests without a User-Agent, and `get_json` sets no
    // headers, so this one is made by hand.
    let resp = client
        .get(ARCLIGHT_RELEASES)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "geode-launcher")
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(Error::msg(format!(
            "Arclight's release list returned HTTP {status}"
        )));
    }
    let releases: Vec<GithubRelease> =
        serde_json::from_str(&text).map_err(|e| Error::json(ARCLIGHT_RELEASES, e))?;
    Ok(releases.into_iter().flat_map(|r| r.assets).collect())
}

async fn arclight_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let assets = arclight_assets(client).await?;
    Ok(sort_newest_first(
        assets
            .iter()
            .filter_map(|a| arclight_parts(&a.name))
            .filter(|(loader, _)| loader.as_str() == "forge")
            .map(|(_, game)| game)
            .collect(),
    ))
}

async fn arclight_jar(client: &reqwest::Client, game: &str) -> Result<ServerJar> {
    let assets = arclight_assets(client).await?;
    // Newest release first in GitHub's ordering, so the first match is current.
    let asset = assets
        .into_iter()
        .find(|a| {
            matches!(arclight_parts(&a.name), Some((loader, mc)) if loader == "forge" && mc == game)
        })
        .ok_or_else(|| Error::msg(format!("Arclight has no build for Minecraft {game}")))?;

    Ok(ServerJar {
        url: asset.browser_download_url,
        file_name: asset.name.clone(),
        sha1: None,
        sha256: None,
        size: asset.size,
        build: asset.name,
    })
}

// ---------------------------------------------------------------------------
// Banner (the same publisher as Mohist, and the same API)
// ---------------------------------------------------------------------------

async fn mohist_family_versions(client: &reqwest::Client, project: &str) -> Result<Vec<String>> {
    let url = format!("{MOHIST_API}/project/{project}/versions");
    let listed: Vec<MohistVersion> = get_json(client, &url).await?;
    let candidates = sort_newest_first(
        listed
            .into_iter()
            .map(|v| v.name)
            .filter(|v| is_release(v))
            .collect(),
    );

    let mut checks = FuturesUnordered::new();
    for version in candidates {
        let client = client.clone();
        let project = project.to_string();
        checks.push(async move {
            let url = format!("{MOHIST_API}/project/{project}/{version}/builds");
            let has_builds = get_json::<Vec<MohistBuild>>(&client, &url)
                .await
                .map(|builds| !builds.is_empty())
                .unwrap_or(false);
            (version, has_builds)
        });
    }

    let mut out = Vec::new();
    while let Some((version, has_builds)) = checks.next().await {
        if has_builds {
            out.push(version);
        }
    }
    Ok(sort_newest_first(out))
}

async fn mohist_family_jar(
    client: &reqwest::Client,
    project: &str,
    version: &str,
) -> Result<ServerJar> {
    let url = format!("{MOHIST_API}/project/{project}/{version}/builds");
    let builds: Vec<MohistBuild> = get_json(client, &url).await?;
    let build = builds
        .first()
        .ok_or_else(|| Error::msg(format!("{project} has no builds for Minecraft {version}")))?;

    Ok(ServerJar {
        url: format!(
            "{MOHIST_API}/project/{project}/{version}/builds/{}/download",
            build.id
        ),
        file_name: format!("{project}-{version}-{}.jar", build.id),
        sha1: None,
        sha256: build.file_sha256.clone(),
        size: None,
        build: build.id.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Versions of this server software that can be installed, newest first.
pub async fn versions(client: &reqwest::Client, kind: ServerKind) -> Result<Vec<String>> {
    if let Some(project) = paper_project(kind) {
        return paper_versions(client, project).await;
    }
    match kind {
        ServerKind::Purpur => purpur_versions(client).await,
        ServerKind::Fabric => fabric_versions(client).await,
        ServerKind::Vanilla => vanilla_versions(client).await,
        ServerKind::Mohist => mohist_versions(client).await,
        ServerKind::Banner => mohist_family_versions(client, "banner").await,
        ServerKind::Forge => forge_versions(client).await,
        ServerKind::NeoForge => neoforge_versions(client).await,
        ServerKind::Quilt => quilt_versions(client).await,
        ServerKind::Spigot => spigot_versions(client).await,
        ServerKind::Arclight => arclight_versions(client).await,
        // Every kind is covered above; this keeps the match total rather than
        // letting a future variant compile into a silent gap.
        other => Err(Error::msg(format!(
            "{} is not set up automatically yet.",
            other.display_name()
        ))),
    }
}

/// Where one version of one server software comes from — a jar to download, or
/// a tool to run that produces one.
pub async fn resolve(client: &reqwest::Client, kind: ServerKind, version: &str) -> Result<Source> {
    if let Some(project) = paper_project(kind) {
        return Ok(Source::Jar(paper_jar(client, project, version).await?));
    }
    Ok(match kind {
        ServerKind::Purpur => Source::Jar(purpur_jar(client, version).await?),
        ServerKind::Fabric => Source::Jar(fabric_jar(client, version).await?),
        ServerKind::Vanilla => Source::Jar(vanilla_jar(client, version).await?),
        ServerKind::Mohist => Source::Jar(mohist_jar(client, version).await?),
        ServerKind::Banner => Source::Jar(mohist_family_jar(client, "banner", version).await?),
        ServerKind::Arclight => Source::Jar(arclight_jar(client, version).await?),

        // Forge and NeoForge ship the same installer, which assembles the
        // server in place and — from 1.17 — produces no jar at all.
        ServerKind::Forge | ServerKind::NeoForge => {
            let jar = if kind == ServerKind::Forge {
                forge_installer(client, version).await?
            } else {
                neoforge_installer(client, version).await?
            };
            Source::Build(Box::new(Builder {
                jar,
                args: vec!["--installServer".into(), "{run}".into()],
                workspace: None,
                jdk: None,
                heap_mb: None,
                preflight: Preflight::None,
                produces: Produces::Server,
                stage: "Installing the server",
            }))
        }

        ServerKind::Quilt => Source::Build(Box::new(Builder {
            jar: quilt_installer(client).await?,
            args: vec![
                "install".into(),
                "server".into(),
                version.to_string(),
                "--install-dir={run}".into(),
                // Without this there is no Minecraft for the loader to launch.
                "--download-server".into(),
            ],
            workspace: None,
            jdk: None,
            heap_mb: None,
            preflight: Preflight::None,
            produces: Produces::Server,
            stage: "Installing the server",
        })),

        ServerKind::Spigot => Source::Build(Box::new(spigot_builder(client, version).await?)),

        other => {
            return Err(Error::msg(format!(
                "{} is not set up automatically yet.",
                other.display_name()
            )))
        }
    })
}

/// Whether Geode can install this software without help.
///
/// Everything is installable now: the jar-publishing platforms directly, the
/// installer-based loaders by running their installer, and Spigot by compiling
/// it with BuildTools the way its licence requires.
pub fn is_installable(_kind: ServerKind) -> bool {
    true
}

/// The Java major version this server will need.
///
/// Mojang states it per Minecraft version, which covers everything built on
/// one. A proxy tracks its own schedule and says nothing we can read, so it
/// gets `None` and the newest Java on the machine.
///
/// Best-effort: a failure here should cost the server a precise answer, not
/// its installation.
pub async fn required_java(
    client: &reqwest::Client,
    kind: ServerKind,
    version: &str,
) -> Option<u32> {
    if kind.is_proxy() {
        return None;
    }
    let manifest = crate::meta::piston::fetch_manifest(client).await.ok()?;
    let entry = manifest.versions.iter().find(|v| v.id == version)?;
    let json: crate::meta::piston::VersionJson = get_json(client, &entry.url).await.ok()?;
    Some(crate::java::required_major(&json))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_are_kept_and_candidates_are_not() {
        assert!(is_release("1.21.1"));
        assert!(is_release("26.2"));
        assert!(!is_release("1.21.11-rc3"));
        assert!(!is_release("4.1.2-SNAPSHOT"));
        assert!(!is_release("24w14a"));
    }

    #[test]
    fn versions_come_back_newest_first() {
        let sorted = sort_newest_first(vec![
            "1.20.1".into(),
            "1.21.11".into(),
            "1.21.9".into(),
            "1.21".into(),
        ]);
        assert_eq!(sorted, vec!["1.21.11", "1.21.9", "1.21", "1.20.1"]);
    }

    #[test]
    fn paper_family_maps_to_its_own_projects() {
        assert_eq!(paper_project(ServerKind::Paper), Some("paper"));
        assert_eq!(paper_project(ServerKind::Folia), Some("folia"));
        assert_eq!(paper_project(ServerKind::Velocity), Some("velocity"));
        assert_eq!(paper_project(ServerKind::Waterfall), Some("waterfall"));
        // Purpur is its own publisher, not a PaperMC project.
        assert_eq!(paper_project(ServerKind::Purpur), None);
        assert_eq!(paper_project(ServerKind::Fabric), None);
    }

    #[test]
    fn every_kind_can_be_installed() {
        // The ones that used to be greyed out are the point of this: each now
        // has a route, whether that is a download, an installer or a build.
        for kind in ServerKind::ALL {
            assert!(is_installable(*kind), "{kind:?}");
        }
    }

    #[test]
    fn forges_own_version_carries_the_minecraft_one() {
        assert_eq!(forge_mc_of("1.20.1-47.3.0"), Some("1.20.1"));
        assert_eq!(forge_mc_of("26.2-65.1.3"), Some("26.2"));
        assert_eq!(forge_mc_of("nonsense"), None);
    }

    #[test]
    fn neoforge_versions_map_onto_both_eras_of_minecraft_numbering() {
        // Three parts: the old scheme, where 1. was dropped from the front.
        assert!(neoforge_mc_candidates("21.1.95").contains(&"1.21.1".to_string()));
        assert!(neoforge_mc_candidates("21.8.54").contains(&"1.21.8".to_string()));
        // A .0 patch means the release had no patch number at all.
        assert!(neoforge_mc_candidates("21.0.167").contains(&"1.21".to_string()));
        // Four parts: date-style Minecraft versions, carried verbatim.
        assert!(neoforge_mc_candidates("26.1.2.109").contains(&"26.1.2".to_string()));
        assert!(neoforge_mc_candidates("26.2.0.87").contains(&"26.2".to_string()));
    }

    #[test]
    fn a_java_class_version_names_a_jdk() {
        // Spigot states requirements as class-file versions, so 1.21.1 asking
        // for [65, 67] means Java 21 to 23.
        assert_eq!(java_major_of_class(65), 21);
        assert_eq!(java_major_of_class(67), 23);
        assert_eq!(java_major_of_class(52), 8);
    }

    #[test]
    fn arclight_assets_are_split_into_loader_and_version() {
        assert_eq!(
            arclight_parts("arclight-forge-1.21.1-1.0.1-8ec9529.jar"),
            Some(("forge".to_string(), "1.21.1".to_string()))
        );
        assert_eq!(
            arclight_parts("arclight-neoforge-1.20.4-1.0.4-80ec5df.jar"),
            Some(("neoforge".to_string(), "1.20.4".to_string()))
        );
        // The coremod's own version is shaped exactly like a Minecraft one, so
        // parsing cannot tell them apart and does not pretend to. It is the
        // loader filter that keeps it out of the version list.
        assert_eq!(
            arclight_parts("arclight-coremod-1.0.0-SNAPSHOT.jar"),
            Some(("coremod".to_string(), "1.0.0".to_string()))
        );
        assert_eq!(arclight_parts("something-else.jar"), None);
    }

    #[test]
    fn maven_metadata_is_read_without_an_xml_parser() {
        let body = "<metadata><versioning><latest>0.15.1</latest>\
                    <versions><version>0.14.0</version><version>0.15.1</version></versions>\
                    </versioning></metadata>";
        assert_eq!(xml_values(body, "version"), vec!["0.14.0", "0.15.1"]);
        assert_eq!(xml_values(body, "latest"), vec!["0.15.1"]);
        assert!(xml_values(body, "absent").is_empty());
    }

    #[test]
    fn the_one_hybrid_we_can_install_is_installable() {
        // Mohist is the "mods and plugins together" case, so it mattering is
        // the point of supporting it at all.
        assert!(is_installable(ServerKind::Mohist));
        assert!(ServerKind::Mohist.takes_mods());
        assert!(ServerKind::Mohist.takes_plugins());
    }

    #[test]
    fn spigot_is_built_rather_than_downloaded() {
        // Its licence does not permit redistributing a built jar, so the only
        // legitimate route is compiling it — which means BuildTools, a chosen
        // JDK, and a workspace kept out of the server folder.
        let builder = Builder {
            jar: ServerJar {
                url: BUILDTOOLS.into(),
                file_name: "BuildTools.jar".into(),
                sha1: None,
                sha256: None,
                size: None,
                build: "1.21.1".into(),
            },
            args: vec!["--rev".into(), "1.21.1".into(), "--output-dir".into(), "{run}".into()],
            workspace: Some("buildtools"),
            jdk: Some(21),
            heap_mb: Some(1024),
            preflight: Preflight::BuildTools,
            produces: Produces::Jar("spigot-1.21.1.jar".into()),
            stage: "Building Spigot",
        };
        // 600MB of build tree must not land inside a folder we back up.
        assert!(builder.workspace.is_some());
        // The output still has to arrive in the server's own directory.
        assert!(builder.args.contains(&"{run}".to_string()));
    }
}

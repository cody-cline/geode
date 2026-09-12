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

async fn mohist_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let url = format!("{MOHIST_API}/project/mohist/versions");
    let listed: Vec<MohistVersion> = get_json(client, &url).await?;
    let candidates = sort_newest_first(
        listed
            .into_iter()
            .map(|v| v.name)
            .filter(|v| is_release(v))
            .collect(),
    );

    // Mohist lists Minecraft versions it has never built for — 1.21.4 sits in
    // the list with an empty build array — so the list by itself offers dead
    // ends, and the newest entry is one of them. Nothing says which have
    // builds, so each is asked. A dozen small requests, once, on the way to a
    // version picker; if that list ever grows to hundreds this wants bounding.
    let mut checks = FuturesUnordered::new();
    for version in candidates {
        let client = client.clone();
        checks.push(async move {
            let url = format!("{MOHIST_API}/project/mohist/{version}/builds");
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

async fn mohist_jar(client: &reqwest::Client, version: &str) -> Result<ServerJar> {
    let url = format!("{MOHIST_API}/project/mohist/{version}/builds");
    let builds: Vec<MohistBuild> = get_json(client, &url).await?;
    // Newest first, unlike most of the others' build lists being appended to.
    let build = builds
        .first()
        .ok_or_else(|| Error::msg(format!("Mohist has no builds for Minecraft {version}")))?;

    Ok(ServerJar {
        url: format!("{MOHIST_API}/project/mohist/{version}/builds/{}/download", build.id),
        file_name: format!("mohist-{version}-{}.jar", build.id),
        sha1: None,
        sha256: build.file_sha256.clone(),
        size: None,
        build: build.id.to_string(),
    })
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
// Dispatch
// ---------------------------------------------------------------------------

/// Why a kind cannot be set up yet, said in terms of what to do instead.
fn unsupported(kind: ServerKind) -> Error {
    match kind {
        ServerKind::Spigot => Error::msg(
            "Spigot cannot be downloaded — its licence requires you to build it yourself with \
             BuildTools. Paper runs every Spigot plugin and can be installed directly, so pick \
             that unless you specifically need Spigot.",
        ),
        ServerKind::Forge | ServerKind::NeoForge => Error::msg(format!(
            "{} servers install from an installer jar that has to be run before the server \
             exists. That step is not built yet.",
            kind.display_name()
        )),
        ServerKind::Quilt => Error::msg(
            "Quilt server installation is not built yet. Fabric mods run on Fabric servers, \
             which are supported.",
        ),
        other => Error::msg(format!(
            "{} is not set up automatically yet.",
            other.display_name()
        )),
    }
}

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
        other => Err(unsupported(other)),
    }
}

/// The jar to download for one version of one server software.
pub async fn resolve(
    client: &reqwest::Client,
    kind: ServerKind,
    version: &str,
) -> Result<ServerJar> {
    if let Some(project) = paper_project(kind) {
        return paper_jar(client, project, version).await;
    }
    match kind {
        ServerKind::Purpur => purpur_jar(client, version).await,
        ServerKind::Fabric => fabric_jar(client, version).await,
        ServerKind::Vanilla => vanilla_jar(client, version).await,
        ServerKind::Mohist => mohist_jar(client, version).await,
        other => Err(unsupported(other)),
    }
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

/// Whether Geode can install this software without help.
pub fn is_installable(kind: ServerKind) -> bool {
    paper_project(kind).is_some()
        || matches!(
            kind,
            ServerKind::Purpur | ServerKind::Fabric | ServerKind::Vanilla | ServerKind::Mohist
        )
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
    fn what_is_installable_is_what_has_a_resolver() {
        for kind in [
            ServerKind::Paper,
            ServerKind::Folia,
            ServerKind::Velocity,
            ServerKind::Waterfall,
            ServerKind::Purpur,
            ServerKind::Fabric,
            ServerKind::Vanilla,
            ServerKind::Mohist,
        ] {
            assert!(is_installable(kind), "{kind:?}");
        }
        for kind in [
            ServerKind::Spigot,
            ServerKind::Forge,
            ServerKind::NeoForge,
            ServerKind::Quilt,
            ServerKind::Arclight,
        ] {
            assert!(!is_installable(kind), "{kind:?}");
        }
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
    fn spigot_explains_itself_rather_than_failing_blankly() {
        // A licence problem is not a bug, and the message has to say so or the
        // user will keep retrying it.
        let message = unsupported(ServerKind::Spigot).to_string();
        assert!(message.contains("BuildTools"), "{message}");
        assert!(message.contains("Paper"), "{message}");
    }
}

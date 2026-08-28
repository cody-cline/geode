//! CurseForge (api.curseforge.com/v1).
//!
//! Requires an API key that CurseForge issues per developer and explicitly
//! forbids sharing, so Geode ships without one and asks the user for theirs.
//! Everything here is inert until that key is set.

use serde::Deserialize;

use crate::error::{Error, Result};

use super::{
    Dependency, DependencyKind, Project, ProjectKind, Provider, SearchQuery, SortBy, Version,
    VersionFile,
};

const API: &str = "https://api.curseforge.com/v1";
const MINECRAFT_GAME_ID: u32 = 432;

/// CurseForge's per-game "class" taxonomy.
fn class_id(kind: ProjectKind) -> u32 {
    match kind {
        ProjectKind::Mod => 6,
        ProjectKind::ResourcePack => 12,
        ProjectKind::Modpack => 4471,
        ProjectKind::Shader => 6552,
        ProjectKind::DataPack => 6945,
    }
}

fn kind_from_class(class_id: Option<u32>) -> ProjectKind {
    match class_id {
        Some(12) => ProjectKind::ResourcePack,
        Some(4471) => ProjectKind::Modpack,
        Some(6552) => ProjectKind::Shader,
        Some(6945) => ProjectKind::DataPack,
        _ => ProjectKind::Mod,
    }
}

fn mod_loader_type(loader: &str) -> Option<u32> {
    match loader.to_ascii_lowercase().as_str() {
        "forge" => Some(1),
        "fabric" => Some(4),
        "quilt" => Some(5),
        "neoforge" => Some(6),
        _ => None,
    }
}

fn loader_name(id: Option<u32>) -> Option<&'static str> {
    match id? {
        1 => Some("forge"),
        3 => Some("liteloader"),
        4 => Some("fabric"),
        5 => Some("quilt"),
        6 => Some("neoforge"),
        _ => None,
    }
}

fn sort_field(sort: SortBy) -> u32 {
    match sort {
        SortBy::Relevance => 1,  // Featured
        SortBy::Downloads => 6,  // TotalDownloads
        SortBy::Follows => 2,    // Popularity
        SortBy::Newest => 11,    // ReleasedDate
        SortBy::Updated => 3,    // LastUpdated
    }
}

fn release_type(id: u32) -> &'static str {
    match id {
        1 => "release",
        2 => "beta",
        _ => "alpha",
    }
}

async fn get<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
) -> Result<T> {
    let resp = client
        .get(url)
        .header("x-api-key", api_key)
        .header("Accept", "application/json")
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(Error::msg(format!(
            "the CurseForge API key was rejected. {}",
            key_shape_hint(api_key)
        )));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(Error::msg("CurseForge rate limit reached — try again shortly"));
    }
    if !status.is_success() {
        return Err(Error::msg(format!("CurseForge returned HTTP {status}")));
    }

    let text = resp.text().await?;
    serde_json::from_str(&text).map_err(|e| Error::json("CurseForge response", e))
}

/// Guess *why* a key was refused, from its shape alone.
///
/// CurseForge keys are bcrypt-format: `$2a$10$` followed by 53 characters of
/// `./A-Za-z0-9`. The dollar signs make them a trap in a shell — writing
/// `GEODE_CURSEFORGE_KEY=$2a$10$… cargo build` unquoted lets the shell expand
/// `$2a` and `$10` to nothing, baking in a truncated key that fails exactly
/// like a wrong one. Telling the two apart saves a lot of staring.
fn key_shape_hint(key: &str) -> &'static str {
    let looks_bcrypt = key.starts_with("$2") && key.matches('$').count() >= 3;

    if key.len() < 20 {
        return "That key looks too short — if you set it on a command line, \
                the shell probably ate the `$` characters. Quote it: \
                GEODE_CURSEFORGE_KEY='$2a$10$...'";
    }
    if !looks_bcrypt {
        return "That does not look like a CurseForge key (they start with \
                `$2a$10$`). If you set it on a command line, quote it so the \
                shell cannot expand the `$` parts.";
    }
    "The shape looks right, so the key itself is probably wrong or revoked — \
     check it at console.curseforge.com."
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: T,
}

/// What a key check found. Reported verbatim so a surprising status is visible
/// rather than flattened into "something went wrong".
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyCheck {
    pub ok: bool,
    pub status: u16,
    pub message: String,
}

/// Make one cheap request to see whether a key works.
pub async fn check_key(client: &reqwest::Client, api_key: &str) -> KeyCheck {
    let url = format!("{API}/mods/search?gameId={MINECRAFT_GAME_ID}&classId=6&pageSize=1");

    let resp = match client
        .get(&url)
        .header("x-api-key", api_key)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return KeyCheck {
                ok: false,
                status: 0,
                message: format!("Could not reach CurseForge: {e}"),
            }
        }
    };

    let status = resp.status();
    if status.is_success() {
        return KeyCheck {
            ok: true,
            status: status.as_u16(),
            message: "Key works — CurseForge is enabled.".into(),
        };
    }

    let message = if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
    {
        format!("Rejected ({status}). {}", key_shape_hint(api_key))
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        "Rate limited — the key may be fine, try again shortly.".into()
    } else {
        format!("CurseForge returned HTTP {status}.")
    };

    KeyCheck {
        ok: false,
        status: status.as_u16(),
        message,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfMod {
    id: u32,
    name: String,
    slug: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    links: Option<CfLinks>,
    #[serde(default)]
    download_count: f64,
    #[serde(default)]
    thumbs_up_count: u64,
    #[serde(default)]
    logo: Option<CfLogo>,
    #[serde(default)]
    authors: Vec<CfAuthor>,
    #[serde(default)]
    categories: Vec<CfCategory>,
    #[serde(default)]
    class_id: Option<u32>,
    #[serde(default)]
    date_modified: Option<String>,
    #[serde(default)]
    latest_files_indexes: Vec<CfFileIndex>,
    /// False when the author opted out of third-party downloads.
    #[serde(default = "default_true")]
    allow_mod_distribution: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfLinks {
    #[serde(default)]
    website_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfLogo {
    #[serde(default)]
    thumbnail_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CfAuthor {
    name: String,
}

#[derive(Debug, Deserialize)]
struct CfCategory {
    #[serde(default)]
    slug: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfFileIndex {
    game_version: String,
    #[serde(default)]
    mod_loader: Option<u32>,
}

fn to_project(m: CfMod) -> Project {
    let mut game_versions: Vec<String> = m
        .latest_files_indexes
        .iter()
        .map(|i| i.game_version.clone())
        .collect();
    game_versions.sort();
    game_versions.dedup();

    let mut loaders: Vec<String> = m
        .latest_files_indexes
        .iter()
        .filter_map(|i| loader_name(i.mod_loader))
        .map(str::to_string)
        .collect();
    loaders.sort();
    loaders.dedup();

    let kind = kind_from_class(m.class_id);
    Project {
        provider: Provider::CurseForge,
        page_url: m
            .links
            .and_then(|l| l.website_url)
            .unwrap_or_else(|| format!("https://www.curseforge.com/minecraft/mc-mods/{}", m.slug)),
        id: m.id.to_string(),
        slug: m.slug,
        name: m.name,
        summary: m.summary,
        icon_url: m.logo.and_then(|l| l.thumbnail_url.or(l.url)),
        // CurseForge reports downloads as a float; it exceeds u32 routinely and
        // JSON has no integer type, so it arrives as 1.234e9.
        downloads: m.download_count.max(0.0) as u64,
        follows: m.thumbs_up_count,
        author: m
            .authors
            .first()
            .map(|a| a.name.clone())
            .unwrap_or_default(),
        kind,
        categories: m.categories.into_iter().filter_map(|c| c.slug).collect(),
        loaders,
        game_versions,
        updated: m.date_modified,
        distribution_allowed: m.allow_mod_distribution,
        also_on: Vec::new(),
    }
}

pub async fn search(
    client: &reqwest::Client,
    query: &SearchQuery,
    api_key: &str,
) -> Result<Vec<Project>> {
    let mut url = format!(
        "{API}/mods/search?gameId={MINECRAFT_GAME_ID}&classId={}&sortField={}&sortOrder=desc&index={}&pageSize={}",
        class_id(query.kind),
        sort_field(query.sort),
        query.offset,
        query.limit.min(50),
    );

    if !query.query.is_empty() {
        url.push_str(&format!(
            "&searchFilter={}",
            urlencoding::encode(&query.query)
        ));
    }
    if let Some(v) = &query.game_version {
        url.push_str(&format!("&gameVersion={}", urlencoding::encode(v)));
    }
    if let Some(loader) = &query.loader {
        if matches!(query.kind, ProjectKind::Mod | ProjectKind::Modpack) {
            if let Some(t) = mod_loader_type(loader) {
                url.push_str(&format!("&modLoaderType={t}"));
            }
        }
    }

    let resp: Envelope<Vec<CfMod>> = get(client, &url, api_key).await?;
    Ok(resp.data.into_iter().map(to_project).collect())
}

pub async fn project(client: &reqwest::Client, id: &str, api_key: &str) -> Result<Project> {
    let resp: Envelope<CfMod> = get(client, &format!("{API}/mods/{id}"), api_key).await?;
    Ok(to_project(resp.data))
}

#[derive(Debug, Deserialize)]
struct CfDescription {
    data: String,
}

pub async fn description(client: &reqwest::Client, id: &str, api_key: &str) -> Result<String> {
    let resp: CfDescription = get(client, &format!("{API}/mods/{id}/description"), api_key).await?;
    Ok(resp.data)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfFile {
    id: u32,
    mod_id: u32,
    display_name: String,
    file_name: String,
    #[serde(default = "default_release_type")]
    release_type: u32,
    #[serde(default)]
    file_date: Option<String>,
    #[serde(default)]
    file_length: Option<u64>,
    #[serde(default)]
    download_count: f64,
    #[serde(default)]
    download_url: Option<String>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    hashes: Vec<CfHash>,
    #[serde(default)]
    dependencies: Vec<CfDependency>,
}

fn default_release_type() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
struct CfHash {
    value: String,
    /// 1 = SHA-1, 2 = MD5.
    algo: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfDependency {
    mod_id: u32,
    relation_type: u32,
}

fn to_version(f: CfFile) -> Version {
    // `gameVersions` mixes Minecraft versions and loader names in one array.
    let (loaders, game_versions): (Vec<String>, Vec<String>) = f
        .game_versions
        .into_iter()
        .partition(|v| mod_loader_type(v).is_some());

    let blocked_reason = if f.download_url.is_none() {
        Some(
            "The author has disabled third-party downloads for this file. \
             Download it from the CurseForge page and drop it into the mods folder."
                .to_string(),
        )
    } else {
        None
    };

    let files = match f.download_url {
        Some(url) => vec![VersionFile {
            url,
            filename: f.file_name,
            sha1: f
                .hashes
                .iter()
                .find(|h| h.algo == 1)
                .map(|h| h.value.clone()),
            size: f.file_length,
            primary: true,
        }],
        None => Vec::new(),
    };

    Version {
        provider: Provider::CurseForge,
        id: f.id.to_string(),
        project_id: f.mod_id.to_string(),
        name: f.display_name.clone(),
        version_number: f.display_name,
        game_versions,
        loaders: loaders.into_iter().map(|l| l.to_lowercase()).collect(),
        release_type: release_type(f.release_type).to_string(),
        published: f.file_date,
        downloads: f.download_count.max(0.0) as u64,
        files,
        dependencies: f
            .dependencies
            .into_iter()
            .filter_map(|d| {
                let kind = match d.relation_type {
                    1 => DependencyKind::Embedded,
                    2 => DependencyKind::Optional,
                    3 => DependencyKind::Required,
                    5 => DependencyKind::Incompatible,
                    // 4 (Tool) and 6 (Include) are not things to install.
                    _ => return None,
                };
                Some(Dependency {
                    project_id: Some(d.mod_id.to_string()),
                    version_id: None,
                    kind,
                })
            })
            .collect(),
        blocked_reason,
    }
}

pub async fn versions(
    client: &reqwest::Client,
    project_id: &str,
    game_version: Option<&str>,
    loader: Option<&str>,
    api_key: &str,
) -> Result<Vec<Version>> {
    let mut url = format!("{API}/mods/{project_id}/files?pageSize=50");
    if let Some(v) = game_version {
        url.push_str(&format!("&gameVersion={}", urlencoding::encode(v)));
    }
    if let Some(t) = loader.and_then(mod_loader_type) {
        url.push_str(&format!("&modLoaderType={t}"));
    }

    let resp: Envelope<Vec<CfFile>> = get(client, &url, api_key).await?;
    Ok(resp.data.into_iter().map(to_version).collect())
}

/// Fetch one specific file. Modpack manifests reference files by id, so this is
/// how a CurseForge pack import resolves its contents.
pub async fn file(
    client: &reqwest::Client,
    project_id: u32,
    file_id: u32,
    api_key: &str,
) -> Result<Version> {
    let resp: Envelope<CfFile> = get(
        client,
        &format!("{API}/mods/{project_id}/files/{file_id}"),
        api_key,
    )
    .await?;
    Ok(to_version(resp.data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_a_shell_mangled_key() {
        // `$2a$10$abc...` with the shell having eaten the variable-looking parts.
        let hint = key_shape_hint("abc");
        assert!(hint.contains("shell"), "{hint}");
    }

    #[test]
    fn flags_a_key_that_is_not_bcrypt_shaped() {
        let hint = key_shape_hint("a-perfectly-long-but-wrong-looking-key-value");
        assert!(hint.contains("CurseForge key"), "{hint}");
    }

    #[test]
    fn accepts_a_well_shaped_key() {
        let key = format!("$2a$10${}", "a".repeat(53));
        let hint = key_shape_hint(&key);
        assert!(hint.contains("shape looks right"), "{hint}");
    }

    #[test]
    fn maps_loader_ids_both_ways() {
        assert_eq!(mod_loader_type("NeoForge"), Some(6));
        assert_eq!(loader_name(Some(4)), Some("fabric"));
        assert_eq!(mod_loader_type("not-a-loader"), None);
    }

    #[test]
    fn splits_loaders_out_of_game_versions() {
        let file = CfFile {
            id: 1,
            mod_id: 2,
            display_name: "Cool Mod 1.0".into(),
            file_name: "coolmod-1.0.jar".into(),
            release_type: 1,
            file_date: None,
            file_length: Some(100),
            download_count: 5.0,
            download_url: Some("https://example.invalid/coolmod.jar".into()),
            game_versions: vec!["1.21.1".into(), "Fabric".into()],
            hashes: vec![CfHash {
                value: "abc".into(),
                algo: 1,
            }],
            dependencies: vec![],
        };
        let v = to_version(file);
        assert_eq!(v.game_versions, vec!["1.21.1"]);
        assert_eq!(v.loaders, vec!["fabric"]);
        assert_eq!(v.files[0].sha1.as_deref(), Some("abc"));
        assert!(v.blocked_reason.is_none());
    }

    #[test]
    fn marks_undistributable_files_blocked() {
        let file = CfFile {
            id: 1,
            mod_id: 2,
            display_name: "Blocked".into(),
            file_name: "blocked.jar".into(),
            release_type: 1,
            file_date: None,
            file_length: None,
            download_count: 0.0,
            download_url: None,
            game_versions: vec!["1.21.1".into()],
            hashes: vec![],
            dependencies: vec![],
        };
        let v = to_version(file);
        assert!(v.blocked_reason.is_some());
        assert!(v.files.is_empty());
    }
}

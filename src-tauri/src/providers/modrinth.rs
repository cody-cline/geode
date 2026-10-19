//! Modrinth (api.modrinth.com/v2). Open, keyless, and the default source.

use serde::Deserialize;

use crate::error::Result;
use crate::net::get_json;

use super::{
    Dependency, DependencyKind, Project, ProjectKind, Provider, SearchQuery, SortBy, Version,
    VersionFile,
};

const API: &str = "https://api.modrinth.com/v2";

fn kind_facet(kind: ProjectKind) -> &'static str {
    match kind {
        ProjectKind::Mod => "mod",
        ProjectKind::Modpack => "modpack",
        ProjectKind::ResourcePack => "resourcepack",
        ProjectKind::Shader => "shader",
        ProjectKind::DataPack => "datapack",
        ProjectKind::Plugin => "plugin",
    }
}

fn kind_from_str(s: &str) -> ProjectKind {
    ProjectKind::parse(s).unwrap_or(ProjectKind::Mod)
}

fn sort_index(sort: SortBy) -> &'static str {
    match sort {
        SortBy::Relevance => "relevance",
        SortBy::Downloads => "downloads",
        SortBy::Follows => "follows",
        SortBy::Newest => "newest",
        SortBy::Updated => "updated",
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    hits: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct SearchHit {
    project_id: String,
    slug: String,
    title: String,
    description: String,
    #[serde(default)]
    project_type: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    follows: u64,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    versions: Vec<String>,
    #[serde(default)]
    date_modified: Option<String>,
    #[serde(default)]
    featured_gallery: Option<String>,
    #[serde(default)]
    gallery: Vec<String>,
    #[serde(default)]
    client_side: Option<String>,
    #[serde(default)]
    server_side: Option<String>,
}

/// Modrinth lumps loaders in with categories. Splitting them keeps the UI's
/// category filter from offering "fabric" as a genre.
const LOADER_CATEGORIES: &[&str] = &[
    "fabric",
    "forge",
    "neoforge",
    "quilt",
    "liteloader",
    "modloader",
    "rift",
    "bukkit",
    "spigot",
    "paper",
    "purpur",
    "sponge",
    "velocity",
    "bungeecord",
    "waterfall",
    "folia",
    "iris",
    "optifine",
    "canvas",
    "vanilla",
    "minecraft",
    "datapack",
];

fn split_loaders(categories: Vec<String>) -> (Vec<String>, Vec<String>) {
    categories
        .into_iter()
        .partition(|c| LOADER_CATEGORIES.contains(&c.as_str()))
}

fn to_project(hit: SearchHit) -> Project {
    let kind = kind_from_str(&hit.project_type);
    let (loaders, categories) = split_loaders(hit.categories);
    Project {
        provider: Provider::Modrinth,
        page_url: format!("https://modrinth.com/{}/{}", hit.project_type, hit.slug),
        id: hit.project_id,
        slug: hit.slug,
        name: hit.title,
        summary: hit.description,
        icon_url: hit.icon_url,
        downloads: hit.downloads,
        follows: hit.follows,
        author: hit.author,
        kind,
        categories,
        loaders,
        game_versions: hit.versions,
        updated: hit.date_modified,
        banner_url: hit
            .featured_gallery
            .or_else(|| hit.gallery.into_iter().next()),
        distribution_allowed: true,
        client_side: hit.client_side,
        server_side: hit.server_side,
        also_on: Vec::new(),
    }
}

pub async fn search(client: &reqwest::Client, query: &SearchQuery) -> Result<Vec<Project>> {
    // Facets are AND across the outer array, OR within each inner array.
    let mut facets: Vec<Vec<String>> =
        vec![vec![format!("project_type:{}", kind_facet(query.kind))]];

    if let Some(v) = &query.game_version {
        facets.push(vec![format!("versions:{v}")]);
    }
    if let Some(loader) = &query.loader {
        // Resource packs and shaders are loader-agnostic; filtering them by
        // loader would return nothing.
        if matches!(query.kind, ProjectKind::Mod | ProjectKind::Modpack) {
            facets.push(vec![format!("categories:{loader}")]);
        }
    }
    for category in &query.categories {
        facets.push(vec![format!("categories:{category}")]);
    }

    let url = format!(
        "{API}/search?query={}&facets={}&index={}&offset={}&limit={}",
        urlencoding::encode(&query.query),
        urlencoding::encode(&serde_json::to_string(&facets)?),
        sort_index(query.sort),
        query.offset,
        query.limit.min(100),
    );

    let resp: SearchResponse = get_json(client, &url).await?;
    Ok(resp.hits.into_iter().map(to_project).collect())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ProjectResponse {
    id: String,
    slug: String,
    title: String,
    description: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    project_type: String,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    followers: u64,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    #[serde(default)]
    updated: Option<String>,
    #[serde(default)]
    gallery: Vec<GalleryEntry>,
    #[serde(default)]
    client_side: Option<String>,
    #[serde(default)]
    server_side: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GalleryEntry {
    url: String,
    #[serde(default)]
    featured: bool,
}

pub async fn project(client: &reqwest::Client, id: &str) -> Result<Project> {
    let url = format!("{API}/project/{}", urlencoding::encode(id));
    let p: ProjectResponse = get_json(client, &url).await?;
    let kind = kind_from_str(&p.project_type);
    let (_, categories) = split_loaders(p.categories);
    Ok(Project {
        provider: Provider::Modrinth,
        page_url: format!("https://modrinth.com/{}/{}", p.project_type, p.slug),
        id: p.id,
        slug: p.slug,
        name: p.title,
        summary: p.description,
        icon_url: p.icon_url,
        downloads: p.downloads,
        follows: p.followers,
        // The search index carries an author; the project endpoint does not,
        // and a separate members call is not worth a round-trip here.
        author: String::new(),
        kind,
        categories,
        loaders: p.loaders,
        game_versions: p.game_versions,
        updated: p.updated,
        banner_url: p
            .gallery
            .iter()
            .find(|g| g.featured)
            .or_else(|| p.gallery.first())
            .map(|g| g.url.clone()),
        distribution_allowed: true,
        client_side: p.client_side,
        server_side: p.server_side,
        also_on: Vec::new(),
    })
}

pub async fn description(client: &reqwest::Client, id: &str) -> Result<String> {
    let url = format!("{API}/project/{}", urlencoding::encode(id));
    let p: ProjectResponse = get_json(client, &url).await?;
    Ok(p.body)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct VersionResponse {
    id: String,
    project_id: String,
    name: String,
    version_number: String,
    #[serde(default)]
    version_type: String,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    date_published: Option<String>,
    #[serde(default)]
    files: Vec<FileResponse>,
    #[serde(default)]
    dependencies: Vec<DependencyResponse>,
}

#[derive(Debug, Deserialize)]
struct FileResponse {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    hashes: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct DependencyResponse {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    version_id: Option<String>,
    #[serde(default)]
    dependency_type: String,
}

fn to_version(v: VersionResponse) -> Version {
    Version {
        provider: Provider::Modrinth,
        id: v.id,
        project_id: v.project_id,
        name: v.name,
        version_number: v.version_number,
        game_versions: v.game_versions,
        loaders: v.loaders,
        release_type: if v.version_type.is_empty() {
            "release".into()
        } else {
            v.version_type
        },
        published: v.date_published,
        downloads: v.downloads,
        files: v
            .files
            .into_iter()
            .map(|f| VersionFile {
                sha1: f.hashes.get("sha1").cloned(),
                sha512: f.hashes.get("sha512").cloned(),
                url: f.url,
                filename: f.filename,
                size: f.size,
                primary: f.primary,
            })
            .collect(),
        dependencies: v
            .dependencies
            .into_iter()
            .map(|d| Dependency {
                project_id: d.project_id,
                version_id: d.version_id,
                kind: match d.dependency_type.as_str() {
                    "required" => DependencyKind::Required,
                    "incompatible" => DependencyKind::Incompatible,
                    "embedded" => DependencyKind::Embedded,
                    _ => DependencyKind::Optional,
                },
            })
            .collect(),
        blocked_reason: None,
    }
}

pub async fn versions(
    client: &reqwest::Client,
    project_id: &str,
    game_version: Option<&str>,
    loader: Option<&str>,
) -> Result<Vec<Version>> {
    let mut url = format!("{API}/project/{}/version", urlencoding::encode(project_id));
    let mut params = Vec::new();
    if let Some(v) = game_version {
        params.push(format!(
            "game_versions={}",
            urlencoding::encode(&format!("[\"{v}\"]"))
        ));
    }
    if let Some(l) = loader {
        params.push(format!(
            "loaders={}",
            urlencoding::encode(&format!("[\"{l}\"]"))
        ));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }

    let versions: Vec<VersionResponse> = get_json(client, &url).await?;
    Ok(versions.into_iter().map(to_version).collect())
}

/// Look a file up by its SHA-1. Used to recognise mods that were dropped into
/// the folder by hand, so they can still be tracked and updated.
pub async fn version_by_hash(client: &reqwest::Client, sha1: &str) -> Result<Option<Version>> {
    let url = format!("{API}/version_file/{sha1}?algorithm=sha1");
    let resp = client.get(&url).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Ok(None);
    }
    let v: VersionResponse = resp.json().await?;
    Ok(Some(to_version(v)))
}

/// Resolve many projects at once. Used to put names and icons on files that a
/// modpack index identified only by id.
pub async fn projects_by_ids(client: &reqwest::Client, ids: &[String]) -> Result<Vec<Project>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    // The endpoint takes ids in the query string, so keep each request short
    // enough that no proxy truncates the URL.
    for chunk in ids.chunks(100) {
        let encoded = serde_json::to_string(chunk)?;
        let url = format!("{API}/projects?ids={}", urlencoding::encode(&encoded));
        let batch: Vec<ProjectResponse> = get_json(client, &url).await?;
        out.extend(batch.into_iter().map(|p| {
            let kind = kind_from_str(&p.project_type);
            let (_, categories) = split_loaders(p.categories);
            Project {
                provider: Provider::Modrinth,
                page_url: format!("https://modrinth.com/{}/{}", p.project_type, p.slug),
                id: p.id,
                slug: p.slug,
                name: p.title,
                summary: p.description,
                icon_url: p.icon_url,
                downloads: p.downloads,
                follows: p.followers,
                author: String::new(),
                kind,
                categories,
                loaders: p.loaders,
                game_versions: p.game_versions,
                updated: p.updated,
                banner_url: p
                    .gallery
                    .iter()
                    .find(|g| g.featured)
                    .or_else(|| p.gallery.first())
                    .map(|g| g.url.clone()),
                distribution_allowed: true,
                client_side: p.client_side,
                server_side: p.server_side,
                also_on: Vec::new(),
            }
        }));
    }
    Ok(out)
}

/// The two algorithms Modrinth's bulk hash endpoint accepts. An enum rather
/// than a string because the one value both goes on the wire and decides which
/// of a candidate file's hashes it is compared against. As a `&str` any third
/// spelling — `"sha-512"`, `"SHA512"` — would be sent as given and then
/// quietly compared against sha1, and the answer to "which of these 200 mods
/// does Modrinth know" would come back empty with complete confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Sha1,
    Sha512,
}

impl HashAlgorithm {
    /// Exactly what the endpoint's `algorithm` field takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha512 => "sha512",
        }
    }
}

/// Identify files by hash. This is how a jar with no provenance — dropped in
/// by hand, or written by a pack index that records only URLs — gets linked
/// back to the project it came from.
///
/// A hash lookup answers with a version, and a version can hold several files:
/// a sources jar, the other loader's build, and the one that was asked about.
/// So each hit comes back paired with the file whose hash actually matched,
/// and an answer carrying no such file is no match at all.
pub async fn files_by_hashes(
    client: &reqwest::Client,
    hashes: &[String],
    algorithm: HashAlgorithm,
) -> Result<std::collections::HashMap<String, (Version, VersionFile)>> {
    let mut found = std::collections::HashMap::new();
    if hashes.is_empty() {
        return Ok(found);
    }

    for chunk in hashes.chunks(500) {
        let body = serde_json::json!({ "hashes": chunk, "algorithm": algorithm.as_str() });
        let resp = client
            .post(format!("{API}/version_files"))
            .json(&body)
            .send()
            .await?;
        // A chunk turned away — rate-limited, or the API having a bad minute —
        // reads to the caller exactly like a chunk where nothing matched. That
        // is tolerable while a miss only means "no provenance yet", and will
        // not be once a miss decides whether a file travels as bytes.
        if !resp.status().is_success() {
            continue;
        }
        found.extend(matches_in(resp.json().await?, algorithm));
    }
    Ok(found)
}

/// One answered chunk turned into the hits it actually carries, kept apart
/// from the request so it can be read against a hand-written body. This is
/// where a hash whose version holds no file of that hash is dropped instead of
/// paired with whatever file the version happened to list: the pack would
/// carry the local file's hashes beside another artifact's URL, and every
/// launcher reading it would fail the checksum.
fn matches_in(
    batch: std::collections::HashMap<String, VersionResponse>,
    algorithm: HashAlgorithm,
) -> std::collections::HashMap<String, (Version, VersionFile)> {
    let mut found = std::collections::HashMap::new();
    for (hash, version) in batch {
        let hash = hash.to_lowercase();
        let version = to_version(version);
        let Some(file) = matched_file(&version, &hash, algorithm) else {
            continue;
        };
        found.insert(hash, (version, file));
    }
    found
}

/// The file in a version that carries the hash that was asked about, if any.
fn matched_file(version: &Version, hash: &str, algorithm: HashAlgorithm) -> Option<VersionFile> {
    version
        .files
        .iter()
        .find(|f| {
            let theirs = match algorithm {
                HashAlgorithm::Sha1 => f.sha1.as_deref(),
                HashAlgorithm::Sha512 => f.sha512.as_deref(),
            };
            theirs.is_some_and(|h| h.eq_ignore_ascii_case(hash))
        })
        .cloned()
}

#[derive(Debug, Deserialize)]
struct CategoryResponse {
    name: String,
    project_type: String,
}

/// Category facets for the filter sidebar.
pub async fn categories(client: &reqwest::Client, kind: ProjectKind) -> Result<Vec<String>> {
    let all: Vec<CategoryResponse> = get_json(client, &format!("{API}/tag/category")).await?;
    let want = kind_facet(kind);
    Ok(all
        .into_iter()
        .filter(|c| c.project_type == want)
        .map(|c| c.name)
        .filter(|n| !LOADER_CATEGORIES.contains(&n.as_str()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One version as the bulk hash endpoint hands it over: the jar that was
    /// asked about is neither the first file nor the primary one.
    const VERSION_WITH_THREE_FILES: &str = r#"{
            "id": "AAAAAAAA",
            "project_id": "PPPPPPPP",
            "name": "Cool Mod 1.2.3",
            "version_number": "1.2.3",
            "version_type": "release",
            "game_versions": ["1.21.1"],
            "loaders": ["fabric", "neoforge"],
            "downloads": 42,
            "files": [
                {
                    "url": "https://cdn.modrinth.com/data/PPPPPPPP/versions/AAAAAAAA/cool-1.2.3-sources.jar",
                    "filename": "cool-1.2.3-sources.jar",
                    "primary": true,
                    "size": 111,
                    "hashes": { "sha1": "a1", "sha512": "a512" }
                },
                {
                    "url": "https://cdn.modrinth.com/data/PPPPPPPP/versions/AAAAAAAA/cool-1.2.3-fabric.jar",
                    "filename": "cool-1.2.3-fabric.jar",
                    "primary": false,
                    "size": 222,
                    "hashes": { "sha1": "b1", "sha512": "b512" }
                },
                {
                    "url": "https://cdn.modrinth.com/data/PPPPPPPP/versions/AAAAAAAA/cool-1.2.3-neoforge.jar",
                    "filename": "cool-1.2.3-neoforge.jar",
                    "primary": false,
                    "size": 333,
                    "hashes": { "sha1": "c1", "sha512": "c512" }
                }
            ]
        }"#;

    /// That version, keyed by the hash the lookup asked about, as one answered
    /// chunk of the endpoint's reply.
    fn answer_for(hash: &str) -> std::collections::HashMap<String, VersionResponse> {
        [(
            hash.to_string(),
            serde_json::from_str(VERSION_WITH_THREE_FILES).unwrap(),
        )]
        .into()
    }

    /// The one mistake that would make a pack install bytes other than the
    /// ones it was exported from: taking the version's primary file instead of
    /// the file whose hash was looked up.
    #[test]
    fn a_hash_lookup_picks_the_file_that_matched_not_the_primary_one() {
        // Upper case going in, to pin the lowercasing of the answer's key.
        let found = matches_in(answer_for("B512"), HashAlgorithm::Sha512);

        let (version, file) = found.get("b512").expect("the fabric jar");
        assert_eq!(version.id, "AAAAAAAA");
        assert_eq!(file.filename, "cool-1.2.3-fabric.jar");
        assert_eq!(
            file.url,
            "https://cdn.modrinth.com/data/PPPPPPPP/versions/AAAAAAAA/cool-1.2.3-fabric.jar"
        );
        assert_eq!(file.size, Some(222));
        assert_eq!(file.sha512.as_deref(), Some("b512"));
        assert_eq!(file.sha1.as_deref(), Some("b1"));
    }

    /// A version that answered but holds no file with the hash asked about is
    /// not an answer. It has to leave the map empty rather than fall back to
    /// some file of that version — naming an arbitrary one would be a wrong
    /// match a receiving launcher then fails the checksum on.
    #[test]
    fn a_hash_with_no_matching_file_in_its_version_is_not_a_match() {
        assert!(matches_in(answer_for("d512"), HashAlgorithm::Sha512).is_empty());
        // And asking with the wrong algorithm is a miss, not a sideways hit.
        assert!(matches_in(answer_for("b512"), HashAlgorithm::Sha1).is_empty());
    }

    /// The algorithm name goes on the wire and chooses which hash a candidate
    /// is compared against, so these two spellings are the whole agreement
    /// between Modrinth's endpoint and the comparison here. Either one wrong
    /// turns every lookup into a confident "nothing matched".
    #[test]
    fn the_two_algorithms_are_spelled_the_way_modrinth_takes_them() {
        assert_eq!(HashAlgorithm::Sha1.as_str(), "sha1");
        assert_eq!(HashAlgorithm::Sha512.as_str(), "sha512");
    }

    #[test]
    fn separates_loaders_from_genres() {
        let (loaders, categories) = split_loaders(vec![
            "fabric".into(),
            "optimization".into(),
            "neoforge".into(),
            "utility".into(),
        ]);
        assert_eq!(loaders, vec!["fabric", "neoforge"]);
        assert_eq!(categories, vec!["optimization", "utility"]);
    }
}

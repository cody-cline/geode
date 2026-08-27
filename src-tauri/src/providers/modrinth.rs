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
        distribution_allowed: true,
        also_on: Vec::new(),
    }
}

pub async fn search(client: &reqwest::Client, query: &SearchQuery) -> Result<Vec<Project>> {
    // Facets are AND across the outer array, OR within each inner array.
    let mut facets: Vec<Vec<String>> = vec![vec![format!(
        "project_type:{}",
        kind_facet(query.kind)
    )]];

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
        distribution_allowed: true,
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

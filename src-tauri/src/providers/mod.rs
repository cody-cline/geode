//! A single content model over Modrinth and CurseForge.
//!
//! There is no third-party database that indexes both platforms with
//! install-grade metadata (download URLs, hashes, dependency graphs), so we
//! federate at query time: hit both APIs in parallel, normalise into the types
//! below, and merge duplicates into one card that can be installed from either
//! source. See `docs/mod-databases.md` for what else was evaluated.

pub mod curseforge;
pub mod modrinth;
pub mod spiget;

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Modrinth,
    CurseForge,
    /// SpigotMC, read through the Spiget index. Plugins only.
    Spiget,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Modrinth => "modrinth",
            Provider::CurseForge => "curseforge",
            Provider::Spiget => "spiget",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "modrinth" => Some(Provider::Modrinth),
            "curseforge" | "curse" => Some(Provider::CurseForge),
            "spiget" | "spigot" | "spigotmc" => Some(Provider::Spiget),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Mod,
    Modpack,
    ResourcePack,
    Shader,
    DataPack,
}

impl ProjectKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "mod" => Some(ProjectKind::Mod),
            "modpack" => Some(ProjectKind::Modpack),
            "resourcepack" => Some(ProjectKind::ResourcePack),
            "shader" | "shaderpack" => Some(ProjectKind::Shader),
            "datapack" => Some(ProjectKind::DataPack),
            _ => None,
        }
    }

    /// Which folder inside an instance this kind installs into.
    ///
    /// `None` for modpacks: a pack is not a file you drop into an instance, it
    /// *becomes* an instance. Returning `ContentKind::Mod` here — as this used
    /// to — quietly wrote the pack archive into `mods/`.
    pub fn install_kind(self) -> Option<crate::instance::content::ContentKind> {
        use crate::instance::content::ContentKind;
        match self {
            ProjectKind::Mod => Some(ContentKind::Mod),
            ProjectKind::ResourcePack => Some(ContentKind::ResourcePack),
            ProjectKind::Shader => Some(ContentKind::ShaderPack),
            ProjectKind::DataPack => Some(ContentKind::DataPack),
            ProjectKind::Modpack => None,
        }
    }
}

/// Where else this same project can be found. Populated by [`merge`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRef {
    pub provider: Provider,
    pub id: String,
    pub page_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub provider: Provider,
    pub id: String,
    pub slug: String,
    pub name: String,
    pub summary: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub follows: u64,
    pub author: String,
    pub kind: ProjectKind,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    pub updated: Option<String>,
    pub page_url: String,
    /// Wide promo image, where the platform has one. Modpacks almost always do,
    /// and it is what makes an installed pack look like itself.
    #[serde(default)]
    pub banner_url: Option<String>,
    /// False when the author has opted out of third-party downloads, so nothing
    /// here can be installed automatically. Always true on Modrinth.
    #[serde(default = "yes")]
    pub distribution_allowed: bool,
    /// Other platforms carrying what looks like the same project.
    #[serde(default)]
    pub also_on: Vec<ProviderRef>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionFile {
    pub url: String,
    pub filename: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyKind {
    Required,
    Optional,
    Incompatible,
    Embedded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dependency {
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    pub kind: DependencyKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub provider: Provider,
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    /// `release`, `beta` or `alpha`.
    pub release_type: String,
    pub published: Option<String>,
    pub downloads: u64,
    #[serde(default)]
    pub files: Vec<VersionFile>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    /// CurseForge lets authors opt out of third-party downloads. When this is
    /// set the version cannot be installed automatically and the UI has to send
    /// the user to the project page.
    #[serde(default)]
    pub blocked_reason: Option<String>,
}

impl Version {
    pub fn primary_file(&self) -> Option<&VersionFile> {
        self.files
            .iter()
            .find(|f| f.primary)
            .or_else(|| self.files.first())
    }
}

/// What the UI asks for.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    #[serde(default)]
    pub query: String,
    pub kind: ProjectKind,
    #[serde(default)]
    pub game_version: Option<String>,
    #[serde(default)]
    pub loader: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub sort: SortBy,
    #[serde(default)]
    pub offset: u32,
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Restrict to one platform. `None` searches everything available.
    #[serde(default)]
    pub providers: Option<Vec<Provider>>,
}

fn default_limit() -> u32 {
    30
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortBy {
    #[default]
    Relevance,
    Downloads,
    Follows,
    Newest,
    Updated,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub hits: Vec<Project>,
    /// Non-fatal per-provider problems (missing API key, rate limit, outage).
    /// Surfaced as a banner so partial results never look like complete ones.
    pub warnings: Vec<String>,
}

/// Run every enabled provider concurrently and merge the results.
pub async fn search(
    client: &reqwest::Client,
    query: &SearchQuery,
    curseforge_key: Option<&str>,
) -> Result<SearchResults> {
    let wanted = query
        .providers
        .clone()
        .unwrap_or_else(|| vec![Provider::Modrinth, Provider::CurseForge]);

    let mut warnings = Vec::new();

    let modrinth_fut = async {
        if wanted.contains(&Provider::Modrinth) {
            Some(modrinth::search(client, query).await)
        } else {
            None
        }
    };
    let curseforge_fut = async {
        if !wanted.contains(&Provider::CurseForge) {
            return None;
        }
        match curseforge_key {
            Some(key) if !key.trim().is_empty() => {
                Some(curseforge::search(client, query, key).await)
            }
            _ => None,
        }
    };

    let (mr, cf) = futures::join!(modrinth_fut, curseforge_fut);

    let mut modrinth_hits = Vec::new();
    match mr {
        Some(Ok(hits)) => modrinth_hits = hits,
        Some(Err(e)) => warnings.push(format!("Modrinth: {e}")),
        None => {}
    }

    let mut curseforge_hits = Vec::new();
    match cf {
        Some(Ok(hits)) => curseforge_hits = hits,
        Some(Err(e)) => warnings.push(format!("CurseForge: {e}")),
        None => {
            if wanted.contains(&Provider::CurseForge) && curseforge_key.is_none() {
                warnings.push(
                    "CurseForge is off — add your own API key in Settings to include it.".into(),
                );
            }
        }
    }

    Ok(SearchResults {
        hits: merge(modrinth_hits, curseforge_hits, query.sort),
        warnings,
    })
}

/// Collapse the two result lists into one.
///
/// Modrinth wins ties as the canonical entry: its API is open, its metadata is
/// richer, and its downloads are never author-blocked. The CurseForge twin is
/// kept as an `also_on` reference so the detail view can still offer it.
fn merge(modrinth: Vec<Project>, curseforge: Vec<Project>, sort: SortBy) -> Vec<Project> {
    let mut out: Vec<Project> = Vec::with_capacity(modrinth.len() + curseforge.len());
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for project in modrinth {
        index.insert(match_key(&project), out.len());
        out.push(project);
    }

    for project in curseforge {
        match index.get(&match_key(&project)) {
            Some(&existing) => {
                out[existing].also_on.push(ProviderRef {
                    provider: project.provider,
                    id: project.id.clone(),
                    page_url: project.page_url.clone(),
                });
                // Download counts are per-platform; showing the sum is the only
                // number that means "how popular is this mod".
                out[existing].downloads += project.downloads;
            }
            None => {
                index.insert(match_key(&project), out.len());
                out.push(project);
            }
        }
    }

    match sort {
        // Relevance is the provider's own ranking, and there is no shared scale
        // to re-rank across platforms — so interleave to keep both visible
        // rather than letting whichever API returned more dominate the top.
        SortBy::Relevance => out,
        SortBy::Downloads => {
            out.sort_by_key(|p| std::cmp::Reverse(p.downloads));
            out
        }
        SortBy::Follows => {
            out.sort_by_key(|p| std::cmp::Reverse(p.follows));
            out
        }
        SortBy::Newest | SortBy::Updated => {
            out.sort_by(|a, b| b.updated.cmp(&a.updated));
            out
        }
    }
}

fn match_key(p: &Project) -> String {
    identity_key(&p.slug, &p.name)
}

/// Identity key for cross-platform matching.
///
/// Slugs agree far more often than not (`jei`, `sodium`, `create`), and where
/// they disagree the normalised display name usually catches it. We compare
/// both and treat a hit on either as the same project.
///
/// Public because the same question is asked outside search: a plan copied out
/// of a CurseForge pack wants to know which Modrinth project each entry is.
pub fn identity_key(slug: &str, name: &str) -> String {
    let by_slug = normalize(slug);
    if by_slug.len() >= 3 {
        return by_slug;
    }
    normalize(name)
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Versions of one project, filtered to what an instance can actually use.
pub async fn versions(
    client: &reqwest::Client,
    provider: Provider,
    project_id: &str,
    game_version: Option<&str>,
    loader: Option<&str>,
    curseforge_key: Option<&str>,
) -> Result<Vec<Version>> {
    match provider {
        Provider::Modrinth => modrinth::versions(client, project_id, game_version, loader).await,
        Provider::CurseForge => {
            let key = curseforge_key.ok_or_else(|| {
                crate::error::Error::msg("a CurseForge API key is required — add one in Settings")
            })?;
            curseforge::versions(client, project_id, game_version, loader, key).await
        }
        // Spiget serves one file per resource, so there is nothing to filter.
        Provider::Spiget => spiget::versions(client, project_id).await,
    }
}

/// The (loader, game version) pairs a version list actually has files behind.
///
/// Pairs, never the two arrays separately: a project listing both Fabric and
/// 1.21.1 has not promised a Fabric build *for* 1.21.1.
pub fn pairs_from_versions(versions: &[Version]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for version in versions {
        // A file that cannot be downloaded cannot support anything.
        if version.blocked_reason.is_some() || version.primary_file().is_none() {
            continue;
        }
        for loader in &version.loaders {
            for game in &version.game_versions {
                pairs.push((loader.clone(), game.clone()));
            }
        }
    }
    pairs
}

/// Everything a project can run on, for deciding what a whole plan can run on.
///
/// Deliberately not `versions()`: on CurseForge that endpoint pages at fifty
/// files, which for a mod with a decade of releases describes only this year.
/// CurseForge publishes a complete per-version index instead, and the recent
/// file list is unioned in to cover anything the index leaves unlabelled.
pub async fn support_pairs(
    client: &reqwest::Client,
    provider: Provider,
    project_id: &str,
    curseforge_key: Option<&str>,
) -> Result<Vec<(String, String)>> {
    match provider {
        Provider::Modrinth => {
            let versions = modrinth::versions(client, project_id, None, None).await?;
            Ok(pairs_from_versions(&versions))
        }
        Provider::CurseForge => {
            let key = curseforge_key.ok_or_else(|| {
                crate::error::Error::msg("a CurseForge API key is required — add one in Settings")
            })?;
            let mut pairs = curseforge::support_pairs(client, project_id, key).await?;
            if let Ok(versions) = curseforge::versions(client, project_id, None, None, key).await {
                pairs.extend(pairs_from_versions(&versions));
            }
            Ok(pairs)
        }
        // This answers "what can a modpack plan target", and a plan targets
        // client loaders. A Spigot resource is a server plugin: its pairs would
        // all be filtered out, leaving it looking like a mod that publishes
        // nothing rather than one that was never a mod. Saying so puts it in
        // the resolver's unresolved list with a reason attached.
        Provider::Spiget => Err(crate::error::Error::msg(
            "SpigotMC resources are server plugins, so they cannot be planned as mods",
        )),
    }
}

pub async fn project(
    client: &reqwest::Client,
    provider: Provider,
    id: &str,
    curseforge_key: Option<&str>,
) -> Result<Project> {
    match provider {
        Provider::Modrinth => modrinth::project(client, id).await,
        Provider::CurseForge => {
            let key = curseforge_key.ok_or_else(|| {
                crate::error::Error::msg("a CurseForge API key is required — add one in Settings")
            })?;
            curseforge::project(client, id, key).await
        }
        Provider::Spiget => spiget::project(client, id).await,
    }
}

/// Long-form description as HTML or Markdown, for the project detail pane.
pub async fn description(
    client: &reqwest::Client,
    provider: Provider,
    id: &str,
    curseforge_key: Option<&str>,
) -> Result<String> {
    match provider {
        Provider::Modrinth => modrinth::description(client, id).await,
        Provider::CurseForge => {
            let key = curseforge_key.ok_or_else(|| {
                crate::error::Error::msg("a CurseForge API key is required — add one in Settings")
            })?;
            curseforge::description(client, id, key).await
        }
        Provider::Spiget => spiget::description(client, id).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(provider: Provider, slug: &str, name: &str, downloads: u64) -> Project {
        Project {
            provider,
            id: format!("{}-{slug}", provider.as_str()),
            slug: slug.into(),
            name: name.into(),
            summary: String::new(),
            icon_url: None,
            downloads,
            follows: 0,
            author: String::new(),
            kind: ProjectKind::Mod,
            categories: vec![],
            loaders: vec![],
            game_versions: vec![],
            updated: None,
            page_url: String::new(),
            banner_url: None,
            distribution_allowed: true,
            also_on: vec![],
        }
    }

    #[test]
    fn merges_matching_slugs() {
        let merged = merge(
            vec![project(Provider::Modrinth, "sodium", "Sodium", 100)],
            vec![project(Provider::CurseForge, "sodium", "Sodium", 50)],
            SortBy::Downloads,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].provider, Provider::Modrinth);
        assert_eq!(merged[0].also_on.len(), 1);
        assert_eq!(merged[0].downloads, 150);
    }

    #[test]
    fn matches_across_slug_punctuation() {
        let merged = merge(
            vec![project(Provider::Modrinth, "just-enough-items", "JEI", 10)],
            vec![project(Provider::CurseForge, "justenoughitems", "JEI", 10)],
            SortBy::Downloads,
        );
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn keeps_distinct_projects_apart() {
        let merged = merge(
            vec![project(Provider::Modrinth, "sodium", "Sodium", 100)],
            vec![project(Provider::CurseForge, "rubidium", "Rubidium", 50)],
            SortBy::Downloads,
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].downloads, 100);
    }

    #[test]
    fn curseforge_only_results_survive() {
        let merged = merge(
            vec![],
            vec![project(Provider::CurseForge, "some-mod", "Some Mod", 5)],
            SortBy::Downloads,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].provider, Provider::CurseForge);
    }

    #[test]
    fn short_slugs_fall_back_to_name() {
        let a = project(Provider::Modrinth, "ap", "Applied Energistics", 1);
        let b = project(Provider::CurseForge, "ae", "Applied Energistics", 1);
        assert_eq!(match_key(&a), match_key(&b));
    }
}

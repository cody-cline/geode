//! Sinytra Connector support.
//!
//! Connector lets Fabric mods run on Forge and NeoForge. When an instance uses
//! a loader/version pair Connector covers, Geode can offer Fabric mods
//! alongside native ones and pull in the runtime they need.
//!
//! Nothing here encodes which Minecraft versions are supported. That answer
//! belongs to Connector, so it is read from the files Connector actually
//! publishes — and read as *pairs*, because a project's `loaders` and
//! `game_versions` arrays are independent unions. Connector's list both
//! Forge and NeoForge across four game versions, but Forge only ever shipped
//! for one of them; trusting the union would offer a combination with no file
//! behind it.

use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::RwLock;

use crate::error::{Error, Result};
use crate::providers::{self, Provider};

/// The runtime a Fabric mod needs on a (Neo)Forge instance.
///
/// Slugs, not versions: these identify the projects, and Modrinth slugs are
/// stable. The version chosen for each is whatever matches the instance.
pub const CONNECTOR_SLUG: &str = "connector";
pub const FORGIFIED_FABRIC_API_SLUG: &str = "forgified-fabric-api";
pub const CONNECTOR_EXTRAS_SLUG: &str = "connector-extras";

/// Installed together whenever a Fabric mod is added through Connector.
pub const STACK: &[&str] = &[
    CONNECTOR_SLUG,
    FORGIFIED_FABRIC_API_SLUG,
    CONNECTOR_EXTRAS_SLUG,
];

/// One loader/game-version pair Connector ships a file for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportPair {
    pub loader: String,
    pub game_version: String,
}

#[derive(Debug, Clone, Default)]
pub struct SupportMatrix {
    pairs: Vec<SupportPair>,
}

impl SupportMatrix {
    pub fn supports(&self, game_version: &str, loader: &str) -> bool {
        let loader = loader.to_ascii_lowercase();
        self.pairs
            .iter()
            .any(|p| p.loader == loader && p.game_version == game_version)
    }

    /// Game versions covered for one loader, for surfacing *why* a pair failed.
    pub fn versions_for(&self, loader: &str) -> Vec<String> {
        let loader = loader.to_ascii_lowercase();
        let mut out: Vec<String> = self
            .pairs
            .iter()
            .filter(|p| p.loader == loader)
            .map(|p| p.game_version.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// Build the matrix from Connector's published files.
async fn fetch_matrix(client: &reqwest::Client) -> Result<SupportMatrix> {
    let versions = providers::versions(
        client,
        Provider::Modrinth,
        CONNECTOR_SLUG,
        None,
        None,
        None,
    )
    .await?;

    let mut pairs = Vec::new();
    for version in versions {
        // A file that cannot be downloaded cannot support anything.
        if version.blocked_reason.is_some() || version.files.is_empty() {
            continue;
        }
        for loader in &version.loaders {
            for game_version in &version.game_versions {
                let pair = SupportPair {
                    loader: loader.to_ascii_lowercase(),
                    game_version: game_version.clone(),
                };
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
        }
    }

    Ok(SupportMatrix { pairs })
}

/// Caches the matrix for the session.
///
/// Browse asks about it whenever the target changes, which would otherwise mean
/// a Modrinth round-trip per keystroke-adjacent interaction.
pub struct SupportCache {
    inner: RwLock<Option<(Instant, SupportMatrix)>>,
}

impl SupportCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    pub async fn get(&self, client: &reqwest::Client) -> Result<SupportMatrix> {
        if let Some((at, matrix)) = self.inner.read().await.as_ref() {
            if at.elapsed() < Duration::from_secs(1800) {
                return Ok(matrix.clone());
            }
        }

        let matrix = fetch_matrix(client).await?;
        *self.inner.write().await = Some((Instant::now(), matrix.clone()));
        Ok(matrix)
    }
}

impl Default for SupportCache {
    fn default() -> Self {
        Self::new()
    }
}

/// What the UI needs to decide whether to offer the Fabric-mods checkbox.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorSupport {
    pub supported: bool,
    /// Game versions this loader is covered for, so the UI can explain a no.
    pub versions_for_loader: Vec<String>,
    /// Slugs installed alongside a Fabric mod, for the UI to name them.
    pub stack: Vec<String>,
}

pub async fn support_for(
    client: &reqwest::Client,
    cache: &SupportCache,
    game_version: &str,
    loader: &str,
) -> Result<ConnectorSupport> {
    // Connector only ever applies to the Forge family.
    if !matches!(loader.to_ascii_lowercase().as_str(), "forge" | "neoforge") {
        return Ok(ConnectorSupport {
            supported: false,
            versions_for_loader: Vec::new(),
            stack: STACK.iter().map(|s| s.to_string()).collect(),
        });
    }

    let matrix = cache.get(client).await?;
    Ok(ConnectorSupport {
        supported: matrix.supports(game_version, loader),
        versions_for_loader: matrix.versions_for(loader),
        stack: STACK.iter().map(|s| s.to_string()).collect(),
    })
}

/// Install Connector and its companions into an instance, skipping any already
/// present. Returns the display names of everything newly added.
///
/// Each companion is resolved against the instance's own Minecraft version and
/// loader, so this stays correct as those projects publish new builds.
pub async fn ensure_stack(
    client: &reqwest::Client,
    paths: &crate::paths::Paths,
    instance: &mut crate::instance::Instance,
    progress: &crate::net::Progress,
) -> Result<Vec<String>> {
    use crate::instance::content::ContentKind;

    let mut added = Vec::new();
    progress.stage("Preparing Connector", STACK.len() as u64).await;

    for slug in STACK {
        progress.check_cancelled()?;

        let already = instance
            .content
            .iter()
            .any(|r| r.slug.as_deref() == Some(*slug) && r.kind == ContentKind::Mod);
        if already {
            progress.advance(1, slug).await;
            continue;
        }

        let project = providers::project(client, Provider::Modrinth, slug, None).await?;
        let versions = providers::versions(
            client,
            Provider::Modrinth,
            slug,
            Some(&instance.mc_version),
            Some(instance.loader.as_str()),
            None,
        )
        .await?;

        let Some(best) = crate::install::content::pick_best(&versions) else {
            return Err(Error::msg(format!(
                "{} has no build for Minecraft {} on {}",
                project.name,
                instance.mc_version,
                instance.loader.display_name()
            )));
        };

        let file = crate::install::content::install(
            client,
            paths,
            instance,
            best,
            ContentKind::Mod,
            &project.name,
            project.icon_url.clone(),
            Some(project.slug.clone()),
            // Marked as a dependency: these came along with a Fabric mod
            // rather than being chosen directly.
            true,
        )
        .await?;

        progress.advance(1, &file).await;
        added.push(project.name);
    }

    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix() -> SupportMatrix {
        SupportMatrix {
            pairs: vec![
                SupportPair {
                    loader: "forge".into(),
                    game_version: "1.20.1".into(),
                },
                SupportPair {
                    loader: "neoforge".into(),
                    game_version: "1.21.1".into(),
                },
                SupportPair {
                    loader: "neoforge".into(),
                    game_version: "26.1.2".into(),
                },
            ],
        }
    }

    #[test]
    fn matches_only_real_pairs() {
        let m = matrix();
        assert!(m.supports("1.20.1", "forge"));
        assert!(m.supports("1.21.1", "neoforge"));
        // The union of loaders and versions would wrongly allow this: Forge
        // appears in the matrix and so does 1.21.1, but never together.
        assert!(!m.supports("1.21.1", "forge"));
        assert!(!m.supports("1.20.1", "neoforge"));
    }

    #[test]
    fn unknown_versions_are_unsupported() {
        assert!(!matrix().supports("1.19.2", "neoforge"));
    }

    #[test]
    fn loader_case_does_not_matter() {
        assert!(matrix().supports("1.21.1", "NeoForge"));
    }

    #[test]
    fn lists_versions_per_loader() {
        assert_eq!(matrix().versions_for("neoforge"), vec!["1.21.1", "26.1.2"]);
        assert_eq!(matrix().versions_for("forge"), vec!["1.20.1"]);
    }

    #[test]
    fn stack_is_the_three_runtime_projects() {
        assert_eq!(STACK.len(), 3);
        assert!(STACK.contains(&CONNECTOR_SLUG));
        assert!(STACK.contains(&FORGIFIED_FABRIC_API_SLUG));
        assert!(STACK.contains(&CONNECTOR_EXTRAS_SLUG));
    }
}

pub mod fabric;
pub mod forge;
pub mod piston;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{Error, Result};
use crate::net::get_json;
use crate::paths::Paths;

/// Which mod loader an instance runs. `Vanilla` means none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Loader {
    #[default]
    Vanilla,
    Fabric,
    Quilt,
    NeoForge,
    Forge,
}

impl Loader {
    pub fn as_str(self) -> &'static str {
        match self {
            Loader::Vanilla => "vanilla",
            Loader::Fabric => "fabric",
            Loader::Quilt => "quilt",
            Loader::NeoForge => "neoforge",
            Loader::Forge => "forge",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Loader::Vanilla => "Vanilla",
            Loader::Fabric => "Fabric",
            Loader::Quilt => "Quilt",
            Loader::NeoForge => "NeoForge",
            Loader::Forge => "Forge",
        }
    }

    /// Loaders that can load mods from a `mods/` folder.
    pub fn supports_mods(self) -> bool {
        !matches!(self, Loader::Vanilla)
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "vanilla" | "minecraft" | "" => Some(Loader::Vanilla),
            "fabric" => Some(Loader::Fabric),
            "quilt" => Some(Loader::Quilt),
            "neoforge" => Some(Loader::NeoForge),
            "forge" => Some(Loader::Forge),
            _ => None,
        }
    }
}

/// The launcher-side id for a resolved (game, loader) pair. This is the
/// directory name under `versions/` and the `--version` argument passed to the
/// game, which some mods surface in the F3 screen.
pub fn version_id(mc_version: &str, loader: Loader, loader_version: &str) -> String {
    match loader {
        Loader::Vanilla => mc_version.to_string(),
        Loader::Fabric => format!("fabric-loader-{loader_version}-{mc_version}"),
        Loader::Quilt => format!("quilt-loader-{loader_version}-{mc_version}"),
        Loader::NeoForge => forge::version_id(forge::Flavor::NeoForge, mc_version, loader_version),
        Loader::Forge => forge::version_id(forge::Flavor::Forge, mc_version, loader_version),
    }
}

// ---------------------------------------------------------------------------
// Manifest cache
// ---------------------------------------------------------------------------

/// The version manifest is ~200 KB and changes only when Mojang publishes.
/// Cache it in memory for the session and on disk as an offline fallback.
pub struct ManifestCache {
    inner: RwLock<Option<(std::time::Instant, piston::VersionManifest)>>,
}

impl ManifestCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    pub async fn get(
        &self,
        client: &reqwest::Client,
        paths: &Paths,
    ) -> Result<piston::VersionManifest> {
        if let Some((at, manifest)) = self.inner.read().await.as_ref() {
            if at.elapsed() < Duration::from_secs(600) {
                return Ok(manifest.clone());
            }
        }

        let disk = paths.cache().join("version_manifest_v2.json");
        match piston::fetch_manifest(client).await {
            Ok(manifest) => {
                if let Ok(bytes) = serde_json::to_vec(&manifest) {
                    let _ = tokio::fs::create_dir_all(&paths.cache()).await;
                    let _ = tokio::fs::write(&disk, bytes).await;
                }
                *self.inner.write().await = Some((std::time::Instant::now(), manifest.clone()));
                Ok(manifest)
            }
            Err(net_err) => {
                // Offline: an instance that is already installed should still
                // be launchable, so fall back to whatever we saw last.
                match tokio::fs::read(&disk).await {
                    Ok(bytes) => serde_json::from_slice(&bytes)
                        .map_err(|e| Error::json("cached version manifest", e)),
                    Err(_) => Err(net_err),
                }
            }
        }
    }
}

impl Default for ManifestCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Fetch (and cache on disk) the vanilla version JSON for one Minecraft version.
pub async fn vanilla_version_json(
    client: &reqwest::Client,
    paths: &Paths,
    manifest: &piston::VersionManifest,
    mc_version: &str,
) -> Result<piston::VersionJson> {
    let cached = paths.version_json(mc_version);
    if let Ok(bytes) = tokio::fs::read(&cached).await {
        if let Ok(v) = serde_json::from_slice::<piston::VersionJson>(&bytes) {
            return Ok(v);
        }
    }

    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == mc_version)
        .ok_or_else(|| Error::msg(format!("unknown Minecraft version '{mc_version}'")))?;

    let json: piston::VersionJson = get_json(client, &entry.url).await?;
    tokio::fs::create_dir_all(paths.version_dir(mc_version)).await?;
    tokio::fs::write(&cached, serde_json::to_vec_pretty(&json)?).await?;
    Ok(json)
}

/// Loader builds available for a Minecraft version, newest first.
pub async fn loader_versions(
    client: &reqwest::Client,
    loader: Loader,
    mc_version: &str,
) -> Result<Vec<String>> {
    Ok(match loader {
        Loader::Vanilla => vec![],
        Loader::Fabric => fabric::loader_versions(client, fabric::Flavor::Fabric, mc_version)
            .await?
            .into_iter()
            .map(|v| v.version)
            .collect(),
        Loader::Quilt => fabric::loader_versions(client, fabric::Flavor::Quilt, mc_version)
            .await?
            .into_iter()
            .map(|v| v.version)
            .collect(),
        Loader::NeoForge => forge::versions(client, forge::Flavor::NeoForge, mc_version).await?,
        Loader::Forge => forge::versions(client, forge::Flavor::Forge, mc_version).await?,
    })
}

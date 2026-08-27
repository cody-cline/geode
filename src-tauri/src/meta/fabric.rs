//! Fabric and Quilt. Both expose the same meta API shape, so one module covers
//! them: a list of loader builds per game version, and a ready-made version
//! JSON with `inheritsFrom` pointing at the vanilla manifest.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::meta::piston::VersionJson;
use crate::net::get_json;

pub const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
pub const QUILT_META: &str = "https://meta.quiltmc.org/v3";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Fabric,
    Quilt,
}

impl Flavor {
    fn base(self) -> &'static str {
        match self {
            Flavor::Fabric => FABRIC_META,
            Flavor::Quilt => QUILT_META,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoaderEntry {
    pub loader: LoaderInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoaderInfo {
    pub version: String,
    #[serde(default)]
    pub stable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderVersion {
    pub version: String,
    pub stable: bool,
}

/// Loader builds compatible with a given Minecraft version, newest first.
pub async fn loader_versions(
    client: &reqwest::Client,
    flavor: Flavor,
    game_version: &str,
) -> Result<Vec<LoaderVersion>> {
    let url = format!(
        "{}/versions/loader/{}",
        flavor.base(),
        urlencoding::encode(game_version)
    );
    let entries: Vec<LoaderEntry> = get_json(client, &url).await?;
    Ok(entries
        .into_iter()
        .map(|e| LoaderVersion {
            version: e.loader.version,
            stable: e.loader.stable,
        })
        .collect())
}

/// The launch profile for a (game, loader) pair. Returned with
/// `inheritsFrom` still set — the caller merges it onto vanilla.
pub async fn profile(
    client: &reqwest::Client,
    flavor: Flavor,
    game_version: &str,
    loader_version: &str,
) -> Result<VersionJson> {
    let url = format!(
        "{}/versions/loader/{}/{}/profile/json",
        flavor.base(),
        urlencoding::encode(game_version),
        urlencoding::encode(loader_version)
    );
    get_json(client, &url).await
}

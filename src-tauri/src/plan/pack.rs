//! Reading a published modpack's mod list, so it can be copied into a plan.
//!
//! A pack is an archive of *files* pinned to one Minecraft version. A plan
//! wants the *projects* — which mods, stripped of the version the pack happened
//! to be built for — so that the planner is free to re-target the whole set.
//!
//! Both formats can be read without installing anything: a `.mrpack` index
//! encodes Modrinth project ids in its download URLs, and a CurseForge manifest
//! lists project ids outright. The CurseForge side needs the user's API key to
//! turn those ids into names, which is the one thing that can make this fail
//! where the Modrinth side succeeds.

use crate::error::{Error, Result};
use crate::instance::modpack::{self, PackMods};
use crate::net::{Download, Progress};
use crate::paths::Paths;
use crate::providers::{self, Provider};

use super::PlanMod;

/// A pack's mod list, together with what the pack says about itself.
pub struct PackContents {
    /// The pack project, so a build made from it can record where it came from.
    pub project: providers::Project,
    pub version_id: String,
    pub version_number: String,
    /// What the pack targets. Anything built from it has to match.
    pub mc_version: String,
    pub loader: crate::meta::Loader,
    pub mods: Vec<PlanMod>,
}

/// Download a pack and list the mods in it, versionless.
pub async fn contents(
    client: &reqwest::Client,
    paths: &Paths,
    provider: Provider,
    project_id: &str,
    version_id: Option<&str>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<PlanMod>> {
    Ok(
        detailed(client, paths, provider, project_id, version_id, curseforge_key, progress)
            .await?
            .mods,
    )
}

/// The same read, keeping the target and provenance a plan throws away.
#[allow(clippy::too_many_arguments)] // one pack's coordinates, travelling together
pub async fn detailed(
    client: &reqwest::Client,
    paths: &Paths,
    provider: Provider,
    project_id: &str,
    version_id: Option<&str>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<PackContents> {
    let project = providers::project(client, provider, project_id, curseforge_key).await?;
    if project.kind != providers::ProjectKind::Modpack {
        return Err(Error::msg(format!("{} is not a modpack", project.name)));
    }

    // Packs are never filtered by loader or version — the pack chose those, and
    // here we are about to throw them away anyway.
    let versions = providers::versions(client, provider, project_id, None, None, curseforge_key)
        .await?;
    let version = match version_id {
        Some(want) => versions
            .iter()
            .find(|v| v.id == want)
            .ok_or_else(|| Error::msg("that pack version is no longer available"))?,
        None => crate::install::content::pick_best(&versions)
            .ok_or_else(|| Error::msg(format!("{} has no downloadable release", project.name)))?,
    };

    if let Some(reason) = &version.blocked_reason {
        return Err(Error::msg(reason.clone()));
    }
    // Taken now, while `version` is still borrowed from the list.
    let chosen_id = version.id.clone();
    let chosen_number = version.version_number.clone();
    let file = version
        .primary_file()
        .ok_or_else(|| Error::msg(format!("{} has no downloadable file", project.name)))?;

    progress.stage("Downloading pack", 1).await;
    let archive = paths.cache().join("packs").join(&file.filename);
    crate::net::fetch_one(
        client,
        &Download::new(file.url.clone(), archive.clone())
            .sha1(file.sha1.clone())
            .size(file.size)
            .label(project.name.clone()),
    )
    .await?;
    progress.advance(1, &file.filename).await;

    progress.stage("Reading pack contents", 1).await;
    // What the pack was built for. A server made from it has to be that.
    let target = modpack::preview(archive.clone()).await?;
    let mods = match modpack::index_mods(archive).await? {
        PackMods::Modrinth(ids) => {
            let projects = providers::modrinth::projects_by_ids(client, &ids).await?;
            // Keep the pack's own order; a resolved project that is not a mod
            // (a shader shipped in mods/, say) is still a mod file, so the only
            // thing filtered here is an id that resolved to nothing.
            ids.iter()
                .filter_map(|id| projects.iter().find(|p| &p.id == id))
                .map(PlanMod::from_project)
                .collect()
        }
        PackMods::CurseForge(ids) => {
            let key = curseforge_key
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| {
                    Error::msg(
                        "reading a CurseForge pack needs your CurseForge API key — \
                         add one in Settings",
                    )
                })?;
            let projects = providers::curseforge::projects_by_ids(client, &ids, key).await?;
            ids.iter()
                .filter_map(|id| {
                    let id = id.to_string();
                    projects.iter().find(|p| p.id == id)
                })
                // The manifest does not distinguish mods from resource packs,
                // so the project's own type is what settles it.
                .filter(|p| p.kind == providers::ProjectKind::Mod)
                .map(PlanMod::from_project)
                .collect()
        }
    };
    progress.finish().await;

    Ok(PackContents {
        project,
        version_id: chosen_id,
        version_number: chosen_number,
        mc_version: target.mc_version,
        loader: target.loader,
        mods,
    })
}

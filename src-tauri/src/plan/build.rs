//! Turning a resolved plan into a real instance.
//!
//! The plan says *which* mods; the chosen target says which loader and game
//! version. Together those pin down exactly one file per mod — including the
//! awkward ones, where the answer is a Fabric build run through Connector, or a
//! build from a neighbouring patch version. Each mod is installed against the
//! pair its own verdict named, not the instance's, or those two cases would
//! quietly fetch the wrong file or nothing at all.

use serde::Serialize;

use crate::error::{Error, Result};
use crate::instance::{self, Instance};
use crate::meta::Loader;
use crate::net::Progress;
use crate::paths::Paths;
use crate::providers::{self, Provider};

use super::resolve::{Fit, Target};
use super::Plan;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    pub instance_id: String,
    pub instance_name: String,
    /// Display names of everything that landed, mods and dependencies alike.
    pub installed: Vec<String>,
    /// What could not be installed, with the reason.
    pub skipped: Vec<String>,
}

/// Which loaders to ask for this mod's files, in order of preference.
///
/// Normally just the target's own. Two exceptions: a Connector fit wants the
/// Fabric build by definition, and a Quilt target should take a Fabric build
/// when the mod ships no Quilt one — Quilt loads those directly, which is why
/// the resolver counted it as a native fit in the first place.
fn loaders_to_try(fit: Fit, target_loader: &str) -> Vec<&'static str> {
    match fit {
        Fit::Connector | Fit::ConnectorNear => vec!["fabric"],
        _ if target_loader == "quilt" => vec!["quilt", "fabric"],
        _ => match target_loader {
            "fabric" => vec!["fabric"],
            "neoforge" => vec!["neoforge"],
            "forge" => vec!["forge"],
            _ => vec![],
        },
    }
}

/// Create the instance the target describes and install the plan into it.
pub async fn build(
    client: &reqwest::Client,
    paths: &Paths,
    plan: &Plan,
    target: &Target,
    name: Option<String>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<BuildReport> {
    let loader = Loader::parse(&target.loader)
        .filter(|l| l.supports_mods())
        .ok_or_else(|| Error::msg(format!("'{}' is not a mod loader", target.loader)))?;

    progress.stage("Finding a loader build", 1).await;
    let builds = crate::meta::loader_versions(client, loader, &target.game_version).await?;
    let loader_version = builds.first().cloned().ok_or_else(|| {
        Error::msg(format!(
            "no {} build exists for Minecraft {}",
            loader.display_name(),
            target.game_version
        ))
    })?;
    progress.advance(1, &loader_version).await;

    let display_name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| plan.name.clone());

    let mut inst = instance::create(
        paths,
        display_name,
        target.game_version.clone(),
        loader,
        loader_version,
    )
    .await?;

    let mut report = BuildReport {
        instance_id: inst.id.clone(),
        instance_name: inst.name.clone(),
        installed: Vec::new(),
        skipped: Vec::new(),
    };

    // The Connector runtime goes in before any Fabric jar does: a Fabric mod in
    // a NeoForge mods folder does nothing on its own, and a failure here should
    // stop before the folder fills up with jars that cannot load.
    let needs_connector = target
        .fits
        .iter()
        .any(|f| matches!(f.fit, Fit::Connector | Fit::ConnectorNear));
    if needs_connector {
        match crate::connector::ensure_stack(client, paths, &mut inst, progress).await {
            Ok(added) => report.installed.extend(added),
            Err(e) => {
                let _ = inst.save(paths).await;
                return Err(e);
            }
        }
    }

    let wanted: Vec<_> = target
        .fits
        .iter()
        .filter(|f| f.fit != Fit::Missing)
        .collect();
    progress.stage("Installing mods", wanted.len() as u64).await;

    for fit in wanted {
        progress.check_cancelled()?;
        progress.advance(1, &fit.name).await;

        // The version this mod's verdict named, which is the target's own
        // except where the resolver reached into a neighbouring patch.
        let game_version = fit
            .via_version
            .clone()
            .unwrap_or_else(|| target.game_version.clone());

        match install_one(
            client,
            paths,
            &mut inst,
            fit.fit,
            &target.loader,
            &fit.provider,
            &fit.project_id,
            &game_version,
            curseforge_key,
            progress,
        )
        .await
        {
            Ok(installed) => report.installed.extend(installed),
            Err(e) => report.skipped.push(format!("{}: {e}", fit.name)),
        }
    }

    for fit in target.fits.iter().filter(|f| f.fit == Fit::Missing) {
        report.skipped.push(format!(
            "{}: nothing runs on {} {}",
            fit.name,
            loader.display_name(),
            target.game_version
        ));
    }

    // Save regardless of what failed: a part-built instance is more useful than
    // a directory the launcher refuses to acknowledge.
    inst.save(paths).await?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)] // one mod's coordinates, travelling together
async fn install_one(
    client: &reqwest::Client,
    paths: &Paths,
    inst: &mut Instance,
    fit: Fit,
    target_loader: &str,
    provider: &str,
    project_id: &str,
    game_version: &str,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    let provider = Provider::parse(provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let project = providers::project(client, provider, project_id, curseforge_key).await?;

    for loader in loaders_to_try(fit, target_loader) {
        let versions = providers::versions(
            client,
            provider,
            project_id,
            Some(game_version),
            Some(loader),
            curseforge_key,
        )
        .await?;

        let Some(best) = crate::install::content::pick_best(&versions) else {
            continue;
        };

        let report = crate::install::content::install_with_dependencies(
            client,
            paths,
            inst,
            best,
            &project,
            // Dependencies follow the mod, not the instance: a Fabric mod
            // running through Connector needs its Fabric dependencies.
            game_version,
            loader,
            curseforge_key,
            progress,
        )
        .await?;

        let mut names = report.installed;
        names.extend(report.skipped.into_iter().map(|s| format!("(skipped) {s}")));
        return Ok(names);
    }

    Err(Error::msg(format!(
        "no installable build for Minecraft {game_version}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connector_fit_always_installs_the_fabric_build() {
        assert_eq!(loaders_to_try(Fit::Connector, "neoforge"), vec!["fabric"]);
        assert_eq!(
            loaders_to_try(Fit::ConnectorNear, "neoforge"),
            vec!["fabric"]
        );
    }

    #[test]
    fn a_native_fit_installs_the_targets_own_loader() {
        assert_eq!(loaders_to_try(Fit::Native, "neoforge"), vec!["neoforge"]);
        assert_eq!(loaders_to_try(Fit::NearVersion, "fabric"), vec!["fabric"]);
    }

    #[test]
    fn quilt_falls_back_to_a_fabric_build() {
        // The resolver counts a Fabric-only mod as native on Quilt, so the
        // install has to be able to actually find that file.
        assert_eq!(loaders_to_try(Fit::Native, "quilt"), vec!["quilt", "fabric"]);
    }
}

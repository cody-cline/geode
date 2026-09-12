//! Turning a modpack plan into a server.
//!
//! A plan is a list of mods and a target the *client* can run. A server is not
//! a client: it renders nothing, so the half of any modpack that exists to make
//! the game look better has no business in its mods folder. Sodium and Iris say
//! so themselves, and this drops them and reports that it did.
//!
//! Only some of a plan's possible targets can become a server at all. Paper and
//! Purpur are not mod loaders, and Forge and NeoForge servers need an installer
//! run that is not built yet — so a plan that resolved to those is refused with
//! the reason rather than half-built.

use serde::Serialize;

use crate::error::{Error, Result};
use crate::instance::content::ContentKind;
use crate::net::Progress;
use crate::paths::Paths;
use crate::plan::resolve::{Fit, Target};
use crate::plan::Plan;
use crate::providers::{self, Provider};

use super::{meta, ServerKind};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerBuildReport {
    pub server_id: String,
    pub server_name: String,
    pub installed: Vec<String>,
    /// What did not make it, and why — client-only mods included, since their
    /// absence is a decision rather than a failure.
    pub skipped: Vec<String>,
}

/// The server software that runs a given client loader's mods.
fn server_kind_for(loader: &str) -> Option<ServerKind> {
    Some(match loader {
        "fabric" => ServerKind::Fabric,
        "quilt" => ServerKind::Quilt,
        "neoforge" => ServerKind::NeoForge,
        "forge" => ServerKind::Forge,
        _ => return None,
    })
}

/// Why this target cannot become a server, in terms of what to do instead.
fn refuse(loader: &str) -> Error {
    match server_kind_for(loader) {
        None => Error::msg(format!(
            "{loader} is not a mod loader a server can run. Resolve the plan to \
             Fabric and build that."
        )),
        Some(kind) => Error::msg(format!(
            "{} servers cannot be installed automatically.",
            kind.display_name()
        )),
    }
}

/// Create a server at this target and install the plan's server-capable mods.
pub async fn from_plan(
    client: &reqwest::Client,
    paths: &Paths,
    plan: &Plan,
    target: &Target,
    name: Option<String>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<ServerBuildReport> {
    let kind = server_kind_for(&target.loader).filter(|k| meta::is_installable(*k));
    let Some(kind) = kind else {
        return Err(refuse(&target.loader));
    };

    let display_name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{} server", plan.name));

    let mut server =
        super::create(paths, display_name, kind, target.game_version.clone()).await?;
    let mut report = ServerBuildReport {
        server_id: server.id.clone(),
        server_name: server.name.clone(),
        installed: Vec::new(),
        skipped: Vec::new(),
    };

    super::install::install_jar(client, paths, &mut server, progress).await?;

    let wanted: Vec<_> = target.fits.iter().filter(|f| f.fit != Fit::Missing).collect();
    progress.stage("Installing mods", wanted.len() as u64).await;
    let dir = paths.server_run(&server.id).join(ContentKind::Mod.folder());

    for fit in wanted {
        progress.check_cancelled()?;
        progress.advance(1, &fit.name).await;

        if matches!(fit.fit, Fit::Connector | Fit::ConnectorNear) {
            // A Fabric mod reaching a Forge server through Sinytra Connector
            // needs Connector itself installed and configured, which is not
            // handled here. Reachable now that Forge servers can be built, so
            // it is reported as a skip rather than silently dropped.
            report
                .skipped
                .push(format!("{}: needs Sinytra Connector", fit.name));
            continue;
        }

        match install_one(
            client,
            &dir,
            &mut server,
            fit,
            &target.loader,
            &target.game_version,
            curseforge_key,
        )
        .await
        {
            Ok(Installed::File(file)) => report.installed.push(file),
            Ok(Installed::NotForServers(name)) => report
                .skipped
                .push(format!("{name}: does not run on a server")),
            Err(e) => report.skipped.push(format!("{}: {e}", fit.name)),
        }
    }

    server.save(paths).await?;
    Ok(report)
}

enum Installed {
    File(String),
    /// Left out on purpose, which is not the same as having failed.
    NotForServers(String),
}

#[allow(clippy::too_many_arguments)] // one mod's coordinates, travelling together
async fn install_one(
    client: &reqwest::Client,
    dir: &std::path::Path,
    server: &mut super::Server,
    fit: &crate::plan::resolve::ModFit,
    loader: &str,
    game_version: &str,
    curseforge_key: Option<&str>,
) -> Result<Installed> {
    let provider = Provider::parse(&fit.provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let project = providers::project(client, provider, &fit.project_id, curseforge_key).await?;

    if !project.runs_on_a_server() {
        return Ok(Installed::NotForServers(project.name));
    }

    // The version this mod's verdict named, which is the target's own except
    // where the resolver reached into a neighbouring patch.
    let game = fit.via_version.as_deref().unwrap_or(game_version);
    let versions = providers::versions(
        client,
        provider,
        &fit.project_id,
        Some(game),
        Some(loader),
        curseforge_key,
    )
    .await?;

    let best = crate::install::content::pick_best(&versions)
        .ok_or_else(|| Error::msg(format!("no build for {loader} {game}")))?;

    let file = crate::install::content::install_into(
        client,
        dir,
        &mut server.content,
        best,
        ContentKind::Mod,
        &project.name,
        project.icon_url.clone(),
        Some(project.slug.clone()),
        false,
    )
    .await?;
    Ok(Installed::File(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_loaders_map_to_the_server_that_runs_their_mods() {
        assert_eq!(server_kind_for("fabric"), Some(ServerKind::Fabric));
        assert_eq!(server_kind_for("neoforge"), Some(ServerKind::NeoForge));
        assert_eq!(server_kind_for("forge"), Some(ServerKind::Forge));
        assert_eq!(server_kind_for("quilt"), Some(ServerKind::Quilt));
    }

    #[test]
    fn paper_is_not_something_a_plan_resolves_to() {
        // A plan targets client loaders. Paper is a server, not a loader, and
        // no plan will ever name it.
        assert_eq!(server_kind_for("paper"), None);
        assert_eq!(server_kind_for("vanilla"), None);
    }

    #[test]
    fn a_target_we_cannot_build_explains_itself() {
        // Two different refusals, and the difference matters: one is "never",
        // the other is "not yet".
        let never = refuse("paper").to_string();
        assert!(never.contains("not a mod loader"), "{never}");
        assert!(never.contains("Fabric"), "{never}");

        // NeoForge is a loader a server can run, so this arm is only reached if
        // its software could not be installed — a different failure entirely.
        let other = refuse("neoforge").to_string();
        assert!(other.contains("NeoForge"), "{other}");
        assert!(!other.contains("not a mod loader"), "{other}");
    }

    #[test]
    fn every_mod_loader_a_plan_targets_can_be_built() {
        // This test previously asserted the opposite, to fail loudly the day a
        // Forge server installer landed. It has now landed: Fabric downloads an
        // assembled server, and Quilt, Forge and NeoForge run their installers.
        let buildable = |loader: &str| {
            server_kind_for(loader)
                .filter(|k| meta::is_installable(*k))
                .is_some()
        };
        assert!(buildable("fabric"));
        assert!(buildable("neoforge"));
        assert!(buildable("forge"));
        assert!(buildable("quilt"));
        // Still not something a plan can resolve to: it is a server, not a
        // loader, so there are no mods built for it.
        assert!(!buildable("paper"));
    }
}

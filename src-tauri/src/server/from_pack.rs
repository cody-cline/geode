//! Turning a published modpack into a server.
//!
//! A modpack is a client thing by default — half of a big pack exists to make
//! the game look better, and none of that belongs in a server's mods folder.
//! That half is kept in its Client mods, for players, and what is left is a
//! list of mods and the loader they were built for, which is exactly what a
//! server needs.
//!
//! The loader is the pack's to choose, but *which server software* runs it is
//! not: a Forge pack runs on Forge, and equally on Mohist or Arclight, which
//! run the same mods and take Bukkit plugins besides. That choice is offered
//! rather than made here.

use crate::error::{Error, Result};
use crate::net::Progress;
use crate::paths::Paths;
use crate::plan::pack::PackContents;

use super::from_plan::ServerBuildReport;
use super::ServerKind;

/// Server software able to run a pack built for this loader, best first.
///
/// The plain loader leads because it is what the pack was made for. The hybrids
/// follow: they run the same mods and add a plugins folder, which is the whole
/// reason this is a list and not a lookup.
pub fn kinds_for(loader: &str) -> Vec<ServerKind> {
    ServerKind::ALL
        .iter()
        .copied()
        .filter(|kind| kind.mod_loader() == Some(loader))
        .collect()
}

/// Create a server from a pack, installing the mods that run on one and keeping
/// the rest for its players.
pub async fn from_modpack(
    client: &reqwest::Client,
    paths: &Paths,
    contents: &PackContents,
    kind: ServerKind,
    name: Option<String>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<ServerBuildReport> {
    let loader = contents.loader.as_str();
    if kind.mod_loader() != Some(loader) {
        return Err(Error::msg(format!(
            "{} was built for {loader}, which a {} server does not run.",
            contents.project.name,
            kind.display_name()
        )));
    }

    let display_name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{} server", contents.project.name));

    let mut server =
        super::create(paths, display_name, kind, contents.mc_version.clone()).await?;
    // Where it came from, recorded the same way an instance records it — so the
    // server can show the pack's artwork and link back to its page.
    server.pack = Some(crate::instance::PackSource::from_project(
        &contents.project,
        contents.version_number.clone(),
        contents.version_id.clone(),
    ));

    let mut report = ServerBuildReport {
        server_id: server.id.clone(),
        server_name: server.name.clone(),
        installed: Vec::new(),
        skipped: Vec::new(),
    };

    super::install::install_jar(client, paths, &mut server, progress).await?;

    progress
        .stage("Installing mods", contents.mods.len() as u64)
        .await;

    for entry in &contents.mods {
        progress.check_cancelled()?;
        progress.advance(1, &entry.name).await;

        match super::install_one(
            client,
            paths,
            &mut server,
            &entry.provider,
            &entry.project_id,
            loader,
            &contents.mc_version,
            curseforge_key,
            crate::install::blocked::Fallback::live(progress),
        )
        .await
        {
            Ok(file) => report.installed.push(file),
            Err(e) => report.skipped.push(format!("{}: {e}", entry.name)),
        }
    }

    server.save(paths).await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forge_pack_can_be_a_hybrid_as_well_as_forge() {
        let kinds = kinds_for("forge");
        // Forge first: it is what the pack was actually built for.
        assert_eq!(kinds.first(), Some(&ServerKind::Forge));
        // And the hybrids, which run the same mods and take plugins too.
        assert!(kinds.contains(&ServerKind::Mohist));
        assert!(kinds.contains(&ServerKind::Arclight));
        // Not something that runs a different loader's mods.
        assert!(!kinds.contains(&ServerKind::Fabric));
    }

    #[test]
    fn a_fabric_pack_offers_banner_alongside_fabric() {
        let kinds = kinds_for("fabric");
        assert_eq!(kinds.first(), Some(&ServerKind::Fabric));
        // Banner is built on Fabric; MohistMC's NeoForge hybrid is Youer.
        assert!(kinds.contains(&ServerKind::Banner));
        assert!(!kinds_for("neoforge").contains(&ServerKind::Banner));
    }

    #[test]
    fn a_loader_no_server_runs_offers_nothing() {
        // Vanilla is not a mod loader, so no pack is built for it and nothing
        // claims to run one.
        assert!(kinds_for("vanilla").is_empty());
    }
}

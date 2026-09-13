//! Installing mods and other content into an instance, with dependency
//! resolution and update checking.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::instance::content::{ContentKind, ContentRecord};
use crate::instance::Instance;
use crate::net::{Download, Progress};
use crate::paths::Paths;
use crate::providers::{self, DependencyKind, Project, Provider, Version};

/// Install one version's primary file into the instance.
///
/// Replaces any earlier file from the same project so updating never leaves two
/// copies of a mod in the folder — the single most common way a modded instance
/// breaks.
#[allow(clippy::too_many_arguments)] // provenance fields travel together
pub async fn install(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    version: &Version,
    kind: ContentKind,
    display_name: &str,
    icon_url: Option<String>,
    slug: Option<String>,
    as_dependency: bool,
) -> Result<String> {
    let dir = crate::instance::content::content_dir(paths, &instance.id, kind);
    install_into(
        client,
        &dir,
        &mut instance.content,
        version,
        kind,
        display_name,
        icon_url,
        slug,
        as_dependency,
    )
    .await
}

/// The same install, into any content folder with any set of records.
///
/// A server's plugins folder is not an instance's mods folder, but nothing
/// about downloading a file and recording where it came from cares which.
#[allow(clippy::too_many_arguments)]
pub async fn install_into(
    client: &reqwest::Client,
    dir: &std::path::Path,
    records: &mut Vec<ContentRecord>,
    version: &Version,
    kind: ContentKind,
    display_name: &str,
    icon_url: Option<String>,
    slug: Option<String>,
    as_dependency: bool,
) -> Result<String> {
    if let Some(reason) = &version.blocked_reason {
        return Err(Error::msg(reason.clone()));
    }
    let file = version
        .primary_file()
        .ok_or_else(|| Error::msg(format!("{display_name} has no downloadable file")))?;

    tokio::fs::create_dir_all(dir).await?;

    // Remove the previous file for this project before writing the new one.
    let previous: Vec<String> = records
        .iter()
        .filter(|r| {
            r.provider == version.provider.as_str()
                && r.project_id == version.project_id
                && r.kind == kind
        })
        .map(|r| r.file_name.clone())
        .collect();

    crate::net::fetch_one(
        client,
        &Download::new(file.url.clone(), dir.join(&file.filename))
            .sha1(file.sha1.clone())
            .size(file.size)
            .label(display_name.to_string()),
    )
    .await?;

    for old in previous {
        if old == file.filename {
            continue;
        }
        let _ = tokio::fs::remove_file(dir.join(&old)).await;
        // Disabled copies carry the `.disabled` suffix.
        let _ = tokio::fs::remove_file(dir.join(format!("{old}.disabled"))).await;
        records.retain(|r| !(r.file_name == old && r.kind == kind));
    }

    records.retain(|r| !(r.file_name == file.filename && r.kind == kind));
    records.push(ContentRecord {
        file_name: file.filename.clone(),
        kind,
        provider: version.provider.as_str().to_string(),
        project_id: version.project_id.clone(),
        version_id: version.id.clone(),
        name: display_name.to_string(),
        version_number: version.version_number.clone(),
        slug,
        icon_url,
        dependency: as_dependency,
        sharing: Default::default(),
        requires: Some(required_ids(version)),
    });

    Ok(file.filename.clone())
}

/// The provider's view of what a version requires, kept as a fallback for jars
/// whose own metadata we cannot read.
fn required_ids(version: &Version) -> Vec<String> {
    version
        .dependencies
        .iter()
        .filter(|d| d.kind == DependencyKind::Required)
        .filter_map(|d| d.project_id.clone())
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub installed: Vec<String>,
    /// Dependencies we could not resolve or download, with the reason.
    pub skipped: Vec<String>,
}

/// Install a version plus everything it requires.
///
/// Walks required dependencies breadth-first, picking the newest compatible
/// version of each. Optional dependencies are deliberately left alone: they are
/// the author saying "this works better with X", not "this needs X", and
/// pulling them in silently is how instances bloat.
///
/// `dep_game_version` and `dep_loader` say what the dependencies should be
/// resolved against, which is not always the instance's own pair: a Fabric mod
/// installed through Connector needs Fabric dependencies, and a mod taken from
/// a neighbouring patch version wants that version's.
#[allow(clippy::too_many_arguments)]
pub async fn install_with_dependencies(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    version: &Version,
    project: &Project,
    dep_game_version: &str,
    dep_loader: &str,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<InstallReport> {
    let kind = project.kind.install_kind().ok_or_else(|| {
        Error::msg(format!(
            "{} is a modpack — it creates its own instance rather than \
             installing into one",
            project.name
        ))
    })?;
    let dir = crate::instance::content::content_dir(paths, &instance.id, kind);
    install_with_dependencies_into(
        client,
        &dir,
        &mut instance.content,
        version,
        project,
        kind,
        dep_game_version,
        dep_loader,
        curseforge_key,
        progress,
    )
    .await
}

/// The same, into a folder and a record list rather than an instance.
///
/// A server keeps identical provenance records and wants identical behaviour —
/// installing a mod that needs three others should bring the three — so it runs
/// the same walk rather than a second copy of it. The caller says which folder
/// and which kind, because a server decides that differently: the same project
/// is a mod in one folder and a plugin in another.
#[allow(clippy::too_many_arguments)] // one install's coordinates, travelling together
pub async fn install_with_dependencies_into(
    client: &reqwest::Client,
    dir: &Path,
    records: &mut Vec<ContentRecord>,
    version: &Version,
    project: &Project,
    kind: ContentKind,
    dep_game_version: &str,
    dep_loader: &str,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<InstallReport> {
    let mut report = InstallReport {
        installed: Vec::new(),
        skipped: Vec::new(),
    };

    progress.stage("Installing", 1).await;
    let file = install_into(
        client,
        dir,
        records,
        version,
        kind,
        &project.name,
        project.icon_url.clone(),
        Some(project.slug.clone()),
        false,
    )
    .await?;
    report.installed.push(file);
    progress.advance(1, &project.name).await;

    // Only things that load other things have dependencies worth chasing. A
    // plugin's are plugins, and they belong in the same folder as their parent,
    // which is why the kind travels down rather than being assumed to be a mod.
    if !matches!(kind, ContentKind::Mod | ContentKind::Plugin) {
        return Ok(report);
    }

    let mut seen: HashSet<String> = records
        .iter()
        .filter(|r| r.provider == version.provider.as_str())
        .map(|r| r.project_id.clone())
        .collect();

    let mut queue: Vec<String> = version
        .dependencies
        .iter()
        .filter(|d| d.kind == DependencyKind::Required)
        .filter_map(|d| d.project_id.clone())
        .collect();

    // Bounded so a cyclic or pathological dependency graph cannot spin forever.
    let mut budget = 32;
    while let Some(project_id) = queue.pop() {
        if budget == 0 {
            report
                .skipped
                .push("dependency chain too deep — stopped resolving".into());
            break;
        }
        budget -= 1;

        if !seen.insert(project_id.clone()) {
            continue;
        }
        progress.check_cancelled()?;

        match resolve_dependency(
            client,
            dir,
            records,
            version.provider,
            &project_id,
            kind,
            dep_game_version,
            dep_loader,
            curseforge_key,
        )
        .await
        {
            Ok(Some((filename, dep_version))) => {
                progress.advance(1, &filename).await;
                report.installed.push(filename);
                queue.extend(
                    dep_version
                        .dependencies
                        .iter()
                        .filter(|d| d.kind == DependencyKind::Required)
                        .filter_map(|d| d.project_id.clone()),
                );
            }
            Ok(None) => report
                .skipped
                .push(format!("no compatible version of dependency {project_id}")),
            Err(e) => report.skipped.push(format!("dependency {project_id}: {e}")),
        }
    }

    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn resolve_dependency(
    client: &reqwest::Client,
    dir: &Path,
    records: &mut Vec<ContentRecord>,
    provider: Provider,
    project_id: &str,
    kind: ContentKind,
    game_version: &str,
    loader: &str,
    curseforge_key: Option<&str>,
) -> Result<Option<(String, Version)>> {
    let project = providers::project(client, provider, project_id, curseforge_key).await?;
    let versions = providers::versions(
        client,
        provider,
        project_id,
        Some(game_version),
        Some(loader),
        curseforge_key,
    )
    .await?;

    let Some(best) = pick_best(&versions) else {
        return Ok(None);
    };

    let filename = install_into(
        client,
        dir,
        records,
        best,
        kind,
        &project.name,
        project.icon_url.clone(),
        Some(project.slug.clone()),
        true,
    )
    .await?;
    Ok(Some((filename, best.clone())))
}

/// Prefer a stable release; fall back to the newest prerelease when that is all
/// the author has published for this game version.
pub fn pick_best(versions: &[Version]) -> Option<&Version> {
    versions
        .iter()
        .find(|v| v.release_type == "release" && v.primary_file().is_some())
        .or_else(|| versions.iter().find(|v| v.primary_file().is_some()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCandidate {
    pub file_name: String,
    pub name: String,
    pub provider: String,
    pub project_id: String,
    pub current_version: String,
    pub new_version_id: String,
    pub new_version_number: String,
    pub kind: ContentKind,
}

/// Check every tracked item for a newer version compatible with this instance.
/// Newer versions of whatever is installed.
///
/// Takes the records and the target rather than an instance: a server keeps
/// exactly the same provenance and asks exactly the same question of it, and
/// there is no sense in two copies of this.
pub async fn check_updates(
    client: &reqwest::Client,
    records: &[ContentRecord],
    game_version: &str,
    mod_loader: Option<&str>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<UpdateCandidate>> {
    let tracked: Vec<ContentRecord> = records.to_vec();
    progress
        .stage("Checking for updates", tracked.len() as u64)
        .await;

    let mut out = Vec::new();
    for record in tracked {
        progress.check_cancelled()?;
        progress.advance(1, &record.name).await;

        let Some(provider) = Provider::parse(&record.provider) else {
            continue;
        };
        if provider == Provider::CurseForge && curseforge_key.is_none() {
            continue;
        }

        // Resource packs, shaders and datapacks are not loader-specific, and
        // neither is a plugin: its platform is not a mod loader, so asking for
        // one would match nothing.
        let loader = (record.kind == ContentKind::Mod)
            .then_some(mod_loader)
            .flatten();
        let versions = match providers::versions(
            client,
            provider,
            &record.project_id,
            Some(game_version),
            loader,
            curseforge_key,
        )
        .await
        {
            Ok(v) => v,
            // One dead project should not abort the whole check.
            Err(_) => continue,
        };

        let Some(best) = pick_best(&versions) else {
            continue;
        };
        if best.id == record.version_id {
            continue;
        }

        out.push(UpdateCandidate {
            file_name: record.file_name.clone(),
            name: record.name.clone(),
            provider: record.provider.clone(),
            project_id: record.project_id.clone(),
            current_version: record.version_number.clone(),
            new_version_id: best.id.clone(),
            new_version_number: best.version_number.clone(),
            kind: record.kind,
        });
    }

    progress.finish().await;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::VersionFile;

    fn version(id: &str, release_type: &str, has_file: bool) -> Version {
        Version {
            provider: Provider::Modrinth,
            id: id.into(),
            project_id: "p".into(),
            name: id.into(),
            version_number: id.into(),
            game_versions: vec![],
            loaders: vec![],
            release_type: release_type.into(),
            published: None,
            downloads: 0,
            files: if has_file {
                vec![VersionFile {
                    url: "https://example.invalid/a.jar".into(),
                    filename: "a.jar".into(),
                    sha1: None,
                    size: None,
                    primary: true,
                }]
            } else {
                vec![]
            },
            dependencies: vec![],
            blocked_reason: None,
        }
    }

    #[test]
    fn parses_ids_from_a_modrinth_cdn_url() {
        let url = "https://cdn.modrinth.com/data/AANobbMI/versions/S3DUMfBo/sodium.jar";
        assert_eq!(
            ids_from_modrinth_url(url),
            Some(("AANobbMI".into(), "S3DUMfBo".into()))
        );
    }

    #[test]
    fn ignores_urls_from_elsewhere() {
        assert!(ids_from_modrinth_url("https://edge.forgecdn.net/files/1/2/x.jar").is_none());
        assert!(ids_from_modrinth_url("https://cdn.modrinth.com/data/OnlyProject").is_none());
        assert!(ids_from_modrinth_url("not a url").is_none());
    }

    #[test]
    fn prefers_stable_release() {
        let versions = vec![
            version("beta-2", "beta", true),
            version("release-1", "release", true),
        ];
        assert_eq!(pick_best(&versions).unwrap().id, "release-1");
    }

    #[test]
    fn falls_back_to_prerelease() {
        let versions = vec![version("beta-2", "beta", true)];
        assert_eq!(pick_best(&versions).unwrap().id, "beta-2");
    }

    #[test]
    fn skips_versions_without_files() {
        let versions = vec![
            version("blocked", "release", false),
            version("ok", "beta", true),
        ];
        assert_eq!(pick_best(&versions).unwrap().id, "ok");
    }

    #[test]
    fn none_when_nothing_installable() {
        let versions = vec![version("blocked", "release", false)];
        assert!(pick_best(&versions).is_none());
    }
}

// ---------------------------------------------------------------------------
// Linking files back to their source
// ---------------------------------------------------------------------------

/// A Modrinth CDN download URL encodes the ids we want:
/// `https://cdn.modrinth.com/data/<PROJECT>/versions/<VERSION>/<file>`
///
/// A `.mrpack` index lists only URLs and hashes, so without this every mod a
/// pack installs would arrive with no provenance — unnamed, untracked and
/// invisible to update checks.
pub fn ids_from_modrinth_url(url: &str) -> Option<(String, String)> {
    let rest = url.split_once("cdn.modrinth.com/data/").map(|(_, r)| r)?;
    let mut parts = rest.split('/');
    let project = parts.next()?;
    if parts.next()? != "versions" {
        return None;
    }
    let version = parts.next()?;
    if project.is_empty() || version.is_empty() {
        return None;
    }
    Some((project.to_string(), version.to_string()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkReport {
    pub identified: usize,
    pub unknown: usize,
}

/// Give provenance to files that have none.
///
/// Works from the file bytes: SHA-1 each untracked file and ask Modrinth which
/// version it is. That covers hand-dropped jars and anything a pack installed
/// by URL alone, and it is exact — a hash match is the same file, not a guess
/// from the filename.
///
/// Modrinth only. CurseForge's equivalent takes a bespoke murmur2 fingerprint
/// rather than a standard hash, so files that exist only there stay untracked.
pub async fn link_untracked(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    jars: &crate::instance::jar::MetaCache,
    progress: &Progress,
) -> Result<LinkReport> {
    use crate::instance::content::{self as content, ContentKind};

    let folders: Vec<(ContentKind, std::path::PathBuf)> = [
        ContentKind::Mod,
        ContentKind::ResourcePack,
        ContentKind::ShaderPack,
        ContentKind::DataPack,
    ]
    .into_iter()
    .map(|kind| (kind, content::content_dir(paths, &instance.id, kind)))
    .collect();

    link_untracked_in(client, &folders, &mut instance.content, jars, progress).await
}

/// The same, over whatever folders the caller names.
///
/// A server's are not an instance's: it holds plugins, it has no shaders at
/// all, and its datapacks live inside its world rather than beside it. So the
/// folders are given rather than assumed from an id.
pub async fn link_untracked_in(
    client: &reqwest::Client,
    folders: &[(crate::instance::content::ContentKind, std::path::PathBuf)],
    records: &mut Vec<ContentRecord>,
    jars: &crate::instance::jar::MetaCache,
    progress: &Progress,
) -> Result<LinkReport> {
    use crate::instance::content::{self as content, ContentKind};

    // Collect the untracked files first so the hash lookup is one request.
    let mut pending: Vec<(ContentKind, String, String)> = Vec::new();
    for (kind, dir) in folders {
        let items = content::list_in(dir, records, *kind, jars).await?;
        for item in items.into_iter().filter(|i| i.from_disk) {
            let path = dir.join(&item.file_name);
            if let Ok(sha1) = crate::net::sha1_file(&path).await {
                pending.push((*kind, item.file_name, sha1.to_lowercase()));
            }
        }
    }

    if pending.is_empty() {
        progress.finish().await;
        return Ok(LinkReport {
            identified: 0,
            unknown: 0,
        });
    }

    progress
        .stage("Identifying files", pending.len() as u64)
        .await;

    let hashes: Vec<String> = pending.iter().map(|(_, _, h)| h.clone()).collect();
    let found = crate::providers::modrinth::versions_by_hashes(client, &hashes).await?;

    // One more call turns ids into names and icons.
    let project_ids: Vec<String> = {
        let mut ids: Vec<String> = found.values().map(|v| v.project_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    };
    let projects = crate::providers::modrinth::projects_by_ids(client, &project_ids)
        .await
        .unwrap_or_default();

    let mut identified = 0;
    for (kind, file_name, sha1) in pending {
        progress.advance(1, &file_name).await;
        let Some(version) = found.get(&sha1) else {
            continue;
        };
        let project = projects.iter().find(|p| p.id == version.project_id);

        // Disabled files carry a `.disabled` suffix; records key off the plain
        // name so toggling does not orphan them.
        let base = file_name
            .strip_suffix(".disabled")
            .unwrap_or(&file_name)
            .to_string();

        records.retain(|r| !(r.file_name == base && r.kind == kind));
        records.push(ContentRecord {
            file_name: base,
            kind,
            provider: Provider::Modrinth.as_str().to_string(),
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            name: project
                .map(|p| p.name.clone())
                .unwrap_or_else(|| version.name.clone()),
            version_number: version.version_number.clone(),
            slug: project.map(|p| p.slug.clone()),
            icon_url: project.and_then(|p| p.icon_url.clone()),
            dependency: false,
            sharing: Default::default(),
            // A hash lookup gives provenance, not a dependency list.
            requires: None,
        });
        identified += 1;
    }

    progress.finish().await;
    Ok(LinkReport {
        identified,
        unknown: hashes.len() - identified,
    })
}

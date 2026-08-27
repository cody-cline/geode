//! Installing mods and other content into an instance, with dependency
//! resolution and update checking.

use std::collections::HashSet;

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
    if let Some(reason) = &version.blocked_reason {
        return Err(Error::msg(reason.clone()));
    }
    let file = version
        .primary_file()
        .ok_or_else(|| Error::msg(format!("{display_name} has no downloadable file")))?;

    let dir = crate::instance::content::content_dir(paths, &instance.id, kind);
    tokio::fs::create_dir_all(&dir).await?;

    // Remove the previous file for this project before writing the new one.
    let previous: Vec<String> = instance
        .content
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
        instance
            .content
            .retain(|r| !(r.file_name == old && r.kind == kind));
    }

    instance
        .content
        .retain(|r| !(r.file_name == file.filename && r.kind == kind));
    instance.content.push(ContentRecord {
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
    });

    Ok(file.filename.clone())
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
#[allow(clippy::too_many_arguments)]
pub async fn install_with_dependencies(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    version: &Version,
    project: &Project,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<InstallReport> {
    let kind = project.kind.install_kind();
    let mut report = InstallReport {
        installed: Vec::new(),
        skipped: Vec::new(),
    };

    progress.stage("Installing", 1).await;
    let file = install(
        client,
        paths,
        instance,
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

    if kind != ContentKind::Mod {
        return Ok(report);
    }

    let mut seen: HashSet<String> = instance
        .content
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
            paths,
            instance,
            version.provider,
            &project_id,
            &instance_game_version(instance),
            instance.loader.as_str(),
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
            Ok(None) => report.skipped.push(format!(
                "no compatible version of dependency {project_id}"
            )),
            Err(e) => report.skipped.push(format!("dependency {project_id}: {e}")),
        }
    }

    Ok(report)
}

fn instance_game_version(instance: &Instance) -> String {
    instance.mc_version.clone()
}

#[allow(clippy::too_many_arguments)]
async fn resolve_dependency(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    provider: Provider,
    project_id: &str,
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

    let filename = install(
        client,
        paths,
        instance,
        best,
        ContentKind::Mod,
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
pub async fn check_updates(
    client: &reqwest::Client,
    instance: &Instance,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<UpdateCandidate>> {
    let tracked: Vec<ContentRecord> = instance.content.clone();
    progress.stage("Checking for updates", tracked.len() as u64).await;

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

        // Resource packs and shaders are not loader-specific.
        let loader = (record.kind == ContentKind::Mod).then(|| instance.loader.as_str());
        let versions = match providers::versions(
            client,
            provider,
            &record.project_id,
            Some(&instance.mc_version),
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
        let versions = vec![version("blocked", "release", false), version("ok", "beta", true)];
        assert_eq!(pick_best(&versions).unwrap().id, "ok");
    }

    #[test]
    fn none_when_nothing_installable() {
        let versions = vec![version("blocked", "release", false)];
        assert!(pick_best(&versions).is_none());
    }
}

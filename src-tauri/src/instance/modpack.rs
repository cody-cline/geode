//! Modpack import and export.
//!
//! Two formats matter in practice: Modrinth's `.mrpack` (an index of download
//! URLs plus an overrides tree) and CurseForge's zip (project/file id pairs
//! that have to be resolved through their API). Both are supported for import;
//! export produces `.mrpack`, which is the open one.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::meta::Loader;
use crate::net::{Download, Progress};
use crate::paths::Paths;

use super::content::{ContentKind, ContentRecord};
use super::Instance;

// ---------------------------------------------------------------------------
// modrinth.index.json
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrIndex {
    pub format_version: u32,
    pub game: String,
    pub version_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub files: Vec<MrFile>,
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrFile {
    pub path: String,
    #[serde(default)]
    pub hashes: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<MrEnv>,
    pub downloads: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MrEnv {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub server: String,
}

/// Turn a jar's declared side into the pack index's `env` pair.
///
/// A jar that did not say is treated as both-sides, which is what every
/// launcher assumes anyway — the alternative is marking it unsupported
/// somewhere and having it silently not install.
fn env_for(side: Option<crate::instance::jar::Side>) -> MrEnv {
    use crate::instance::jar::Side;
    let (client, server) = match side {
        Some(Side::Client) => ("required", "unsupported"),
        Some(Side::Server) => ("unsupported", "required"),
        Some(Side::Both) | None => ("required", "required"),
    };
    MrEnv {
        client: client.into(),
        server: server.into(),
    }
}

/// Read the loader and game version out of a pack's `dependencies` map.
fn loader_from_dependencies(deps: &HashMap<String, String>) -> (String, Loader, String) {
    let mc = deps.get("minecraft").cloned().unwrap_or_default();
    for (key, loader) in [
        ("fabric-loader", Loader::Fabric),
        ("quilt-loader", Loader::Quilt),
        ("neoforge", Loader::NeoForge),
        ("forge", Loader::Forge),
    ] {
        if let Some(version) = deps.get(key) {
            return (mc, loader, version.clone());
        }
    }
    (mc, Loader::Vanilla, String::new())
}

// ---------------------------------------------------------------------------
// CurseForge manifest.json
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfManifest {
    minecraft: CfMinecraft,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    files: Vec<CfManifestFile>,
    #[serde(default = "default_overrides")]
    overrides: String,
}

fn default_overrides() -> String {
    "overrides".into()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfMinecraft {
    version: String,
    #[serde(default)]
    mod_loaders: Vec<CfModLoader>,
}

#[derive(Debug, Deserialize)]
struct CfModLoader {
    id: String,
    #[serde(default)]
    primary: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfManifestFile {
    #[serde(rename = "projectID")]
    project_id: u32,
    #[serde(rename = "fileID")]
    file_id: u32,
    #[serde(default = "default_required")]
    required: bool,
}

fn default_required() -> bool {
    true
}

/// `forge-47.2.0` / `fabric-0.15.0` / `neoforge-21.4.10`.
fn parse_cf_loader(id: &str) -> (Loader, String) {
    let (name, version) = id.split_once('-').unwrap_or((id, ""));
    let loader = match name.to_ascii_lowercase().as_str() {
        "forge" => Loader::Forge,
        "fabric" => Loader::Fabric,
        "quilt" => Loader::Quilt,
        "neoforge" => Loader::NeoForge,
        _ => Loader::Vanilla,
    };
    (loader, version.to_string())
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// What we learned from a pack file before committing to installing it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackPreview {
    pub name: String,
    pub version: String,
    pub summary: Option<String>,
    pub mc_version: String,
    pub loader: Loader,
    pub loader_version: String,
    pub file_count: usize,
    pub format: String,
    /// True when finishing the import needs a CurseForge API key.
    pub needs_curseforge_key: bool,
}

enum PackKind {
    Modrinth(MrIndex),
    CurseForge(CfManifest),
}

fn read_pack(archive: &Path) -> Result<PackKind> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;

    if let Ok(mut entry) = zip.by_name("modrinth.index.json") {
        let mut raw = String::new();
        entry.read_to_string(&mut raw)?;
        let index: MrIndex =
            serde_json::from_str(&raw).map_err(|e| Error::json("modrinth.index.json", e))?;
        return Ok(PackKind::Modrinth(index));
    }

    let mut entry = zip.by_name("manifest.json").map_err(|_| {
        Error::msg("not a modpack — expected modrinth.index.json or manifest.json inside")
    })?;
    let mut raw = String::new();
    entry.read_to_string(&mut raw)?;
    let manifest: CfManifest =
        serde_json::from_str(&raw).map_err(|e| Error::json("manifest.json", e))?;
    Ok(PackKind::CurseForge(manifest))
}

pub async fn preview(archive: PathBuf) -> Result<PackPreview> {
    tokio::task::spawn_blocking(move || {
        let pack = read_pack(&archive)?;
        Ok(match pack {
            PackKind::Modrinth(index) => {
                let (mc, loader, loader_version) = loader_from_dependencies(&index.dependencies);
                PackPreview {
                    name: index.name,
                    version: index.version_id,
                    summary: index.summary,
                    mc_version: mc,
                    loader,
                    loader_version,
                    file_count: index.files.len(),
                    format: "mrpack".into(),
                    needs_curseforge_key: false,
                }
            }
            PackKind::CurseForge(manifest) => {
                let primary = manifest
                    .minecraft
                    .mod_loaders
                    .iter()
                    .find(|l| l.primary)
                    .or_else(|| manifest.minecraft.mod_loaders.first());
                let (loader, loader_version) = primary
                    .map(|l| parse_cf_loader(&l.id))
                    .unwrap_or((Loader::Vanilla, String::new()));
                PackPreview {
                    name: manifest.name,
                    version: manifest.version,
                    summary: None,
                    mc_version: manifest.minecraft.version,
                    loader,
                    loader_version,
                    file_count: manifest.files.len(),
                    format: "curseforge".into(),
                    needs_curseforge_key: !manifest.files.is_empty(),
                }
            }
        })
    })
    .await
    .map_err(|e| Error::msg(format!("reading pack panicked: {e}")))?
}

/// The mods a pack installs, identified as projects rather than files.
///
/// This is what a *plan* wants from a pack: which mods are in it, not which
/// builds — a plan has no version of its own to pin them to.
#[derive(Debug, Clone)]
pub enum PackMods {
    /// Modrinth project ids, read straight out of the index's download URLs.
    Modrinth(Vec<String>),
    /// CurseForge project ids, which need the API to become names.
    CurseForge(Vec<u32>),
}

/// Read a pack's mod list without installing anything.
pub async fn index_mods(archive: PathBuf) -> Result<PackMods> {
    tokio::task::spawn_blocking(move || {
        Ok(match read_pack(&archive)? {
            PackKind::Modrinth(index) => {
                let mut ids = Vec::new();
                for file in &index.files {
                    // Only the mods folder: a plan is a list of mods, and a
                    // pack's resource packs and shaders are not that.
                    let path = Path::new(&file.path);
                    let in_mods = path
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|d| d.to_str())
                        == Some("mods");
                    if !in_mods {
                        continue;
                    }
                    if let Some((project_id, _)) = file
                        .downloads
                        .iter()
                        .find_map(|u| crate::install::content::ids_from_modrinth_url(u))
                    {
                        if !ids.contains(&project_id) {
                            ids.push(project_id);
                        }
                    }
                }
                PackMods::Modrinth(ids)
            }
            PackKind::CurseForge(manifest) => {
                // A CurseForge manifest does not say which folder a file lands
                // in, so everything it lists is a candidate; resolving the ids
                // is what reveals the resource packs, and those are dropped
                // there rather than guessed at here.
                let mut ids = Vec::new();
                for file in &manifest.files {
                    if !ids.contains(&file.project_id) {
                        ids.push(file.project_id);
                    }
                }
                PackMods::CurseForge(ids)
            }
        })
    })
    .await
    .map_err(|e| Error::msg(format!("reading pack panicked: {e}")))?
}

/// Install a pack into an existing (already created) instance directory.
pub async fn import_into(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<()> {
    let game_dir = paths.game_dir(&instance.id);
    let archive_owned = archive.to_path_buf();

    let pack = tokio::task::spawn_blocking({
        let archive = archive_owned.clone();
        move || read_pack(&archive)
    })
    .await
    .map_err(|e| Error::msg(format!("reading pack panicked: {e}")))??;

    match pack {
        PackKind::Modrinth(index) => {
            progress.stage("Extracting overrides", 1).await;
            extract_overrides(
                &archive_owned,
                &game_dir,
                &["overrides", "client-overrides"],
            )
            .await?;
            progress.advance(1, "overrides").await;

            let downloads: Vec<Download> = index
                .files
                .iter()
                .filter(|f| {
                    // Server-only files have no business in a client instance.
                    f.env
                        .as_ref()
                        .map(|e| e.client != "unsupported")
                        .unwrap_or(true)
                })
                .filter_map(|f| {
                    let rel = Path::new(&f.path);
                    if !crate::paths::is_safe_relative(rel) {
                        return None;
                    }
                    let url = f.downloads.first()?;
                    Some(
                        Download::new(url.clone(), game_dir.join(rel))
                            .sha1(f.hashes.get("sha1").cloned())
                            .size(f.file_size)
                            .label(f.path.clone()),
                    )
                })
                .collect();

            progress
                .stage("Downloading pack contents", downloads.len() as u64)
                .await;
            crate::net::fetch_all(client, downloads, progress, 10).await?;

            record_mrpack_provenance(client, instance, &index.files, progress).await;

            instance.pack = Some(super::PackSource::local(
                index.name,
                index.version_id,
                Some("modrinth".into()),
            ));
        }

        PackKind::CurseForge(manifest) => {
            let key = curseforge_key.filter(|k| !k.trim().is_empty()).ok_or_else(|| {
                Error::msg(
                    "importing a CurseForge pack needs your CurseForge API key — add one in Settings",
                )
            })?;

            progress.stage("Extracting overrides", 1).await;
            extract_overrides(&archive_owned, &game_dir, &[manifest.overrides.as_str()]).await?;
            progress.advance(1, "overrides").await;

            progress
                .stage("Resolving pack contents", manifest.files.len() as u64)
                .await;

            let mut downloads = Vec::new();
            let mut records = Vec::new();
            let mut blocked = Vec::new();

            for entry in &manifest.files {
                progress.check_cancelled()?;
                let version = match crate::providers::curseforge::file(
                    client,
                    entry.project_id,
                    entry.file_id,
                    key,
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        progress
                            .advance(1, &format!("skipped {}: {e}", entry.project_id))
                            .await;
                        continue;
                    }
                };
                progress.advance(1, &version.name).await;

                let Some(file) = version.primary_file() else {
                    // Author-blocked downloads cannot be fetched on the user's
                    // behalf; collect them so we can tell them what to do.
                    blocked.push(version.name.clone());
                    continue;
                };

                downloads.push(
                    Download::new(file.url.clone(), game_dir.join("mods").join(&file.filename))
                        .sha1(file.sha1.clone())
                        .size(file.size)
                        .label(file.filename.clone()),
                );
                records.push(ContentRecord {
                    file_name: file.filename.clone(),
                    kind: ContentKind::Mod,
                    provider: "curseforge".into(),
                    project_id: version.project_id.clone(),
                    version_id: version.id.clone(),
                    name: version.name.clone(),
                    version_number: version.version_number.clone(),
                    slug: None,
                    icon_url: None,
                    dependency: !entry.required,
                    // Pack manifests carry no dependency graph; the jars do.
                    requires: None,
                });
            }

            progress
                .stage("Downloading pack contents", downloads.len() as u64)
                .await;
            crate::net::fetch_all(client, downloads, progress, 8).await?;

            instance.content.extend(records);
            instance.pack = Some(super::PackSource::local(
                manifest.name,
                manifest.version,
                Some("curseforge".into()),
            ));

            if !blocked.is_empty() {
                return Err(Error::msg(format!(
                    "{} file(s) could not be downloaded because their authors disabled \
                     third-party downloads. Install them by hand from CurseForge: {}",
                    blocked.len(),
                    blocked.join(", ")
                )));
            }
        }
    }

    Ok(())
}

/// Copy the pack's override trees into the game directory. Later roots win, so
/// `client-overrides` correctly shadows `overrides`.
async fn extract_overrides(archive: &Path, game_dir: &Path, roots: &[&str]) -> Result<()> {
    let archive = archive.to_path_buf();
    let game_dir = game_dir.to_path_buf();
    let roots: Vec<String> = roots.iter().map(|s| s.to_string()).collect();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&archive)?;
        let mut zip = zip::ZipArchive::new(file)?;

        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            let Some(rel) = roots
                .iter()
                .find_map(|root| name.strip_prefix(&format!("{root}/")))
            else {
                continue;
            };

            let rel_path = Path::new(rel);
            if !crate::paths::is_safe_relative(rel_path) {
                continue;
            }
            let dest = game_dir.join(rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&dest)?;
            std::io::copy(&mut entry, &mut out)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| Error::msg(format!("extracting overrides panicked: {e}")))?
}

/// Give every file a `.mrpack` installed a link back to its project.
///
/// The index records only a URL and a hash, so without this a freshly
/// installed pack shows dozens of "untracked" rows with filenames for names and
/// no update path. The URL already contains the project and version ids, so no
/// extra lookup is needed to establish provenance — only one batched call to
/// turn those ids into names and icons.
///
/// Best-effort: a pack is perfectly usable with imperfect labels, so a failure
/// here is swallowed rather than failing the install.
async fn record_mrpack_provenance(
    client: &reqwest::Client,
    instance: &mut Instance,
    files: &[MrFile],
    progress: &Progress,
) {
    let mut pending: Vec<(ContentKind, String, String, String)> = Vec::new();

    for file in files {
        let path = Path::new(&file.path);
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let kind = match path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|d| d.to_str())
        {
            Some("mods") => ContentKind::Mod,
            Some("resourcepacks") => ContentKind::ResourcePack,
            Some("shaderpacks") => ContentKind::ShaderPack,
            Some("datapacks") => ContentKind::DataPack,
            _ => continue,
        };
        let Some((project_id, version_id)) = file
            .downloads
            .iter()
            .find_map(|u| crate::install::content::ids_from_modrinth_url(u))
        else {
            continue;
        };
        pending.push((kind, name.to_string(), project_id, version_id));
    }

    if pending.is_empty() {
        return;
    }

    progress
        .stage("Linking pack contents", pending.len() as u64)
        .await;

    let ids: Vec<String> = {
        let mut v: Vec<String> = pending.iter().map(|(_, _, p, _)| p.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let projects = crate::providers::modrinth::projects_by_ids(client, &ids)
        .await
        .unwrap_or_default();

    for (kind, file_name, project_id, version_id) in pending {
        progress.advance(1, &file_name).await;
        let project = projects.iter().find(|p| p.id == project_id);
        instance
            .content
            .retain(|r| !(r.file_name == file_name && r.kind == kind));
        instance.content.push(ContentRecord {
            file_name: file_name.clone(),
            kind,
            provider: "modrinth".into(),
            project_id,
            version_id,
            name: project
                .map(|p| p.name.clone())
                .unwrap_or_else(|| file_name.clone()),
            version_number: String::new(),
            slug: project.map(|p| p.slug.clone()),
            icon_url: project.and_then(|p| p.icon_url.clone()),
            dependency: false,
            requires: None,
        });
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Write the instance out as a `.mrpack`.
///
/// Content installed from Modrinth becomes an index entry (a URL, not a copy),
/// which is what keeps packs small and redistributable. Everything else —
/// configs, hand-dropped jars, CurseForge files we may not redistribute — goes
/// into `overrides/`.
pub async fn export_mrpack(
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    include_saves: bool,
    progress: &Progress,
) -> Result<()> {
    let game_dir = paths.game_dir(&instance.id);

    let mut index = MrIndex {
        format_version: 1,
        game: "minecraft".into(),
        version_id: instance
            .pack
            .as_ref()
            .map(|p| p.version.clone())
            .unwrap_or_else(|| "1.0.0".into()),
        name: instance.name.clone(),
        summary: (!instance.notes.is_empty()).then(|| instance.notes.clone()),
        files: Vec::new(),
        dependencies: HashMap::new(),
    };

    index
        .dependencies
        .insert("minecraft".into(), instance.mc_version.clone());
    let loader_key = match instance.loader {
        Loader::Fabric => Some("fabric-loader"),
        Loader::Quilt => Some("quilt-loader"),
        Loader::NeoForge => Some("neoforge"),
        Loader::Forge => Some("forge"),
        Loader::Vanilla => None,
    };
    if let Some(key) = loader_key {
        index
            .dependencies
            .insert(key.into(), instance.loader_version.clone());
    }

    // Only Modrinth files get indexed — CurseForge's terms do not permit
    // redistributing their download URLs this way.
    let mut indexed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for record in &instance.content {
        if record.provider != "modrinth" {
            continue;
        }
        let rel = PathBuf::from(record.kind.folder()).join(&record.file_name);
        let full = game_dir.join(&rel);
        let Ok(meta) = tokio::fs::metadata(&full).await else {
            continue;
        };
        let Ok(sha1) = crate::net::sha1_file(&full).await else {
            continue;
        };
        // Both hashes, because the readers disagree about which is optional.
        // Modrinth refuses an upload without sha512, and Prism reads it with
        // `requireString` and no fallback — so a pack carrying only sha1 is
        // one no other launcher will open.
        let Ok(sha512) = crate::net::sha512_file(&full).await else {
            continue;
        };

        // What the jar itself says about where it belongs, so a pack installed
        // on a server does not drag a renderer along with it.
        let probe = full.clone();
        let side = tokio::task::spawn_blocking(move || crate::instance::jar::read(&probe))
            .await
            .ok()
            .flatten()
            .and_then(|meta| meta.side);

        index.files.push(MrFile {
            path: rel.to_string_lossy().replace('\\', "/"),
            hashes: [("sha1".to_string(), sha1), ("sha512".to_string(), sha512)]
                .into_iter()
                .collect(),
            // Always both sides, never omitted. Prism reads the client value
            // with a default of "unsupported", so a file that leaves it out
            // is a file Prism silently declines to install.
            env: Some(env_for(side)),
            downloads: vec![format!(
                "https://cdn.modrinth.com/data/{}/versions/{}/{}",
                record.project_id,
                record.version_id,
                urlencoding::encode(&record.file_name)
            )],
            file_size: Some(meta.len()),
        });
        indexed.insert(rel);
    }

    progress.stage("Writing pack", 1).await;

    let mut skip_dirs: Vec<&str> = vec![
        "logs",
        "crash-reports",
        "screenshots",
        ".fabric",
        "versions",
    ];
    if !include_saves {
        skip_dirs.push("saves");
    }

    let index_json = serde_json::to_vec_pretty(&index)?;
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::create(&dest)?;
        let mut zip = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        zip.start_file("modrinth.index.json", options)?;
        zip.write_all(&index_json)?;

        let mut stack = vec![game_dir.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(rel) = path.strip_prefix(&game_dir) else {
                    continue;
                };
                let top = rel.components().next().and_then(|c| c.as_os_str().to_str());
                if top.is_some_and(|t| skip_dirs.contains(&t)) {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if indexed.contains(rel) {
                    continue;
                }
                let name = format!("overrides/{}", rel.to_string_lossy().replace('\\', "/"));
                zip.start_file(name, options)?;
                let mut source = std::fs::File::open(&path)?;
                std::io::copy(&mut source, &mut zip)?;
            }
        }

        zip.finish()?;
        Ok(())
    })
    .await
    .map_err(|e| Error::msg(format!("writing pack panicked: {e}")))??;

    progress.finish().await;
    Ok(())
}

#[cfg(test)]
mod tests {

    /// A pack index has to say where each file belongs, and say it for both
    /// sides — Prism defaults a missing client value to "unsupported", so a
    /// half-filled `env` is a file that silently does not install.
    #[test]
    fn a_files_env_follows_what_the_jar_declared() {
        use crate::instance::jar::Side;

        let client = env_for(Some(Side::Client));
        assert_eq!(client.client, "required");
        assert_eq!(client.server, "unsupported");

        let server = env_for(Some(Side::Server));
        assert_eq!(server.client, "unsupported");
        assert_eq!(server.server, "required");

        // Both-sides and "the jar did not say" are treated the same, because
        // the alternative is marking something unsupported on a guess.
        for undeclared in [Some(Side::Both), None] {
            let env = env_for(undeclared);
            assert_eq!(env.client, "required");
            assert_eq!(env.server, "required");
        }
    }
    use super::*;

    #[test]
    fn reads_fabric_dependencies() {
        let deps: HashMap<String, String> = [
            ("minecraft".to_string(), "1.21.1".to_string()),
            ("fabric-loader".to_string(), "0.16.9".to_string()),
        ]
        .into_iter()
        .collect();
        let (mc, loader, version) = loader_from_dependencies(&deps);
        assert_eq!(mc, "1.21.1");
        assert_eq!(loader, Loader::Fabric);
        assert_eq!(version, "0.16.9");
    }

    #[test]
    fn vanilla_pack_has_no_loader() {
        let deps: HashMap<String, String> = [("minecraft".to_string(), "1.21.1".to_string())]
            .into_iter()
            .collect();
        let (_, loader, _) = loader_from_dependencies(&deps);
        assert_eq!(loader, Loader::Vanilla);
    }

    #[test]
    fn parses_curseforge_loader_ids() {
        assert_eq!(
            parse_cf_loader("forge-47.2.0"),
            (Loader::Forge, "47.2.0".into())
        );
        assert_eq!(
            parse_cf_loader("neoforge-21.4.10-beta"),
            (Loader::NeoForge, "21.4.10-beta".into())
        );
        assert_eq!(
            parse_cf_loader("fabric-0.15.0"),
            (Loader::Fabric, "0.15.0".into())
        );
    }
}

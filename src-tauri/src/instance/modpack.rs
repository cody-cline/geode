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

/// The most a pack's index or manifest may inflate to.
///
/// A pack of a thousand mods has an index well under a megabyte. The file
/// is read whole into memory and then parsed, and deflate turns a run of
/// spaces into a thousandth of its size, so without a ceiling a pack a few
/// megabytes long could ask for gigabytes of memory before it said anything.
const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;

/// Read one of a pack's JSON entries, refusing one too large to be real.
///
/// Enforced on the bytes as they inflate rather than on the size the entry's
/// header gives, which a crafted zip can understate.
pub(crate) fn read_json_entry(entry: impl Read, name: &str) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    entry.take(MAX_JSON_BYTES + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_JSON_BYTES {
        return Err(Error::msg(format!(
            "this pack's {name} is over {} MB, far larger than any real pack's, so it was not read",
            MAX_JSON_BYTES / (1024 * 1024)
        )));
    }
    Ok(raw)
}

/// The most a pack's list of files may take to read, in bytes of the file.
///
/// A zip's central directory is read whole into memory before any entry can
/// be looked at, at several hundred bytes held for every file it lists. A pack
/// of fifty thousand files lists them in a few megabytes; a crafted one small
/// enough to download can list millions, and ask for gigabytes to hold them.
const MAX_DIRECTORY_READ: u64 = 16 * 1024 * 1024;

/// Open a pack, refusing one whose list of files is too large to be real.
pub(crate) fn open_pack(archive: &Path) -> Result<zip::ZipArchive<std::fs::File>> {
    open_pack_within(archive, MAX_DIRECTORY_READ)
}

/// [`open_pack`] with the budget a test can reach.
///
/// Opened once through a reader that stops after `budget` bytes, then again
/// for real. Counting what is read, rather than trusting the count of files a
/// zip declares, holds however the zip reader settles on a directory — and it
/// will try every end-of-directory record in the file before giving up.
fn open_pack_within(archive: &Path, budget: u64) -> Result<zip::ZipArchive<std::fs::File>> {
    struct Budgeted<'a> {
        file: std::fs::File,
        left: u64,
        exhausted: &'a std::cell::Cell<bool>,
    }
    impl Read for Budgeted<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.left == 0 {
                self.exhausted.set(true);
                return Err(std::io::Error::other("read budget spent"));
            }
            let most = buf
                .len()
                .min(usize::try_from(self.left).unwrap_or(usize::MAX));
            let read = self.file.read(&mut buf[..most])?;
            self.left -= read as u64;
            Ok(read)
        }
    }
    impl std::io::Seek for Budgeted<'_> {
        fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
            self.file.seek(to)
        }
    }

    let exhausted = std::cell::Cell::new(false);
    let probed = zip::ZipArchive::new(Budgeted {
        file: std::fs::File::open(archive)?,
        left: budget,
        exhausted: &exhausted,
    })
    .map(drop);
    if exhausted.get() {
        return Err(Error::msg(
            "this pack lists far more files than any real pack, or its list could not be found, so it was not read",
        ));
    }
    probed?;
    Ok(zip::ZipArchive::new(std::fs::File::open(archive)?)?)
}

fn read_pack(archive: &Path) -> Result<PackKind> {
    let mut zip = open_pack(archive)?;

    if let Ok(entry) = zip.by_name("modrinth.index.json") {
        let raw = read_json_entry(entry, "modrinth.index.json")?;
        let index: MrIndex =
            serde_json::from_slice(&raw).map_err(|e| Error::json("modrinth.index.json", e))?;
        return Ok(PackKind::Modrinth(index));
    }

    let entry = zip.by_name("manifest.json").map_err(|_| {
        Error::msg("not a modpack — expected modrinth.index.json or manifest.json inside")
    })?;
    let raw = read_json_entry(entry, "manifest.json")?;
    let manifest: CfManifest =
        serde_json::from_slice(&raw).map_err(|e| Error::json("manifest.json", e))?;
    Ok(PackKind::CurseForge(manifest))
}

/// A Modrinth pack's index; `None` for a CurseForge pack, whose files are
/// named by id and resolved through CurseForge.
pub(crate) fn read_index(archive: &Path) -> Result<Option<MrIndex>> {
    Ok(match read_pack(archive)? {
        PackKind::Modrinth(index) => Some(index),
        PackKind::CurseForge(_) => None,
    })
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
                    sharing: Default::default(),
                    origin: Default::default(),
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

/// How many bytes a pack's overrides may unpack to, all together.
///
/// A real pack's overrides are mostly jars and zips, which deflate cannot
/// shrink any further, with configs beside them; even text rarely deflates
/// past ten to one. A run of zeros deflates about a thousand to one, which is
/// what a pack built to fill a disk is made of — and one that arrives from a
/// server is only held to a limit on its download, not on what it unpacks to.
///
/// Measured against the pack's own size, so a large legitimate pack is never
/// refused for being large, with a floor so a small one that happens to be all
/// configs is not refused either.
fn overrides_budget(archive_bytes: u64) -> u64 {
    const RATIO: u64 = 32;
    const FLOOR: u64 = 16 * 1024 * 1024;
    archive_bytes.saturating_mul(RATIO).max(FLOOR)
}

/// Copy the pack's override trees into the game directory. Later roots win, so
/// `client-overrides` correctly shadows `overrides`.
async fn extract_overrides(archive: &Path, game_dir: &Path, roots: &[&str]) -> Result<()> {
    let archive = archive.to_path_buf();
    let game_dir = game_dir.to_path_buf();
    let roots: Vec<String> = roots.iter().map(|s| s.to_string()).collect();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let archive_bytes = std::fs::metadata(&archive)?.len();
        let budget = overrides_budget(archive_bytes);
        let mut unpacked: u64 = 0;
        let mut zip = open_pack(&archive)?;

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
            // Counted on the bytes as they inflate. The entry's header states a
            // size too, and a crafted zip can understate it.
            let left = budget - unpacked;
            let copied = std::io::copy(&mut (&mut entry).take(left + 1), &mut out)?;
            if copied > left {
                drop(out);
                let _ = std::fs::remove_file(&dest);
                return Err(Error::msg(format!(
                    "this pack unpacks to more than {} MB from a {} MB file, which no real \
                     pack does — it was stopped before it could fill the disk",
                    budget / (1024 * 1024),
                    archive_bytes.div_ceil(1024 * 1024)
                )));
            }
            unpacked += copied;
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
            sharing: Default::default(),
            origin: Default::default(),
            requires: None,
        });
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Whether the caller is carrying this file as a reference instead of bytes.
///
/// Matched on the enabled name, because that is how a content record is keyed:
/// turning a mod off renames it to `foo.jar.disabled` on disk and leaves the
/// record saying `foo.jar`. Comparing the two directly would miss every
/// disabled file — and missing it here is the bad direction, since the file
/// would be written into `overrides/` while the manifest claimed it had been
/// left out, which is both halves wrong at once.
fn carried_by_reference(referenced: &std::collections::HashSet<PathBuf>, rel: &Path) -> bool {
    if referenced.is_empty() {
        return false;
    }
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let (base, _) = crate::instance::content::strip_disabled(name);
    referenced.contains(&rel.with_file_name(base))
}

/// Extra entries to place in the zip beside the index.
///
/// This is how a `.geodepack` is written: the same `.mrpack` exactly, plus a
/// `geode/` directory. Keeping it as an argument rather than a second writer
/// means there is only one place that knows how to lay a pack out, and the
/// Geode half can never drift from the half other launchers read.
pub type Extras = Vec<(String, Vec<u8>)>;

/// Write the instance out as a `.mrpack`.
///
/// Content installed from Modrinth becomes an index entry — a URL and a pair of
/// hashes, not a copy — which is what keeps a pack small and keeps its mods
/// current. Everything else goes into `overrides/` as bytes, except whatever
/// the caller names in `referenced`.
///
/// `referenced` is how a `.geodepack` avoids shipping other people's jars: its
/// `geode/manifest.json` records a `SourceRef` for each of those paths, so the
/// recipient fetches them by id through their own credentials. Only a caller
/// writing that manifest may pass anything here — a plain `.mrpack` has nowhere
/// to record a reference, so leaving a file out of it would lose the mod with
/// nothing anywhere to say so. The set is keyed the way the content records
/// are, on the enabled file name, so a disabled file matches too.
///
/// It would be possible to put a CurseForge download URL straight into the
/// index — the four-host allowlist in Modrinth's format is enforced by their
/// *upload* validator, not by readers, and Prism imports any URL it is given.
/// We do not, because a URL baked into a pack keeps working after an author
/// turns third-party downloads off, while a reference resolved at import time
/// sees that decision. The recipient's answer should be today's, not the one
/// that was true when somebody else exported.
pub async fn export_mrpack(
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    include_saves: bool,
    extras: Extras,
    referenced: &std::collections::HashSet<PathBuf>,
    progress: &Progress,
) -> Result<()> {
    let mut skip_dirs: Vec<&'static str> = vec![
        "logs",
        "crash-reports",
        "screenshots",
        ".fabric",
        "versions",
    ];
    if !include_saves {
        skip_dirs.push("saves");
    }

    let input = PackInput {
        root: paths.game_dir(&instance.id),
        name: &instance.name,
        notes: &instance.notes,
        version: instance
            .pack
            .as_ref()
            .map(|p| p.version.clone())
            .unwrap_or_else(|| "1.0.0".into()),
        mc_version: &instance.mc_version,
        loader: instance.loader,
        loader_version: &instance.loader_version,
        content: instance.content.iter().collect(),
        walk: Walk::Except(skip_dirs),
    };
    write_mrpack(input, dest, extras, referenced, progress).await
}

/// Which entries under a pack's root the writer may carry as bytes.
pub enum Walk {
    /// Everything except these top-level entries. Right for an instance, whose
    /// game directory is the player's own and mostly worth sending.
    ///
    /// This walk follows links. The player exporting is the one who made them
    /// — a `saves` or `resourcepacks` shared between instances is a common
    /// setup — and the file it writes goes wherever they take it, so the pack
    /// should hold what they see in game.
    Except(Vec<&'static str>),
    /// Nothing but these, and never through a link. Right for anything holding
    /// files that must never leave the machine, where a list of what to keep
    /// out is only ever one forgotten name from shipping a secret.
    ///
    /// A link is refused wherever it sits, because an allowlist of folders
    /// says nothing about where a link inside one of them points: a
    /// `mods/backup` linked to the directory above would carry
    /// `server.properties` under an allowed name. Nobody looks at this pack
    /// before it goes, either — it is built for whoever presents the code.
    Only {
        /// Folders carried whole, at any depth.
        trees: Vec<&'static str>,
        /// Single files, by path from the root as they are named on disk.
        /// Carried only while each is still a regular file.
        files: std::collections::HashSet<PathBuf>,
    },
}

/// What the walk does with one entry it finds.
enum Step {
    Descend,
    Carry,
    Skip,
}

impl Walk {
    /// Decide on one entry, given its path from the root.
    fn step(&self, rel: &Path, entry: &std::fs::DirEntry) -> Step {
        let top = rel.components().next().and_then(|c| c.as_os_str().to_str());
        match self {
            Walk::Except(skip) => {
                if top.is_some_and(|t| skip.contains(&t)) {
                    Step::Skip
                } else if entry.path().is_dir() {
                    Step::Descend
                } else {
                    Step::Carry
                }
            }
            Walk::Only { trees, files } => {
                // The entry's own type, which is the link itself rather than
                // what it points at. `Path::is_dir` and `File::open` both
                // follow, and are how a link would get through.
                let Ok(kind) = entry.file_type() else {
                    return Step::Skip;
                };
                let in_tree = top.is_some_and(|t| trees.contains(&t));
                if kind.is_dir() {
                    if in_tree || files.iter().any(|f| f.starts_with(rel)) {
                        Step::Descend
                    } else {
                        Step::Skip
                    }
                } else if kind.is_file()
                    && ((in_tree && rel.components().count() > 1) || files.contains(rel))
                {
                    Step::Carry
                } else {
                    Step::Skip
                }
            }
        }
    }
}

/// Everything the writer needs to know about what it is packing.
///
/// An instance and a server both become the same kind of pack, but only one of
/// them is an [`Instance`]. Handing the writer plain values instead is what
/// lets both go through the one function that knows how a pack is laid out.
pub struct PackInput<'a> {
    /// The directory `overrides/` mirrors.
    pub root: PathBuf,
    pub name: &'a str,
    pub notes: &'a str,
    pub version: String,
    pub mc_version: &'a str,
    pub loader: Loader,
    pub loader_version: &'a str,
    /// Records the index may draw Modrinth entries from. A record left out of
    /// this list is never indexed, whatever it says.
    pub content: Vec<&'a ContentRecord>,
    pub walk: Walk,
}

/// The one pack writer. See [`export_mrpack`] for what goes where.
pub async fn write_mrpack(
    input: PackInput<'_>,
    dest: PathBuf,
    extras: Extras,
    referenced: &std::collections::HashSet<PathBuf>,
    progress: &Progress,
) -> Result<()> {
    let game_dir = input.root;

    let mut index = MrIndex {
        format_version: 1,
        game: "minecraft".into(),
        version_id: input.version,
        name: input.name.to_string(),
        summary: (!input.notes.is_empty()).then(|| input.notes.to_string()),
        files: Vec::new(),
        dependencies: HashMap::new(),
    };

    index
        .dependencies
        .insert("minecraft".into(), input.mc_version.to_string());
    let loader_key = match input.loader {
        Loader::Fabric => Some("fabric-loader"),
        Loader::Quilt => Some("quilt-loader"),
        Loader::NeoForge => Some("neoforge"),
        Loader::Forge => Some("forge"),
        Loader::Vanilla => None,
    };
    if let Some(key) = loader_key {
        index
            .dependencies
            .insert(key.into(), input.loader_version.to_string());
    }

    // Only Modrinth files get indexed. Anything else either travels as a
    // reference in the Geode half, when the caller is writing one, or as bytes
    // in `overrides/` — see `export_mrpack`'s notes for why not a URL.
    let mut indexed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for record in &input.content {
        if record.provider != "modrinth" {
            continue;
        }
        let rel = PathBuf::from(record.kind.folder()).join(&record.file_name);
        let full = game_dir.join(&rel);
        // Under a walk that follows no links, the index does not either: a
        // link named like a record would otherwise have its target hashed and
        // described, beside a real turned-off twin that is carried anyway.
        let meta = match input.walk {
            Walk::Except(_) => tokio::fs::metadata(&full).await.ok(),
            Walk::Only { .. } => tokio::fs::symlink_metadata(&full)
                .await
                .ok()
                .filter(|meta| meta.is_file()),
        };
        let Some(meta) = meta else {
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

    let walk = input.walk;
    let index_json = serde_json::to_vec_pretty(&index)?;
    let referenced = referenced.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::create(&dest)?;
        let mut zip = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        zip.start_file("modrinth.index.json", options)?;
        zip.write_all(&index_json)?;

        for (name, bytes) in &extras {
            zip.start_file(name.clone(), options)?;
            zip.write_all(bytes)?;
        }

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
                match walk.step(rel, &entry) {
                    Step::Skip => continue,
                    Step::Descend => {
                        stack.push(path);
                        continue;
                    }
                    Step::Carry => {}
                }
                if indexed.contains(rel) || carried_by_reference(&referenced, rel) {
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

    /// What an instance's pack leaves behind: the game's logs and caches, and
    /// its worlds unless asked for. Pinned because the same writer walks a
    /// server's run directory the other way round, by allowlist, and the
    /// instance's walk must not drift along with it.
    #[tokio::test]
    async fn an_instance_pack_leaves_out_the_games_own_clutter() {
        let paths = Paths {
            root: std::env::temp_dir().join(format!("geode-mrpack-walk-{}", std::process::id())),
        };
        let _ = tokio::fs::remove_dir_all(&paths.root).await;
        let mut inst = Instance::new("Walk".into(), "1.21.1".into(), Loader::Fabric, "0.16.0".into());
        inst.id = "walk".into();

        let game = paths.game_dir(&inst.id);
        for rel in [
            "logs/latest.log",
            "crash-reports/crash.txt",
            "screenshots/shot.png",
            ".fabric/cache.bin",
            "versions/1.21.1/1.21.1.json",
            "saves/world/level.dat",
            "config/mod.toml",
            "options.txt",
            "mods/hand.jar",
        ] {
            let path = game.join(rel);
            tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
            tokio::fs::write(path, b"x").await.unwrap();
        }

        let kept = [
            "modrinth.index.json",
            "overrides/config/mod.toml",
            "overrides/mods/hand.jar",
            "overrides/options.txt",
        ];
        for include_saves in [false, true] {
            let dest = paths.root.join(format!("walk-{include_saves}.mrpack"));
            export_mrpack(
                &paths,
                &inst,
                dest.clone(),
                include_saves,
                Vec::new(),
                &Default::default(),
                &Progress::silent("t"),
            )
            .await
            .unwrap();

            let zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
            let mut names: Vec<String> = zip.file_names().map(str::to_string).collect();
            names.sort();
            let mut expected: Vec<String> = kept.iter().map(|s| s.to_string()).collect();
            if include_saves {
                expected.push("overrides/saves/world/level.dat".into());
                expected.sort();
            }
            assert_eq!(names, expected, "include_saves={include_saves}");
        }
    }

    // -----------------------------------------------------------------------
    // Packs built to exhaust the machine reading them
    // -----------------------------------------------------------------------

    /// A zip written from parts, each entry compressed the way it says.
    fn write_zip(dest: &Path, entries: &[(&str, &[u8], zip::CompressionMethod)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        for (name, bytes, method) in entries {
            let options: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().compression_method(*method);
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("geode-modpack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const INDEX: &str = r#"{"formatVersion":1,"game":"minecraft","versionId":"1","name":"P","files":[],"dependencies":{"minecraft":"1.21.1"}}"#;
    const CURSEFORGE: &str = r#"{"minecraft":{"version":"1.21.1","modLoaders":[]},"name":"P","files":[]}"#;
    const MIB: usize = 1024 * 1024;

    /// An index is read whole and then parsed, so one that inflates to
    /// gigabytes from a small file is a way to exhaust a player's memory. The
    /// padding is spaces, which is still valid JSON: only the size refuses it.
    #[tokio::test]
    async fn an_index_too_large_to_be_real_is_refused() {
        use zip::CompressionMethod::Deflated;
        let dir = scratch("bigindex");

        for (entry, json) in [("modrinth.index.json", INDEX), ("manifest.json", CURSEFORGE)] {
            let small = dir.join("small.mrpack");
            let padded = [json.as_bytes(), &[b' '; 1024]].concat();
            write_zip(&small, &[(entry, &padded, Deflated)]);
            preview(small).await.unwrap_or_else(|e| panic!("{entry} with a little padding: {e}"));

            let huge = dir.join("huge.mrpack");
            let padded = [json.as_bytes(), &vec![b' '; 9 * MIB]].concat();
            write_zip(&huge, &[(entry, &padded, Deflated)]);
            let err = preview(huge).await.expect_err("an oversized index must be refused");
            assert!(err.to_string().contains(entry), "{err}");
            assert!(err.to_string().contains("not read"), "{err}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn unpacking(dir: &Path) -> (Paths, Instance) {
        let paths = Paths { root: dir.to_path_buf() };
        let mut inst = Instance::new("P".into(), "1.21.1".into(), Loader::Vanilla, String::new());
        inst.id = "unpacked".into();
        (paths, inst)
    }

    /// Deflate turns a run of zeros into a thousandth of its size, so a pack
    /// that is small to download can unpack to fill a disk. It is stopped at
    /// its budget, and the file it was writing is not left behind half done.
    #[tokio::test]
    async fn an_override_bomb_is_stopped_before_it_fills_the_disk() {
        use zip::CompressionMethod::Deflated;
        let dir = scratch("bomb");
        let archive = dir.join("bomb.mrpack");
        write_zip(
            &archive,
            &[
                ("modrinth.index.json", INDEX.as_bytes(), Deflated),
                ("overrides/config/ok.toml", b"ok = true\n", Deflated),
                ("overrides/mods/filler.jar", &vec![0u8; 17 * MIB], Deflated),
            ],
        );
        assert!(std::fs::metadata(&archive).unwrap().len() < MIB as u64, "not a bomb");

        let (paths, mut inst) = unpacking(&dir);
        let err = import_into(&reqwest::Client::new(), &paths, &mut inst, &archive, None, &Progress::silent("t"))
            .await
            .expect_err("the bomb must be refused");
        assert!(err.to_string().contains("fill the disk"), "{err}");

        let game = paths.game_dir(&inst.id);
        assert!(game.join("config/ok.toml").is_file(), "what came before the bomb was lost");
        assert!(!game.join("mods/filler.jar").exists(), "the half-written file was left behind");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other side of the same budget: a pack that is large because its
    /// files are large, not because they inflate, is never refused for it.
    #[tokio::test]
    async fn a_large_pack_that_does_not_inflate_still_unpacks() {
        use zip::CompressionMethod::{Deflated, Stored};
        let dir = scratch("large");
        let archive = dir.join("large.mrpack");
        write_zip(
            &archive,
            &[
                ("modrinth.index.json", INDEX.as_bytes(), Deflated),
                ("overrides/mods/large.jar", &vec![0u8; 17 * MIB], Stored),
            ],
        );

        let (paths, mut inst) = unpacking(&dir);
        import_into(&reqwest::Client::new(), &paths, &mut inst, &archive, None, &Progress::silent("t"))
            .await
            .unwrap();
        let written = std::fs::metadata(paths.game_dir(&inst.id).join("mods/large.jar")).unwrap();
        assert_eq!(written.len(), 17 * MIB as u64);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A zip of `files` empty entries with `name_len`-byte names, written by
    /// hand because a writer would take its time over tens of thousands.
    fn many_files_zip(dest: &Path, files: u16, name_len: usize) {
        let mut locals = Vec::new();
        let mut directory = Vec::new();
        for n in 0..files {
            let name = format!(
                "overrides/{n:0>width$}",
                width = name_len - "overrides/".len()
            );
            let offset = locals.len() as u32;
            // Local header: signature, version, flags, method, time, date, CRC,
            // both sizes, name and extra lengths.
            locals.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            locals.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            locals.extend_from_slice(&[0; 12]);
            locals.extend_from_slice(&(name.len() as u16).to_le_bytes());
            locals.extend_from_slice(&[0; 2]);
            locals.extend_from_slice(name.as_bytes());
            // Central header: the same, plus comment length, disk, attributes
            // and where the local header is.
            directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            directory.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            directory.extend_from_slice(&[0; 12]);
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&[0; 12]);
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let mut zip = locals;
        let directory_start = zip.len() as u32;
        zip.extend_from_slice(&directory);
        zip.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        zip.extend_from_slice(&[0; 4]);
        zip.extend_from_slice(&files.to_le_bytes());
        zip.extend_from_slice(&files.to_le_bytes());
        zip.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        zip.extend_from_slice(&directory_start.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        std::fs::write(dest, zip).unwrap();
    }

    /// Listing its files is the first thing opening a pack does, before any
    /// limit on what an entry holds can apply — so a pack that lists too many
    /// is refused while they are being read, not after they are all in memory.
    #[tokio::test]
    async fn a_pack_listing_too_many_files_is_refused_while_they_are_read() {
        let dir = scratch("listing");

        // Over the real limit, through the first thing a join does with a pack.
        let crowded = dir.join("crowded.mrpack");
        many_files_zip(&crowded, 65_000, 270);
        let listing = zip::ZipArchive::new(std::fs::File::open(&crowded).unwrap())
            .expect("a well-formed zip, so only its size is being refused");
        assert_eq!(listing.len(), 65_000);
        drop(listing);
        let err = preview(crowded)
            .await
            .expect_err("a pack listing too many files");
        assert!(
            err.to_string()
                .contains("far more files than any real pack"),
            "{err}"
        );

        // A budget is only as good as its count: a small listing fits a small
        // budget, and does not fit a smaller one.
        let listed = dir.join("listed.mrpack");
        many_files_zip(&listed, 100, 20);
        assert_eq!(
            open_pack_within(&listed, 16 * 1024)
                .map(|zip| zip.len())
                .ok(),
            Some(100)
        );
        let err = open_pack_within(&listed, 2 * 1024).expect_err("over a smaller budget");
        assert!(
            err.to_string()
                .contains("far more files than any real pack"),
            "{err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

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

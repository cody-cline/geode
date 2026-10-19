//! Modpack import and export.
//!
//! Two formats matter in practice: Modrinth's `.mrpack` (an index of download
//! URLs plus an overrides tree) and CurseForge's zip (project/file id pairs
//! that have to be resolved through their API). Both are read and both are
//! written.
//!
//! They differ in what they are able to say, not only in how they say it, so
//! which one a sharer picks changes what their pack holds: an `.mrpack` carries
//! a URL and a pair of hashes for anything Modrinth has, a CurseForge pack
//! carries ids for anything CurseForge has, and what neither can name travels
//! as bytes. [`crate::install::identity::travel`] is the one place that
//! decides which of those happens to a file, and [`ExportReport`] is how the
//! sharer is told.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::install::identity::{travel, Ask, Carry, Catalogue, Copied, Identity, Travel};
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
///
/// A server-only jar is optional on a client rather than unsupported. It does
/// nothing there — the loader skips a mod for the other side — and players
/// choose whether to keep these, so a launcher is told it may install one, not
/// that it must not. Nothing unexpected is installed on a server either way.
fn env_for(side: Option<crate::instance::jar::Side>) -> MrEnv {
    use crate::instance::jar::Side;
    let (client, server) = match side {
        Some(Side::Client) => ("required", "unsupported"),
        Some(Side::Server) => ("optional", "required"),
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

/// The loader version other launchers expect, which for Forge is not the one
/// Geode stores.
///
/// Geode keeps Forge's version in the long Maven form it is published under —
/// `1.20.1-47.2.0` — because `meta::forge` finds a build by filtering Forge's
/// maven metadata on the `{mc_version}-` prefix, so the prefix is part of what
/// it reads back. Every pack format wants what follows it: a CurseForge
/// `modLoaders` id is `forge-47.2.0`, and Prism reads an mrpack's
/// `dependencies["forge"]` straight into its own Forge version after a plain
/// string removal, so the long form names a version it cannot resolve.
/// NeoForge, Fabric and Quilt are stored bare already and pass through.
///
/// Nothing here hardcodes a version: the prefix is built from the instance's
/// own `mc_version`, and a version that does not carry it is left alone.
fn interop_loader_version<'a>(
    loader: Loader,
    loader_version: &'a str,
    mc_version: &str,
) -> &'a str {
    match loader {
        Loader::Forge => loader_version
            .strip_prefix(&format!("{mc_version}-"))
            .unwrap_or(loader_version),
        _ => loader_version,
    }
}

/// The id a CurseForge manifest names a loader by, which
/// [`parse_cf_loader`] reads back.
///
/// `None` for vanilla, whose `modLoaders` array is written empty — what Prism's
/// own exporter does for an instance with no loader, and what its reader
/// expects.
fn cf_loader_id(loader: Loader, loader_version: &str, mc_version: &str) -> Option<String> {
    (loader != Loader::Vanilla).then(|| {
        format!(
            "{}-{}",
            loader.as_str(),
            interop_loader_version(loader, loader_version, mc_version)
        )
    })
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
/// for real. Counting what is read holds however the zip reader settles on a
/// directory — and it will try every end-of-directory record in the file
/// before giving up. What it does not cover is the count of files the zip
/// declares: the reader sizes its list from that count before reading any
/// entry, and the seeks to find the count spend almost none of the budget.
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
                progress,
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
            extract_overrides(
                &archive_owned,
                &game_dir,
                &[manifest.overrides.as_str()],
                progress,
            )
            .await?;
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
                    origin: Default::default(),
                    // Pack manifests carry no dependency graph; the jars do.
                    requires: None,
                    side: None,
                    server_off: false,
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
///
/// The ceiling is what the ratio cannot give: a join accepts a pack of up to
/// 2 GiB (`MAX_PACK_BYTES`, share/join.rs), which the ratio alone would let
/// unpack to sixty-odd, so it is set at four times that and far past what any
/// real pack unpacks to. A local pack that genuinely goes past it is refused
/// with the message below, which names the limit it hit.
fn overrides_budget(archive_bytes: u64) -> u64 {
    const RATIO: u64 = 32;
    const FLOOR: u64 = 16 * 1024 * 1024;
    const CEILING: u64 = 8 * 1024 * 1024 * 1024;
    archive_bytes.saturating_mul(RATIO).clamp(FLOOR, CEILING)
}

/// Copy the pack's override trees into the game directory. Later roots win, so
/// `client-overrides` correctly shadows `overrides`.
///
/// Stops at the next entry when the run is cancelled. If it does not finish —
/// cancelled, or refused for unpacking past its budget — it unlinks the files
/// it brought into being and only those, so a file the player already had
/// keeps the pack's copy rather than being taken away from them.
async fn extract_overrides(
    archive: &Path,
    game_dir: &Path,
    roots: &[&str],
    progress: &Progress,
) -> Result<()> {
    let archive = archive.to_path_buf();
    let game_dir = game_dir.to_path_buf();
    let roots: Vec<String> = roots.iter().map(|s| s.to_string()).collect();
    let progress = progress.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let archive_bytes = std::fs::metadata(&archive)?.len();
        let budget = overrides_budget(archive_bytes);
        let mut unpacked: u64 = 0;
        let mut zip = open_pack(&archive)?;
        // What this run brought into being, to take back if it does not finish.
        // A file that was already there and was overwritten stays overwritten:
        // the pack's copy is the one that was asked for, and an update extracts
        // over a game directory full of the player's own files.
        let mut created: Vec<PathBuf> = Vec::new();

        let mut unpack = || -> Result<()> {
            for i in 0..zip.len() {
                progress.check_cancelled()?;
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
                let fresh = !dest.exists();
                let mut out = std::fs::File::create(&dest)?;
                if fresh {
                    created.push(dest.clone());
                }
                // Counted on the bytes as they inflate. The entry's header states a
                // size too, and a crafted zip can understate it.
                let left = budget - unpacked;
                let copied = std::io::copy(&mut (&mut entry).take(left + 1), &mut out)?;
                if copied > left {
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
        };

        let outcome = unpack();
        if outcome.is_err() {
            for dest in &created {
                let _ = std::fs::remove_file(dest);
            }
        }
        outcome
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
            origin: Default::default(),
            requires: None,
            side: None,
            server_off: false,
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

/// The half of a pack only Geode writes, and what having it lets the rest say.
///
/// One argument rather than three loose ones, because the three are the same
/// fact: a caller cannot write a `geode/manifest.json` and then tell the writer
/// the pack has nowhere to record a reference. The default is a plain
/// `.mrpack` — no extras, nothing referenced, and nothing it can say beyond an
/// index.
#[derive(Default)]
pub struct GeodeHalf {
    pub extras: Extras,
    /// Files the manifest names a source for, so their bytes stay out of the
    /// zip. Keyed the way content records are, on the enabled file name, so a
    /// disabled file matches too.
    pub referenced: std::collections::HashSet<PathBuf>,
    pub carry: Carry,
}

/// What an export did with the content it found, so the UI can say it rather
/// than promise something.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    /// Files the recipient fetches themselves, from the index or by id.
    pub referenced: usize,
    /// Files whose bytes travelled because nothing could name them.
    pub embedded_unknown: usize,
    /// Files whose bytes travelled although a platform does name them,
    /// because this format has no channel that can carry a reference to it.
    /// Counted apart because the remedy is a different format rather than a
    /// mod nobody can find.
    pub embedded_elsewhere: usize,
    /// Files whose bytes travelled only because this format has nowhere to say
    /// a mod is turned off.
    pub embedded_turned_off: usize,
    /// Files whose bytes travelled because the only platform that names them
    /// is one whose author turned third-party downloads off. Counted apart
    /// because no other format would have linked them either.
    pub embedded_blocked: usize,
    /// Files whose bytes travelled because the pack already names the same
    /// file by the same ids — a second copy of a mod under another name.
    /// Counted apart because nothing was lost and no other format would have
    /// done better: the mod is named, and the spare copy rides along in
    /// `overrides/` so the recipient ends up with both.
    pub embedded_duplicate: usize,
    /// Files that could not go into the pack at all, by the nearest readable
    /// form of their name.
    pub unnameable: Vec<String>,
    /// What could not be asked, in words the user can read.
    pub trouble: Vec<String>,
    /// Who the pack says wrote it, for a format that requires an author.
    /// `None` for one that does not ask — there is nothing to tell the sharer
    /// then. Reported because this is the one thing in a pack that is about
    /// the sharer rather than the instance, and a name goes out with the file.
    pub author: Option<String>,
}

/// Write the instance out as a plain `.mrpack`.
///
/// Every file Modrinth can name by hash becomes an index entry — a URL and a
/// pair of hashes, not a copy — whatever the instance's own records happen to
/// say. Everything else goes into `overrides/` as bytes, and the report says
/// how many of each, because a plain `.mrpack` has no second channel to
/// explain itself with.
///
/// It would be possible to put a CurseForge download URL straight into the
/// index: the four-host allowlist in Modrinth's format is enforced by their
/// *upload* validator, not by readers. We do not, for two reasons. A URL baked
/// into a pack keeps working after an author turns third-party downloads off,
/// while a reference resolved at import time sees that decision — the
/// recipient's answer should be today's, not the one that was true when
/// somebody else exported. And Prism no longer takes any URL it is given:
/// since 11.1.0 a pack imported from a file goes through an untrusted-mods
/// dialog listing every index URL that is not https on `cdn.modrinth.com`,
/// whose OK unlocks after three seconds and whose decline abandons the whole
/// import. Embedded jars are listed there too, which is a reason to reference
/// rather than carry — never a reason to reference from the wrong host.
pub async fn export_mrpack(
    ask: Ask<'_>,
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    include_saves: bool,
    progress: &Progress,
) -> Result<ExportReport> {
    let catalogue = crate::install::identity::for_instance(ask, paths, instance, progress).await?;
    write_instance_pack(
        paths,
        instance,
        dest,
        include_saves,
        GeodeHalf::default(),
        &catalogue,
        progress,
    )
    .await
}

/// The same writer, for a caller that has already built the catalogue and has
/// a Geode half to go with it.
///
/// A `.geodepack` needs the catalogue before the pack is written — its
/// manifest must not name a source for a file the index is carrying — so it
/// builds one and hands it here.
pub async fn write_instance_pack(
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    include_saves: bool,
    half: GeodeHalf,
    catalogue: &Catalogue,
    progress: &Progress,
) -> Result<ExportReport> {
    write_mrpack(
        instance_input(paths, instance, include_saves),
        dest,
        half,
        catalogue,
        progress,
    )
    .await
}

/// What an instance's own pack carries, and what it leaves behind.
///
/// Shared by every format an instance exports as, so what a pack of this
/// instance holds is one decision rather than one per writer.
fn instance_input<'a>(paths: &Paths, instance: &'a Instance, include_saves: bool) -> PackInput<'a> {
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

    PackInput {
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
        walk: Walk::Except(skip_dirs),
    }
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
    pub walk: Walk,
}

/// Where a file on a server's disk goes in the pack its players get.
///
/// A server keeps client mods in `client-mods/`, which no loader reads, and a
/// player's game has no such folder: there they are ordinary mods. So
/// `client-mods/x.jar` is written as `mods/x.jar`, and everything else keeps
/// its path.
fn pack_rel(rel: &Path) -> PathBuf {
    let mut parts = rel.components();
    match parts.next() {
        Some(first) if first.as_os_str() == ContentKind::ClientMod.folder() => {
            Path::new(ContentKind::Mod.folder()).join(parts.as_path())
        }
        _ => rel.to_path_buf(),
    }
}

/// The one pack writer. See [`export_mrpack`] for what goes where.
///
/// A pack walked by allowlist is a server's pack for its players, and only
/// that pack moves client mods into `mods/`. They are indexed there as client
/// only, or carried under `client-overrides/`, which a launcher installing the
/// pack on a server leaves out. An instance's pack writes every path as it is.
pub async fn write_mrpack(
    input: PackInput<'_>,
    dest: PathBuf,
    half: GeodeHalf,
    catalogue: &Catalogue,
    progress: &Progress,
) -> Result<ExportReport> {
    let game_dir = input.root;
    let players_pack = matches!(input.walk, Walk::Only { .. });
    let mut report = ExportReport {
        trouble: catalogue.trouble.clone(),
        ..Default::default()
    };

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
        // The interop form, not the stored one: Prism reads this value straight
        // into its own loader version, and Geode's own importer takes either —
        // `installer_url` puts the Minecraft version back when it is absent.
        index.dependencies.insert(
            key.into(),
            interop_loader_version(input.loader, input.loader_version, input.mc_version)
                .to_string(),
        );
    }

    // What each file does is the catalogue's answer, not the records': a jar
    // Modrinth can name by hash is indexed whatever Geode happens to have
    // written down about it, and a record Modrinth has disowned is not.
    //
    // Sorted by where the file sits, so exporting the same instance twice is
    // the same bytes twice. The catalogue is a map, and the record order that
    // used to decide this no longer comes into it.
    let mut identities: Vec<&Identity> = catalogue.identities().collect();
    identities.sort_by(|a, b| a.disk_rel.cmp(&b.disk_rel));

    let mut indexed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for ident in identities {
        let reference = match travel(ident, half.carry) {
            Travel::Linked(reference) => reference,
            // Named in the Geode half, which the caller wrote and whose paths
            // reach the walk through `half.referenced`.
            Travel::Named(_) => {
                report.referenced += 1;
                continue;
            }
            // Counted by reason. "Nothing has this mod" and "this format
            // cannot link to where it came from" are different things to be
            // told, and only the first of them is nobody's fault.
            Travel::Copied(why) => {
                match why {
                    Copied::Unknown => report.embedded_unknown += 1,
                    Copied::Elsewhere => report.embedded_elsewhere += 1,
                    Copied::TurnedOff => report.embedded_turned_off += 1,
                    Copied::Blocked => report.embedded_blocked += 1,
                }
                continue;
            }
        };

        // Where it belongs — the folder, then what the jar itself says, then
        // the listing — so a pack installed on a server does not drag a
        // renderer along with it.
        let probe = game_dir.join(&ident.disk_rel);
        let jar_side = tokio::task::spawn_blocking(move || crate::instance::jar::read(&probe))
            .await
            .ok()
            .flatten()
            .and_then(|meta| meta.side);
        let side = crate::instance::content::side_of(ident.kind, jar_side, ident.listed_side);

        // A player's game has no `client-mods/`: there they are ordinary mods.
        let folder = if players_pack && ident.kind == ContentKind::ClientMod {
            ContentKind::Mod.folder()
        } else {
            ident.kind.folder()
        };
        index.files.push(MrFile {
            // Built from the folder and the enabled name rather than converted
            // from a path: Modrinth types this field as UTF-8, and a lossy
            // conversion names a file no reader will ever write. A name that is
            // not UTF-8 never becomes a candidate, and the walk below is the
            // one place that reports it.
            //
            // The enabled name, even for a file that is turned off: a
            // reference arrives fresh from the platform, and the Geode half's
            // note is what switches it back off. A pack that cannot say that
            // carries the bytes instead — see `Carry`.
            path: format!("{folder}/{}", ident.file_name),
            // Both hashes, of the local bytes, because the readers disagree
            // about which is optional: Modrinth refuses an upload without
            // sha512, and Prism reads it with `requireString` and no fallback,
            // so a pack carrying only sha1 is one no other launcher will open.
            hashes: [
                ("sha1".to_string(), ident.sha1.clone()),
                ("sha512".to_string(), ident.sha512.clone()),
            ]
            .into_iter()
            .collect(),
            // Always both sides, never omitted. Prism reads the client value
            // with a default of "unsupported", so a file that leaves it out
            // is a file Prism silently declines to install.
            env: Some(env_for(Some(side))),
            // The matched file's own URL when a lookup gave one. Without a
            // lookup there is only the record's ids and the name on disk, so
            // the URL is built from them — which is what every Geode pack
            // carried before the matcher existed, and what an offline export
            // still has to fall back on.
            downloads: vec![reference.url.clone().unwrap_or_else(|| {
                format!(
                    "https://cdn.modrinth.com/data/{}/versions/{}/{}",
                    reference.project_id,
                    reference.version_id,
                    urlencoding::encode(&ident.file_name)
                )
            })],
            file_size: Some(ident.size),
        });
        indexed.insert(ident.disk_rel.clone());
        report.referenced += 1;
    }

    progress.stage("Writing pack", 1).await;

    let walk = input.walk;
    let GeodeHalf {
        extras, referenced, ..
    } = half;
    let mut named: Extras = vec![(
        "modrinth.index.json".to_string(),
        serde_json::to_vec_pretty(&index)?,
    )];
    named.extend(extras);

    report.unnameable = tokio::task::spawn_blocking(move || {
        lay_out(
            &dest,
            &game_dir,
            &walk,
            Layout {
                named,
                carried: indexed,
                referenced,
                players_pack,
            },
        )
    })
    .await
    .map_err(|e| Error::msg(format!("writing pack panicked: {e}")))??;

    progress.finish().await;
    Ok(report)
}

/// Everything a pack's zip holds besides the files its walk carries as bytes.
struct Layout {
    /// Written first, in order: the format's own index or manifest, then a
    /// `.geodepack`'s `geode/` directory.
    named: Extras,
    /// Paths on disk whose bytes the pack already carries by name, so they
    /// stay out of `overrides/`.
    carried: std::collections::HashSet<PathBuf>,
    /// The same, for what a Geode half's notes name: keyed on the enabled file
    /// name the way records are, so a disabled file matches too.
    referenced: std::collections::HashSet<PathBuf>,
    /// A server's pack for its players, where `client-mods/` becomes `mods/`
    /// and lands under `client-overrides/`.
    players_pack: bool,
}

/// Lay a pack out: what it names first, then every file the walk carries that
/// nothing has named already. Returns the paths that could not travel.
///
/// The one place that knows how a pack's zip is put together, so the formats
/// cannot come to disagree about where an override goes or which files are left
/// out of one. Blocking, because `zip` is.
fn lay_out(dest: &Path, game_dir: &Path, walk: &Walk, layout: Layout) -> Result<Vec<String>> {
    let Layout {
        named,
        carried,
        referenced,
        players_pack,
    } = layout;

    let file = std::fs::File::create(dest)?;
    let mut zip = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for (name, bytes) in &named {
        zip.start_file(name.clone(), options)?;
        zip.write_all(bytes)?;
    }

    let mut unnameable = Vec::new();
    let mut stack = vec![game_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(game_dir) else {
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
            let shown = if players_pack {
                pack_rel(rel)
            } else {
                rel.to_path_buf()
            };
            if carried.contains(rel) || carried_by_reference(&referenced, &shown) {
                continue;
            }
            let root = if players_pack && rel.starts_with(ContentKind::ClientMod.folder()) {
                "client-overrides"
            } else {
                "overrides"
            };
            // A zip entry name is text. A path that is not UTF-8 would go in as
            // U+FFFD and name a file nobody can match back, so it is left out
            // and reported instead — the sharer is told what did not travel
            // rather than the recipient finding a mangled name.
            let Some(shown) = shown.to_str() else {
                unnameable.push(rel.to_string_lossy().to_string());
                continue;
            };
            zip.start_file(format!("{root}/{}", shown.replace('\\', "/")), options)?;
            let mut source = std::fs::File::open(&path)?;
            std::io::copy(&mut source, &mut zip)?;
        }
    }

    zip.finish()?;
    Ok(unnameable)
}

// ---------------------------------------------------------------------------
// Export: a CurseForge pack
// ---------------------------------------------------------------------------

/// The manifest a CurseForge pack is written from.
///
/// Its own type rather than [`CfManifest`] with `Serialize` on it, because the
/// two are not the same shape: `manifestType` and `manifestVersion` are a hard
/// reject in every other launcher's reader and two fields Geode's own reader
/// does not look at, so a pack written from the reader's struct would round
/// trip through Geode and be refused everywhere else. They are literals here
/// for the same reason — nothing should be able to pass the wrong one.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CfManifestOut {
    manifest_type: &'static str,
    manifest_version: u32,
    name: String,
    version: String,
    /// Required by the format, and never empty: a reader's own default is for
    /// a *missing* author (Prism: `manifest["author"].toString("Anonymous")`,
    /// whose default fires only for a value that is not a string), so an empty
    /// one would show as a blank. [`write_cfpack`] writes the stand-in itself.
    author: String,
    overrides: &'static str,
    minecraft: CfMinecraftOut,
    files: Vec<CfFileOut>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CfMinecraftOut {
    version: String,
    mod_loaders: Vec<CfModLoaderOut>,
}

#[derive(Debug, Serialize)]
struct CfModLoaderOut {
    id: String,
    primary: bool,
}

/// One file the recipient fetches by id.
///
/// The ids are capitalised `projectID` and `fileID` in the format, which
/// camelCase gets wrong, so both are renamed by hand — as the reader's
/// [`CfManifestFile`] does, for the same reason.
#[derive(Debug, Serialize)]
struct CfFileOut {
    #[serde(rename = "projectID")]
    project_id: u32,
    #[serde(rename = "fileID")]
    file_id: u32,
    required: bool,
}

/// Write the instance out as a CurseForge pack, which the CurseForge app and
/// Prism both open.
///
/// Every mod CurseForge can name travels as a `{projectID, fileID}` pair the
/// recipient resolves with their own key, and everything else goes into
/// `overrides/` as bytes.
///
/// What this format cannot carry, which is what the report is for:
///
/// - A Modrinth mod CurseForge does not have. There is no slot for a URL and
///   no slot for a hash, so the jar travels.
/// - Anything but a mod, as a reference. The manifest names ids and no path,
///   so the recipient picks a folder by sniffing the downloaded bytes and
///   leaves what it cannot identify in `mods/` — which is where a shader pack
///   would land, doing nothing. So a resource pack, shader or data pack
///   travels as bytes under the path the pack does state.
/// - A per-side `env`. A CurseForge pack is a client's pack, and nothing in it
///   says a mod is the server's.
pub async fn export_cfpack(
    ask: Ask<'_>,
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    author: &str,
    include_saves: bool,
    progress: &Progress,
) -> Result<ExportReport> {
    let catalogue = crate::install::identity::for_instance(ask, paths, instance, progress).await?;
    write_cfpack(
        instance_input(paths, instance, include_saves),
        author,
        dest,
        &catalogue,
        progress,
    )
    .await
}

/// The same writer, from a catalogue already built — which is the seam a
/// preview uses, and what the tests write packs through.
async fn write_cfpack(
    input: PackInput<'_>,
    author: &str,
    dest: PathBuf,
    catalogue: &Catalogue,
    progress: &Progress,
) -> Result<ExportReport> {
    let game_dir = input.root;
    // The format requires an author and an instance has nowhere to keep one,
    // so an export can arrive here with nothing. "Anonymous" is what a reader
    // shows for an author it cannot find, and writing it is what makes that
    // true: an empty string is a string, so a reader's own default never fires
    // and the pack would credit a blank.
    let author = match author.trim() {
        "" => "Anonymous",
        named => named,
    };
    let mut report = ExportReport {
        trouble: catalogue.trouble.clone(),
        author: Some(author.to_string()),
        ..Default::default()
    };

    let mut manifest = CfManifestOut {
        manifest_type: "minecraftModpack",
        manifest_version: 1,
        name: input.name.to_string(),
        version: input.version,
        author: author.to_string(),
        overrides: "overrides",
        minecraft: CfMinecraftOut {
            version: input.mc_version.to_string(),
            mod_loaders: cf_loader_id(input.loader, input.loader_version, input.mc_version)
                .map(|id| CfModLoaderOut { id, primary: true })
                .into_iter()
                .collect(),
        },
        files: Vec::new(),
    };

    // Sorted by where the file sits, so exporting the same instance twice is
    // the same bytes twice — the same reason the mrpack writer sorts.
    let mut identities: Vec<&Identity> = catalogue.identities().collect();
    identities.sort_by(|a, b| a.disk_rel.cmp(&b.disk_rel));

    let mut carried: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut credits: Vec<(u32, String)> = Vec::new();
    for ident in identities {
        let reference = match travel(ident, Carry::CFPACK) {
            Travel::Named(reference) => reference,
            // `Carry::CFPACK` has no index for anything to be linked from, so
            // this does not arise. Were that ever to change, bytes are the
            // safe answer rather than a panic in the middle of an export.
            Travel::Linked(_) => {
                report.embedded_elsewhere += 1;
                continue;
            }
            // Counted by reason, as the mrpack writer counts them: the sharer
            // can act on "this format cannot link to where it came from" and
            // nobody can act on "nothing has this mod".
            Travel::Copied(why) => {
                match why {
                    Copied::Unknown => report.embedded_unknown += 1,
                    Copied::Elsewhere => report.embedded_elsewhere += 1,
                    Copied::TurnedOff => report.embedded_turned_off += 1,
                    Copied::Blocked => report.embedded_blocked += 1,
                }
                continue;
            }
        };

        // The format names a file by two integers, and every reader types them
        // that way. A reference whose ids are not numbers is no reference
        // here, so the bytes travel instead of a pair nothing can read.
        let (Ok(project_id), Ok(file_id)) = (
            reference.project_id.parse::<u32>(),
            reference.version_id.parse::<u32>(),
        ) else {
            report.embedded_unknown += 1;
            continue;
        };

        // Prism reads `files[]` into a map keyed on the file id
        // (PackManifest.cpp), so a second entry with the same id silently
        // replaces the first. Geode meets this for real, since one jar copied
        // under a second name has the same fingerprint and so the same ids.
        // The duplicate is left out of the manifest and its bytes travel, so
        // the recipient still ends up with both files.
        //
        // Its own count, and not the format's limit: this pack just linked
        // that mod, so "it cannot link to where this came from" is the one
        // thing the sharer must not be told about it.
        if manifest.files.iter().any(|f| f.file_id == file_id) {
            report.embedded_duplicate += 1;
            continue;
        }

        manifest.files.push(CfFileOut {
            project_id,
            file_id,
            // The nearest this format comes to saying a mod is turned off, and
            // what its readers turn back into a `.disabled` file.
            required: !ident.disabled,
        });
        credits.push((project_id, ident.file_name.clone()));
        carried.insert(ident.disk_rel.clone());
        report.referenced += 1;
    }

    progress.stage("Writing pack", 1).await;

    let walk = input.walk;
    let named: Extras = vec![
        (
            "manifest.json".to_string(),
            serde_json::to_vec_pretty(&manifest)?,
        ),
        ("modlist.html".to_string(), modlist_html(&credits)),
    ];
    report.unnameable = tokio::task::spawn_blocking(move || {
        lay_out(
            &dest,
            &game_dir,
            &walk,
            Layout {
                named,
                carried,
                // Only a `.geodepack` has notes for a file to be named in.
                referenced: std::collections::HashSet::new(),
                // Never a server's pack for its players: what a share serves
                // is the mrpack.
                players_pack: false,
            },
        )
    })
    .await
    .map_err(|e| Error::msg(format!("writing pack panicked: {e}")))??;

    progress.finish().await;
    Ok(report)
}

/// The credit list a CurseForge pack carries, which nothing reads.
///
/// One line per file the manifest names — the ones whose names appear nowhere
/// else in the zip, since everything carried as bytes is visible in
/// `overrides/`. The text is the file's own name and the link is the project's
/// numbered page: a fingerprint answer carries no slug and no author, and a
/// plain credit beats an invented one. Nothing depends on the link resolving.
fn modlist_html(credits: &[(u32, String)]) -> Vec<u8> {
    let mut out = String::from("<ul>\n");
    for (project_id, file_name) in credits {
        let name = file_name
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        out.push_str(&format!(
            "<li><a href=\"https://www.curseforge.com/projects/{project_id}\">{name}</a></li>\n"
        ));
    }
    out.push_str("</ul>\n");
    out.into_bytes()
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

        // Optional rather than unsupported: it does nothing on a client, and
        // whether a player keeps it is theirs to choose.
        let server = env_for(Some(Side::Server));
        assert_eq!(server.client, "optional");
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
    use crate::paths::Scratch;

    /// What an instance's pack leaves behind: the game's logs and caches, and
    /// its worlds unless asked for. Pinned because the same writer walks a
    /// server's run directory the other way round, by allowlist, and the
    /// instance's walk must not drift along with it.
    #[tokio::test]
    async fn an_instance_pack_leaves_out_the_games_own_clutter() {
        let scratch = Scratch::new("mrpack-walk");
        let paths = scratch.paths.clone();
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
                Ask::Nobody,
                &paths,
                &inst,
                dest.clone(),
                include_saves,
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

    /// Only a server's pack for its players turns `client-mods/` into mods. An
    /// instance's folder of that name is the player's own, and goes as it is.
    #[tokio::test]
    async fn an_instance_export_is_unchanged_by_client_mods() {
        let scratch = Scratch::new("mrpack-client-mods");
        let paths = scratch.paths.clone();
        let mut inst = Instance::new(
            "Own".into(),
            "1.21.1".into(),
            Loader::Fabric,
            "0.16.0".into(),
        );
        inst.id = "own".into();
        let game = paths.game_dir(&inst.id);
        tokio::fs::create_dir_all(game.join("client-mods"))
            .await
            .unwrap();
        tokio::fs::write(game.join("client-mods/kept.jar"), b"x")
            .await
            .unwrap();

        let dest = paths.root.join("own.mrpack");
        export_mrpack(
            Ask::Nobody,
            &paths,
            &inst,
            dest.clone(),
            false,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        let zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let mut names: Vec<String> = zip.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["modrinth.index.json", "overrides/client-mods/kept.jar"]
        );
    }

    /// One entry out of a written pack.
    fn read_entry(archive: &Path, name: &str) -> Vec<u8> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        let mut out = Vec::new();
        zip.by_name(name)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    /// The loader version a pack names is the one other launchers read, which
    /// for Forge is not the one Geode stores.
    ///
    /// Geode keeps the long Maven form (`1.20.1-47.2.0`) because `meta::forge`
    /// finds a build by filtering on that prefix. Prism reads this key straight
    /// into its own Forge version after a plain string removal, so a pack
    /// carrying the long form names a build it cannot resolve. Every other
    /// loader is stored bare and travels as it is.
    #[tokio::test]
    async fn a_packs_loader_version_is_the_one_other_launchers_read() {
        let scratch = Scratch::new("mrpack-deps");
        let paths = scratch.paths.clone();

        for (loader, stored, mc, key, expected) in [
            (Loader::Forge, "1.20.1-47.2.0", "1.20.1", "forge", "47.2.0"),
            // A version that does not carry the prefix is left alone, so an
            // instance imported from somebody else's pack exports the string
            // it arrived with.
            (Loader::Forge, "47.2.0", "1.20.1", "forge", "47.2.0"),
            (
                Loader::NeoForge,
                "21.1.248",
                "1.21.1",
                "neoforge",
                "21.1.248",
            ),
            (
                Loader::Fabric,
                "0.16.9",
                "1.21.1",
                "fabric-loader",
                "0.16.9",
            ),
        ] {
            let mut inst = Instance::new("Deps".into(), mc.into(), loader, stored.into());
            inst.id = format!("deps-{}-{}", loader.as_str(), stored);
            tokio::fs::create_dir_all(paths.game_dir(&inst.id))
                .await
                .unwrap();

            let dest = paths.root.join(format!("{}.mrpack", inst.id));
            export_mrpack(
                Ask::Nobody,
                &paths,
                &inst,
                dest.clone(),
                false,
                &Progress::silent("t"),
            )
            .await
            .unwrap();

            let index: MrIndex =
                serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
            assert_eq!(
                index.dependencies.get(key).map(String::as_str),
                Some(expected),
                "{loader:?} stored as {stored}"
            );
            assert_eq!(
                index.dependencies.get("minecraft").map(String::as_str),
                Some(mc)
            );
        }
    }

    // -----------------------------------------------------------------------
    // Export: a CurseForge pack
    // -----------------------------------------------------------------------

    /// An instance on disk with each named file written, and a catalogue of
    /// what is in it. No platform is asked anything: each test says what the
    /// platforms hold through the catalogue's own stubs, which go in through
    /// the code a real answer does.
    async fn cf_fixture(tag: &str, files: &[(&str, &str)]) -> (Scratch, Instance, Catalogue) {
        let scratch = Scratch::new(&format!("cfpack-{tag}"));
        let mut inst = Instance::new(
            "Kitchen Sink".into(),
            "1.21.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        );
        inst.id = format!("cf-{tag}");
        let game = scratch.paths.game_dir(&inst.id);
        tokio::fs::create_dir_all(&game).await.unwrap();
        for (dir, name) in files {
            let holding = game.join(dir);
            tokio::fs::create_dir_all(&holding).await.unwrap();
            tokio::fs::write(holding.join(name), format!("the bytes of {name}"))
                .await
                .unwrap();
        }
        let catalogue = crate::install::identity::for_instance(
            Ask::Nobody,
            &scratch.paths,
            &inst,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        (scratch, inst, catalogue)
    }

    /// Write the fixture out as a CurseForge pack.
    async fn cfpack_from(
        paths: &Paths,
        inst: &Instance,
        dest: &Path,
        catalogue: &Catalogue,
    ) -> ExportReport {
        cfpack_credited_to(paths, inst, dest, catalogue, "Somebody").await
    }

    /// The same, for the one field a pack carries about the sharer rather than
    /// the instance.
    async fn cfpack_credited_to(
        paths: &Paths,
        inst: &Instance,
        dest: &Path,
        catalogue: &Catalogue,
        author: &str,
    ) -> ExportReport {
        write_cfpack(
            instance_input(paths, inst, false),
            author,
            dest.to_path_buf(),
            catalogue,
            &Progress::silent("t"),
        )
        .await
        .unwrap()
    }

    /// Everything inside a written pack, by entry name.
    fn entries(archive: &Path) -> Vec<String> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        let mut names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        names.sort();
        names
    }

    /// A written pack's manifest as JSON, so what it carries can be asserted
    /// with its types and not only its values.
    fn manifest_of(archive: &Path) -> serde_json::Value {
        serde_json::from_slice(&read_entry(archive, "manifest.json")).unwrap()
    }

    /// Two of these fields are a hard reject in Prism's reader and neither is
    /// one Geode's own reader looks at, so a pack written without them would
    /// round trip through Geode and open nowhere else. The ids are capitalised
    /// the way the format spells them and typed the way it types them.
    #[tokio::test]
    async fn a_curseforge_manifest_is_one_prism_would_accept() {
        let (scratch, inst, mut catalogue) = cf_fixture("accept", &[("mods", "create.jar")]).await;
        catalogue.pretend_curseforge_holds(&[("create.jar", 328085, 4593548, true)]);
        let dest = scratch.paths.root.join("accept.zip");
        cfpack_from(&scratch.paths, &inst, &dest, &catalogue).await;

        let manifest = manifest_of(&dest);
        assert_eq!(manifest["manifestType"], "minecraftModpack");
        assert_eq!(manifest["manifestVersion"], serde_json::json!(1));
        assert_eq!(manifest["overrides"], "overrides");
        assert_eq!(manifest["name"], "Kitchen Sink");
        assert_eq!(manifest["author"], "Somebody");
        assert_eq!(manifest["minecraft"]["version"], "1.21.1");
        assert_eq!(
            manifest["minecraft"]["modLoaders"],
            serde_json::json!([{ "id": "fabric-0.16.9", "primary": true }])
        );
        assert_eq!(
            manifest["files"],
            serde_json::json!([{ "projectID": 328085, "fileID": 4593548, "required": true }])
        );
    }

    /// The format requires an author and an instance has nowhere to keep one,
    /// so an export can arrive with nothing to credit. A reader's own default
    /// does not cover it: Prism reads `manifest["author"].toString("Anonymous")`
    /// and that default fires only for a value that is not a string, so an
    /// empty one is shown as a blank. The stand-in is written here instead,
    /// and the report says which name went into the pack either way, because
    /// the name is the sharer's own and it travels with the file.
    #[tokio::test]
    async fn an_author_nobody_gave_is_the_one_a_reader_shows() {
        let (scratch, inst, catalogue) = cf_fixture("author", &[("mods", "create.jar")]).await;
        for (given, written) in [("", "Anonymous"), ("   ", "Anonymous"), ("Ada", "Ada")] {
            let dest = scratch.paths.root.join(format!("author-{written}.zip"));
            let report = cfpack_credited_to(&scratch.paths, &inst, &dest, &catalogue, given).await;
            assert_eq!(manifest_of(&dest)["author"], written, "given {given:?}");
            assert_eq!(report.author.as_deref(), Some(written), "given {given:?}");
        }
    }

    /// The owner's conversion, in the direction this format needs it: a mod
    /// Geode knows from Modrinth travels as a pair of CurseForge ids, because
    /// a CurseForge manifest has no slot for a Modrinth download and the jar
    /// would otherwise be shipped.
    #[tokio::test]
    async fn a_modrinth_mod_curseforge_also_has_becomes_an_id_pair() {
        let (scratch, inst, mut catalogue) = cf_fixture("both", &[("mods", "sodium.jar")]).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar"]);
        catalogue.pretend_curseforge_holds(&[("sodium.jar", 394468, 5678, true)]);
        let dest = scratch.paths.root.join("both.zip");
        let report = cfpack_from(&scratch.paths, &inst, &dest, &catalogue).await;

        assert_eq!(
            manifest_of(&dest)["files"],
            serde_json::json!([{ "projectID": 394468, "fileID": 5678, "required": true }])
        );
        assert!(
            !entries(&dest).contains(&"overrides/mods/sodium.jar".to_string()),
            "{:?}",
            entries(&dest)
        );
        assert_eq!((report.referenced, bundled(&report)), (1, (0, 0, 0, 0, 0)));
    }

    /// And the other way: Modrinth naming a mod buys this format nothing, so
    /// the jar travels — and the sharer is told it is the format's limit
    /// rather than a mod nobody has.
    #[tokio::test]
    async fn a_mod_only_modrinth_has_travels_as_bytes() {
        let (scratch, inst, mut catalogue) = cf_fixture("mronly", &[("mods", "sodium.jar")]).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar"]);
        catalogue.pretend_curseforge_holds(&[]);
        let dest = scratch.paths.root.join("mronly.zip");
        let report = cfpack_from(&scratch.paths, &inst, &dest, &catalogue).await;

        assert_eq!(manifest_of(&dest)["files"], serde_json::json!([]));
        assert!(
            entries(&dest).contains(&"overrides/mods/sodium.jar".to_string()),
            "{:?}",
            entries(&dest)
        );
        assert_eq!((report.referenced, bundled(&report)), (0, (0, 1, 0, 0, 0)));
    }

    /// The distribution rule survives into this writer: a file whose author
    /// turned third-party downloads off is named by nothing, because a pair of
    /// ids the recipient cannot resolve is worse than the bytes.
    #[tokio::test]
    async fn an_author_blocked_curseforge_match_is_embedded_not_referenced() {
        let (scratch, inst, mut catalogue) = cf_fixture("blocked", &[("mods", "locked.jar")]).await;
        catalogue.pretend_curseforge_holds(&[("locked.jar", 111, 222, false)]);
        let dest = scratch.paths.root.join("blocked.zip");
        let report = cfpack_from(&scratch.paths, &inst, &dest, &catalogue).await;

        assert_eq!(manifest_of(&dest)["files"], serde_json::json!([]));
        assert!(
            entries(&dest).contains(&"overrides/mods/locked.jar".to_string()),
            "{:?}",
            entries(&dest)
        );
        assert_eq!((report.referenced, bundled(&report)), (0, (0, 0, 0, 1, 0)));
    }

    /// The one string the whole loader contract rests on, for both the
    /// CurseForge id and the mrpack dependency it shares a helper with — and
    /// read back as what it was written from, since Geode is a reader of this
    /// format too.
    #[test]
    fn forge_gets_its_short_version_and_neoforge_its_own() {
        for (loader, stored, mc, expected) in [
            (
                Loader::Forge,
                "1.20.1-47.2.0",
                "1.20.1",
                Some("forge-47.2.0"),
            ),
            (Loader::Forge, "47.2.0", "1.20.1", Some("forge-47.2.0")),
            (
                Loader::NeoForge,
                "21.1.248",
                "1.21.1",
                Some("neoforge-21.1.248"),
            ),
            (Loader::Fabric, "0.16.9", "1.21.1", Some("fabric-0.16.9")),
            (Loader::Quilt, "0.27.1", "1.21.1", Some("quilt-0.27.1")),
            // Vanilla writes an empty `modLoaders` array rather than an id.
            (Loader::Vanilla, "", "1.21.1", None),
        ] {
            let id = cf_loader_id(loader, stored, mc);
            assert_eq!(id.as_deref(), expected, "{loader:?} stored as {stored}");
            if let Some(id) = id {
                assert_eq!(
                    parse_cf_loader(&id),
                    (
                        loader,
                        interop_loader_version(loader, stored, mc).to_string()
                    )
                );
            }
        }
    }

    /// Prism reads `files[]` into a map keyed on the file id, so two entries
    /// with the same id silently become one. One jar copied under a second
    /// name has the same fingerprint and so the same ids, which is how Geode
    /// meets this for real. The duplicate's bytes travel instead, so nothing
    /// is lost either way.
    ///
    /// Counted as the duplicate it is and not as the format's limit: this pack
    /// has just linked that mod, so telling the sharer it "cannot link to
    /// where it came from" would be a sentence about the one file that proves
    /// it wrong.
    #[tokio::test]
    async fn a_mod_present_twice_is_named_once() {
        let (scratch, inst, mut catalogue) = cf_fixture(
            "twice",
            &[("mods", "create.jar"), ("mods", "create-copy.jar")],
        )
        .await;
        catalogue.pretend_curseforge_holds(&[
            ("create.jar", 7, 99, true),
            ("create-copy.jar", 7, 99, true),
        ]);
        let dest = scratch.paths.root.join("twice.zip");
        let report = cfpack_from(&scratch.paths, &inst, &dest, &catalogue).await;

        assert_eq!(
            manifest_of(&dest)["files"],
            serde_json::json!([{ "projectID": 7, "fileID": 99, "required": true }])
        );
        let names = entries(&dest);
        let embedded = [
            "overrides/mods/create.jar",
            "overrides/mods/create-copy.jar",
        ]
        .iter()
        .filter(|path| names.contains(&path.to_string()))
        .count();
        assert_eq!(embedded, 1, "{names:?}");
        assert_eq!((report.referenced, bundled(&report)), (1, (0, 0, 0, 0, 1)));
    }

    /// `required: false` is the nearest this format comes to saying a mod is
    /// turned off, and what its readers turn back into a `.disabled` file. So
    /// a turned-off mod costs no bytes here, unlike in an `.mrpack`.
    #[tokio::test]
    async fn a_turned_off_mod_is_named_as_one_the_recipient_may_skip() {
        let (scratch, inst, mut catalogue) =
            cf_fixture("off", &[("mods", "extra.jar.disabled")]).await;
        catalogue.pretend_curseforge_holds(&[("extra.jar", 5, 55, true)]);
        let dest = scratch.paths.root.join("off.zip");
        let report = cfpack_from(&scratch.paths, &inst, &dest, &catalogue).await;

        assert_eq!(
            manifest_of(&dest)["files"],
            serde_json::json!([{ "projectID": 5, "fileID": 55, "required": false }])
        );
        assert!(
            !entries(&dest)
                .iter()
                .any(|name| name.starts_with("overrides/mods/")),
            "{:?}",
            entries(&dest)
        );
        assert_eq!((report.referenced, bundled(&report)), (1, (0, 0, 0, 0, 0)));
    }

    /// Named or carried, never both and never neither — the same assertion the
    /// mrpack writer has, per file. A resource pack is here because this
    /// format names no path: a reference would land wherever the recipient's
    /// sniffing put it, so its bytes travel even though CurseForge names it.
    #[tokio::test]
    async fn a_referenced_file_is_never_also_embedded() {
        let (scratch, inst, mut catalogue) = cf_fixture(
            "xor",
            &[
                ("mods", "named.jar"),
                ("mods", "unknown.jar"),
                ("resourcepacks", "pretty.zip"),
                ("config", "thing.toml"),
            ],
        )
        .await;
        catalogue
            .pretend_curseforge_holds(&[("named.jar", 1, 11, true), ("pretty.zip", 2, 22, true)]);
        let dest = scratch.paths.root.join("xor.zip");
        let report = cfpack_from(&scratch.paths, &inst, &dest, &catalogue).await;

        let names = entries(&dest);
        let manifest = manifest_of(&dest);
        let ids: Vec<u64> = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["fileID"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![11], "only the mod can be named: {manifest}");

        for (path, file_id) in [
            ("mods/named.jar", Some(11)),
            ("mods/unknown.jar", None),
            ("resourcepacks/pretty.zip", None),
            ("config/thing.toml", None),
        ] {
            let embedded = names.contains(&format!("overrides/{path}"));
            let named = file_id.is_some_and(|id| ids.contains(&id));
            assert!(
                embedded != named,
                "{path}: embedded={embedded} named={named}"
            );
        }
        assert_eq!((report.referenced, bundled(&report)), (1, (1, 1, 0, 0, 0)));
    }

    /// The two halves agree: a pack this writer wrote is one Geode's own
    /// reader understands, down to the loader version the id carries.
    #[tokio::test]
    async fn geode_reads_back_its_own_curseforge_pack() {
        let (scratch, mut inst, catalogue) = cf_fixture("roundtrip", &[]).await;
        inst.mc_version = "1.20.1".into();
        inst.loader = Loader::Forge;
        inst.loader_version = "1.20.1-47.2.0".into();

        let dest = scratch.paths.root.join("roundtrip.zip");
        cfpack_from(&scratch.paths, &inst, &dest, &catalogue).await;

        let preview = preview(dest).await.unwrap();
        assert_eq!(preview.format, "curseforge");
        assert_eq!(preview.name, "Kitchen Sink");
        assert_eq!(preview.mc_version, "1.20.1");
        assert_eq!(preview.loader, Loader::Forge);
        assert_eq!(preview.loader_version, "47.2.0");
        assert_eq!(preview.file_count, 0);
    }

    /// What the report says travelled as bytes, by reason: nothing named it,
    /// something did but this format cannot carry a reference to it, it was
    /// turned off, and its author turned third-party downloads off.
    fn bundled(report: &ExportReport) -> (usize, usize, usize, usize, usize) {
        (
            report.embedded_unknown,
            report.embedded_elsewhere,
            report.embedded_turned_off,
            report.embedded_blocked,
            report.embedded_duplicate,
        )
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

    fn scratch_dir(tag: &str) -> Scratch {
        let scratch = Scratch::new(&format!("modpack-{tag}"));
        std::fs::create_dir_all(&scratch.paths.root).unwrap();
        scratch
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
        let scratch = scratch_dir("bigindex");
        let dir = scratch.paths.root.clone();

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
    }

    fn unpacking(dir: &Path) -> (Paths, Instance) {
        let paths = Paths { root: dir.to_path_buf() };
        let mut inst = Instance::new("P".into(), "1.21.1".into(), Loader::Vanilla, String::new());
        inst.id = "unpacked".into();
        (paths, inst)
    }

    /// Deflate turns a run of zeros into a thousandth of its size, so a pack
    /// that is small to download can unpack to fill a disk. It is stopped at
    /// its budget, and nothing it wrote is left behind — neither the file it
    /// was part way through nor the ones that landed before it.
    #[tokio::test]
    async fn an_override_bomb_is_stopped_before_it_fills_the_disk() {
        use zip::CompressionMethod::Deflated;
        let scratch = scratch_dir("bomb");
        let dir = scratch.paths.root.clone();
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
        assert!(!game.join("config/ok.toml").exists(), "what came before the bomb stayed");
        assert!(!game.join("mods/filler.jar").exists(), "the half-written file was left behind");
    }

    /// The other half of that clean-up: an update extracts over a game
    /// directory the player has been living in, so only what the run itself
    /// brought into being may be taken back.
    #[tokio::test]
    async fn a_refused_pack_takes_back_only_what_it_wrote() {
        use zip::CompressionMethod::Deflated;
        let scratch = scratch_dir("refused");
        let dir = scratch.paths.root.clone();
        let archive = dir.join("bomb.mrpack");
        write_zip(
            &archive,
            &[
                ("modrinth.index.json", INDEX.as_bytes(), Deflated),
                ("overrides/config/theirs.toml", b"theirs = true\n", Deflated),
                ("overrides/mods/filler.jar", &vec![0u8; 17 * MIB], Deflated),
            ],
        );

        let (paths, mut inst) = unpacking(&dir);
        let game = paths.game_dir(&inst.id);
        std::fs::create_dir_all(game.join("config")).unwrap();
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("config/mine.toml"), b"mine = true\n").unwrap();
        std::fs::write(game.join("config/theirs.toml"), b"old = true\n").unwrap();
        // The entry that trips the budget lands on a file the player already
        // had, which is the one the run must not take away.
        std::fs::write(game.join("mods/filler.jar"), b"mine\n").unwrap();

        let err = import_into(&reqwest::Client::new(), &paths, &mut inst, &archive, None, &Progress::silent("t"))
            .await
            .expect_err("the bomb must be refused");
        assert!(err.to_string().contains("fill the disk"), "{err}");

        assert_eq!(
            std::fs::read(game.join("config/mine.toml")).unwrap(),
            b"mine = true\n",
            "a file the pack never named was deleted"
        );
        // Overwritten is not created: the pack's copy of a file that was
        // already there is the one that was asked for, and it stays.
        assert_eq!(
            std::fs::read(game.join("config/theirs.toml")).unwrap(),
            b"theirs = true\n",
            "a file the pack overwrote was rolled back"
        );
        assert!(
            game.join("mods/filler.jar").exists(),
            "the file the budget stopped on was the player's, and it was deleted"
        );
    }

    /// Unpacking a joined server's pack is the longest step of an update the
    /// player waits through on their way to Play, so Cancel has to be heard
    /// inside it rather than only after it.
    #[tokio::test]
    async fn a_cancelled_extraction_stops_where_it_is() {
        use zip::CompressionMethod::Deflated;
        let scratch = scratch_dir("cancelled");
        let dir = scratch.paths.root.clone();
        let archive = dir.join("pack.mrpack");
        write_zip(
            &archive,
            &[
                ("modrinth.index.json", INDEX.as_bytes(), Deflated),
                ("overrides/config/ok.toml", b"ok = true\n", Deflated),
            ],
        );

        let (paths, mut inst) = unpacking(&dir);
        let progress = Progress::silent("t");
        progress
            .cancel_flag()
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let err = import_into(&reqwest::Client::new(), &paths, &mut inst, &archive, None, &progress)
            .await
            .expect_err("a cancelled import");
        assert!(matches!(err, Error::Cancelled), "{err}");
        assert!(
            !paths.game_dir(&inst.id).join("config/ok.toml").exists(),
            "the cancelled run wrote anyway"
        );
    }

    /// The ratio is measured against the pack, so a pack the size of the
    /// largest a join accepts would be allowed sixty-odd gigabytes by the ratio
    /// alone. The ceiling is what says no to that, and the floor still lets a
    /// tiny pack of configs through.
    #[test]
    fn what_a_pack_may_unpack_to_has_a_floor_and_a_ceiling() {
        assert_eq!(overrides_budget(0), 16 * MIB as u64);
        assert_eq!(overrides_budget(4 * MIB as u64), 128 * MIB as u64);
        // `MAX_PACK_BYTES`, share/join.rs.
        let largest_join = 2 * 1024 * 1024 * 1024;
        assert_eq!(overrides_budget(largest_join), 8 * 1024 * 1024 * 1024);
    }

    /// The other side of the same budget: a pack that is large because its
    /// files are large, not because they inflate, is never refused for it.
    #[tokio::test]
    async fn a_large_pack_that_does_not_inflate_still_unpacks() {
        use zip::CompressionMethod::{Deflated, Stored};
        let scratch = scratch_dir("large");
        let dir = scratch.paths.root.clone();
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
        let scratch = scratch_dir("listing");
        let dir = scratch.paths.root.clone();

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

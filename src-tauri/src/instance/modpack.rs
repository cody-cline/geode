//! Modpack import and export.
//!
//! Two formats matter in practice: Modrinth's `.mrpack` (an index of download
//! URLs plus an overrides tree) and CurseForge's zip (project/file id pairs
//! that have to be resolved through their API). Both are read and both are
//! written.
//!
//! A third thing can be read and not written here: a Prism / MultiMC
//! instance, which is not a pack but a whole instance, and which arrives as a
//! zip or as the folder itself. [`super::prism`] knows that format; this
//! module only recognises it and brings its files in.
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

use crate::error::{Context, Error, Result};
use crate::install::identity::{
    travel, Ask, Carry, Catalogue, Copied, Identity, PlatformRef, Travel,
};
use crate::meta::Loader;
use crate::net::{Download, Progress};
use crate::paths::Paths;

use super::content::{ContentKind, ContentRecord};
use super::export::Selection;
use super::{prism, Instance};

// ---------------------------------------------------------------------------
// modrinth.index.json
// ---------------------------------------------------------------------------

/// The one revision of Modrinth's pack format there is, written and read.
const MR_FORMAT: u32 = 1;

/// The one game it describes. Modrinth's own validator refuses anything else
/// outright ("Game {0} does not exist!").
const MR_GAME: &str = "minecraft";

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
    /// Where the recipient fetches this file from — several hosts allowed, and
    /// a mirror listed second is there for when the first will not answer.
    ///
    /// Defaulted although the format does not default it, because the format's
    /// other readers do not either: one entry missing the key makes an index
    /// unparsable, and refusing a whole pack of two hundred mods over one file
    /// nobody can fetch is worse than importing the rest and naming that one.
    #[serde(default)]
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

/// Which side a pack's own `env` pair says a file belongs on.
///
/// The inverse of [`env_for`], and read on import for the same reason it is
/// written on export: a pack that says `{client: required, server:
/// unsupported}` has told us this is a client mod, and throwing that away
/// leaves every file an import installs classified by nobody.
///
/// `None` where the pair does not settle it — no `env` object, or one that
/// states only half of itself. A guess here is worse than nothing: it ends up
/// keeping a mod off a server, or sending a client's rendering mod to one.
fn side_from_env(env: &MrEnv) -> Option<crate::instance::jar::Side> {
    use crate::instance::jar::Side;
    match (env.client.as_str(), env.server.as_str()) {
        (_, "unsupported") => Some(Side::Client),
        ("unsupported", _) => Some(Side::Server),
        ("optional", "required") => Some(Side::Server),
        ("", _) | (_, "") => None,
        _ => Some(Side::Both),
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
    /// What this manifest is. Optional here only so that a manifest without it
    /// parses far enough to be refused by name — see [`check_is_modpack`].
    #[serde(default)]
    manifest_type: Option<String>,
    #[serde(default)]
    manifest_version: Option<u32>,
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

/// The two fields a CurseForge modpack declares itself with, and the values
/// [`CfManifestOut`] writes.
const MODPACK_MANIFEST: &str = "minecraftModpack";
const MODPACK_MANIFEST_VERSION: u32 = 1;

/// Refuse a `manifest.json` that is not a modpack's.
///
/// Nothing read these two fields before, so any JSON with a `minecraft` object
/// was accepted as a pack — a CurseForge export of something else got as far
/// as the preview dialog and then failed with whatever its contents happened
/// to break. Every other launcher rejects such a file outright, and so does
/// this.
fn check_is_modpack(manifest: &CfManifest) -> Result<()> {
    match manifest.manifest_type.as_deref() {
        Some(MODPACK_MANIFEST) => {}
        Some(other) => {
            return Err(Error::msg(format!(
                "this file's manifest.json says it is a {other}, not a modpack, \
                 so there is nothing here to install"
            )))
        }
        None => {
            return Err(Error::msg(
                "this file's manifest.json does not say what it is — a CurseForge \
                 modpack declares \"manifestType\": \"minecraftModpack\", and a file \
                 without it is not a pack any launcher will read",
            ))
        }
    }
    match manifest.manifest_version {
        Some(MODPACK_MANIFEST_VERSION) => Ok(()),
        Some(other) => Err(Error::msg(format!(
            "this pack's manifest.json is version {other} and Geode reads \
             version {MODPACK_MANIFEST_VERSION} — it was written by something newer than this"
        ))),
        None => Err(Error::msg(
            "this pack's manifest.json does not say which version it is, so it \
             cannot be read as a modpack",
        )),
    }
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
pub(super) fn interop_loader_version<'a>(
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

/// Where one of a CurseForge pack's files belongs inside an instance.
#[derive(Clone, Copy)]
enum CfPlacement {
    /// Content Geode tracks, in the folder its kind owns.
    Tracked(ContentKind),
    /// A world: `saves/`, where this format's other readers put one, and no
    /// content record, because a world is not content Geode tracks.
    Saves,
    /// Nothing an instance has a place for.
    Nowhere,
}

/// Which folder a pack's file goes in, read off its *project*.
///
/// A CurseForge manifest states no path — only `{projectID, fileID}` — so the
/// folder can only come from what the project is. Everything used to be
/// installed as a [`ContentKind::Mod`] into `mods/`, which is why a pack's
/// resource packs and shader packs arrived somewhere nothing reads them.
///
/// A class that says nothing useful still means `mods/`, because
/// `kind_from_class` defaults there — the same thing Prism does with a file it
/// cannot identify, and recoverable in a way that inventing a folder is not.
fn cf_placement(kind: crate::providers::ProjectKind) -> CfPlacement {
    use crate::providers::ProjectKind;
    match kind {
        ProjectKind::World => CfPlacement::Saves,
        // `install_kind` says `plugins/` for a plugin, which is an answer
        // about a *server*: no client reads that folder, `content.rs` says an
        // instance that somehow has one lists nothing, and `providers/mod.rs`
        // says a plugin is never installed into an instance at all. So class 5
        // says nothing useful about a client, which is the `mods/` case —
        // visible and recoverable, where a folder nothing reads is neither.
        // Prism has no mapping for class 5 either, and leaves one in `mods/`.
        ProjectKind::Plugin => CfPlacement::Tracked(ContentKind::Mod),
        // `install_kind` is the existing answer to "which folder", and already
        // says `None` for a pack inside a pack.
        other => match other.install_kind() {
            Some(kind) => CfPlacement::Tracked(kind),
            None => CfPlacement::Nowhere,
        },
    }
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
    /// A Prism / MultiMC instance, as a zip or as the folder itself. Not a
    /// pack at all: it carries its files rather than naming them, and what it
    /// does name it names in a packwiz index — see [`prism`].
    Prism(prism::PrismInstance),
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
    // A folder is an instance somebody pointed at — only a file can be a pack.
    if archive.is_dir() {
        return prism::from_folder(archive)?
            .map(PackKind::Prism)
            .ok_or_else(|| {
                Error::msg(
                    "that folder is not an instance — expected an instance.cfg inside it, as a \
                     Prism or MultiMC instance has",
                )
            });
    }

    let mut zip = open_pack(archive)?;

    if let Ok(entry) = zip.by_name("modrinth.index.json") {
        let raw = read_json_entry(entry, "modrinth.index.json")?;
        let index: MrIndex =
            serde_json::from_slice(&raw).map_err(|e| Error::json("modrinth.index.json", e))?;
        // Read and acted on rather than read and ignored. Both are stated in
        // every index and both are checked by Modrinth and by Prism, because a
        // format that revises itself will put the revision here: a version 2
        // pack read as if it were version 1 installs whatever the fields still
        // happen to mean, which is a wrong instance rather than a refusal.
        if index.format_version != MR_FORMAT {
            return Err(Error::msg(format!(
                "this pack is written in version {} of Modrinth's pack format, and this build of \
                 Geode reads version {MR_FORMAT} — update Geode and try again",
                index.format_version
            )));
        }
        if index.game != MR_GAME {
            return Err(Error::msg(format!(
                "this pack is for {}, not Minecraft, so it was not imported",
                index.game
            )));
        }
        return Ok(PackKind::Modrinth(index));
    }

    if let Ok(entry) = zip.by_name("manifest.json") {
        let raw = read_json_entry(entry, "manifest.json")?;
        let manifest: CfManifest =
            serde_json::from_slice(&raw).map_err(|e| Error::json("manifest.json", e))?;
        // A manifest.json at the root is a claim to be a CurseForge pack, so a
        // bad one is refused rather than left to the reader below: nothing else
        // puts that file there.
        check_is_modpack(&manifest)?;
        return Ok(PackKind::CurseForge(manifest));
    }

    // Prism's own order, so a zip that satisfies two is read the way Prism
    // would read it: the two pack manifests are matched at the exact root and
    // win, and `instance.cfg` is matched at any depth.
    if let Some(instance) = prism::from_zip(&mut zip, archive)? {
        return Ok(PackKind::Prism(instance));
    }

    Err(Error::msg(
        "not a modpack — expected modrinth.index.json, manifest.json or a Prism instance.cfg \
         inside",
    ))
}

/// A Modrinth pack's index; `None` for a CurseForge pack, whose files are
/// named by id and resolved through CurseForge, and for a Prism instance,
/// which has no index of downloads at all.
pub(crate) fn read_index(archive: &Path) -> Result<Option<MrIndex>> {
    Ok(match read_pack(archive)? {
        PackKind::Modrinth(index) => Some(index),
        PackKind::CurseForge(_) | PackKind::Prism(_) => None,
    })
}

/// The trees a `.mrpack` means for a client, in the order they land —
/// `client-overrides` shadows `overrides`. `server-overrides/` is deliberately
/// not one of them; see [`import_into_at`].
const MRPACK_CLIENT_TREES: [&str; 2] = ["overrides", "client-overrides"];

/// The trees inside a pack whose files a client import writes into the
/// instance, as prefixes of the archive's own entry names.
///
/// Read off the pack rather than assumed. A `.mrpack`'s two client trees are
/// fixed by that format, but a CurseForge manifest *names* its tree, and while
/// `overrides` is both the convention and this reader's default, the field is
/// the pack's to set and [`import_into_at`] extracts whatever it says. Anything
/// working out what a pack carries has to look where the import will write, or
/// it decides a file the pack still ships has departed.
pub(crate) fn carried_trees(archive: &Path) -> Result<Vec<String>> {
    Ok(match read_pack(archive)? {
        PackKind::Modrinth(_) => MRPACK_CLIENT_TREES.iter().map(|t| t.to_string()).collect(),
        PackKind::CurseForge(manifest) => vec![manifest.overrides],
        // Not a pack at all: an instance carries its whole game directory
        // rather than a tree to write into one, and nothing brings an instance
        // up to a newer published version of itself.
        PackKind::Prism(_) => Vec::new(),
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
            PackKind::Prism(instance) => PackPreview {
                name: instance.name,
                // An instance is not a release of anything: it has a name and
                // no version, and its packwiz index versions the files rather
                // than the whole.
                version: String::new(),
                summary: None,
                mc_version: instance.mc_version,
                loader: instance.loader,
                loader_version: instance.loader_version,
                file_count: instance.file_count,
                format: "prism".into(),
                // Nothing in an instance is fetched: every file it names it
                // also carries.
                needs_curseforge_key: false,
            },
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
            // A plan is built from a pack a provider published, and nobody
            // publishes an instance — so this says what to do instead rather
            // than reading half of a mod list.
            PackKind::Prism(_) => {
                return Err(Error::msg(
                    "that is a Prism instance, not a modpack — import it as an instance instead \
                     of building a plan from it",
                ))
            }
        })
    })
    .await
    .map_err(|e| Error::msg(format!("reading pack panicked: {e}")))?
}

/// What an import did that its caller has to pass on.
#[derive(Debug, Default)]
pub struct Imported {
    /// Files the pack named and this did not install, each line naming the
    /// file and what the user has to do with it — see [`import_into`].
    pub left_out: Vec<String>,
    /// Files the pack carried and this left as the user already had them,
    /// because they could not be written. Only ever the paths a pack syncs; a
    /// write that fails anywhere else fails the import.
    pub not_written: Vec<NotWritten>,
}

/// One file the pack carried that the user keeps their own copy of, because
/// this could not write over it.
#[derive(Debug, Clone)]
pub struct NotWritten {
    /// Where it sits inside the game directory, as the pack named it. The
    /// caller remembers these so the file is offered again rather than taken
    /// as landed — see `share::pack::update`.
    pub path: String,
    /// What to tell the user: the file, and why it could not be written.
    pub why: String,
}

/// Install a pack into an existing (already created) instance directory, with
/// every override file written whole.
///
/// Returns what the pack named and this did not install, each line naming the
/// file and what the user has to do with it. That is a list and not an error
/// on purpose: by the time any of it is known the instance exists and most of
/// the pack is in it, and an import that comes back as a failure invites a
/// retry — which makes a second instance. `commands::import_modpack` states
/// the same rule, and `PackImport.problems` is the shape it reports through.
pub async fn import_into(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    import_into_at(
        client,
        crate::providers::curseforge::API,
        crate::providers::modrinth::API,
        paths,
        instance,
        archive,
        curseforge_key,
        &Overrides::replace(),
        progress,
    )
    .await
    .map(|imported| imported.left_out)
}

/// The same, with the override files decided per path and both API bases
/// injectable.
///
/// `overrides` is how a server's pack keeps a player's own settings: it is
/// asked about every file in the pack's override trees, and `share::pack` is
/// the only place that knows what to answer. Everything else passes
/// [`Overrides::replace`], which is the behaviour this had before there was
/// anything to decide.
///
/// Injectable for the reason `curseforge::check_key_at` and
/// `curseforge::match_fingerprints_at` are: what a CurseForge import does is
/// decided entirely by what it asks and what it makes of the answer, so a test
/// serves those answers itself rather than reaching the real APIs with a key
/// no test has. A `.mrpack` fetches only the URLs its own index names, so
/// neither base reaches that arm.
///
/// Modrinth is asked one thing and only in one case: a file whose CurseForge
/// author switched third-party downloads off is looked for among that author's
/// own Modrinth releases, by the sha1 CurseForge published for it. See
/// [`crate::install::blocked`].
#[allow(clippy::too_many_arguments)] // two bases and one instance's coordinates
pub async fn import_into_at(
    client: &reqwest::Client,
    base: &str,
    modrinth_base: &str,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    curseforge_key: Option<&str>,
    overrides: &Overrides,
    progress: &Progress,
) -> Result<Imported> {
    let game_dir = paths.game_dir(&instance.id);
    let archive_owned = archive.to_path_buf();

    let pack = tokio::task::spawn_blocking({
        let archive = archive_owned.clone();
        move || read_pack(&archive)
    })
    .await
    .map_err(|e| Error::msg(format!("reading pack panicked: {e}")))??;

    // Everything the pack named and the import did not install, each carrying
    // its own reason, reported together at the end. A file nobody is allowed
    // to fetch, a file CurseForge did not answer for and a world that arrived
    // as an archive are different problems and the user can act on all of
    // them, so none is left to a progress line nobody reads. Nothing a
    // `.mrpack` holds reaches this: its index names its own URLs.
    let mut left_out: Vec<String> = Vec::new();
    // Files the pack carried that the user keeps their own copy of because
    // this could not write over it. Reported beside `left_out` rather than in
    // it: one is a file to go and fetch, the other is a file already on disk.
    let mut not_written: Vec<NotWritten> = Vec::new();

    match pack {
        PackKind::Modrinth(index) => {
            progress.stage("Extracting overrides", 1).await;
            // `server-overrides/` is deliberately not here. It is the trees
            // this pack means for a server, and nothing in Geode builds a
            // server out of a pack's files — `server::from_pack` installs the
            // mods the index names, by project id, and never opens the
            // archive. Extracting it into a client instance would overwrite
            // the client's own config with a server's.
            not_written.extend(
                extract_trees(
                    &archive_owned,
                    &game_dir,
                    &MRPACK_CLIENT_TREES.map(|tree| (tree, "")),
                    overrides,
                    progress,
                )
                .await?,
            );
            progress.advance(1, "overrides").await;

            let mut downloads: Vec<Download> = Vec::new();
            for file in &index.files {
                // Server-only files have no business in a client instance.
                // `optional` on a client is not one of them: the format means
                // a choice there, and Prism asks and writes `.disabled` for
                // what the user leaves out. Geode installs them enabled —
                // decided, not overlooked, because nobody asked for a prompt.
                if file
                    .env
                    .as_ref()
                    .is_some_and(|e| e.client == "unsupported")
                {
                    continue;
                }
                let rel = Path::new(&file.path);
                if !crate::paths::is_safe_relative(rel) {
                    continue;
                }
                let Some(url) = file.downloads.first() else {
                    left_out.push(file.path.clone());
                    continue;
                };
                downloads.push(
                    Download::new(url.clone(), game_dir.join(rel))
                        // The second host the pack lists, where it lists one.
                        // A server's pack is no wider for it: every URL in one
                        // is held to Modrinth by `check_server_downloads`
                        // before a byte is fetched, the mirror included.
                        .mirror(file.downloads.get(1).cloned())
                        .sha1(file.hashes.get("sha1").cloned())
                        .size(file.file_size)
                        .label(file.path.clone()),
                );
            }

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
            not_written.extend(
                extract_trees(
                    &archive_owned,
                    &game_dir,
                    &[(manifest.overrides.as_str(), "")],
                    overrides,
                    progress,
                )
                .await?,
            );
            progress.advance(1, "overrides").await;

            progress
                .stage("Resolving pack contents", manifest.files.len() as u64)
                .await;

            // Two batched calls for the whole pack, where this was one request
            // per file: a three-hundred-mod pack was three hundred round
            // trips. And a failure ends the import — it used to be swallowed
            // into a progress note, so a key that stopped working part way
            // through produced an instance quietly short of mods.
            let file_ids: Vec<u32> = manifest.files.iter().map(|f| f.file_id).collect();
            let by_file_id: HashMap<u32, crate::providers::Version> =
                crate::providers::curseforge::files_by_ids_at(client, base, &file_ids, key)
                    .await
                    .ctx("asking CurseForge what this pack's files are")?
                    .into_iter()
                    .filter_map(|v| v.id.parse::<u32>().ok().map(|id| (id, v)))
                    .collect();

            // The second endpoint is what makes a file land in the right
            // folder: a manifest states no path, so the folder comes from the
            // project. One batched call for the distinct ids, and a project
            // the answer leaves out simply has no class, which reads as a mod.
            //
            // The ids asked about are the `modId`s the files answer returned,
            // not the `projectID`s the manifest claimed. A manifest is
            // somebody else's JSON and its pair can be wrong or stale — a
            // project transferred or merged upstream changes the owner of its
            // files — and asking about a project that does not own the file
            // means placing the file by the wrong class while recording the
            // right project. The per-file endpoint this replaced validated the
            // pair server-side; the batch one cannot, so the authoritative
            // answer already on the wire is the one used. In manifest order,
            // so the request bodies do not depend on a map's iteration order.
            let mut project_ids: Vec<u32> = Vec::new();
            for entry in &manifest.files {
                let Some(id) = by_file_id
                    .get(&entry.file_id)
                    .and_then(|v| v.project_id.parse::<u32>().ok())
                else {
                    continue;
                };
                if !project_ids.contains(&id) {
                    project_ids.push(id);
                }
            }
            let kinds: HashMap<String, crate::providers::ProjectKind> =
                crate::providers::curseforge::projects_by_ids_at(client, base, &project_ids, key)
                    .await
                    .ctx("asking CurseForge what this pack's mods are")?
                    .iter()
                    .map(|p| (p.id.clone(), p.kind))
                    .collect();

            let mut downloads = Vec::new();
            let mut records = Vec::new();
            // The worlds this pack bundles, as `(what CurseForge calls it, the
            // zip it is arriving as)`, unpacked once their bytes have landed.
            let mut worlds: Vec<(String, PathBuf)> = Vec::new();

            for entry in &manifest.files {
                progress.check_cancelled()?;

                // Looked up rather than taken, so a manifest that lists the
                // same file twice is two copies of one file and not one file
                // and one phantom.
                let Some(version) = by_file_id.get(&entry.file_id).cloned() else {
                    progress
                        .advance(1, &format!("file {}", entry.file_id))
                        .await;
                    left_out.push(format!(
                        "project {} file {} (CurseForge did not return it — it may have been withdrawn)",
                        entry.project_id, entry.file_id
                    ));
                    continue;
                };
                progress.advance(1, &version.name).await;

                // Worked out before the file is, because it is also the advice
                // for a file Geode may not fetch: a blocked resource pack the
                // user is told to drop into `mods/` is the very mis-placement
                // the rest of this is here to stop making.
                let kind = kinds
                    .get(&version.project_id)
                    .copied()
                    .unwrap_or(crate::providers::ProjectKind::Mod);
                let placement = cf_placement(kind);
                let (folder, record_kind) = match placement {
                    CfPlacement::Tracked(kind) => (kind.folder(), Some(kind)),
                    CfPlacement::Saves => ("saves", None),
                    CfPlacement::Nowhere => {
                        left_out.push(format!(
                            "{} (a pack inside a pack is not a file an instance holds)",
                            version.name
                        ));
                        continue;
                    }
                };

                // The author turned third-party downloads off. Exactly one
                // thing is tried instead, and only the hashes can authorise
                // it: that same author's own Modrinth release of the
                // identical bytes. No mirror, no guessed address — see
                // `install::blocked`.
                let mut version = version;
                let file = match version.primary_file().cloned() {
                    Some(file) => file,
                    None => {
                        let by_hand = match placement {
                            CfPlacement::Saves => {
                                "unpack it into the instance's saves folder".to_string()
                            }
                            _ => format!("drop it into the instance's {folder} folder"),
                        };
                        match crate::install::blocked::instead_of(
                            client,
                            &version,
                            &by_hand,
                            crate::install::blocked::Fallback::at(modrinth_base, progress),
                        )
                        .await
                        {
                            // Installed from Modrinth's own URL, and recorded
                            // below as Modrinth, because every field of the
                            // record is read off the version being installed.
                            crate::install::blocked::Instead::Modrinth {
                                version: authors_own,
                                file,
                            } => {
                                version = authors_own;
                                file
                            }
                            crate::install::blocked::Instead::Refused(because) => {
                                left_out.push(format!("{} ({because})", version.name));
                                continue;
                            }
                        }
                    }
                };

                // `required: false` is this format saying the user may do
                // without the file, and `.disabled` is what that means on
                // disk — the same thing Prism's importer writes for one. The
                // record keeps the enabled name, which is how a toggle finds
                // its provenance again.
                //
                // Only for content, since `.disabled` is a convention about
                // what a loader reads and nothing loads a world.
                let on_disk = match record_kind {
                    Some(_) if !entry.required => {
                        super::content::disabled_name(&file.filename)
                    }
                    _ => file.filename.clone(),
                };

                // The name comes from CurseForge rather than from anything
                // Geode wrote, so it is held to the same rule as an entry in
                // somebody else's archive.
                let rel = Path::new(folder).join(&on_disk);
                if !crate::paths::is_safe_relative(&rel) {
                    left_out.push(format!(
                        "{} (CurseForge calls its file \"{}\", which is not a name Geode \
                         will write inside an instance)",
                        version.name, file.filename
                    ));
                    continue;
                }

                downloads.push(
                    Download::new(file.url.clone(), game_dir.join(&rel))
                        .sha1(file.sha1.clone())
                        .size(file.size)
                        .label(rel.to_string_lossy().to_string()),
                );
                // Arriving is not the end of it for a world. Minecraft reads a
                // world as a directory holding `level.dat` — which is what
                // Geode's own writer produces — and CurseForge publishes one
                // as a zip of that directory. So the zip is unpacked once the
                // downloads are in, below: a map pack whose world the player
                // has to find and unzip by hand is an import that calls itself
                // a success and leaves an empty singleplayer list.
                if matches!(placement, CfPlacement::Saves) {
                    let zipped = game_dir.join(&rel);
                    // A manifest listing the same file twice is two entries
                    // downloading to the one path, which the loop above
                    // tolerates. Unpacking it twice would not: the first
                    // unpack deletes the zip, so the second fails on a missing
                    // file and the failure reads as advice about a zip the
                    // player no longer has.
                    if !worlds.iter().any(|(_, already)| already == &zipped) {
                        worlds.push((version.name.clone(), zipped));
                    }
                }

                if let Some(kind) = record_kind {
                    records.push(ContentRecord {
                        file_name: file.filename.clone(),
                        kind,
                        // Whichever platform the bytes came from. For nearly
                        // every file that is CurseForge; for one whose author
                        // blocked it there and published it on Modrinth
                        // themselves, it is Modrinth, because that is where
                        // Geode fetched it from.
                        provider: version.provider.as_str().to_string(),
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
                        chosen_side: None,
                        keep_on_server: false,
                        server_off: false,
                    });
                }
            }

            progress
                .stage("Downloading pack contents", downloads.len() as u64)
                .await;
            crate::net::fetch_all(client, downloads, progress, 8).await?;

            // A map pack's world is the whole point of installing it, so it is
            // unpacked into the folder the game reads rather than left as a
            // zip. Only a world Geode could not make a folder out of is left
            // for the player, named in the report with what it needs.
            if !worlds.is_empty() {
                progress
                    .stage("Unpacking worlds", worlds.len() as u64)
                    .await;
            }
            let saves = game_dir.join("saves");
            for (name, zipped) in worlds {
                progress.advance(1, &name).await;
                let on_disk = zipped.file_name().unwrap_or_default().to_string_lossy();
                let by_hand = |what: &str| {
                    format!(
                        "{name} (a world{what} — it is in the instance's saves folder as \
                         \"{on_disk}\", and the game only reads a world as a folder, so unzip it \
                         there)"
                    )
                };
                match unpack_world(&zipped, &saves, overrides, progress).await {
                    Ok(folders) if !folders.is_empty() => {
                        // The zip has nothing left to say, and leaving it in
                        // `saves/` beside the world it became is clutter the
                        // game ignores and the player has to wonder about.
                        let _ = tokio::fs::remove_file(&zipped).await;
                        // Every folder, because the zip is gone: a map
                        // published in chapters arrives as several worlds in
                        // one archive, and one the notice does not name is one
                        // the player cannot be told about afterwards.
                        let arrived = folders
                            .iter()
                            .map(|folder| format!("\"{folder}\""))
                            .collect::<Vec<_>>()
                            .join(", ");
                        progress.notice(
                            if folders.len() == 1 {
                                "A world came with this pack"
                            } else {
                                "Worlds came with this pack"
                            },
                            match folders.len() {
                                1 => format!(
                                    "\"{name}\" is unpacked into this instance's saves folder as \
                                     {arrived}, so it is in the game's singleplayer list."
                                ),
                                several => format!(
                                    "\"{name}\" held {several} worlds, unpacked into this \
                                     instance's saves folder as {arrived}, so they are in the \
                                     game's singleplayer list."
                                ),
                            },
                        );
                    }
                    // A zip with no `level.dat` in it is not a world, whatever
                    // CurseForge files it under, and inventing a save folder
                    // for it would be worse than saying so.
                    Ok(_) => left_out.push(by_hand("")),
                    // Cancel is the user's answer and not a thing to report.
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(e) => left_out.push(by_hand(&format!(" Geode could not unpack — {e}"))),
                }
            }

            instance.content.extend(records);
            instance.pack = Some(super::PackSource::local(
                manifest.name,
                manifest.version,
                Some("curseforge".into()),
            ));
        }

        PackKind::Prism(prism_instance) => {
            // Nothing is fetched: an instance carries its files, and Prism's
            // own importer downloads nothing either. What it does carry is a
            // packwiz index naming where each of those files came from, which
            // is read once they have landed.
            if archive_owned.is_dir() {
                // A folder the user pointed at, not an archive: copied the way
                // an instance is duplicated, with no unpack budget to enforce
                // and no entry names to distrust. Counted file by file, since
                // a real instance is tens of thousands of them and the copy is
                // the longest thing this import does — the preview already
                // counted them, so the bar has a total to move against and
                // Cancel stops it where it stands.
                progress
                    .stage("Copying the instance", prism_instance.file_count as u64)
                    .await;
                super::copy_dir(
                    &archive_owned.join(&prism_instance.game_dir),
                    &game_dir,
                    progress,
                )
                .await?;
            } else {
                progress.stage("Copying the instance", 1).await;
                not_written.extend(
                    extract_trees(
                        &archive_owned,
                        &game_dir,
                        &[(prism_instance.game_root().as_str(), "")],
                        overrides,
                        progress,
                    )
                    .await?,
                );
                progress.advance(1, &prism_instance.game_dir).await;
            }

            instance.notes = prism_instance.notes;
            // Only the values whose gate `instance.cfg` set, one by one: the
            // rest of an instance's overrides are this machine's business.
            instance.overrides.memory_mb = prism_instance.overrides.memory_mb;
            instance.overrides.java_args = prism_instance.overrides.java_args;
            instance.overrides.width = prism_instance.overrides.width;
            instance.overrides.height = prism_instance.overrides.height;
            instance.overrides.env = prism_instance.overrides.env;
            instance.overrides.wrapper_command = prism_instance.overrides.wrapper_command;

            progress.stage("Reading the instance's index", 1).await;
            instance
                .content
                .extend(prism::index_records(&game_dir).await);
            progress.advance(1, "mods").await;
        }
    }

    Ok(Imported {
        left_out,
        not_written,
    })
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

/// How one file out of a pack's override trees lands in the instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// The pack's copy goes over whatever is there, and a write that fails
    /// fails the import. Every file in a `.mrpack` lands this way, and so does
    /// every file a pack says nothing about.
    Replace,
    /// The pack's copy goes over whatever is there, but a write that fails is
    /// reported and the import carries on. For the paths a server syncs: a
    /// settings file the player has made read-only should cost them a line in
    /// a report, not the game — it used to fail the import, which failed the
    /// update, which stopped the launch, on that Play and on every one after.
    Offer,
    /// The player's own copy stays where it is, and the pack's is not read.
    Keep,
}

/// What decides how each of a pack's override files lands.
///
/// [`Overrides::replace`] answers [`Landing::Replace`] for everything, which
/// is what importing a pack has always done and what every pack but a server's
/// still gets. A server's pack is extracted through one built in `share::pack`,
/// the only place that knows what the policies in a pack's settings half mean:
/// nothing here understands a policy, it only asks.
#[derive(Clone)]
pub struct Overrides(std::sync::Arc<Decide>);

/// Asked about one file: where it lands under the directory the tree is
/// written into — the game directory for a pack's own override trees, `saves/`
/// for a world arriving as a zip — and whether the user already has a file
/// there.
type Decide = dyn Fn(&Path, bool) -> Landing + Send + Sync;

impl Overrides {
    /// Every file written whole, and a write that fails fails the import.
    pub fn replace() -> Self {
        Self(std::sync::Arc::new(|_, _| Landing::Replace))
    }

    /// Decided per file, from where it lands under the destination and
    /// whether the player already has a file there.
    pub fn decided(by: impl Fn(&Path, bool) -> Landing + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(by))
    }

    fn landing(&self, rel: &Path, present: bool) -> Landing {
        (self.0)(rel, present)
    }
}

/// Copy chosen trees out of an archive into a directory. Later trees win, so
/// `client-overrides` correctly shadows `overrides`.
///
/// Each one is the entry prefix to strip and where it goes under `dest_dir`.
/// The second is `""` for a pack's own trees, which mirror the archive, and a
/// folder name for a world — an archive holding several worlds is unpacked in
/// one pass, so the inflate budget below is the archive's own rather than one
/// budget per world.
///
/// A prefix of `""` is the archive's own root, which is how a world zip whose
/// `level.dat` sits at the top is unpacked: every entry in it is the tree.
///
/// Returns the files it left as the player had them because they could not be
/// written, each line naming the file and why — only ever the paths the
/// `overrides` answers [`Landing::Offer`] for, which for every pack but a
/// server's is none of them.
///
/// Stops at the next entry when the run is cancelled. If it does not finish —
/// cancelled, or refused for unpacking past its budget — it unlinks the files
/// it brought into being and only those, so a file the player already had
/// keeps the pack's copy rather than being taken away from them.
async fn extract_trees(
    archive: &Path,
    dest_dir: &Path,
    trees: &[(&str, &str)],
    overrides: &Overrides,
    progress: &Progress,
) -> Result<Vec<NotWritten>> {
    let archive = archive.to_path_buf();
    let dest_dir = dest_dir.to_path_buf();
    let trees: Vec<(String, PathBuf)> = trees
        .iter()
        .map(|(prefix, under)| (prefix.to_string(), PathBuf::from(under)))
        .collect();
    let overrides = overrides.clone();
    let progress = progress.clone();

    tokio::task::spawn_blocking(move || -> Result<Vec<NotWritten>> {
        let archive_bytes = std::fs::metadata(&archive)?.len();
        let budget = overrides_budget(archive_bytes);
        let mut unpacked: u64 = 0;
        let mut zip = open_pack(&archive)?;
        // What this run brought into being, to take back if it does not finish.
        // A file that was already there and was overwritten stays overwritten:
        // the pack's copy is the one that was asked for, and an update extracts
        // over a game directory full of the player's own files.
        let mut created: Vec<PathBuf> = Vec::new();
        // Left as the player had them, rather than aborting the whole import.
        // Not unlinked either: there is nothing of this run's to take back.
        let mut not_written: Vec<NotWritten> = Vec::new();

        let mut unpack = |not_written: &mut Vec<NotWritten>| -> Result<()> {
            for i in 0..zip.len() {
                progress.check_cancelled()?;
                let mut entry = zip.by_index(i)?;
                if entry.is_dir() {
                    continue;
                }
                let name = entry.name().to_string();
                let Some((rel, under)) = trees.iter().find_map(|(prefix, under)| {
                    if prefix.is_empty() {
                        Some((name.as_str(), under))
                    } else {
                        name.strip_prefix(&format!("{prefix}/"))
                            .map(|rel| (rel, under))
                    }
                }) else {
                    continue;
                };

                let rel_path = Path::new(rel);
                if !crate::paths::is_safe_relative(rel_path) {
                    continue;
                }
                // Asked about where the file lands rather than about its path
                // inside the tree, which are the same thing for a pack's own
                // trees and are not for a world: `region/r.0.0.mca` says
                // nothing about which world it belongs to, and `datapacks/`
                // inside one is not the instance's own.
                let under_dest = under.join(rel_path);
                let dest = dest_dir.join(&under_dest);
                let fresh = !dest.exists();
                let landing = overrides.landing(&under_dest, !fresh);
                if landing == Landing::Keep {
                    continue;
                }
                // Both the folder and the file: a player who made the folder
                // read-only stops the write just as surely as one who made the
                // file read-only, and for a path the server syncs neither is a
                // reason they cannot play.
                let opened = std::fs::create_dir_all(dest.parent().unwrap_or(&dest_dir))
                    .and_then(|()| std::fs::File::create(&dest));
                let mut out = match opened {
                    Ok(out) => out,
                    Err(e) if landing == Landing::Offer => {
                        not_written.push(NotWritten {
                            path: rel.to_string(),
                            why: format!(
                                "{rel} could not be written, so what you already had is still \
                                 there: {e}"
                            ),
                        });
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
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

        let outcome = unpack(&mut not_written);
        if outcome.is_err() {
            for dest in &created {
                let _ = std::fs::remove_file(dest);
            }
        }
        outcome.map(|()| not_written)
    })
    .await
    .map_err(|e| Error::msg(format!("unpacking an archive panicked: {e}")))?
}

/// Every world inside a zip of them, as `(the entry prefix to unpack, the
/// folder's name)`.
///
/// Both shapes a real world zip comes in. The common one wraps everything in a
/// folder — `CoolMap/level.dat`, `CoolMap/region/...` — and that folder *is*
/// the world, so the name its author gave it is the one to keep. The other has
/// `level.dat` at the archive's root, where nothing inside says what the world
/// is called and the file's own name is all there is to go on.
///
/// Read off `level.dat`, because that is the file the game requires and the
/// only thing that makes a directory a world. Every one at the shallowest
/// depth is a world: a map published in chapters puts them side by side in one
/// archive, and a world that happens to carry another world deeper inside
/// unpacks as itself. Sorted, so one archive always unpacks the same way.
///
/// Empty when there is no `level.dat` at all — that is not a world, and
/// guessing would leave a junk folder in somebody's singleplayer list — and
/// when any wrapping folder is not a name Geode will make a directory for,
/// because a zip that names its way out of `saves/` is not one to unpack half
/// of.
fn worlds_in(names: &[String], named_after_the_file: &str) -> Vec<(String, String)> {
    let depth = |prefix: &str| {
        if prefix.is_empty() {
            0
        } else {
            prefix.split('/').count()
        }
    };

    let mut prefixes: Vec<&str> = names
        .iter()
        .filter_map(|name| {
            let (prefix, file) = name.rsplit_once('/').unwrap_or(("", name.as_str()));
            (file == "level.dat").then_some(prefix)
        })
        .collect();
    let Some(shallowest) = prefixes.iter().map(|prefix| depth(prefix)).min() else {
        return Vec::new();
    };
    prefixes.retain(|prefix| depth(prefix) == shallowest);
    prefixes.sort_unstable();
    prefixes.dedup();

    let mut worlds = Vec::with_capacity(prefixes.len());
    for prefix in prefixes {
        let folder = match prefix.rsplit('/').next() {
            Some(last) if !last.is_empty() => last,
            _ => named_after_the_file,
        };
        // The name comes out of somebody else's archive, so it is held to more
        // than the `is_safe_relative` its entries are: exactly one ordinary
        // component. That refuses `..` and `/` as that rule does, and also `.`
        // and `a/b`, each of which would unpack a world somewhere other than
        // its own folder under `saves/`.
        let mut parts = Path::new(folder).components();
        let one_plain_name =
            matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none();
        if !one_plain_name {
            return Vec::new();
        }
        worlds.push((prefix.to_string(), folder.to_string()));
    }
    worlds
}

/// Turn the worlds that arrived as a zip into the folders the game reads, and
/// say what those folders ended up being called.
///
/// Empty for an archive that holds no world: it is left exactly where it is,
/// for the report to name. Several for an archive that holds several, all
/// unpacked, because the caller deletes the archive afterwards and a world
/// dropped here is one the player cannot get back.
///
/// Unpacked through [`extract_trees`], in one pass, so the worlds are held to
/// everything a pack's overrides are *and* to one budget between them — entry
/// names that climb out of the destination are skipped, the inflate budget
/// stops a zip bomb, Cancel is heard between entries, and a run that does not
/// finish takes back only what it wrote.
///
/// Through the caller's own `overrides`, and that is load-bearing: an import
/// into a new instance answers [`Landing::Replace`] for everything, because a
/// world nobody has yet is written whole; a pack *update* answers
/// [`Landing::Keep`] for a file the player already has, because by then they
/// have been playing the world this zip ships and there is no getting a save
/// back. Written whole here once, it overwrote their `level.dat` and every
/// region they had explored.
async fn unpack_world(
    archive: &Path,
    saves: &Path,
    overrides: &Overrides,
    progress: &Progress,
) -> Result<Vec<String>> {
    let names = {
        let archive = archive.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let mut zip = open_pack(&archive)?;
            Ok((0..zip.len())
                .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
                .collect())
        })
        .await
        .map_err(|e| Error::msg(format!("reading a world panicked: {e}")))??
    };

    let named_after_the_file = archive
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    let worlds = worlds_in(&names, &named_after_the_file);
    if worlds.is_empty() {
        return Ok(Vec::new());
    }

    let trees: Vec<(&str, &str)> = worlds
        .iter()
        .map(|(prefix, folder)| (prefix.as_str(), folder.as_str()))
        .collect();
    extract_trees(archive, saves, &trees, overrides, progress).await?;
    Ok(worlds.into_iter().map(|(_, folder)| folder).collect())
}

/// Give every file a `.mrpack` installed a link back to its project.
///
/// The index records only a URL and a hash, so without this a freshly
/// installed pack shows dozens of "untracked" rows with filenames for names and
/// no update path. The URL already contains the project and version ids, so no
/// extra lookup is needed to establish provenance — only one batched call to
/// turn those ids into names and icons.
///
/// The pack's own `env` pair comes through as the file's side, because the
/// pack said so and is the only one here who has: a version number is the one
/// thing this cannot know, since the id in the URL is not a number anybody
/// reads, and that is filled afterwards by the pass that names everything else
/// ([`crate::install::content::name_what_arrived`]).
///
/// Best-effort: a pack is perfectly usable with imperfect labels, so a failure
/// here is swallowed rather than failing the install.
async fn record_mrpack_provenance(
    client: &reqwest::Client,
    instance: &mut Instance,
    files: &[MrFile],
    progress: &Progress,
) {
    type Pending = (
        ContentKind,
        String,
        String,
        String,
        Option<crate::instance::jar::Side>,
    );
    let mut pending: Vec<Pending> = Vec::new();

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
        pending.push((
            kind,
            name.to_string(),
            project_id,
            version_id,
            file.env.as_ref().and_then(side_from_env),
        ));
    }

    if pending.is_empty() {
        return;
    }

    progress
        .stage("Linking pack contents", pending.len() as u64)
        .await;

    let ids: Vec<String> = {
        let mut v: Vec<String> = pending.iter().map(|(_, _, p, ..)| p.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let projects = crate::providers::modrinth::projects_by_ids(client, &ids)
        .await
        .unwrap_or_default();

    for (kind, file_name, project_id, version_id, side) in pending {
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
            // Not in the URL and not worth a request of its own — the naming
            // pass has the matched version in hand and fills it there.
            version_number: String::new(),
            slug: project.map(|p| p.slug.clone()),
            icon_url: project.and_then(|p| p.icon_url.clone()),
            dependency: false,
            origin: Default::default(),
            requires: None,
            side,
            chosen_side: None,
            keep_on_server: false,
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

/// What a pack will do with one file.
///
/// [`Travel`] is what [`travel`] decides from the file and the format; this is
/// that plus the one reason only the writer can know, because it depends on
/// what the pack has already said about another file. Each writer's planning
/// pass produces these, and [`ExportReport::count`] is the only place they
/// become numbers — which is what lets the export dialog show the counts
/// before the pack is written and be right.
pub enum Journey<'a> {
    /// An index entry: a URL and a pair of hashes the recipient fetches with
    /// nobody's credentials.
    Linked(&'a PlatformRef),
    /// Named by id in the pack's own by-id channel.
    Named(&'a PlatformRef),
    /// Bytes, for one of the reasons [`travel`] gives.
    Copied(Copied),
    /// A second copy of a file this pack already names by the same ids, whose
    /// bytes ride along so the recipient still ends up with both. Not a limit
    /// of the format — the mod *is* named — which is why it is counted apart.
    Duplicate,
}

impl ExportReport {
    /// Count one file's journey.
    ///
    /// The one place a [`Journey`] becomes a number, so a dry run and the
    /// writer that follows it cannot disagree about what the export did.
    pub fn count(&mut self, journey: &Journey<'_>) {
        match journey {
            Journey::Linked(_) | Journey::Named(_) => self.referenced += 1,
            Journey::Copied(Copied::Unknown) => self.embedded_unknown += 1,
            Journey::Copied(Copied::Elsewhere) => self.embedded_elsewhere += 1,
            Journey::Copied(Copied::TurnedOff) => self.embedded_turned_off += 1,
            Journey::Copied(Copied::Blocked) => self.embedded_blocked += 1,
            Journey::Duplicate => self.embedded_duplicate += 1,
        }
    }
}

/// The catalogue's files a pack of this selection carries, in the order every
/// writer walks them.
///
/// Sorted by where the file sits, so exporting the same instance twice is the
/// same bytes twice. The catalogue is a map, and the record order that used to
/// decide this no longer comes into it.
///
/// A file in a group the sharer left out is not here at all. The writers name
/// what they find here, so naming one would have the recipient fetch a mod
/// whose bytes stayed at home — and [`ExportReport::count`] would report it as
/// having travelled.
pub(super) fn in_writing_order<'a>(
    catalogue: &'a Catalogue,
    selection: &Selection,
) -> Vec<&'a Identity> {
    let mut identities: Vec<&Identity> = catalogue
        .identities()
        .filter(|ident| selection.carries(&ident.disk_rel))
        .collect();
    identities.sort_by(|a, b| a.disk_rel.cmp(&b.disk_rel));
    identities
}

/// What an `.mrpack`-shaped pack would do with each of the catalogue's files.
///
/// Nothing but [`travel`], because an index entry and a `.geodepack` note can
/// both say everything they need to about one file on their own: there is no
/// second file whose presence changes the answer.
pub(super) fn plan_pack<'a>(
    catalogue: &'a Catalogue,
    carry: Carry,
    selection: &Selection,
) -> Vec<(&'a Identity, Journey<'a>)> {
    in_writing_order(catalogue, selection)
        .into_iter()
        .map(|ident| {
            let journey = match travel(ident, carry) {
                Travel::Linked(reference) => Journey::Linked(reference),
                Travel::Named(reference) => Journey::Named(reference),
                Travel::Copied(why) => Journey::Copied(why),
            };
            (ident, journey)
        })
        .collect()
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
    selection: &Selection,
    progress: &Progress,
) -> Result<ExportReport> {
    let catalogue = crate::install::identity::for_instance(ask, paths, instance, progress).await?;
    write_instance_pack(
        paths,
        instance,
        dest,
        selection,
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
    selection: &Selection,
    half: GeodeHalf,
    catalogue: &Catalogue,
    progress: &Progress,
) -> Result<ExportReport> {
    write_mrpack(
        instance_input(paths, instance, selection),
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
pub(super) fn instance_input<'a>(
    paths: &Paths,
    instance: &'a Instance,
    selection: &Selection,
) -> PackInput<'a> {
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
        walk: Walk::Groups(selection.clone()),
    }
}

/// Which entries under a pack's root the writer may carry as bytes.
pub enum Walk {
    /// The groups of an instance the sharer chose, and the ones the format
    /// never carries left out. Right for an instance, whose game directory is
    /// the player's own and mostly worth sending.
    ///
    /// A list of what to *keep out* used to live here, which is how a pack
    /// came to decide for the sharer that their screenshots were nobody's
    /// business. [`super::export::group_of`] is the one place that reads the
    /// layout now, so the dialog's counts and this walk cannot disagree.
    ///
    /// This walk follows links. The player exporting is the one who made them
    /// — a `saves` or `resourcepacks` shared between instances is a common
    /// setup — and the file it writes goes wherever they take it, so the pack
    /// should hold what they see in game.
    Groups(Selection),
    /// Nothing but these, and never through a link. Right for anything holding
    /// files that must never leave the machine, where a list of what to keep
    /// out is only ever one forgotten name from shipping a secret.
    ///
    /// A link is refused wherever it sits, because an allowlist says nothing
    /// about where a link inside it points: a `mods/backup` linked to the
    /// directory above would carry `server.properties` under an allowed name.
    /// Nobody looks at this pack before it goes, either — it is built for
    /// whoever presents the code.
    ///
    /// Every entry is a file and not a folder, so what the pack carries is
    /// the list its caller worked out rather than whatever the folders held at
    /// the moment the zip was written. That is what lets one function answer
    /// "what travels" for the admin looking at the picker, for the revision
    /// players check against, and for this walk — three answers that must
    /// never disagree, since a second walk that drifted would tell a player a
    /// number the pack does not carry.
    Only {
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
            Walk::Groups(selection) => {
                let chosen = top
                    .and_then(super::export::group_of)
                    .is_some_and(|group| selection.travels(group));
                if !chosen {
                    Step::Skip
                } else if entry.path().is_dir() {
                    Step::Descend
                } else {
                    Step::Carry
                }
            }
            Walk::Only { files } => {
                // The entry's own type, which is the link itself rather than
                // what it points at. `Path::is_dir` and `File::open` both
                // follow, and are how a link would get through.
                let Ok(kind) = entry.file_type() else {
                    return Step::Skip;
                };
                if kind.is_dir() {
                    if files.iter().any(|f| f.starts_with(rel)) {
                        Step::Descend
                    } else {
                        Step::Skip
                    }
                } else if kind.is_file() && files.contains(rel) {
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

impl PackInput<'_> {
    /// Which of the instance's groups this pack carries, as the writers'
    /// planning passes need it.
    ///
    /// Taken off the walk rather than passed a second time, so what a pack
    /// names and what its walk carries cannot come to disagree.
    fn selection(&self) -> Selection {
        match &self.walk {
            Walk::Groups(selection) => selection.clone(),
            // A pack walked by allowlist is a server's pack for its players,
            // whose catalogue holds exactly the files that walk named: no
            // group was left out for a writer to honour.
            Walk::Only { .. } => Selection::everything(),
        }
    }
}

/// The folder a server keeps its players' own files in, beside the one it
/// keeps their mods in.
///
/// No loader reads either, which is the whole point of both: a client mod in
/// `client-mods/` cannot slow the server down or crash it on boot, and a copy
/// of `options.txt` in here is not a file the server has any use for. It is
/// the only way Minecraft's own settings can travel at all — they are the
/// player's files and exist nowhere on a server's disk — and like client mods
/// it goes under `client-overrides/`, so a launcher installing this pack on a
/// server leaves it out.
pub(crate) const CLIENT_FILES: &str = "client-files";

/// Where a file on a server's disk goes in the pack its players get.
///
/// A server keeps client mods in `client-mods/`, which no loader reads, and a
/// player's game has no such folder: there they are ordinary mods. So
/// `client-mods/x.jar` is written as `mods/x.jar`. [`CLIENT_FILES`] is the
/// same idea for a file that is not a mod, and a player's game has no folder
/// for those either — `client-files/options.txt` is written as `options.txt`,
/// at the top of the game directory, which is where Minecraft keeps it.
/// Everything else keeps its path.
pub(crate) fn pack_rel(rel: &Path) -> PathBuf {
    let mut parts = rel.components();
    match parts.next() {
        Some(first) if first.as_os_str() == ContentKind::ClientMod.folder() => {
            Path::new(ContentKind::Mod.folder()).join(parts.as_path())
        }
        Some(first) if first.as_os_str() == CLIENT_FILES => parts.as_path().to_path_buf(),
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
    let selection = input.selection();
    let game_dir = input.root;
    let players_pack = matches!(input.walk, Walk::Only { .. });
    let mut report = ExportReport {
        trouble: catalogue.trouble.clone(),
        ..Default::default()
    };

    let mut index = MrIndex {
        format_version: MR_FORMAT,
        game: MR_GAME.into(),
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
    // Worked out by the same planning pass the export dialog's dry run calls,
    // and counted by the same `count`, so what the sharer was shown before
    // pressing the button is what the report tells them afterwards.
    let mut indexed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for (ident, journey) in plan_pack(catalogue, half.carry, &selection) {
        report.count(&journey);
        let reference = match journey {
            Journey::Linked(reference) => reference,
            // Named in the Geode half, which the caller wrote and whose paths
            // reach the walk through `half.referenced`. Counted, as every
            // journey is, and nothing more to write here.
            //
            // `Duplicate` never arrives: it is a CurseForge manifest's limit,
            // and this format's by-id channel carries a path alongside the ids
            // so a second copy of a mod gets a note of its own.
            Journey::Named(_) | Journey::Copied(_) | Journey::Duplicate => continue,
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
        // Only where it belongs, not who said so: an index entry has one field
        // for it, and a reader of the pack cannot be told that nobody knew.
        let side = crate::instance::content::side_of(
            ident.kind,
            ident.chosen_side,
            jar_side,
            ident.listed_side,
        )
        .side;

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
                tree: "overrides",
            },
        )
    })
    .await
    .map_err(|e| Error::msg(format!("writing pack panicked: {e}")))??;

    progress.finish().await;
    Ok(report)
}

/// Everything a pack's zip holds besides the files its walk carries as bytes.
pub(super) struct Layout {
    /// Written first, in order: the format's own index or manifest, then a
    /// `.geodepack`'s `geode/` directory.
    pub(super) named: Extras,
    /// Paths on disk whose bytes the pack already carries by name, so they
    /// stay out of `overrides/`.
    pub(super) carried: std::collections::HashSet<PathBuf>,
    /// The same, for what a Geode half's notes name: keyed on the enabled file
    /// name the way records are, so a disabled file matches too.
    pub(super) referenced: std::collections::HashSet<PathBuf>,
    /// A server's pack for its players, where `client-mods/` becomes `mods/`
    /// and lands under `client-overrides/`.
    pub(super) players_pack: bool,
    /// The directory the walk's files are written under.
    ///
    /// `overrides/`, for every pack: a pack's own files are the format's, and
    /// what the walk carries overrides them. A Prism instance zip is the
    /// exception — there the walked files *are* the instance, and they go
    /// under `minecraft/`, the game directory Prism reads.
    pub(super) tree: &'static str,
}

/// Lay a pack out: what it names first, then every file the walk carries that
/// nothing has named already. Returns the paths that could not travel.
///
/// The one place that knows how a pack's zip is put together, so the formats
/// cannot come to disagree about where an override goes or which files are left
/// out of one. Blocking, because `zip` is.
pub(super) fn lay_out(
    dest: &Path,
    game_dir: &Path,
    walk: &Walk,
    layout: Layout,
) -> Result<Vec<String>> {
    let Layout {
        named,
        carried,
        referenced,
        players_pack,
        tree,
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
            // Both of a server's client-side folders, for the same reason: a
            // launcher installing this pack on a server has to leave them out,
            // and `client-overrides/` is the one thing in the format that says
            // so.
            let root = if players_pack
                && (rel.starts_with(ContentKind::ClientMod.folder())
                    || rel.starts_with(CLIENT_FILES))
            {
                "client-overrides"
            } else {
                tree
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
    selection: &Selection,
    progress: &Progress,
) -> Result<ExportReport> {
    let catalogue = crate::install::identity::for_instance(ask, paths, instance, progress).await?;
    write_cfpack(
        instance_input(paths, instance, selection),
        author,
        dest,
        &catalogue,
        progress,
    )
    .await
}

/// One file's journey through a CurseForge pack, with the manifest entry for
/// the files it names.
pub(super) struct CfPlanned<'a> {
    pub(super) ident: &'a Identity,
    pub(super) journey: Journey<'a>,
    /// The `files[]` entry, present exactly when the journey is
    /// [`Journey::Named`] — deciding needs the ids parsed, so parsing them
    /// twice is how the plan and the manifest would come to disagree. The
    /// writer's own, which is why it is not on show outside this file.
    entry: Option<CfFileOut>,
}

/// What a CurseForge pack would do with each of the catalogue's files.
///
/// Two of the reasons here are the manifest's and not [`travel`]'s, which is
/// why this is a pass over the whole catalogue rather than a decision per
/// file: whether a second entry would collide depends on what the pack has
/// already said.
pub(super) fn plan_cfpack<'a>(
    catalogue: &'a Catalogue,
    selection: &Selection,
) -> Vec<CfPlanned<'a>> {
    let mut planned: Vec<CfPlanned<'a>> = Vec::new();
    for ident in in_writing_order(catalogue, selection) {
        let reference = match travel(ident, Carry::CFPACK) {
            Travel::Named(reference) => reference,
            // `Carry::CFPACK` has no index for anything to be linked from, so
            // this does not arise. Were that ever to change, bytes are the
            // safe answer rather than a panic in the middle of an export.
            Travel::Linked(_) => {
                planned.push(CfPlanned {
                    ident,
                    journey: Journey::Copied(Copied::Elsewhere),
                    entry: None,
                });
                continue;
            }
            Travel::Copied(why) => {
                planned.push(CfPlanned {
                    ident,
                    journey: Journey::Copied(why),
                    entry: None,
                });
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
            planned.push(CfPlanned {
                ident,
                journey: Journey::Copied(Copied::Unknown),
                entry: None,
            });
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
        if planned
            .iter()
            .any(|other| other.entry.as_ref().is_some_and(|e| e.file_id == file_id))
        {
            planned.push(CfPlanned {
                ident,
                journey: Journey::Duplicate,
                entry: None,
            });
            continue;
        }

        planned.push(CfPlanned {
            ident,
            journey: Journey::Named(reference),
            entry: Some(CfFileOut {
                project_id,
                file_id,
                // The nearest this format comes to saying a mod is turned off,
                // and what its readers turn back into a `.disabled` file.
                required: !ident.disabled,
            }),
        });
    }
    planned
}

/// The same writer, from a catalogue already built — which is the seam a
/// preview uses, and what the tests write packs through.
pub(super) async fn write_cfpack(
    input: PackInput<'_>,
    author: &str,
    dest: PathBuf,
    catalogue: &Catalogue,
    progress: &Progress,
) -> Result<ExportReport> {
    let selection = input.selection();
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
        manifest_type: MODPACK_MANIFEST,
        manifest_version: MODPACK_MANIFEST_VERSION,
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

    let mut carried: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut credits: Vec<(u32, String)> = Vec::new();
    for planned in plan_cfpack(catalogue, &selection) {
        let CfPlanned {
            ident,
            journey,
            entry,
        } = planned;
        report.count(&journey);
        let Some(entry) = entry else { continue };
        credits.push((entry.project_id, ident.file_name.clone()));
        carried.insert(ident.disk_rel.clone());
        manifest.files.push(entry);
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
                tree: "overrides",
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
    use crate::instance::export::Group;
    use crate::paths::Scratch;

    /// What an instance's pack carries is the sharer's choice, group by group,
    /// and a cache is in no group at all. Pinned because the same writer walks
    /// a server's run directory the other way round, by allowlist, and the
    /// instance's walk must not drift along with it.
    #[tokio::test]
    async fn an_instance_pack_carries_the_groups_it_was_asked_for() {
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

        // What the default selection carries, which is what every caller that
        // is not the export dialog still asks for: no worlds, no screenshots,
        // and never the loader's own caches.
        let kept = [
            "modrinth.index.json",
            "overrides/config/mod.toml",
            "overrides/mods/hand.jar",
            "overrides/options.txt",
        ];
        // Each run names the groups asked for and the entries that answer,
        // so a group gained or lost names itself.
        let asked: [(&str, Selection, Vec<&str>); 4] = [
            ("the default", Selection::default(), Vec::new()),
            (
                "worlds too",
                Selection::with_saves(true),
                vec!["overrides/saves/world/level.dat"],
            ),
            (
                "screenshots and logs too",
                Selection::of(
                    [
                        Group::Mods,
                        Group::Config,
                        Group::ResourcePacks,
                        Group::ShaderPacks,
                        Group::DataPacks,
                        Group::Other,
                        Group::Logs,
                    ]
                    .to_vec(),
                ),
                vec![
                    "overrides/crash-reports/crash.txt",
                    "overrides/logs/latest.log",
                    "overrides/screenshots/shot.png",
                ],
            ),
            // Nothing of the player's at all: the index, and not one file.
            ("nothing", Selection::of(Vec::new()), Vec::new()),
        ];
        for (what, selection, extra) in asked {
            let dest = paths.root.join(format!("walk-{}.mrpack", what.replace(' ', "-")));
            export_mrpack(
                Ask::Nobody,
                &paths,
                &inst,
                dest.clone(),
                &selection,
                &Progress::silent("t"),
            )
            .await
            .unwrap();

            let zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
            let mut names: Vec<String> = zip.file_names().map(str::to_string).collect();
            names.sort();
            let mut expected: Vec<String> = if what == "nothing" {
                vec!["modrinth.index.json".to_string()]
            } else {
                kept.iter().map(|s| s.to_string()).collect()
            };
            expected.extend(extra.iter().map(|s| s.to_string()));
            expected.sort();
            assert_eq!(names, expected, "asked for {what}");
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
            &Selection::default(),
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
                &Selection::default(),
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
            instance_input(paths, inst, &Selection::default()),
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

    /// A group the sharer unticked is named by nothing either. The manifest is
    /// a list of ids the recipient's launcher fetches, so an entry for a mod
    /// whose bytes the walk left at home installs the very file that was
    /// excluded — and the report would have called it referenced.
    #[tokio::test]
    async fn a_group_left_out_is_named_in_no_manifest_entry() {
        let (scratch, inst, mut catalogue) =
            cf_fixture("cf-left-out", &[("mods", "create.jar"), ("config", "x.toml")]).await;
        catalogue.pretend_curseforge_holds(&[("create.jar", 328085, 4593548, true)]);
        let dest = scratch.paths.root.join("cf-left-out.zip");

        let report = write_cfpack(
            instance_input(&scratch.paths, &inst, &Selection::of(vec![Group::Config])),
            "Somebody",
            dest.clone(),
            &catalogue,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert_eq!(manifest_of(&dest)["files"], serde_json::json!([]));
        assert_eq!((report.referenced, bundled(&report)), (0, (0, 0, 0, 0, 0)));
        assert_eq!(
            entries(&dest),
            vec!["manifest.json", "modlist.html", "overrides/config/x.toml"]
        );
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
    const CURSEFORGE: &str = r#"{"manifestType":"minecraftModpack","manifestVersion":1,"minecraft":{"version":"1.21.1","modLoaders":[]},"name":"P","files":[]}"#;
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

    /// A pack the user chose themselves has nothing synced in it, so a file it
    /// cannot write is what it has always been: the import fails, and the user
    /// is told rather than left with an instance quietly short of a file.
    /// Only a sender that says which of its files are settings — see
    /// [`Overrides`] — buys the gentler treatment.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_plain_pack_still_fails_when_it_cannot_write_a_file() {
        use std::os::unix::fs::PermissionsExt;
        use zip::CompressionMethod::Deflated;
        let scratch = scratch_dir("readonly");
        let dir = scratch.paths.root.clone();
        let archive = dir.join("pack.mrpack");
        write_zip(
            &archive,
            &[
                ("modrinth.index.json", INDEX.as_bytes(), Deflated),
                ("overrides/config/theirs.toml", b"theirs = true\n", Deflated),
            ],
        );

        let (paths, mut inst) = unpacking(&dir);
        let game = paths.game_dir(&inst.id);
        std::fs::create_dir_all(game.join("config")).unwrap();
        let mine = game.join("config/theirs.toml");
        std::fs::write(&mine, b"mine = true\n").unwrap();
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o444)).unwrap();

        let err = import_into(
            &reqwest::Client::new(),
            &paths,
            &mut inst,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .expect_err("a file that could not be written must fail a plain pack");
        assert!(err.to_string().contains("Permission denied"), "{err}");
        // And what was there is what is still there, untouched.
        assert_eq!(std::fs::read(&mine).unwrap(), b"mine = true\n");
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o644)).unwrap();
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

    // -----------------------------------------------------------------------
    // A bundled world, unpacked into the folder the game reads
    // -----------------------------------------------------------------------

    /// A `saves/` directory with a world zip sitting in it, the way one lands
    /// from a CurseForge pack that bundles a map.
    fn arrived_world(tag: &str, file: &str, entries: &[(&str, &[u8])]) -> (Scratch, PathBuf) {
        let scratch = scratch_dir(tag);
        let saves = scratch.paths.root.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        let deflated: Vec<(&str, &[u8], zip::CompressionMethod)> = entries
            .iter()
            .map(|(name, bytes)| (*name, *bytes, zip::CompressionMethod::Deflated))
            .collect();
        write_zip(&saves.join(file), &deflated);
        (scratch, saves)
    }

    /// The common shape a map is published in: every entry under one folder,
    /// which *is* the world, so the name its author gave it is the name it
    /// keeps — not the name of whatever the download was called.
    ///
    /// A world that happens to carry another world deeper inside unpacks as
    /// itself, which is the shallowest `level.dat` winning.
    #[tokio::test]
    async fn a_world_wrapped_in_a_folder_keeps_that_folders_name() {
        let (_scratch, saves) = arrived_world(
            "world-wrapped",
            "download.zip",
            &[
                ("Sky Village/level.dat", b"NBT"),
                ("Sky Village/region/r.0.0.mca", b"REGION"),
                ("Sky Village/datapacks/inner/level.dat", b"DEEPER"),
            ],
        );

        let folders = unpack_world(
            &saves.join("download.zip"),
            &saves,
            &Overrides::replace(),
            &Progress::silent("t"),
        )
        .await
        .expect("a world zip unpacks");

        assert_eq!(folders, vec!["Sky Village".to_string()]);
        assert_eq!(
            std::fs::read(saves.join("Sky Village/level.dat")).unwrap(),
            b"NBT",
            "the game reads a world by its level.dat, so that is what has to be there"
        );
        assert_eq!(
            std::fs::read(saves.join("Sky Village/region/r.0.0.mca")).unwrap(),
            b"REGION",
            "the world arrived without the rest of itself"
        );
        assert!(
            !saves.join("download").exists(),
            "the download's own name was used for a world that had its own"
        );
        assert!(
            !saves.join("Sky Village/Sky Village").exists(),
            "the wrapping folder was unpacked inside itself"
        );
    }

    /// The world a pack bundles arrives again with every version of that pack,
    /// and by the second one the player has been playing it. Written whole,
    /// the pack's pristine copy went over their `level.dat` and every region
    /// they had explored — the one thing in an instance that cannot be
    /// downloaded again.
    ///
    /// Through the rule a pack update really applies, not a stand-in for it,
    /// because that rule is the whole protection: `saves/` is not a content
    /// folder, so a file the player already has is theirs and is kept, and
    /// `datapacks/` *inside* a world is not the instance's own datapacks
    /// folder either.
    #[tokio::test]
    async fn a_world_the_player_has_been_playing_is_not_written_over() {
        let (_scratch, saves) = arrived_world(
            "world-theirs",
            "map.zip",
            &[
                ("Spawn Town/level.dat", b"THE PACK'S"),
                ("Spawn Town/region/r.0.0.mca", b"PRISTINE"),
                ("Spawn Town/region/r.0.1.mca", b"NEW THIS VERSION"),
                ("Spawn Town/datapacks/rules.zip", b"THE PACK'S DATAPACK"),
            ],
        );
        // The world as the player left it: the same folder, their own bytes,
        // and a region they explored that the pack's copy knows nothing about.
        let theirs = saves.join("Spawn Town");
        std::fs::create_dir_all(theirs.join("region")).unwrap();
        std::fs::create_dir_all(theirs.join("datapacks")).unwrap();
        std::fs::write(theirs.join("level.dat"), b"THEIR SAVE").unwrap();
        std::fs::write(theirs.join("region/r.0.0.mca"), b"EXPLORED").unwrap();
        std::fs::write(theirs.join("region/r.9.9.mca"), b"THEIRS ALONE").unwrap();
        std::fs::write(theirs.join("datapacks/rules.zip"), b"THEIR DATAPACK").unwrap();

        let folders = unpack_world(
            &saves.join("map.zip"),
            &saves,
            &crate::instance::pack_update::how_the_packs_files_land(),
            &Progress::silent("t"),
        )
        .await
        .expect("a world zip unpacks");

        assert_eq!(folders, vec!["Spawn Town".to_string()]);
        assert_eq!(
            std::fs::read(theirs.join("level.dat")).unwrap(),
            b"THEIR SAVE",
            "their save was overwritten with the pack's pristine copy"
        );
        assert_eq!(
            std::fs::read(theirs.join("region/r.0.0.mca")).unwrap(),
            b"EXPLORED",
            "a region they had built in was overwritten"
        );
        assert_eq!(
            std::fs::read(theirs.join("region/r.9.9.mca")).unwrap(),
            b"THEIRS ALONE"
        );
        assert_eq!(
            std::fs::read(theirs.join("datapacks/rules.zip")).unwrap(),
            b"THEIR DATAPACK",
            "a datapack inside their world is not the instance's datapacks folder"
        );
        assert_eq!(
            std::fs::read(theirs.join("region/r.0.1.mca")).unwrap(),
            b"NEW THIS VERSION",
            "and a file they do not have still arrives, or the new version reaches them \
             at all"
        );
    }

    /// A map published in chapters is several worlds side by side in one
    /// archive, and every one of them is a world the player downloaded. The
    /// import deletes the zip once it has unpacked it, so a world left behind
    /// here is one nobody can get back without downloading the pack again.
    #[tokio::test]
    async fn every_world_in_one_archive_is_unpacked() {
        let (_scratch, saves) = arrived_world(
            "world-siblings",
            "chapters.zip",
            &[
                ("Chapter 1/level.dat", b"ONE"),
                ("Chapter 1/region/r.0.0.mca", b"REGION ONE"),
                ("Chapter 2/level.dat", b"TWO"),
                ("Chapter 2/region/r.0.0.mca", b"REGION TWO"),
                // Deeper than the shallowest, so it is part of Chapter 2
                // rather than a world of its own.
                ("Chapter 2/datapacks/inner/level.dat", b"DEEPER"),
            ],
        );

        let folders = unpack_world(
            &saves.join("chapters.zip"),
            &saves,
            &Overrides::replace(),
            &Progress::silent("t"),
        )
        .await
        .expect("a world zip unpacks");

        assert_eq!(
            folders,
            vec!["Chapter 1".to_string(), "Chapter 2".to_string()]
        );
        assert_eq!(
            std::fs::read(saves.join("Chapter 1/level.dat")).unwrap(),
            b"ONE"
        );
        assert_eq!(
            std::fs::read(saves.join("Chapter 2/region/r.0.0.mca")).unwrap(),
            b"REGION TWO",
            "the second world arrived without the rest of itself"
        );
        assert!(
            !saves.join("Chapter 2/Chapter 2").exists(),
            "a world was unpacked inside itself"
        );
    }

    /// The other shape: `level.dat` at the archive's root, where nothing
    /// inside the zip says what the world is called and the file's own name is
    /// all there is to go on.
    #[tokio::test]
    async fn a_world_zipped_at_its_root_is_named_after_the_file() {
        let (_scratch, saves) = arrived_world(
            "world-root",
            "Spawn Town.zip",
            &[
                ("level.dat", b"NBT"),
                ("region/r.0.0.mca", b"REGION"),
                ("icon.png", b"PNG"),
            ],
        );

        let folders = unpack_world(
            &saves.join("Spawn Town.zip"),
            &saves,
            &Overrides::replace(),
            &Progress::silent("t"),
        )
        .await
        .expect("a world zip unpacks");

        assert_eq!(folders, vec!["Spawn Town".to_string()]);
        assert_eq!(
            std::fs::read(saves.join("Spawn Town/level.dat")).unwrap(),
            b"NBT"
        );
        assert_eq!(
            std::fs::read(saves.join("Spawn Town/region/r.0.0.mca")).unwrap(),
            b"REGION"
        );
        assert!(
            !saves.join("level.dat").exists(),
            "a world unpacked loose into saves/ is a world the game does not read"
        );
    }

    /// A zip CurseForge files under its worlds class that holds no `level.dat`
    /// is not a world. It is left exactly as it arrived, because a save folder
    /// invented out of a guess shows up in the singleplayer list and nothing
    /// in the game will open it.
    #[tokio::test]
    async fn a_zip_with_no_level_dat_is_not_a_world() {
        let (_scratch, saves) = arrived_world(
            "world-none",
            "resources.zip",
            &[("pack.mcmeta", b"{}"), ("assets/icon.png", b"PNG")],
        );

        let folders = unpack_world(
            &saves.join("resources.zip"),
            &saves,
            &Overrides::replace(),
            &Progress::silent("t"),
        )
        .await
        .expect("a zip that is not a world is not a failure");

        assert!(folders.is_empty(), "{folders:?}");
        let left: Vec<String> = std::fs::read_dir(&saves)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            left,
            vec!["resources.zip".to_string()],
            "something was unpacked out of a zip holding no world"
        );
    }

    /// A world zip is somebody else's archive. An entry that climbs out of the
    /// folder it is being unpacked into is skipped, and a wrapping folder that
    /// names its way out of `saves/` is not a world shape at all.
    #[tokio::test]
    async fn a_world_that_climbs_out_of_its_folder_does_not_get_out() {
        let (scratch, saves) = arrived_world(
            "world-climb",
            "map.zip",
            &[("level.dat", b"NBT"), ("../../pwned.txt", b"OWNED")],
        );
        let root = scratch.paths.root.clone();

        let folders = unpack_world(
            &saves.join("map.zip"),
            &saves,
            &Overrides::replace(),
            &Progress::silent("t"),
        )
        .await
        .expect("the world itself still unpacks");
        assert_eq!(folders, vec!["map".to_string()]);
        assert!(saves.join("map/level.dat").exists());
        for escaped in [root.join("pwned.txt"), saves.join("pwned.txt")] {
            assert!(!escaped.exists(), "{} was written", escaped.display());
        }

        // And the same trick played with the wrapping folder, which would
        // otherwise be unpacked as `saves/..`.
        let (climbing, saves) = arrived_world("world-climb-root", "map.zip", &[("../level.dat", b"NBT")]);
        let root = climbing.paths.root.clone();
        let folders = unpack_world(
            &saves.join("map.zip"),
            &saves,
            &Overrides::replace(),
            &Progress::silent("t"),
        )
        .await
        .expect("a zip Geode will not make a folder for is not a failure");
        assert!(
            folders.is_empty(),
            "a world said to live outside saves/ was unpacked anyway"
        );
        assert!(!root.join("level.dat").exists(), "it was written outside saves/");
        assert!(!saves.join("level.dat").exists());
    }

    /// The budget a pack's overrides are held to, on a world for the same
    /// reason: a zip that is small to download and vast unpacked would fill
    /// the disk. The player keeps the zip, and the import names it.
    #[tokio::test]
    async fn a_world_that_inflates_past_its_budget_is_stopped() {
        let (_scratch, saves) = arrived_world(
            "world-bomb",
            "bomb.zip",
            &[
                ("Bomb/level.dat", b"NBT"),
                ("Bomb/region/r.0.0.mca", &vec![0u8; 17 * MIB]),
            ],
        );
        let archive = saves.join("bomb.zip");
        assert!(
            std::fs::metadata(&archive).unwrap().len() < MIB as u64,
            "not a bomb"
        );

        let err = unpack_world(
            &archive,
            &saves,
            &Overrides::replace(),
            &Progress::silent("t"),
        )
        .await
        .expect_err("a world that unpacks past its budget must be refused");

        assert!(err.to_string().contains("fill the disk"), "{err}");
        assert!(
            !saves.join("Bomb/level.dat").exists(),
            "the refused run left part of a world behind"
        );
        assert!(archive.exists(), "the zip the player can still unpack was deleted");
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

    // -----------------------------------------------------------------------
    // What the reader will and will not put up with
    // -----------------------------------------------------------------------

    /// An index with whatever `files` the caller hands over, as JSON.
    fn index_of(files: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "1",
            "name": "P",
            "files": files,
            "dependencies": { "minecraft": "1.21.1" },
        }))
        .unwrap()
    }

    /// A client that cannot reach anything, so a test that fetches fails
    /// rather than going out to the real internet.
    fn offline() -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap()
    }

    /// The format does not default `downloads`, so one entry missing the key
    /// made the whole index unparsable here while Prism imported the pack and
    /// complained about that one file. Two hundred mods are worth more than
    /// the one nobody can fetch — and that one is named rather than quietly
    /// dropped.
    #[tokio::test]
    async fn one_file_without_a_downloads_key_does_not_reject_the_pack() {
        use zip::CompressionMethod::Deflated;
        let scratch = scratch_dir("nodownloads");
        let dir = scratch.paths.root.clone();
        let archive = dir.join("pack.mrpack");
        let index = index_of(serde_json::json!([
            {
                "path": "mods/here.jar",
                "hashes": {},
                "fileSize": 5,
                "downloads": ["http://127.0.0.1:9/here.jar"],
            },
            { "path": "mods/nowhere.jar", "hashes": {} },
        ]));
        write_zip(&archive, &[("modrinth.index.json", &index, Deflated)]);

        let (paths, mut inst) = unpacking(&dir);
        let game = paths.game_dir(&inst.id);
        // Already on disk at the size the index states, so the one entry that
        // does name a download needs no fetching.
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/here.jar"), b"here!").unwrap();

        let unfetchable = import_into(
            &offline(),
            &paths,
            &mut inst,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .expect("a pack with one unfetchable file still imports");

        assert_eq!(unfetchable, vec!["mods/nowhere.jar".to_string()]);
        assert!(game.join("mods/here.jar").is_file(), "the rest of the pack");
    }

    /// Several hosts per file is legal, and a mirror listed second is there
    /// for the day the first one is gone. Only the first was ever tried, so a
    /// pack that named a dead host first failed whole.
    #[tokio::test]
    async fn a_second_download_url_is_tried_when_the_first_fails() {
        use http_body_util::Full;
        use hyper::body::Bytes;
        use hyper::service::service_fn;
        use hyper::Response;
        use hyper_util::rt::TokioIo;
        use zip::CompressionMethod::Deflated;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mirror = format!("http://{}/mirrored.jar", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let service = service_fn(|_request| async {
                        Ok::<_, std::convert::Infallible>(Response::new(Full::new(
                            Bytes::from_static(b"the jar"),
                        )))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });

        let scratch = scratch_dir("mirror");
        let dir = scratch.paths.root.clone();
        let archive = dir.join("pack.mrpack");
        let index = index_of(serde_json::json!([{
            "path": "mods/mirrored.jar",
            "hashes": {},
            // Port 9 discards whatever reaches it and nothing listens there,
            // so the first host refuses at once.
            "downloads": ["http://127.0.0.1:9/mirrored.jar", mirror],
        }]));
        write_zip(&archive, &[("modrinth.index.json", &index, Deflated)]);

        let (paths, mut inst) = unpacking(&dir);
        import_into(
            &reqwest::Client::new(),
            &paths,
            &mut inst,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .expect("the mirror should have served it");

        let landed = paths.game_dir(&inst.id).join("mods/mirrored.jar");
        assert_eq!(std::fs::read(landed).unwrap(), b"the jar");
    }

    /// `formatVersion` and `game` were read and then ignored, so a pack
    /// written in a revision of the format this build has never seen was read
    /// as if it were this one — which is a wrong instance rather than a
    /// refusal. Modrinth's own validator and Prism both check them.
    #[tokio::test]
    async fn a_pack_from_a_future_format_version_is_refused() {
        use zip::CompressionMethod::Deflated;
        let scratch = scratch_dir("futurepack");
        let dir = scratch.paths.root.clone();

        for (tag, index, say) in [
            (
                "version",
                serde_json::json!({
                    "formatVersion": 2, "game": "minecraft", "versionId": "1",
                    "name": "P", "files": [], "dependencies": {},
                }),
                "version 2 of Modrinth's pack format",
            ),
            (
                "game",
                serde_json::json!({
                    "formatVersion": 1, "game": "terraria", "versionId": "1",
                    "name": "P", "files": [], "dependencies": {},
                }),
                "for terraria, not Minecraft",
            ),
        ] {
            let archive = dir.join(format!("{tag}.mrpack"));
            let raw = serde_json::to_vec(&index).unwrap();
            write_zip(&archive, &[("modrinth.index.json", &raw, Deflated)]);
            let err = preview(archive)
                .await
                .unwrap_err();
            assert!(err.to_string().contains(say), "{tag}: {err}");
        }
    }

    /// The `env` pair is the pack's own word on which side a file belongs on,
    /// and import threw it away — so every file a pack installed was
    /// classified by nobody. Written and read through one pair of functions,
    /// so a round trip is the whole agreement.
    #[test]
    fn a_side_survives_the_pack_that_carried_it() {
        use crate::instance::jar::Side;
        for side in [Side::Client, Side::Server, Side::Both] {
            assert_eq!(side_from_env(&env_for(Some(side))), Some(side), "{side:?}");
        }
        // Half an `env` settles nothing, and a guess here keeps a mod off a
        // server or sends a rendering mod to one.
        assert_eq!(
            side_from_env(&MrEnv {
                client: "required".into(),
                server: String::new(),
            }),
            None
        );
    }
}

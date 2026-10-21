//! Reading a Prism Launcher / MultiMC instance.
//!
//! An instance is a folder rather than a pack: `instance.cfg` and
//! `mmc-pack.json` at its root, the game under `minecraft/`, and every jar
//! present as bytes rather than as something to fetch — Prism's own importer
//! takes the tree as it finds it and downloads nothing, so a zipped instance
//! has no way to say "fetch this".
//!
//! What makes it worth reading carefully is the packwiz index Prism keeps
//! beside the jars: `mods/.index/<slug>.pw.toml` names the project and version
//! each file came from, so a hundred mods arrive named instead of anonymous.
//! Nothing here turns bytes into a platform reference — the index already did
//! that, and [`crate::install::identity`] remains the only place that asks a
//! platform what a file is.
//!
//! Geode never goes looking for Prism's data directory. The user points at an
//! instance folder or at an exported zip, and this reads what they pointed at
//! and nothing above it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::install::identity::{travel, Ask, Carry, Catalogue, Copied, Identity, Travel};
use crate::meta::Loader;
use crate::net::Progress;
use crate::paths::Paths;

use super::content::{ContentKind, ContentRecord};
use super::export::Selection;
use super::jar::Side;
use super::modpack::{
    instance_input, interop_loader_version, lay_out, ExportReport, Extras, Journey, Layout,
};
use super::{Instance, InstanceOverrides};

const CFG: &str = "instance.cfg";
const PACK: &str = "mmc-pack.json";
/// The game directory Prism writes, and the one it prefers on read.
const GAME_DIR: &str = "minecraft";
/// The one it falls back to, and only when there is no `minecraft/`.
const OLD_GAME_DIR: &str = ".minecraft";

/// What `mmc-pack.json` calls Minecraft itself.
const MINECRAFT_UID: &str = "net.minecraft";

/// What `mmc-pack.json` calls each loader, and what Prism shows it as.
///
/// Another launcher's names for the loaders Geode already knows, not a claim
/// about which versions anything supports — a uid carries no version, and the
/// version comes from the component beside it. Anything else in the list
/// (`org.lwjgl3`, most often) is ignored on read, as Prism ignores what it
/// does not recognise, and left out on write, because Prism's own resolver
/// puts it back.
///
/// One table for both directions, so a uid cannot come to mean one loader
/// reading and another writing.
const LOADER_UIDS: [(&str, Loader, &str); 4] = [
    ("net.fabricmc.fabric-loader", Loader::Fabric, "Fabric Loader"),
    ("org.quiltmc.quilt-loader", Loader::Quilt, "Quilt Loader"),
    ("net.minecraftforge", Loader::Forge, "Forge"),
    ("net.neoforged", Loader::NeoForge, "NeoForge"),
];

/// Where Prism keeps the packwiz entry for each kind of file. The first folder
/// is the one written; every one is read.
///
/// `shaderpacks/` is the odd one: its `.pw.toml` files sit flat beside the
/// zips with no `.index` folder at all, which is provenance an importer that
/// only looks in `.index` loses. Flat is where they sit *now*, and only now:
/// Prism moves them out of `shaderpacks/.index` with a migration that runs the
/// first time that folder is loaded, so an instance whose shader page was
/// never opened under a new Prism — and every MultiMC-era export — still keeps
/// them in the old place.
const INDEXED: [(ContentKind, &[&str]); 3] = [
    (ContentKind::Mod, &["mods/.index"]),
    (ContentKind::ResourcePack, &["resourcepacks/.index"]),
    (
        ContentKind::ShaderPack,
        &["shaderpacks", "shaderpacks/.index"],
    ),
];

/// A Prism instance as its own two files describe it.
#[derive(Debug, Clone)]
pub struct PrismInstance {
    pub name: String,
    pub notes: String,
    pub mc_version: String,
    pub loader: Loader,
    pub loader_version: String,
    /// Only the values whose `Override*` gate was set. Prism ignores the
    /// others itself, so reading them would import settings the instance was
    /// never running under.
    pub overrides: InstanceOverrides,
    /// Where the instance's own files sit inside the zip: `""` when it was
    /// zipped flat the way Prism's export writes it, `"Instance/"` when
    /// somebody zipped the folder it sits in. Always `""` for a folder.
    pub prefix: String,
    /// `minecraft` or `.minecraft` — the game directory to bring across.
    pub game_dir: String,
    /// How many files that directory holds, so the import can say.
    pub file_count: usize,
}

impl PrismInstance {
    /// The game directory's path inside the zip, which is the root an
    /// extraction strips.
    pub fn game_root(&self) -> String {
        format!("{}{}", self.prefix, self.game_dir)
    }
}

/// The instance inside an exported zip, when it holds one.
///
/// `instance.cfg` is matched on its basename at any depth and its directory
/// becomes the instance root, which is what Prism does — so a zip somebody
/// made from the folder above the instance still reads. The shallowest match
/// wins, where Prism takes whichever it meets first in the zip's own order:
/// one instance has one `instance.cfg`, and a zip with several reads the same
/// way every time this way.
pub fn from_zip(
    zip: &mut zip::ZipArchive<std::fs::File>,
    picked: &Path,
) -> Result<Option<PrismInstance>> {
    let Some(prefix) = zip
        .file_names()
        .filter(|name| name.rsplit('/').next() == Some(CFG))
        .min_by_key(|name| (name.matches('/').count(), *name))
        .map(|name| name[..name.len() - CFG.len()].to_string())
    else {
        return Ok(None);
    };

    let holds = |dir: &str| {
        let under = format!("{prefix}{dir}/");
        zip.file_names().any(|name| name.starts_with(&under))
    };
    let game_dir = if !holds(GAME_DIR) && holds(OLD_GAME_DIR) {
        OLD_GAME_DIR
    } else {
        GAME_DIR
    };
    let under = format!("{prefix}{game_dir}/");
    let file_count = zip
        .file_names()
        .filter(|name| name.starts_with(&under) && !name.ends_with('/'))
        .count();

    let Some(cfg) = entry_text(zip, &format!("{prefix}{CFG}"))? else {
        return Ok(None);
    };
    let Some(pack) = entry_text(zip, &format!("{prefix}{PACK}"))? else {
        return Err(missing_pack());
    };

    let mut instance = from_texts(&cfg, &pack, &fallback_name(picked))?;
    instance.prefix = prefix;
    instance.game_dir = game_dir.to_string();
    instance.file_count = file_count;
    Ok(Some(instance))
}

/// The instance in a folder the user pointed at, when it holds one.
///
/// The folder itself, and nothing above or beside it: an instance directory is
/// what the user picked, and its parent is Prism's own business.
pub fn from_folder(dir: &Path) -> Result<Option<PrismInstance>> {
    let Ok(cfg) = std::fs::read_to_string(dir.join(CFG)) else {
        return Ok(None);
    };
    let Ok(pack) = std::fs::read_to_string(dir.join(PACK)) else {
        return Err(missing_pack());
    };

    let game_dir = if !dir.join(GAME_DIR).is_dir() && dir.join(OLD_GAME_DIR).is_dir() {
        OLD_GAME_DIR
    } else {
        GAME_DIR
    };

    let mut instance = from_texts(&cfg, &pack, &fallback_name(dir))?;
    instance.game_dir = game_dir.to_string();
    instance.file_count = count_files(&dir.join(game_dir));
    Ok(Some(instance))
}

/// Every file the instance's packwiz index names, read out of the game
/// directory it was brought into.
///
/// Best-effort by design: an instance is perfectly usable with imperfect
/// labels, so an entry that cannot be read leaves its file untracked rather
/// than failing an import that has already copied everything in.
pub async fn index_records(game_dir: &Path) -> Vec<ContentRecord> {
    let mut out = Vec::new();
    for (kind, folders) in INDEXED {
        for &folder in folders {
            let Ok(mut entries) = tokio::fs::read_dir(game_dir.join(folder)).await else {
                continue;
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name();
                let Some(slug) = name.to_str().and_then(|n| n.strip_suffix(".pw.toml")) else {
                    continue;
                };
                let Ok(text) = tokio::fs::read_to_string(entry.path()).await else {
                    continue;
                };
                if let Some(record) = record_from_index(kind, slug, &text) {
                    out.push(record);
                }
            }
        }
    }
    out
}

/// One `.pw.toml` as a content record, when it says enough to be one.
///
/// Both tables are required, as they are to packwiz's own reader, which
/// discards an entry whole when either is missing — so a half-written entry
/// degrades to no provenance rather than to wrong provenance, and the jar
/// simply arrives untracked.
///
/// The ids are read in the shapes Prism writes: Modrinth's as strings, and
/// CurseForge's as integers. Anything else is an entry this does not
/// understand, which is the discarding case again.
fn record_from_index(kind: ContentKind, slug: &str, text: &str) -> Option<ContentRecord> {
    let root: toml::Table = text.parse().ok()?;
    root.get("download")?.as_table()?;
    let update = root.get("update")?.as_table()?;

    // The enabled name, which is both what the jar on disk is called and how a
    // content record is keyed.
    //
    // A bare name and nothing else. A recorded name is joined onto the
    // content folder and deleted when the file is updated — that is how
    // `install::content::install_into` clears the previous file for a project
    // — so an entry naming `../../something` would have the next Update reach
    // outside the instance. Somebody else's index is not a path, and an entry
    // that puts one there is the discarding case again.
    let file_name = root.get("filename")?.as_str()?.to_string();
    if Path::new(&file_name).file_name() != Some(file_name.as_ref()) {
        return None;
    }

    let (provider, project_id, version_id) =
        if let Some(modrinth) = update.get("modrinth").and_then(|v| v.as_table()) {
            (
                "modrinth",
                modrinth.get("mod-id")?.as_str()?.to_string(),
                modrinth.get("version")?.as_str()?.to_string(),
            )
        } else {
            // CurseForge's, then — and an entry with neither table is one this
            // does not understand, which is the discarding case again.
            let curseforge = update.get("curseforge").and_then(|v| v.as_table())?;
            (
                "curseforge",
                curseforge.get("project-id")?.as_integer()?.to_string(),
                curseforge.get("file-id")?.as_integer()?.to_string(),
            )
        };

    Some(ContentRecord {
        name: root
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&file_name)
            .to_string(),
        file_name,
        kind,
        provider: provider.to_string(),
        project_id,
        version_id,
        // Every `x-prismlauncher-*` key is optional: the writer always emits
        // them, and plenty of real index files do not have them.
        version_number: root
            .get("x-prismlauncher-version-number")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        // Prism names the file after the project's slug, which is the one
        // thing in the entry the format itself does not carry.
        slug: Some(slug.to_string()),
        icon_url: None,
        dependency: false,
        origin: Default::default(),
        // The entry lists what the mod depends on, but by project rather than
        // by version, so nobody has looked at what *this* build needs.
        requires: None,
        side: side_from_index(root.get("side").and_then(|v| v.as_str())),
        server_off: false,
    })
}

/// packwiz's own `side` values. Anything else — including the empty string
/// Prism writes when nobody said — is nobody having said.
fn side_from_index(side: Option<&str>) -> Option<Side> {
    match side? {
        "client" => Some(Side::Client),
        "server" => Some(Side::Server),
        "both" => Some(Side::Both),
        _ => None,
    }
}

/// What `instance.cfg` and `mmc-pack.json` say, whichever source they came
/// from. The caller fills in where they came from.
fn from_texts(cfg: &str, pack: &str, fallback_name: &str) -> Result<PrismInstance> {
    let general = general(cfg);

    // Prism refuses an instance whose type is anything but OneSix, and so does
    // this: another type is a layout neither launcher reads.
    if let Some(kind) = general.get("InstanceType").filter(|t| !t.is_empty()) {
        if !kind.eq_ignore_ascii_case("OneSix") {
            return Err(Error::msg(format!(
                "this instance's InstanceType is {kind}, and only a OneSix instance can be \
                 imported — Prism itself would not open this one either"
            )));
        }
    }

    let profile: Profile = serde_json::from_str(pack).map_err(|e| Error::json(PACK, e))?;
    if profile.format_version != 1 {
        return Err(Error::msg(format!(
            "this instance's {PACK} says it is format version {}, and only version 1 can be \
             read — Prism itself refuses every other",
            profile.format_version
        )));
    }

    let mut mc_version = String::new();
    let mut loader = Loader::Vanilla;
    let mut loader_version = String::new();
    for component in profile.components {
        // A component Prism is not applying is not part of this instance:
        // `Component::isEnabled` skips one, and the version page is where a
        // loader gets turned off. Reading it anyway would install a loader the
        // exported instance runs vanilla without.
        if component.disabled {
            continue;
        }
        if component.uid == MINECRAFT_UID {
            mc_version = component.version;
        } else if let Some((_, named, _)) =
            LOADER_UIDS.iter().find(|(uid, _, _)| *uid == component.uid)
        {
            // Forge's version goes through as it stands. Prism writes the
            // short form (`47.2.0`) where Geode stores the long one
            // (`1.20.1-47.2.0`), and `meta::forge` puts the game version back
            // on a version that does not carry it.
            loader = *named;
            loader_version = component.version;
        }
    }
    if mc_version.is_empty() {
        return Err(Error::msg(format!(
            "this instance's {PACK} does not say which Minecraft version it is for, so there is \
             nothing to install"
        )));
    }

    let mut overrides = InstanceOverrides::default();
    // Each value only when its own gate is set. Prism falls back to its global
    // settings for an ungated value, so reading one would import a setting the
    // instance was never running under.
    if gated(&general, "OverrideMemory") {
        // MiB, which is what Geode holds too.
        overrides.memory_mb = general.get("MaxMemAlloc").and_then(|v| v.parse().ok());
    }
    if gated(&general, "OverrideJavaArgs") {
        overrides.java_args = general
            .get("JvmArgs")
            .filter(|args| !args.is_empty())
            .cloned();
    }
    if gated(&general, "OverrideWindow") {
        overrides.width = general.get("MinecraftWinWidth").and_then(|v| v.parse().ok());
        overrides.height = general
            .get("MinecraftWinHeight")
            .and_then(|v| v.parse().ok());
    }
    // `JavaPath` is never read, gate or no gate: a path to somebody else's
    // runtime means nothing on this machine, and Geode picks its own.

    Ok(PrismInstance {
        name: general
            .get("name")
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| fallback_name.to_string()),
        notes: general.get("notes").cloned().unwrap_or_default(),
        mc_version,
        loader,
        loader_version,
        overrides,
        prefix: String::new(),
        game_dir: GAME_DIR.to_string(),
        file_count: 0,
    })
}

/// `mmc-pack.json`, the version profile.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    /// Exactly the integer 1, and Prism fails the whole file over any other
    /// value — which leaves an instance that cannot launch, so an instance
    /// claiming another version is not one to read.
    format_version: u32,
    #[serde(default)]
    components: Vec<Component>,
}

#[derive(Debug, Deserialize)]
struct Component {
    uid: String,
    #[serde(default)]
    version: String,
    /// Written by Prism for a component its resolver is to leave out. Absent
    /// from most profiles, which is the enabled case.
    #[serde(default)]
    disabled: bool,
}

/// The `[General]` section of a QSettings-style INI.
///
/// Not TOML: a line is split on its first `=` and everything after it is the
/// value, which QSettings then has its own escapes for — see
/// [`ini_unescaped`], which is what turns a note's `\n` back into a line
/// break. Every other section is skipped, `[UI]` above all: it is Prism's
/// table column widths, and its keys contain backslashes.
fn general(text: &str) -> HashMap<&str, String> {
    let mut out = HashMap::new();
    // QSettings writes `[General]` first, but a key before any header belongs
    // to no section and reads the same way.
    let mut in_general = true;
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_general = header.eq_ignore_ascii_case("General");
            continue;
        }
        if !in_general || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.insert(key.trim(), ini_unescaped(value.trim()));
        }
    }
    out
}

/// What QSettings would have read back out of one INI value.
///
/// The counterpart of [`ini_escaped`], and the reason a note Prism wrote over
/// three lines arrives as three lines rather than with the literal `\n` the
/// file holds.
fn ini_unescaped(value: &str) -> String {
    let value = value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value);
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            // A backslash before anything else stands for the character after
            // it, which covers the `\\` and `\"` QSettings writes.
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// One value as QSettings writes it into an INI file.
///
/// `instance.cfg` is QSettings' own format, and a value written raw into it is
/// how an instance Prism cannot read gets made: a note over two lines would
/// turn its second line into a key, and a note holding `=` would turn into a
/// key with the wrong name. So the characters that would end the line become
/// backslash escapes, and a value whose own characters would otherwise be read
/// as a list separator or a comment is quoted whole, which is what QSettings
/// does with them.
fn ini_escaped(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    if value.contains([';', ',', '=', '"']) || value.starts_with(' ') || value.ends_with(' ') {
        return format!("\"{out}\"");
    }
    out
}

/// Whether one of the `Override*` gates is on.
fn gated(general: &HashMap<&str, String>, key: &str) -> bool {
    general
        .get(key)
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// One of the instance's own small text files.
///
/// Held to the same ceiling as a pack's index, and for the same reason: read
/// whole and then parsed, it is otherwise a way to exhaust memory from a small
/// zip.
fn entry_text(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<Option<String>> {
    let Ok(entry) = zip.by_name(name) else {
        return Ok(None);
    };
    let raw = super::modpack::read_json_entry(entry, name)?;
    Ok(Some(String::from_utf8_lossy(&raw).into_owned()))
}

/// An instance with no version profile beside its `instance.cfg`.
fn missing_pack() -> Error {
    Error::msg(format!(
        "this is a Prism instance with no {PACK} beside its {CFG}, so there is no Minecraft \
         version in it to read"
    ))
}

/// What to call an instance whose `instance.cfg` does not say: the name of the
/// thing the user picked, which for an instance folder is the name Prism shows
/// it under.
fn fallback_name(picked: &Path) -> String {
    picked
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Imported instance")
        .to_string()
}

/// How many files a folder holds, down its whole tree.
///
/// Links are not followed, which is what the copy that comes after does not do
/// either.
fn count_files(dir: &Path) -> usize {
    let mut stack = vec![dir.to_path_buf()];
    let mut files = 0;
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                Ok(kind) if kind.is_file() => files += 1,
                _ => {}
            }
        }
    }
    files
}

// ---------------------------------------------------------------------------
// Writing a Prism instance
// ---------------------------------------------------------------------------

/// What `iconKey` names the instance's artwork by in the zip.
///
/// Prism looks for `<iconKey>.*` at the instance root, accepting any of
/// [`ICON_SUFFIXES`], and installs what it finds into its icon list under that
/// key — a list that is **global to the install**, and whose existing entry
/// under the key is deleted and replaced. So the key names this instance, the
/// way Prism scopes its own imports' (`Modrinth_%1_Icon`): one constant would
/// have the second Geode instance somebody imports take over the artwork of
/// the first, which still points at the key.
fn icon_key(instance: &Instance) -> String {
    format!("Geode_{}_Icon", instance.id)
}

/// Prism's own stand-in, and what an instance with no artwork is written with.
const DEFAULT_ICON_KEY: &str = "default";

/// The image formats Prism will take as an instance icon.
///
/// Its own list, not a guess: anything else at the root is skipped by its
/// icon search, so writing one would leave a file in the zip that nothing
/// reads.
const ICON_SUFFIXES: [&str; 7] = ["svg", "png", "ico", "gif", "jpg", "jpeg", "webp"];

/// Write the instance out as a zip Prism opens as an instance of its own —
/// settings, loader profile, artwork and all.
///
/// **Every file travels as bytes, named or not.** `processMultiMC` takes the
/// extracted tree as it finds it and downloads nothing, so this format has no
/// way to say "fetch this" and leaving a jar out would hand Prism a mod it
/// shows as missing and will not go and get. The packwiz index written beside
/// the jars is therefore provenance rather than a saving: it is the difference
/// between a hundred named mods the recipient's Prism can check for updates
/// and a hundred anonymous jars. A sharer who wants Prism to do the
/// downloading wants an `.mrpack`, which Prism turns back into this same index
/// on the way in.
///
/// So [`ExportReport::referenced`] counts the files the recipient's launcher
/// will know by name, and the embedded counts say why the rest arrive
/// anonymous — not, as in every other format, what the zip did and did not
/// have to carry.
pub async fn export_instance(
    ask: Ask<'_>,
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    selection: &Selection,
    progress: &Progress,
) -> Result<ExportReport> {
    let catalogue = crate::install::identity::for_instance(ask, paths, instance, progress).await?;
    write_instance(paths, instance, dest, selection, &catalogue, progress).await
}

/// The same writer, from a catalogue already built — the seam a preview uses,
/// and what the tests write instances through.
pub(super) async fn write_instance(
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    selection: &Selection,
    catalogue: &Catalogue,
    progress: &Progress,
) -> Result<ExportReport> {
    let input = instance_input(paths, instance, selection);
    let game_dir = input.root;
    let mut report = ExportReport {
        trouble: catalogue.trouble.clone(),
        ..Default::default()
    };

    // The names and slugs are the instance's own records' to give: a hash
    // lookup answers with ids and a URL, and nobody's project page is fetched
    // to write a pack.
    let records: HashMap<(ContentKind, &str), &ContentRecord> = instance
        .content
        .iter()
        .map(|record| ((record.kind, record.file_name.as_str()), record))
        .collect();

    let mut index: Extras = Vec::new();
    for planned in plan_instance(catalogue, instance, selection) {
        let Planned {
            ident,
            journey,
            entry: planned,
        } = planned;
        report.count(&journey);
        let Some(PlannedEntry {
            folder,
            slug,
            download,
            update,
        }) = planned
        else {
            continue;
        };
        let record = records
            .get(&(ident.kind, ident.file_name.as_str()))
            .copied();

        // Where it belongs — the folder, then what the jar itself says, then
        // the listing — the same three answers in the same order the pack
        // writers ask them in.
        let probe = game_dir.join(&ident.disk_rel);
        let jar_side = tokio::task::spawn_blocking(move || crate::instance::jar::read(&probe))
            .await
            .ok()
            .flatten()
            .and_then(|meta| meta.side);
        let side = super::content::side_of(ident.kind, jar_side, ident.listed_side);

        let entry = IndexEntry {
            name: record
                .map(|record| record.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| ident.file_name.clone()),
            filename: ident.file_name.clone(),
            side: match side {
                Side::Client => "client",
                Side::Server => "server",
                Side::Both => "both",
            },
            // The one `x-prismlauncher-*` key written, because it is the one
            // Geode holds an answer to. The others are claims about what a mod
            // supports — which loaders, which Minecraft versions — and Geode
            // knows only what this instance is, not what the mod would run on.
            version_number: record
                .map(|record| record.version_number.clone())
                .filter(|version| !version.is_empty()),
            download,
            update,
        };

        index.push((
            format!("{GAME_DIR}/{folder}/{slug}.pw.toml"),
            toml::to_string(&entry)
                .map_err(|e| {
                    Error::msg(format!(
                        "could not write the index entry for {}: {e}",
                        ident.file_name
                    ))
                })?
                .into_bytes(),
        ));
    }

    let icon = icon_entry(paths, instance).await;
    let key = if icon.is_some() {
        icon_key(instance)
    } else {
        DEFAULT_ICON_KEY.to_string()
    };
    let mut named: Extras = vec![
        (CFG.to_string(), instance_cfg(instance, &key)),
        (PACK.to_string(), mmc_pack(instance)?),
    ];
    named.extend(icon);
    named.extend(index);

    progress.stage("Writing instance", 1).await;

    let walk = input.walk;
    let stale = index_on_disk(&game_dir).await;
    report.unnameable = tokio::task::spawn_blocking(move || {
        lay_out(
            &dest,
            &game_dir,
            &walk,
            Layout {
                named,
                // The `.pw.toml` files already on disk, and nothing else: an
                // instance imported from Prism brought its old index along,
                // and letting it travel too would put a second entry beside
                // every one written here whose slug differs, and would keep an
                // entry for a jar since deleted — metadata Prism shows as a
                // mod that is not there. The catalogue is the one source.
                //
                // Every jar travels: see [`export_instance`].
                carried: stale,
                // Only a `.geodepack` has notes for a file to be named in.
                referenced: HashSet::new(),
                // Never a server's pack for its players: what a share serves
                // is the mrpack.
                players_pack: false,
                // Not `overrides/`: these files are the instance itself.
                tree: GAME_DIR,
            },
        )
    })
    .await
    .map_err(|e| Error::msg(format!("writing the instance panicked: {e}")))??;

    progress.finish().await;
    Ok(report)
}

/// One file's journey into a Prism instance, and the index entry for the files
/// that get one.
pub(super) struct Planned<'a> {
    pub(super) ident: &'a Identity,
    pub(super) journey: Journey<'a>,
    /// Present exactly where the journey names the file. Not re-derived by the
    /// writer: working out whether an entry is possible *is* working out the
    /// entry, and doing it twice is how a preview comes to promise a name the
    /// export then does not write. The writer's own, which is why it is not on
    /// show outside this file.
    entry: Option<PlannedEntry>,
}

/// Where one file's packwiz entry goes and what it says, bar the two fields
/// that need the jar itself read.
struct PlannedEntry {
    folder: &'static str,
    slug: String,
    download: IndexDownload,
    update: IndexUpdate,
}

/// What a Prism instance zip would do with each of the catalogue's files.
///
/// Three of the reasons here are this format's and not [`travel`]'s — an
/// instance keeps no packwiz index for a data pack, packwiz knows two
/// providers and no others, and one mod gets one entry however many copies of
/// its jar sit in the folder — which is why this is a pass over the whole
/// catalogue rather than a decision per file.
pub(super) fn plan_instance<'a>(
    catalogue: &'a Catalogue,
    instance: &Instance,
    selection: &Selection,
) -> Vec<Planned<'a>> {
    let records: HashMap<(ContentKind, &str), &ContentRecord> = instance
        .content
        .iter()
        .map(|record| ((record.kind, record.file_name.as_str()), record))
        .collect();

    let mut planned: Vec<Planned<'a>> = Vec::new();
    let mut written: HashSet<String> = HashSet::new();
    // The pack writers' own order, and their rule about what a pack of this
    // selection holds: an index entry for a jar the walk left at home is Prism
    // showing a mod that is not installed.
    for ident in super::modpack::in_writing_order(catalogue, selection) {
        let carried = |why: Copied| Planned {
            ident,
            journey: Journey::Copied(why),
            entry: None,
        };
        let reference = match travel(ident, Carry::PRISM) {
            Travel::Linked(reference) | Travel::Named(reference) => reference,
            Travel::Copied(why) => {
                planned.push(carried(why));
                continue;
            }
        };
        // Where Prism keeps this kind's provenance, when it keeps any at all.
        // A platform names this file and an instance has nowhere to say so:
        // Prism keeps no packwiz index for a data pack. The format's own
        // limit, which is a different thing to hear from "nobody has this".
        let Some(folder) = index_folder(ident.kind) else {
            planned.push(carried(Copied::Elsewhere));
            continue;
        };

        // The two providers are not interchangeable here: what a Modrinth
        // entry says is a URL and a sha512, what a CurseForge one says is a
        // pair of ids and a sha1, and neither reader will take the other's
        // shape.
        let (download, update) = match reference.provider.as_str() {
            "modrinth" => (
                IndexDownload {
                    mode: "url",
                    // The matched file's own URL when a lookup gave one.
                    // Without a lookup there is only the record's ids and the
                    // name on disk, so the URL is built from them — the same
                    // fallback the mrpack writer has, and for the same reason.
                    url: reference.url.clone().unwrap_or_else(|| {
                        format!(
                            "https://cdn.modrinth.com/data/{}/versions/{}/{}",
                            reference.project_id,
                            reference.version_id,
                            urlencoding::encode(&ident.file_name)
                        )
                    }),
                    // The hash algorithm is the provider's, not a preference:
                    // Prism reads a Modrinth entry's hash as sha512 and a
                    // CurseForge entry's as sha1, and verifies the download
                    // with it.
                    hash_format: "sha512",
                    hash: ident.sha512.clone(),
                },
                IndexUpdate {
                    modrinth: Some(ModrinthUpdate {
                        mod_id: reference.project_id.clone(),
                        version: reference.version_id.clone(),
                    }),
                    curseforge: None,
                },
            ),
            "curseforge" => {
                // CurseForge cannot be written as a URL here — the file is
                // fetched through the API with the recipient's own key, which
                // is what `metadata:curseforge` means and why the url is
                // empty. The format types both ids as integers and Prism reads
                // them that way, so a reference whose ids are not numbers is
                // no reference here.
                let (Ok(project_id), Ok(file_id)) = (
                    reference.project_id.parse::<u32>(),
                    reference.version_id.parse::<u32>(),
                ) else {
                    planned.push(carried(Copied::Unknown));
                    continue;
                };
                (
                    IndexDownload {
                        mode: "metadata:curseforge",
                        url: String::new(),
                        hash_format: "sha1",
                        hash: ident.sha1.clone(),
                    },
                    IndexUpdate {
                        modrinth: None,
                        curseforge: Some(CurseForgeUpdate {
                            project_id,
                            file_id,
                        }),
                    },
                )
            }
            // packwiz knows these two providers and no others, and an entry
            // naming a third would be read as one of them — a pair of ids
            // with no room to say whose they are.
            _ => {
                planned.push(carried(Copied::Elsewhere));
                continue;
            }
        };

        // The entry's file name is the project's slug, so one mod gets one
        // entry however many copies of its jar are in the folder. Geode meets
        // this for real: a jar copied under a second name has the same bytes
        // and so the same record.
        let slug = slug_for(
            ident,
            records.get(&(ident.kind, ident.file_name.as_str())).copied(),
        );
        if !written.insert(format!("{folder}/{slug}")) {
            planned.push(Planned {
                ident,
                journey: Journey::Duplicate,
                entry: None,
            });
            continue;
        }

        planned.push(Planned {
            ident,
            journey: Journey::Named(reference),
            entry: Some(PlannedEntry {
                folder,
                slug,
                download,
                update,
            }),
        });
    }
    planned
}

/// Where a packwiz entry for this kind of file goes, when there is anywhere.
///
/// [`INDEXED`] read the other way round, so the reader and the writer cannot
/// come to disagree about where a shader pack's metadata sits — the first
/// folder of the kind's, which is where Prism keeps it today. `None` for a
/// data pack: an instance has no packwiz index for one, so there is nowhere to
/// write its provenance down.
pub(super) fn index_folder(kind: ContentKind) -> Option<&'static str> {
    INDEXED
        .iter()
        .find(|(indexed, _)| *indexed == kind)
        .and_then(|(_, folders)| folders.first().copied())
}

/// What to call one file's index entry.
///
/// The project's slug, which is what Prism names an entry by and what the
/// reader takes back out of the name. A file with no slug recorded falls back
/// to its own name, which is still a stable handle for the one mod — and
/// anything that could leave the folder it is written into is replaced, since
/// this becomes a path inside the zip.
fn slug_for(ident: &Identity, record: Option<&ContentRecord>) -> String {
    let proposed = record
        .and_then(|record| record.slug.clone())
        .filter(|slug| !slug.is_empty())
        .unwrap_or_else(|| {
            Path::new(&ident.file_name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(&ident.file_name)
                .to_string()
        });
    let cleaned: String = proposed
        .chars()
        .map(|ch| match ch {
            'a'..='z' | '0'..='9' | '-' | '_' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            _ => '-',
        })
        .collect();
    match cleaned.trim_matches('-') {
        "" => "mod".to_string(),
        slug => slug.to_string(),
    }
}

/// Every packwiz entry already in the game directory, by its path from it.
async fn index_on_disk(game_dir: &Path) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    for (_, folders) in INDEXED {
        for &folder in folders {
            let Ok(mut entries) = tokio::fs::read_dir(game_dir.join(folder)).await else {
                continue;
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name();
                if name.to_str().is_some_and(|name| name.ends_with(".pw.toml")) {
                    out.insert(PathBuf::from(folder).join(name));
                }
            }
        }
    }
    out
}

/// `instance.cfg`, which is what makes the zip an instance rather than a
/// folder of files.
///
/// Each overridable value is written **with its own gate**, because Prism
/// falls back to its global setting for an ungated one — a `MaxMemAlloc` with
/// no `OverrideMemory` beside it is the thing that silently does nothing. A
/// value Geode does not have is omitted along with its gate rather than
/// written as a zero.
///
/// `JavaPath` is never written, gate or no gate: a path to this machine's Java
/// means nothing on anybody else's, and Geode does not read one on the way in
/// either.
fn instance_cfg(instance: &Instance, icon_key: &str) -> Vec<u8> {
    let mut keys: Vec<(&str, String)> = vec![
        // Without this key Prism reads the whole file with its pre-1.3
        // parser, whose comment and quoting rules are not the QSettings ones
        // [`ini_escaped`] writes: a `#` anywhere in a note or in `JvmArgs`
        // ends the line there, and a quoted value keeps its quotes. Every
        // `instance.cfg` Prism writes carries it.
        ("ConfigVersion", "1.3".to_string()),
        // Without this the instance does not appear in Prism at all: its
        // instance list refuses any type but `OneSix`.
        ("InstanceType", "OneSix".to_string()),
        ("name", instance.name.clone()),
        ("notes", instance.notes.clone()),
        ("iconKey", icon_key.to_string()),
        // Not a pack whose updates Prism should go looking for. Nothing here
        // came from a CurseForge or Modrinth pack *page*, so there is no pack
        // id to track, and claiming one would have Prism offer updates from
        // somewhere this instance never came from.
        ("ManagedPack", "false".to_string()),
    ];
    if let Some(memory) = instance.overrides.memory_mb {
        keys.push(("OverrideMemory", "true".to_string()));
        keys.push(("MaxMemAlloc", memory.to_string()));
        // What Geode itself launches with, so the instance behaves in Prism
        // the way it behaves here.
        keys.push(("MinMemAlloc", (memory / 2).max(512).to_string()));
    }
    if let Some(args) = instance
        .overrides
        .java_args
        .as_ref()
        .filter(|args| !args.is_empty())
    {
        keys.push(("OverrideJavaArgs", "true".to_string()));
        keys.push(("JvmArgs", args.clone()));
    }
    if instance.overrides.width.is_some() || instance.overrides.height.is_some() {
        keys.push(("OverrideWindow", "true".to_string()));
        if let Some(width) = instance.overrides.width {
            keys.push(("MinecraftWinWidth", width.to_string()));
        }
        if let Some(height) = instance.overrides.height {
            keys.push(("MinecraftWinHeight", height.to_string()));
        }
    }

    // Alphabetical, which is both what QSettings writes and what makes
    // exporting the same instance twice the same bytes twice.
    keys.sort();
    let mut text = String::from("[General]\n");
    for (key, value) in keys {
        text.push_str(&format!("{key}={}\n", ini_escaped(&value)));
    }
    text.into_bytes()
}

/// `mmc-pack.json`, the version profile.
///
/// Only `uid` is required of a component, and the `cached*` fields besides the
/// name are Prism's own resolver cache: `cachedRequires` is rebuilt from
/// Minecraft's own metadata, which is also where `org.lwjgl3` comes back from,
/// so writing either would be writing something Prism is about to replace with
/// its own answer.
fn mmc_pack(instance: &Instance) -> Result<Vec<u8>> {
    let mut components = vec![ComponentOut {
        uid: MINECRAFT_UID,
        version: instance.mc_version.clone(),
        cached_name: "Minecraft",
        important: true,
    }];
    if let Some((uid, _, cached_name)) = LOADER_UIDS
        .iter()
        .find(|(_, loader, _)| *loader == instance.loader)
    {
        components.push(ComponentOut {
            uid,
            // The interop form, not the stored one: Geode keeps Forge's
            // version as `1.20.1-47.2.0` because `meta::forge` filters maven
            // metadata on that prefix, while Prism's Forge component is what
            // follows it. Nothing is hardcoded — the prefix comes from this
            // instance's own Minecraft version.
            version: interop_loader_version(
                instance.loader,
                &instance.loader_version,
                &instance.mc_version,
            )
            .to_string(),
            cached_name,
            important: false,
        });
    }
    Ok(serde_json::to_vec_pretty(&ProfileOut {
        components,
        format_version: 1,
    })?)
}

/// The instance's artwork at the zip root, under the name `iconKey` names.
///
/// `None` when there is none, when the file has gone, or when it is in a
/// format Prism's icon search does not look at — in which case the cfg is
/// written with Prism's own `default` key rather than one naming a file
/// nothing will read.
async fn icon_entry(paths: &Paths, instance: &Instance) -> Option<(String, Vec<u8>)> {
    // The file name alone. `instance.icon` is a name inside the instance's own
    // media folder, and that is the only place this reads from.
    let name = Path::new(instance.icon.as_ref()?).file_name()?;
    let suffix = Path::new(&name)
        .extension()
        .and_then(|suffix| suffix.to_str())?
        .to_ascii_lowercase();
    if !ICON_SUFFIXES.contains(&suffix.as_str()) {
        return None;
    }
    let bytes = tokio::fs::read(paths.instance_media(&instance.id).join(name))
        .await
        .ok()?;
    Some((format!("{}.{suffix}", icon_key(instance)), bytes))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileOut {
    components: Vec<ComponentOut>,
    /// Exactly the integer 1. Prism fails the whole file over any other value,
    /// and the reader here refuses one for the same reason.
    format_version: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComponentOut {
    uid: &'static str,
    version: String,
    cached_name: &'static str,
    /// Set on Minecraft itself, which is the component Prism treats as the one
    /// the instance is *of* rather than a dependency of it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    important: bool,
}

/// One `.pw.toml`.
///
/// Serialised rather than formatted, so the escaping of a mod's name is the
/// `toml` crate's problem and not a hand-rolled writer's. The field order is
/// the file's: every plain value before the two tables, which is the only
/// order TOML can be written in.
#[derive(Debug, Serialize)]
struct IndexEntry {
    name: String,
    /// The **enabled** name, whatever the jar beside it is called. Prism
    /// matches a resource to its metadata with `.disabled` stripped off, so an
    /// entry naming the turned-off file would match nothing and the mod would
    /// arrive anonymous.
    filename: String,
    side: &'static str,
    #[serde(
        rename = "x-prismlauncher-version-number",
        skip_serializing_if = "Option::is_none"
    )]
    version_number: Option<String>,
    download: IndexDownload,
    update: IndexUpdate,
}

#[derive(Debug, Serialize)]
struct IndexDownload {
    mode: &'static str,
    url: String,
    #[serde(rename = "hash-format")]
    hash_format: &'static str,
    hash: String,
}

/// Which platform the entry is updated from — one of the two, never both and
/// never neither, because packwiz's own reader discards an entry with no
/// `[update.<provider>]` table at all.
#[derive(Debug, Serialize)]
struct IndexUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    modrinth: Option<ModrinthUpdate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    curseforge: Option<CurseForgeUpdate>,
}

#[derive(Debug, Serialize)]
struct ModrinthUpdate {
    #[serde(rename = "mod-id")]
    mod_id: String,
    version: String,
}

/// Integers, which is how the format types them and how Prism reads them.
#[derive(Debug, Serialize)]
struct CurseForgeUpdate {
    #[serde(rename = "project-id")]
    project_id: u32,
    #[serde(rename = "file-id")]
    file_id: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    use crate::instance::export::Group;
    use crate::instance::modpack::{import_into, preview};
    use crate::instance::Instance;
    use crate::net::Progress;
    use crate::paths::{Paths, Scratch};

    /// An `instance.cfg` as Prism writes one: the keys every real instance
    /// has, these lines added to `[General]`, and the `[UI]` section that
    /// follows — whose keys hold backslashes and whose values have nothing to
    /// do with the instance. A key repeated in `extra` wins, the way the
    /// file's own last word does.
    fn cfg(extra: &[&str]) -> String {
        let mut text = String::from(
            "[General]\n\
             ConfigVersion=1.3\n\
             InstanceType=OneSix\n\
             AutomaticJava=true\n\
             iconKey=default\n\
             name=Survival\n\
             notes=\n",
        );
        for line in extra {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str("[UI]\nmods_Page\\Columns=@ByteArray(1,2)\nMaxMemAlloc=1\n");
        text
    }

    /// `mmc-pack.json` for a Minecraft version and, where there is one, a
    /// loader component named by uid. `org.lwjgl3` rides along as it does in
    /// every real profile, to be ignored.
    fn mmc(mc: &str, loader: Option<(&str, &str)>) -> String {
        let mut components = vec![
            r#"{"uid":"org.lwjgl3","version":"3.3.3","dependencyOnly":true}"#.to_string(),
            format!(r#"{{"uid":"net.minecraft","version":"{mc}","important":true}}"#),
        ];
        if let Some((uid, version)) = loader {
            components.push(format!(
                r#"{{"uid":"{uid}","version":"{version}","cachedName":"Loader"}}"#
            ));
        }
        format!(
            r#"{{"components":[{}],"formatVersion":1}}"#,
            components.join(",")
        )
    }

    /// A Modrinth entry in the shape Prism writes: sha512, `mode = 'url'` and
    /// the file's real CDN url.
    const MODRINTH_ENTRY: &str = "filename = 'VulkanMod-0.6.8+26.1.2.jar'
name = 'VulkanMod'
side = 'client'
x-prismlauncher-loaders = [ 'fabric' ]
x-prismlauncher-release-type = 'release'
x-prismlauncher-version-number = '0.6.8+26.1.2'

[download]
hash = '7ceb53215a09516f0e452adb1ffd17e4ac48d40e'
hash-format = 'sha512'
mode = 'url'
url = 'https://cdn.modrinth.com/data/JYQhtZtO/versions/CEmnv55N/VulkanMod.jar'

[update.modrinth]
mod-id = 'JYQhtZtO'
version = 'CEmnv55N'
";

    /// A CurseForge entry in the shape Prism writes: sha1,
    /// `mode = 'metadata:curseforge'`, an empty url, and integer ids.
    const CURSEFORGE_ENTRY: &str = "filename = 'coasters_extras-neoforge-1.21.1-1.2.jar'
name = 'Create Coasters Extras'
side = 'server'
x-prismlauncher-loaders = [ 'neoforge' ]
x-prismlauncher-release-type = 'release'

[download]
hash = '1615edd0c781c0489713ee9e81c2e03bbc989828'
hash-format = 'sha1'
mode = 'metadata:curseforge'
url = ''

[update.curseforge]
file-id = 8718286
project-id = 1649197
";

    /// A zip of these entries, in the order given.
    fn write_zip(dest: &Path, entries: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        for (name, body) in entries {
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file(*name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    /// The same entries as a folder on disk.
    fn write_tree(dir: &Path, entries: &[(&str, &str)]) {
        for (name, body) in entries {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }

    /// An instance folder and an exported zip of the very same files, so a
    /// test can prove both sources read alike.
    fn both_sources(root: &Path, entries: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let folder = root.join("folder");
        write_tree(&folder, entries);
        let archive = root.join("exported.zip");
        write_zip(&archive, entries);
        (folder, archive)
    }

    /// Import a source the way the app does: `preview` for the name, version
    /// and loader, then `import_into` for everything else.
    async fn import(paths: &Paths, id: &str, source: &Path) -> Result<Instance> {
        let described = preview(source.to_path_buf()).await?;
        let mut instance = Instance::new(
            described.name,
            described.mc_version,
            described.loader,
            described.loader_version,
        );
        instance.id = id.to_string();
        import_into(
            &reqwest::Client::new(),
            paths,
            &mut instance,
            source,
            None,
            &Progress::silent("t"),
        )
        .await?;
        Ok(instance)
    }

    /// What an instance ended up knowing about its files, in one order —
    /// `read_dir` hands them over in whatever order it likes.
    fn provenance(instance: &Instance) -> Vec<String> {
        let mut rows: Vec<String> = instance
            .content
            .iter()
            .map(|r| {
                format!(
                    "{:?} {} {} {} {}",
                    r.kind, r.file_name, r.provider, r.project_id, r.version_id
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// A zip Prism would read as an instance, nested a folder deep — which is
    /// how a zip somebody made from the folder above the instance arrives, and
    /// which Prism accepts because it matches `instance.cfg` at any depth.
    #[tokio::test]
    async fn a_prism_instance_zip_is_recognised_and_read() {
        let scratch = Scratch::new("prism-zip");
        std::fs::create_dir_all(&scratch.paths.root).unwrap();
        let archive = scratch.paths.root.join("CVGS.zip");
        let general = cfg(&[]);
        let profile = mmc("1.21.1", Some(("net.neoforged", "21.1.248")));
        write_zip(
            &archive,
            &[
                ("CVGS/instance.cfg", &general),
                ("CVGS/mmc-pack.json", &profile),
                ("CVGS/minecraft/mods/x.jar", "jar"),
            ],
        );

        let described = preview(archive).await.unwrap();
        assert_eq!(described.format, "prism");
        assert_eq!(described.name, "Survival");
        assert_eq!(described.mc_version, "1.21.1");
        assert_eq!(described.loader, Loader::NeoForge);
        assert_eq!(described.loader_version, "21.1.248");
        assert_eq!(described.file_count, 1, "the game directory's own files");
    }

    /// Prism refuses an instance whose type it does not recognise, and so does
    /// this: what arrives otherwise is a layout neither launcher can read.
    #[tokio::test]
    async fn an_instance_type_prism_would_refuse_is_refused_here_too() {
        let scratch = Scratch::new("prism-type");
        let folder = scratch.paths.root.join("Something");
        let general = cfg(&["InstanceType=Something"]);
        let profile = mmc("1.21.1", None);
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                ("mmc-pack.json", &profile),
            ],
        );

        let err = preview(folder).await.expect_err("an unknown instance type");
        assert!(err.to_string().contains("InstanceType"), "{err}");
    }

    /// `mmc-pack.json` is version 1 or nothing: Prism fails the whole profile
    /// over any other value, which leaves an instance that cannot launch.
    #[tokio::test]
    async fn a_format_version_other_than_one_is_refused() {
        let scratch = Scratch::new("prism-format");
        let folder = scratch.paths.root.join("Future");
        let general = cfg(&[]);
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                (
                    "mmc-pack.json",
                    r#"{"components":[{"uid":"net.minecraft","version":"1.21.1"}],"formatVersion":2}"#,
                ),
            ],
        );

        let err = preview(folder).await.expect_err("a profile from the future");
        assert!(err.to_string().contains("version 1"), "{err}");
    }

    /// The gates are the thing that silently does nothing in Prism when they
    /// are written wrong: an ungated value is one the instance was never
    /// running under, because Prism takes its global setting instead.
    #[tokio::test]
    async fn overridden_settings_are_read_and_unoverridden_ones_are_not() {
        let scratch = Scratch::new("prism-gates");
        let paths = scratch.paths.clone();
        let folder = paths.root.join("Gated");
        let general = cfg(&[
            "OverrideMemory=true",
            "MaxMemAlloc=6144",
            "OverrideJavaArgs=false",
            "JvmArgs=-XX:whatever",
            "OverrideWindow=true",
            "MinecraftWinWidth=1600",
            "MinecraftWinHeight=900",
            "notes=Keep the elytra in the ender chest",
        ]);
        let profile = mmc("1.21.1", Some(("net.fabricmc.fabric-loader", "0.16.9")));
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                ("mmc-pack.json", &profile),
            ],
        );

        let instance = import(&paths, "gated", &folder).await.unwrap();
        assert_eq!(instance.overrides.memory_mb, Some(6144));
        assert_eq!(instance.overrides.java_args, None, "an ungated value");
        assert_eq!(instance.overrides.width, Some(1600));
        assert_eq!(instance.overrides.height, Some(900));
        assert_eq!(instance.notes, "Keep the elytra in the ender chest");
    }

    /// A path to somebody else's Java runtime means nothing on this machine,
    /// gate or no gate — and Geode does not export one either.
    #[tokio::test]
    async fn a_java_path_is_never_imported() {
        let scratch = Scratch::new("prism-java");
        let paths = scratch.paths.clone();
        let folder = paths.root.join("Java");
        let general = cfg(&[
            "OverrideJavaLocation=true",
            "JavaPath=/somebody/elses/java",
        ]);
        let profile = mmc("1.21.1", None);
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                ("mmc-pack.json", &profile),
            ],
        );

        let instance = import(&paths, "java", &folder).await.unwrap();
        assert_eq!(instance.overrides.java_path, None);
        // Nor anywhere else it could have landed.
        assert_eq!(instance.overrides.java_args, None);
    }

    /// The whole point of reading a Prism instance: a hundred jars arrive
    /// named, from the packwiz index Prism keeps beside them, instead of
    /// anonymous.
    #[tokio::test]
    async fn a_packwiz_index_becomes_content_records() {
        let scratch = Scratch::new("prism-index");
        let paths = scratch.paths.clone();
        let folder = paths.root.join("Indexed");
        let general = cfg(&[]);
        let profile = mmc("1.21.1", Some(("net.neoforged", "21.1.248")));
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                ("mmc-pack.json", &profile),
                ("minecraft/mods/.index/vulkanmod.pw.toml", MODRINTH_ENTRY),
                (
                    "minecraft/mods/.index/create-coasters-extras.pw.toml",
                    CURSEFORGE_ENTRY,
                ),
            ],
        );

        let instance = import(&paths, "indexed", &folder).await.unwrap();
        let modrinth = instance
            .content
            .iter()
            .find(|r| r.provider == "modrinth")
            .expect("the Modrinth entry");
        assert_eq!(modrinth.file_name, "VulkanMod-0.6.8+26.1.2.jar");
        assert_eq!(modrinth.kind, ContentKind::Mod);
        assert_eq!(modrinth.project_id, "JYQhtZtO");
        assert_eq!(modrinth.version_id, "CEmnv55N");
        assert_eq!(modrinth.name, "VulkanMod");
        assert_eq!(modrinth.version_number, "0.6.8+26.1.2");
        assert_eq!(modrinth.slug.as_deref(), Some("vulkanmod"));
        assert_eq!(modrinth.side, Some(Side::Client));

        let curseforge = instance
            .content
            .iter()
            .find(|r| r.provider == "curseforge")
            .expect("the CurseForge entry");
        assert_eq!(
            curseforge.file_name,
            "coasters_extras-neoforge-1.21.1-1.2.jar"
        );
        // Integers in the file, strings in a record, and the pair the right
        // way round: the project is the mod, the version is the file.
        assert_eq!(curseforge.project_id, "1649197");
        assert_eq!(curseforge.version_id, "8718286");
        assert_eq!(curseforge.name, "Create Coasters Extras");
        assert_eq!(curseforge.side, Some(Side::Server));
    }

    /// A recorded file name becomes a path: `install::content::install_into`
    /// deletes the previous file for a project by the name on its record, so
    /// an index naming `../../something` turns the next Update into a delete
    /// outside the instance. packwiz names a bare file, and an entry that puts
    /// a path there is discarded whole.
    #[tokio::test]
    async fn an_index_naming_a_path_rather_than_a_file_is_discarded() {
        let scratch = Scratch::new("prism-escape");
        let paths = scratch.paths.clone();
        let folder = paths.root.join("Escaping");
        let general = cfg(&[]);
        let profile = mmc("1.21.1", None);
        let named = |filename: &str| {
            MODRINTH_ENTRY.replace("filename = 'VulkanMod-0.6.8+26.1.2.jar'", filename)
        };
        let escaping = named("filename = '../../../../../PRECIOUS.txt'");
        let nested = named("filename = 'mods/VulkanMod.jar'");
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                ("mmc-pack.json", &profile),
                ("minecraft/mods/.index/vulkanmod.pw.toml", &escaping),
                ("minecraft/mods/.index/nested.pw.toml", &nested),
            ],
        );

        let instance = import(&paths, "escaping", &folder).await.unwrap();
        assert!(
            instance.content.is_empty(),
            "a name no update may join onto the mods folder: {:?}",
            provenance(&instance)
        );
    }

    /// Prism moves shader metadata out of `shaderpacks/.index` with a
    /// migration that runs the first time that folder is loaded, so an
    /// instance whose shader page was never opened under a new Prism — and
    /// every MultiMC-era export — still keeps it there. Reading only the flat
    /// layout loses it without a word, which is the failure this stage exists
    /// to prevent.
    #[tokio::test]
    async fn shader_metadata_in_the_old_index_folder_is_not_missed() {
        let scratch = Scratch::new("prism-shaders-index");
        let paths = scratch.paths.clone();
        let folder = paths.root.join("Unmigrated");
        let general = cfg(&[]);
        let profile = mmc("1.21.1", None);
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                ("mmc-pack.json", &profile),
                (
                    "minecraft/shaderpacks/.index/complementary.pw.toml",
                    MODRINTH_ENTRY,
                ),
                ("minecraft/shaderpacks/c.zip", "zip"),
            ],
        );

        let instance = import(&paths, "unmigrated", &folder).await.unwrap();
        let shader = instance.content.first().expect("the shader's entry");
        assert_eq!(shader.kind, ContentKind::ShaderPack);
        assert_eq!(shader.project_id, "JYQhtZtO");
    }

    /// A component Prism is not applying is not part of the instance: an
    /// `mmc-pack.json` whose loader says `"disabled": true` runs vanilla
    /// there, and importing it as that loader installs something the exported
    /// instance never ran.
    #[tokio::test]
    async fn a_disabled_loader_component_is_not_the_instances_loader() {
        let scratch = Scratch::new("prism-disabled");
        let folder = scratch.paths.root.join("Off");
        let general = cfg(&[]);
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                (
                    "mmc-pack.json",
                    r#"{"components":[{"uid":"net.minecraft","version":"1.21.1","important":true},
                        {"uid":"net.neoforged","version":"21.1.248","disabled":true}],
                        "formatVersion":1}"#,
                ),
            ],
        );

        let described = preview(folder).await.unwrap();
        assert_eq!(described.loader, Loader::Vanilla);
        assert_eq!(described.loader_version, "");
    }

    /// An instance folder is tens of thousands of files and gigabytes of
    /// world, and copying it is the longest thing an import does — so the bar
    /// has the folder's own count to move against, and Cancel stops the copy
    /// where it stands instead of after the last file.
    #[tokio::test]
    async fn a_folder_copy_is_counted_and_can_be_cancelled() {
        let scratch = Scratch::new("prism-cancel");
        let paths = scratch.paths.clone();
        let folder = paths.root.join("Big");
        let general = cfg(&[]);
        let profile = mmc("1.21.1", None);
        let jars: Vec<(String, String)> = (0..40)
            .map(|i| (format!("minecraft/mods/m{i}.jar"), "jar".to_string()))
            .collect();
        let mut entries: Vec<(&str, &str)> = vec![
            ("instance.cfg", general.as_str()),
            ("mmc-pack.json", profile.as_str()),
        ];
        entries.extend(jars.iter().map(|(n, b)| (n.as_str(), b.as_str())));
        write_tree(&folder, &entries);

        let described = preview(folder.clone()).await.unwrap();
        assert_eq!(described.file_count, jars.len());
        let mut instance = Instance::new(
            described.name,
            described.mc_version,
            described.loader,
            described.loader_version,
        );
        instance.id = "cancelled".to_string();

        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let progress = Progress::new(recorder.clone(), "t");
        progress
            .cancel_flag()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let err = import_into(
            &reqwest::Client::new(),
            &paths,
            &mut instance,
            &folder,
            None,
            &progress,
        )
        .await
        .expect_err("a cancelled copy stops");
        assert!(matches!(err, Error::Cancelled), "{err}");

        let planned = recorder
            .events()
            .into_iter()
            .find(|(_, e)| e["stage"] == "Copying the instance")
            .map(|(_, e)| e["total"].as_u64())
            .expect("the copy reports how much it will copy");
        assert_eq!(
            planned,
            Some(jars.len() as u64),
            "the whole copy counted as one step"
        );
        let arrived = std::fs::read_dir(paths.game_dir("cancelled").join("mods"))
            .map(|dir| dir.count())
            .unwrap_or(0);
        assert_eq!(arrived, 0, "it stopped before copying, not after");
    }

    /// Shader metadata sits flat in `shaderpacks/` with no `.index` folder —
    /// Prism's own quirk, and provenance an importer that only looks in
    /// `.index` loses without a word.
    #[tokio::test]
    async fn shader_metadata_beside_the_zips_is_not_missed() {
        let scratch = Scratch::new("prism-shaders");
        let paths = scratch.paths.clone();
        let folder = paths.root.join("Shaders");
        let general = cfg(&[]);
        let profile = mmc("1.21.1", None);
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                ("mmc-pack.json", &profile),
                ("minecraft/shaderpacks/complementary.pw.toml", MODRINTH_ENTRY),
            ],
        );

        let instance = import(&paths, "shaders", &folder).await.unwrap();
        let shader = instance.content.first().expect("the shader's entry");
        assert_eq!(shader.kind, ContentKind::ShaderPack);
        assert_eq!(shader.project_id, "JYQhtZtO");
    }

    /// Packwiz's own reader discards an entry whole when either table is
    /// missing, so a half-written one degrades to no provenance rather than to
    /// wrong provenance — and the jar simply arrives untracked.
    #[tokio::test]
    async fn an_index_entry_missing_its_update_table_is_discarded_whole() {
        let scratch = Scratch::new("prism-halfwritten");
        let paths = scratch.paths.clone();
        let folder = paths.root.join("Half");
        let general = cfg(&[]);
        let profile = mmc("1.21.1", None);
        let half = "filename = 'mystery.jar'\nname = 'Mystery'\n\n[download]\nhash-format = 'sha512'\nhash = 'abc'\nmode = 'url'\nurl = 'https://example.invalid/mystery.jar'\n";
        write_tree(
            &folder,
            &[
                ("instance.cfg", &general),
                ("mmc-pack.json", &profile),
                ("minecraft/mods/.index/mystery.pw.toml", half),
                ("minecraft/mods/mystery.jar", "jar"),
            ],
        );

        let instance = import(&paths, "half", &folder).await.unwrap();
        assert!(instance.content.is_empty(), "{:?}", provenance(&instance));
        assert!(
            paths.game_dir("half").join("mods/mystery.jar").exists(),
            "the jar itself still arrived"
        );
    }

    /// `.minecraft` is the game directory only when there is no `minecraft`,
    /// which is what Prism's own `gameRoot` does — and both sources have to
    /// agree, since each decides it for itself.
    #[tokio::test]
    async fn dot_minecraft_is_used_only_when_minecraft_is_absent() {
        let scratch = Scratch::new("prism-gamedir");
        let paths = scratch.paths.clone();
        let general = cfg(&[]);
        let profile = mmc("1.21.1", None);

        for (tag, files, expected) in [
            ("plain", vec![("minecraft/mods/plain.jar", "jar")], "plain.jar"),
            ("dotted", vec![(".minecraft/mods/dot.jar", "jar")], "dot.jar"),
            (
                "both",
                vec![
                    ("minecraft/mods/plain.jar", "jar"),
                    (".minecraft/mods/dot.jar", "jar"),
                ],
                "plain.jar",
            ),
        ] {
            let mut entries = vec![
                ("instance.cfg", general.as_str()),
                ("mmc-pack.json", profile.as_str()),
            ];
            entries.extend(files);
            let root = paths.root.join(tag);
            let (folder, archive) = both_sources(&root, &entries);

            for (source, from) in [(folder, "folder"), (archive, "zip")] {
                let id = format!("{tag}-{from}");
                import(&paths, &id, &source).await.unwrap();
                let mods = paths.game_dir(&id).join("mods");
                assert!(
                    mods.join(expected).exists(),
                    "{tag} as a {from}: {expected} did not arrive"
                );
                let arrived = std::fs::read_dir(&mods).unwrap().count();
                assert_eq!(arrived, 1, "{tag} as a {from}: both directories arrived");
            }
        }
    }

    /// The user points at an instance folder or at a zip of one, and gets the
    /// same instance either way.
    #[tokio::test]
    async fn a_prism_directory_imports_the_same_as_its_zip() {
        let scratch = Scratch::new("prism-either-way");
        let paths = scratch.paths.clone();
        let general = cfg(&["OverrideMemory=true", "MaxMemAlloc=4096", "notes=Mine"]);
        let profile = mmc("1.21.1", Some(("net.minecraftforge", "47.2.0")));
        let (folder, archive) = both_sources(
            &paths.root.join("source"),
            &[
                ("instance.cfg", general.as_str()),
                ("mmc-pack.json", profile.as_str()),
                ("minecraft/mods/.index/vulkanmod.pw.toml", MODRINTH_ENTRY),
                ("minecraft/mods/VulkanMod-0.6.8+26.1.2.jar", "jar"),
                ("minecraft/config/mine.toml", "mine = true"),
            ],
        );

        let from_folder = import(&paths, "from-folder", &folder).await.unwrap();
        let from_zip = import(&paths, "from-zip", &archive).await.unwrap();

        assert_eq!(from_folder.name, from_zip.name);
        assert_eq!(from_folder.notes, from_zip.notes);
        assert_eq!(from_folder.mc_version, from_zip.mc_version);
        assert_eq!(from_folder.loader, from_zip.loader);
        // Forge's short form, which is what Prism writes and what `meta::forge`
        // puts the game version back onto.
        assert_eq!(from_zip.loader_version, "47.2.0");
        assert_eq!(from_folder.overrides.memory_mb, Some(4096));
        assert_eq!(from_zip.overrides.memory_mb, Some(4096));
        assert_eq!(provenance(&from_folder), provenance(&from_zip));
        assert_eq!(provenance(&from_zip).len(), 1);

        for id in ["from-folder", "from-zip"] {
            let game = paths.game_dir(id);
            assert!(game.join("mods/VulkanMod-0.6.8+26.1.2.jar").exists(), "{id}");
            assert!(game.join("config/mine.toml").exists(), "{id}");
        }
    }

    // -----------------------------------------------------------------------
    // Writing a Prism instance
    // -----------------------------------------------------------------------

    /// An instance on disk with each named file written, and a catalogue of
    /// what is in it. No platform is asked anything: each test says what the
    /// platforms hold through the catalogue's own stubs, which go in through
    /// the code a real answer does.
    async fn exportable(tag: &str, files: &[(&str, &str)]) -> (Scratch, Instance, Catalogue) {
        let scratch = Scratch::new(&format!("prism-out-{tag}"));
        let mut inst = Instance::new(
            "Kitchen Sink".into(),
            "1.21.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        );
        inst.id = format!("out-{tag}");
        let game = scratch.paths.game_dir(&inst.id);
        tokio::fs::create_dir_all(&game).await.unwrap();
        for (dir, name) in files {
            let holding = game.join(dir);
            tokio::fs::create_dir_all(&holding).await.unwrap();
            tokio::fs::write(holding.join(name), format!("the bytes of {name}"))
                .await
                .unwrap();
        }
        let catalogue = catalogue_of(&scratch.paths, &inst).await;
        (scratch, inst, catalogue)
    }

    /// The catalogue again, for a test that gave the instance its records
    /// after the fixture was built.
    async fn catalogue_of(paths: &Paths, inst: &Instance) -> Catalogue {
        crate::install::identity::for_instance(Ask::Nobody, paths, inst, &Progress::silent("t"))
            .await
            .unwrap()
    }

    /// Write the fixture out as a Prism instance.
    async fn write_to(
        paths: &Paths,
        inst: &Instance,
        dest: &Path,
        catalogue: &Catalogue,
    ) -> ExportReport {
        write_chosen(paths, inst, dest, &Selection::default(), catalogue).await
    }

    /// The same, for a test about what the sharer chose to carry.
    async fn write_chosen(
        paths: &Paths,
        inst: &Instance,
        dest: &Path,
        selection: &Selection,
        catalogue: &Catalogue,
    ) -> ExportReport {
        write_instance(
            paths,
            inst,
            dest.to_path_buf(),
            selection,
            catalogue,
            &Progress::silent("t"),
        )
        .await
        .unwrap()
    }

    /// One provenance record, in the shape an install leaves behind.
    fn record(
        file_name: &str,
        kind: ContentKind,
        provider: &str,
        project_id: &str,
        version_id: &str,
        name: &str,
        slug: &str,
    ) -> ContentRecord {
        ContentRecord {
            file_name: file_name.to_string(),
            kind,
            provider: provider.to_string(),
            project_id: project_id.to_string(),
            version_id: version_id.to_string(),
            name: name.to_string(),
            version_number: "1.2.3".to_string(),
            slug: Some(slug.to_string()),
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: None,
            side: None,
            server_off: false,
        }
    }

    /// Everything inside a written zip, by entry name.
    fn entries(archive: &Path) -> Vec<String> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        let mut names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        names.sort();
        names
    }

    /// One entry's text.
    fn text_of(archive: &Path, name: &str) -> String {
        use std::io::Read;
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        let mut entry = zip
            .by_name(name)
            .unwrap_or_else(|_| panic!("{name} is not in the zip: {:?}", entries(archive)));
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        text
    }

    /// One `.pw.toml`, parsed.
    fn index_entry(archive: &Path, name: &str) -> toml::Table {
        text_of(archive, name).parse().unwrap()
    }

    /// A zip Prism lays out flat, with no wrapper folder, because that is what
    /// its own export writes and what its importer expects to find. The two
    /// pack manifests must stay out of the root: Prism matches those at the
    /// exact root and `instance.cfg` at any depth, first match over the zip's
    /// own order wins, so a zip holding two would be read as whichever it met
    /// first.
    #[tokio::test]
    async fn a_prism_zip_is_laid_out_the_way_prism_lays_one_out() {
        let (scratch, mut inst, catalogue) = exportable(
            "layout",
            &[
                ("mods", "sodium.jar"),
                // A pack manifest that belongs where it is, to prove the rule
                // is about the root and not about the name.
                ("config", "manifest.json"),
            ],
        )
        .await;
        inst.icon = Some("icon.png".into());
        let media = scratch.paths.instance_media(&inst.id);
        tokio::fs::create_dir_all(&media).await.unwrap();
        tokio::fs::write(media.join("icon.png"), "an image")
            .await
            .unwrap();
        let dest = scratch.paths.root.join("layout.zip");
        write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        assert_eq!(
            entries(&dest),
            vec![
                "Geode_out-layout_Icon.png",
                "instance.cfg",
                "minecraft/config/manifest.json",
                "minecraft/mods/sodium.jar",
                "mmc-pack.json",
            ],
            "the instance's own files at the root, the game under minecraft/"
        );
        assert_eq!(text_of(&dest, "Geode_out-layout_Icon.png"), "an image");
        assert!(
            text_of(&dest, "instance.cfg").contains("iconKey=Geode_out-layout_Icon"),
            "the key Prism looks the artwork up by"
        );
    }

    /// Without `InstanceType=OneSix` the instance does not appear in Prism at
    /// all, and an overridable value without its own gate beside it is the
    /// thing Prism silently ignores in favour of its global setting.
    #[tokio::test]
    async fn an_instance_cfg_carries_instance_type_and_its_gates() {
        let (scratch, mut inst, catalogue) = exportable("gates", &[]).await;
        inst.overrides.memory_mb = Some(6144);
        inst.overrides.width = Some(1600);
        inst.overrides.height = Some(900);
        // Left unset, so neither the gate nor the value may appear.
        inst.overrides.java_args = None;
        let dest = scratch.paths.root.join("gates.zip");
        write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        let cfg = text_of(&dest, "instance.cfg");
        let lines: Vec<&str> = cfg.lines().collect();
        assert!(lines.contains(&"InstanceType=OneSix"), "{cfg}");
        assert!(lines.contains(&"OverrideMemory=true"), "{cfg}");
        assert!(lines.contains(&"MaxMemAlloc=6144"), "{cfg}");
        // What Geode itself launches with, so the instance behaves the same.
        assert!(lines.contains(&"MinMemAlloc=3072"), "{cfg}");
        assert!(lines.contains(&"OverrideWindow=true"), "{cfg}");
        assert!(lines.contains(&"MinecraftWinWidth=1600"), "{cfg}");
        assert!(!cfg.contains("OverrideJavaArgs"), "{cfg}");
        assert!(!cfg.contains("JvmArgs"), "{cfg}");
    }

    /// Without `ConfigVersion` Prism reads the whole file with its pre-1.3
    /// parser, whose rules are not the QSettings ones [`ini_escaped`] writes
    /// for: it ends a value at an unescaped `#`, so a note or a JVM argument
    /// holding one arrives cut in half — and Prism saves the mangled value
    /// back on its first write.
    #[tokio::test]
    async fn an_instance_cfg_says_which_config_version_it_is() {
        let (scratch, mut inst, catalogue) = exportable("cfgversion", &[]).await;
        inst.notes = "Use the #1 preset".into();
        let dest = scratch.paths.root.join("cfgversion.zip");
        write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        let cfg = text_of(&dest, "instance.cfg");
        let lines: Vec<&str> = cfg.lines().collect();
        assert!(lines.contains(&"ConfigVersion=1.3"), "{cfg}");
        assert!(lines.contains(&"notes=Use the #1 preset"), "{cfg}");
    }

    /// Prism's icon list is global to the install, and installing an
    /// instance's artwork deletes whatever already sits under the key it
    /// names. So the key names the instance, as Prism's own imports' do: one
    /// constant key for every export would have the second instance a
    /// recipient imports take over the first's picture.
    #[tokio::test]
    async fn two_exports_name_their_artwork_apart() {
        let mut travelled = Vec::new();
        for tag in ["first", "second"] {
            let (scratch, mut inst, catalogue) = exportable(&format!("icons-{tag}"), &[]).await;
            inst.icon = Some("icon.png".into());
            let media = scratch.paths.instance_media(&inst.id);
            tokio::fs::create_dir_all(&media).await.unwrap();
            tokio::fs::write(media.join("icon.png"), format!("the {tag} image"))
                .await
                .unwrap();
            let dest = scratch.paths.root.join("icons.zip");
            write_to(&scratch.paths, &inst, &dest, &catalogue).await;

            let names = entries(&dest);
            let image = names
                .iter()
                .find(|name| name.ends_with(".png"))
                .expect("the artwork travelled")
                .clone();
            assert!(
                text_of(&dest, "instance.cfg")
                    .contains(&format!("iconKey={}", image.trim_end_matches(".png"))),
                "the cfg looks the artwork up by the name it travelled under: {image}"
            );
            travelled.push(image);
        }

        assert_ne!(
            travelled[0], travelled[1],
            "one key for both exports is one picture for both instances"
        );
    }

    /// A path to this machine's Java means nothing on anybody else's, which is
    /// why the reader never takes one either and why a `.geodepack` does not
    /// carry one.
    #[tokio::test]
    async fn a_java_path_is_never_exported() {
        let (scratch, mut inst, catalogue) = exportable("java", &[]).await;
        inst.overrides.java_path = Some("/home/somebody/jdk21/bin/java".into());
        let dest = scratch.paths.root.join("java.zip");
        write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        let cfg = text_of(&dest, "instance.cfg");
        assert!(!cfg.contains("JavaPath"), "{cfg}");
        assert!(!cfg.contains("/home/somebody"), "{cfg}");
        assert!(!cfg.contains("OverrideJavaLocation"), "{cfg}");
    }

    /// Prism names the loader by a uid and takes the version from the
    /// component beside it — and Forge's is the short form, because Geode
    /// stores the long one for its own metadata lookup.
    #[tokio::test]
    async fn an_mmc_pack_names_the_loader_by_uid() {
        for (tag, mc, loader, stored, expected) in [
            ("vanilla", "1.21.1", Loader::Vanilla, "", None),
            (
                "fabric",
                "1.21.1",
                Loader::Fabric,
                "0.16.9",
                Some(("net.fabricmc.fabric-loader", "0.16.9")),
            ),
            (
                "quilt",
                "1.21.1",
                Loader::Quilt,
                "0.27.0",
                Some(("org.quiltmc.quilt-loader", "0.27.0")),
            ),
            (
                "forge",
                "1.20.1",
                Loader::Forge,
                "1.20.1-47.2.0",
                Some(("net.minecraftforge", "47.2.0")),
            ),
            (
                "neoforge",
                "1.21.1",
                Loader::NeoForge,
                "21.1.248",
                Some(("net.neoforged", "21.1.248")),
            ),
        ] {
            let scratch = Scratch::new(&format!("prism-uid-{tag}"));
            let mut inst = Instance::new("Profile".into(), mc.into(), loader, stored.into());
            inst.id = format!("uid-{tag}");
            tokio::fs::create_dir_all(scratch.paths.game_dir(&inst.id))
                .await
                .unwrap();
            let catalogue = catalogue_of(&scratch.paths, &inst).await;
            let dest = scratch.paths.root.join("uid.zip");
            write_to(&scratch.paths, &inst, &dest, &catalogue).await;

            let profile: serde_json::Value =
                serde_json::from_str(&text_of(&dest, "mmc-pack.json")).unwrap();
            // The integer 1 and nothing else: Prism fails the whole file over
            // any other value, including the string "1".
            assert_eq!(profile["formatVersion"], serde_json::json!(1), "{tag}");
            let components = profile["components"].as_array().unwrap();
            assert_eq!(components[0]["uid"], "net.minecraft", "{tag}");
            assert_eq!(components[0]["version"], mc, "{tag}");
            assert_eq!(components[0]["important"], serde_json::json!(true), "{tag}");
            match expected {
                Some((uid, version)) => {
                    assert_eq!(components.len(), 2, "{tag}");
                    assert_eq!(components[1]["uid"], uid, "{tag}");
                    assert_eq!(components[1]["version"], version, "{tag}");
                    // Prism's own resolver cache, rebuilt on load — writing it
                    // would be writing what Prism is about to replace.
                    assert!(components[1].get("cachedRequires").is_none(), "{tag}");
                }
                None => assert_eq!(components.len(), 1, "{tag}: no loader to name"),
            }
        }
    }

    /// The point of writing an index at all: a mod the catalogue can name
    /// arrives with its project, its version and a hash Prism can check,
    /// instead of as an anonymous jar.
    ///
    /// The two providers are not interchangeable here. Modrinth is a URL and a
    /// sha512; CurseForge cannot be a URL at all — it is fetched through the
    /// API with the recipient's own key, which is what `metadata:curseforge`
    /// means — and its ids are integers and its hash a sha1.
    #[tokio::test]
    async fn every_named_mod_gets_an_index_entry_prism_would_read() {
        let (scratch, mut inst, _) =
            exportable("named", &[("mods", "sodium.jar"), ("mods", "create.jar")]).await;
        inst.content = vec![
            record(
                "sodium.jar",
                ContentKind::Mod,
                "modrinth",
                "AANobbMI",
                "Hu34AbCd",
                "Sodium",
                "sodium",
            ),
            record(
                "create.jar",
                ContentKind::Mod,
                "curseforge",
                "328085",
                "4593548",
                "Create",
                "create",
            ),
        ];
        let mut catalogue = catalogue_of(&scratch.paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar"]);
        let dest = scratch.paths.root.join("named.zip");
        let report = write_to(&scratch.paths, &inst, &dest, &catalogue).await;
        assert_eq!(report.referenced, 2);

        let modrinth = index_entry(&dest, "minecraft/mods/.index/sodium.pw.toml");
        assert_eq!(modrinth["name"].as_str(), Some("Sodium"));
        assert_eq!(modrinth["filename"].as_str(), Some("sodium.jar"));
        assert_eq!(modrinth["side"].as_str(), Some("both"));
        let download = modrinth["download"].as_table().expect("a download table");
        assert_eq!(download["mode"].as_str(), Some("url"));
        assert_eq!(download["hash-format"].as_str(), Some("sha512"));
        // The matched file's own URL, not one built from the name on disk: a
        // renamed jar's constructed URL is a 404.
        assert!(
            download["url"]
                .as_str()
                .unwrap()
                .starts_with("https://cdn.modrinth.com/data/mr-project/versions/mr-version/"),
            "{download:?}"
        );
        assert_eq!(
            modrinth["update"]["modrinth"]["mod-id"].as_str(),
            Some("mr-project")
        );
        assert!(modrinth["update"].get("curseforge").is_none());

        let curseforge = index_entry(&dest, "minecraft/mods/.index/create.pw.toml");
        assert_eq!(curseforge["filename"].as_str(), Some("create.jar"));
        let download = curseforge["download"].as_table().expect("a download table");
        assert_eq!(download["mode"].as_str(), Some("metadata:curseforge"));
        assert_eq!(download["url"].as_str(), Some(""), "there is none to give");
        assert_eq!(download["hash-format"].as_str(), Some("sha1"));
        // Integers, which is how the format types them and how Prism reads
        // them: a quoted id is one it will not resolve.
        let update = curseforge["update"]["curseforge"]
            .as_table()
            .expect("an update table");
        assert_eq!(update["project-id"].as_integer(), Some(328085));
        assert_eq!(update["file-id"].as_integer(), Some(4593548));
    }

    /// An index entry is the one thing in this zip that is not the jar beside
    /// it, so a group the sharer unticked must leave neither behind. Prism
    /// reads a `.pw.toml` with no file as a mod that is not installed, and the
    /// report would have called it named and checkable for updates.
    #[tokio::test]
    async fn a_group_left_out_leaves_no_index_entry_behind() {
        let (scratch, mut inst, _) =
            exportable("left-out", &[("mods", "sodium.jar"), ("config", "mod.toml")]).await;
        inst.content = vec![record(
            "sodium.jar",
            ContentKind::Mod,
            "modrinth",
            "AANobbMI",
            "Hu34AbCd",
            "Sodium",
            "sodium",
        )];
        let mut catalogue = catalogue_of(&scratch.paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar"]);

        let dest = scratch.paths.root.join("left-out.zip");
        let report = write_chosen(
            &scratch.paths,
            &inst,
            &dest,
            &Selection::of(vec![Group::Config]),
            &catalogue,
        )
        .await;

        assert_eq!(report.referenced, 0, "a file left at home was called named");
        let names = entries(&dest);
        assert!(
            !names.iter().any(|name| name.contains(".pw.toml")),
            "{names:?}"
        );
        assert!(
            names.contains(&"minecraft/config/mod.toml".to_string()),
            "the group that was asked for travelled: {names:?}"
        );
    }

    /// Shader metadata sits flat in `shaderpacks/` with no `.index` folder.
    /// Prism's own quirk, honoured on the way out as well as on the way in —
    /// an entry in the wrong place is provenance Prism never reads.
    #[tokio::test]
    async fn shader_metadata_is_written_flat() {
        let (scratch, mut inst, _) =
            exportable("shaders", &[("shaderpacks", "complementary.zip")]).await;
        inst.content = vec![record(
            "complementary.zip",
            ContentKind::ShaderPack,
            "modrinth",
            "R6NEzAwj",
            "Dk9AbCdE",
            "Complementary Unbound",
            "complementary-unbound",
        )];
        let mut catalogue = catalogue_of(&scratch.paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["complementary.zip"]);
        let dest = scratch.paths.root.join("shaders.zip");
        write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        let names = entries(&dest);
        assert!(
            names.contains(&"minecraft/shaderpacks/complementary-unbound.pw.toml".to_string()),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|name| name.contains("shaderpacks/.index")),
            "{names:?}"
        );
    }

    /// The limit of the format, respected rather than optimised around:
    /// `processMultiMC` takes the extracted tree as it finds it and downloads
    /// nothing, so a jar left out because it was named is a mod Prism shows as
    /// missing and will never fetch.
    #[tokio::test]
    async fn every_jar_travels_even_when_it_is_named() {
        let (scratch, mut inst, _) = exportable("bytes", &[("mods", "sodium.jar")]).await;
        inst.content = vec![record(
            "sodium.jar",
            ContentKind::Mod,
            "modrinth",
            "AANobbMI",
            "Hu34AbCd",
            "Sodium",
            "sodium",
        )];
        let mut catalogue = catalogue_of(&scratch.paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar"]);
        let dest = scratch.paths.root.join("bytes.zip");
        let report = write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        assert_eq!(report.referenced, 1, "it was named");
        let names = entries(&dest);
        assert!(
            names.contains(&"minecraft/mods/sodium.jar".to_string()),
            "and it travelled anyway: {names:?}"
        );
        assert!(
            names.contains(&"minecraft/mods/.index/sodium.pw.toml".to_string()),
            "{names:?}"
        );
        assert_eq!(
            text_of(&dest, "minecraft/mods/sodium.jar"),
            "the bytes of sodium.jar"
        );
    }

    /// A turned-off mod travels under its `.disabled` name, which is what
    /// arrives turned off — and its entry names the enabled file, because
    /// Prism matches a resource to its metadata with `.disabled` stripped off
    /// and an entry naming the file on disk would match nothing.
    #[tokio::test]
    async fn a_turned_off_mod_is_named_by_the_file_prism_matches() {
        let (scratch, mut inst, _) = exportable("off", &[("mods", "sodium.jar.disabled")]).await;
        inst.content = vec![record(
            "sodium.jar",
            ContentKind::Mod,
            "modrinth",
            "AANobbMI",
            "Hu34AbCd",
            "Sodium",
            "sodium",
        )];
        let mut catalogue = catalogue_of(&scratch.paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar"]);
        let dest = scratch.paths.root.join("off.zip");
        let report = write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        assert_eq!(report.referenced, 1);
        assert_eq!(report.embedded_turned_off, 0, "the name carries the state");
        let names = entries(&dest);
        assert!(
            names.contains(&"minecraft/mods/sodium.jar.disabled".to_string()),
            "{names:?}"
        );
        let entry = index_entry(&dest, "minecraft/mods/.index/sodium.pw.toml");
        assert_eq!(entry["filename"].as_str(), Some("sodium.jar"));
    }

    /// An instance imported from Prism brought its old index along. The index
    /// written here replaces it rather than joining it: two entries for one
    /// mod is what a differing slug would produce, and an entry for a jar
    /// since deleted is a mod Prism shows as missing.
    #[tokio::test]
    async fn an_index_already_on_disk_is_replaced_rather_than_doubled() {
        let (scratch, mut inst, _) = exportable("stale", &[("mods", "sodium.jar")]).await;
        let index = scratch.paths.game_dir(&inst.id).join("mods/.index");
        tokio::fs::create_dir_all(&index).await.unwrap();
        // Prism's name for the same mod, and an entry for a jar that has gone.
        tokio::fs::write(index.join("sodium-the-mod.pw.toml"), MODRINTH_ENTRY)
            .await
            .unwrap();
        tokio::fs::write(index.join("deleted-long-ago.pw.toml"), MODRINTH_ENTRY)
            .await
            .unwrap();
        inst.content = vec![record(
            "sodium.jar",
            ContentKind::Mod,
            "modrinth",
            "AANobbMI",
            "Hu34AbCd",
            "Sodium",
            "sodium",
        )];
        let mut catalogue = catalogue_of(&scratch.paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar"]);
        let dest = scratch.paths.root.join("stale.zip");
        write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        let written: Vec<String> = entries(&dest)
            .into_iter()
            .filter(|name| name.ends_with(".pw.toml"))
            .collect();
        assert_eq!(written, vec!["minecraft/mods/.index/sodium.pw.toml"]);
    }

    /// Round-tripping through Geode's own reader proves this writer and that
    /// reader agree about the format. It does not prove Prism agrees — only
    /// opening a real export in Prism does that — but a regression here fails
    /// in the test suite instead of in somebody's launcher.
    #[tokio::test]
    async fn geode_reads_back_its_own_prism_export() {
        let (scratch, mut inst, _) = exportable(
            "roundtrip",
            &[("mods", "sodium.jar"), ("config", "sodium-options.json")],
        )
        .await;
        inst.name = "Kitchen Sink".into();
        // Two lines and a comma, which is what QSettings has its own escapes
        // for and what a value written raw would turn into a broken file.
        inst.notes = "Keep the elytra in the ender chest,\nand the shulkers too".into();
        inst.mc_version = "1.20.1".into();
        inst.loader = Loader::Forge;
        inst.loader_version = "1.20.1-47.2.0".into();
        inst.overrides.memory_mb = Some(8192);
        inst.overrides.width = Some(1920);
        inst.overrides.height = Some(1080);
        inst.content = vec![record(
            "sodium.jar",
            ContentKind::Mod,
            "modrinth",
            "AANobbMI",
            "Hu34AbCd",
            "Sodium",
            "sodium",
        )];
        let catalogue = catalogue_of(&scratch.paths, &inst).await;
        let dest = scratch.paths.root.join("roundtrip.zip");
        write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        let back = import(&scratch.paths, "read-back", &dest).await.unwrap();
        assert_eq!(back.name, inst.name);
        assert_eq!(back.notes, inst.notes);
        assert_eq!(back.mc_version, "1.20.1");
        assert_eq!(back.loader, Loader::Forge);
        // The short form went out, and `meta::forge` puts the game version
        // back on the way in.
        assert_eq!(back.loader_version, "47.2.0");
        assert_eq!(back.overrides.memory_mb, Some(8192));
        assert_eq!(back.overrides.width, Some(1920));
        assert_eq!(back.overrides.height, Some(1080));
        assert_eq!(
            provenance(&back),
            vec!["Mod sodium.jar modrinth AANobbMI Hu34AbCd"]
        );
        let game = scratch.paths.game_dir("read-back");
        assert!(game.join("mods/sodium.jar").exists());
        assert!(game.join("config/sodium-options.json").exists());
    }

    /// The hard rejects, asserted here so a regression fails in the test suite
    /// rather than as "this instance cannot be opened" in Prism: an instance
    /// type its list refuses, a profile version it fails the whole file over,
    /// and a second `instance.cfg` that would make the zip's format a matter
    /// of iteration order.
    #[tokio::test]
    async fn a_prism_export_is_an_instance_prism_accepts() {
        let (scratch, mut inst, _) = exportable(
            "accepts",
            &[("mods", "sodium.jar"), ("saves/world", "level.dat")],
        )
        .await;
        inst.content = vec![record(
            "sodium.jar",
            ContentKind::Mod,
            "modrinth",
            "AANobbMI",
            "Hu34AbCd",
            "Sodium",
            "sodium",
        )];
        let catalogue = catalogue_of(&scratch.paths, &inst).await;
        let dest = scratch.paths.root.join("accepts.zip");
        write_to(&scratch.paths, &inst, &dest, &catalogue).await;

        let names = entries(&dest);
        assert_eq!(
            names.iter().filter(|name| *name == "instance.cfg").count(),
            1,
            "exactly one, so the zip's format is not a matter of order: {names:?}"
        );
        for refused in ["modrinth.index.json", "manifest.json", "bin/modpack.jar"] {
            assert!(
                !names.contains(&refused.to_string()),
                "{refused} at the root would be read as another format: {names:?}"
            );
        }
        for name in &names {
            assert!(
                !name.starts_with('/') && !name.contains(".."),
                "nothing that escapes the instance root: {name}"
            );
        }

        let cfg = text_of(&dest, "instance.cfg");
        let general = general(&cfg);
        assert_eq!(general.get("InstanceType").map(String::as_str), Some("OneSix"));
        let profile: serde_json::Value =
            serde_json::from_str(&text_of(&dest, "mmc-pack.json")).unwrap();
        assert_eq!(profile["formatVersion"], serde_json::json!(1));
        for component in profile["components"].as_array().unwrap() {
            assert!(
                component["uid"].as_str().is_some_and(|uid| !uid.is_empty()),
                "a component with no uid is one Prism drops: {component}"
            );
        }
        // An index entry is only provenance where packwiz's own reader will
        // take it, which needs both tables.
        let entry = index_entry(&dest, "minecraft/mods/.index/sodium.pw.toml");
        assert!(entry["download"].as_table().is_some(), "{entry:?}");
        assert!(entry["update"]["modrinth"].as_table().is_some(), "{entry:?}");
    }
}

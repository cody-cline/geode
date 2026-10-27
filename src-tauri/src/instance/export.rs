//! What an export would do, worked out before anything is written.
//!
//! An export used to be a button and a report afterwards: you pressed
//! "Export .mrpack" and were told what had happened to your mods. This module
//! is the other half — what the instance holds, what the format you picked can
//! do with it, and what each mod would do — so the sharer chooses with the
//! answer in front of them rather than reading it after the file is on disk.
//!
//! Nothing here works anything out a second time. The groups come from the
//! same [`group_of`] the pack's own walk decides with, the capability notes
//! come off the format's own writer, and what happens to each file is the
//! writers' own planning pass through [`identity::travel`]. A preview that
//! computed its own answer would eventually promise something the export then
//! did differently, and the only reason to show it is that it is true.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::install::identity::{self, Ask, ById, Carry, Catalogue, Copied, Identity};
use crate::instance::content::ContentKind;
use crate::instance::modpack::{ExportReport, Journey};
use crate::instance::{prism, Instance};
use crate::net::Progress;
use crate::paths::Paths;

/// One of the things an instance holds, as the export dialog lists them.
///
/// The player's own view of their game directory rather than the writer's:
/// `logs`, `crash-reports` and `screenshots` are one thing to decide about,
/// and everything loose at the root is another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Group {
    Mods,
    Config,
    ResourcePacks,
    ShaderPacks,
    DataPacks,
    Saves,
    /// Screenshots, logs and crash reports.
    Logs,
    /// Everything else at the instance's root — `options.txt`, a mod's own
    /// folder, whatever the player put there.
    Other,
}

/// Every group, in the order the dialog shows them.
pub const ALL: [Group; 8] = [
    Group::Mods,
    Group::Config,
    Group::ResourcePacks,
    Group::ShaderPacks,
    Group::DataPacks,
    Group::Saves,
    Group::Logs,
    Group::Other,
];

/// What a pack of an instance carried before the sharer was asked, and what
/// every caller that is not the export dialog still carries.
///
/// The `saves` and `logs` groups left out, which is exactly what the
/// `include_saves: false` every button passed used to mean.
const BY_DEFAULT: [Group; 6] = [
    Group::Mods,
    Group::Config,
    Group::ResourcePacks,
    Group::ShaderPacks,
    Group::DataPacks,
    Group::Other,
];

/// Entries of a game directory no pack has ever carried, and none should.
///
/// Caches rather than the player's files, every one of them rebuilt on the
/// next launch. They are in no group at all rather than falling into
/// [`Group::Other`], because somebody ticking "everything else" is asking for
/// their own files, not for a gigabyte of cache.
///
/// The list was written for Fabric and then measured against a real, played
/// NeoForge instance with Sinytra Connector — 56 mods, 380 MB — where the
/// first two matched nothing at all and these five held 51 MB:
///
/// * `.fabric` — the loader's unpacked jars.
/// * `versions` — a launcher's copy of the game itself.
/// * `.cache` — Connector's remapped jars, 47 MB of that instance on its own.
/// * `.sable` — extracted natives.
/// * `.mixin.out` — mixin's dumped classes, written only with debug on.
/// * `dynamic-resource-pack-cache`, `dynamic-data-pack-cache` — packs
///   Supplementaries and its kin generate, each beside the `hash.txt` they
///   regenerate themselves from.
///
/// Nothing is here on the strength of its name. `pfm/` on that same instance
/// is 28 MB and holds nothing but a `cache/` folder, and it is deliberately
/// not listed: it is a mod's own directory that happens to hold a cache
/// today, and a pack that dropped the player's files out of one would be a
/// worse failure than a pack that is large.
const NEVER: [&str; 7] = [
    ".fabric",
    "versions",
    ".cache",
    ".sable",
    ".mixin.out",
    "dynamic-resource-pack-cache",
    "dynamic-data-pack-cache",
];

impl Group {
    /// The content kind whose folder this group is, where it is one.
    ///
    /// What makes a group referenceable: a mod, a resource pack, a shader pack
    /// or a data pack is bytes a platform can name, and a world or a config
    /// file is not.
    pub fn kind(self) -> Option<ContentKind> {
        match self {
            Group::Mods => Some(ContentKind::Mod),
            Group::ResourcePacks => Some(ContentKind::ResourcePack),
            Group::ShaderPacks => Some(ContentKind::ShaderPack),
            Group::DataPacks => Some(ContentKind::DataPack),
            Group::Config | Group::Saves | Group::Logs | Group::Other => None,
        }
    }

    /// Whether a pack of this format is able to carry any of this group as a
    /// reference rather than as bytes.
    ///
    /// Read off what the format itself can do rather than written out once per
    /// group, so the dialog cannot come to promise something the writers then
    /// do differently. Mostly that is [`Carry`]'s answer; the exception is a
    /// Prism instance, whose index is a folder per kind and has none for a
    /// data pack — more than a `Carry` is able to say.
    pub fn can_reference(self, format: PackFormat) -> bool {
        // Nothing names a config file, a world or a screenshot, so there is no
        // format that could reference one: those bytes travel or they stay at
        // home.
        let Some(kind) = self.kind() else {
            return false;
        };
        // `prism::plan_instance` copies a file whose kind has no index folder
        // however well a platform names it, so the one that holds that fact is
        // the one asked — not `Carry::PRISM`, which can only say that an index
        // exists.
        if format == PackFormat::Prism {
            return prism::index_folder(kind).is_some();
        }
        let carry = format.carry();
        match kind {
            ContentKind::Mod => carry.index || carry.by_id != ById::Nothing,
            // Anything but a mod needs a channel that can say which folder the
            // file belongs in. A CurseForge manifest names two integers and no
            // path, so a shader pack referenced there would arrive in `mods/`.
            _ => carry.index || carry.by_id == ById::AnyFile,
        }
    }
}

/// Which group a top-level entry of the game directory belongs to, when any.
///
/// The one place the layout of an instance is turned into the sharer's view of
/// it, so the counts the dialog shows and the files the pack carries cannot
/// come to disagree.
pub fn group_of(top: &str) -> Option<Group> {
    if NEVER.contains(&top) {
        return None;
    }
    // By the kind's own folder name, so renaming one cannot leave this
    // behind pointing at a folder that no longer exists.
    if let Some(group) = ALL
        .iter()
        .copied()
        .find(|group| group.kind().is_some_and(|kind| kind.folder() == top))
    {
        return Some(group);
    }
    Some(match top {
        "config" => Group::Config,
        "saves" => Group::Saves,
        "logs" | "crash-reports" | "screenshots" => Group::Logs,
        _ => Group::Other,
    })
}

/// Which of an instance's own file groups travel in the pack.
///
/// This replaced the `include_saves` boolean every writer took. The boolean
/// could answer one of the questions a sharer has, and the folders it did not
/// name were decided for them by a list inside the writer — so a pack never
/// carried a screenshot, and nobody was ever asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    groups: Vec<Group>,
}

impl Selection {
    pub fn of(groups: Vec<Group>) -> Self {
        Self { groups }
    }

    pub fn travels(&self, group: Group) -> bool {
        self.groups.contains(&group)
    }

    /// Whether one of the catalogue's files travels at all.
    ///
    /// Decided from where the file sits on disk, through the same [`group_of`]
    /// the walk decides on, so a file the walk leaves at home is a file no
    /// writer names either. A pack that named one would have the recipient
    /// fetch the very mod the sharer left out.
    pub fn carries(&self, disk_rel: &Path) -> bool {
        disk_rel
            .components()
            .next()
            .and_then(|top| top.as_os_str().to_str())
            .and_then(group_of)
            .is_some_and(|group| self.travels(group))
    }

    /// Every group, for a pack whose files were chosen one by one rather than
    /// by group: a server's pack for its players is walked by allowlist, and
    /// every file in its catalogue is there because that walk named it.
    pub fn everything() -> Self {
        Self::of(ALL.to_vec())
    }

    /// The old boolean, for the callers that still have one to give: a pack
    /// staged for Google Drive is published from its own dialog, which asks
    /// about worlds and nothing else.
    pub fn with_saves(include_saves: bool) -> Self {
        let mut groups = BY_DEFAULT.to_vec();
        if include_saves {
            groups.push(Group::Saves);
        }
        Self::of(groups)
    }
}

impl Default for Selection {
    /// What a pack of this instance carried before anybody was asked.
    fn default() -> Self {
        Self::with_saves(false)
    }
}

/// How much of one group there is to carry.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupTally {
    pub group: Group,
    pub files: usize,
    pub bytes: u64,
}

/// What the instance holds, group by group.
///
/// Every group, including the empty ones: a sharer deciding whether to send
/// their worlds is owed "no worlds here" rather than a row that is not on
/// screen.
pub async fn tally(game_dir: PathBuf) -> Result<Vec<GroupTally>> {
    tokio::task::spawn_blocking(move || tally_in(&game_dir))
        .await
        .map_err(|e| Error::msg(format!("looking through the instance panicked: {e}")))
}

/// One walk of the whole game directory, counting every file into its group.
///
/// **What the walk costs:** one `read_dir` per directory and one `metadata` per
/// entry, so an instance with a large world is tens of thousands of stats —
/// milliseconds on a warm cache and seconds on a cold disk. That is why the
/// dialog asks for this once, when it opens: the groups are a fact about the
/// instance, and neither the format picked nor a box ticked changes them.
fn tally_in(game_dir: &Path) -> Vec<GroupTally> {
    let mut totals: HashMap<Group, (usize, u64)> = HashMap::new();
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
            // The group is decided by the top-level entry a file sits under,
            // which is what the pack's own walk decides on too.
            let Some(top) = rel
                .components()
                .next()
                .and_then(|part| part.as_os_str().to_str())
            else {
                continue;
            };
            let Some(group) = group_of(top) else {
                continue;
            };
            // Follows links, because the pack's walk follows them: a `saves`
            // or `resourcepacks` shared between instances is a common setup,
            // and it is what the player sees in game.
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else {
                let counted = totals.entry(group).or_default();
                counted.0 += 1;
                counted.1 += meta.len();
            }
        }
    }
    ALL.iter()
        .map(|group| {
            let (files, bytes) = totals.get(group).copied().unwrap_or_default();
            GroupTally {
                group: *group,
                files,
                bytes,
            }
        })
        .collect()
}

/// The formats an instance exports as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PackFormat {
    Geodepack,
    Mrpack,
    Curseforge,
    Prism,
}

impl PackFormat {
    /// What a pack of this format is able to say about a file.
    ///
    /// The one mapping, so the dry run and the export cannot be given
    /// different answers to the same question.
    pub fn carry(self) -> Carry {
        match self {
            PackFormat::Geodepack => Carry::GEODEPACK,
            PackFormat::Mrpack => Carry::MRPACK,
            PackFormat::Curseforge => Carry::CFPACK,
            PackFormat::Prism => Carry::PRISM,
        }
    }
}

/// Whether a format can carry one group as a reference at all.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCarry {
    pub group: Group,
    pub can_reference: bool,
}

/// Which of the three things a pack does with a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Travels {
    /// An index entry: a URL and hashes the recipient fetches with nobody's
    /// credentials.
    Linked,
    /// Named by id in the pack's own channel, and fetched with the recipient's
    /// own key.
    Named,
    /// Its bytes go in the pack.
    Copied,
}

/// Why a file's bytes travel.
///
/// [`Copied`]'s four reasons, and the fifth only a writer can know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Why {
    Unknown,
    Elsewhere,
    TurnedOff,
    Blocked,
    Duplicate,
}

impl Why {
    fn of(journey: &Journey<'_>) -> Option<Self> {
        Some(match journey {
            Journey::Linked(_) | Journey::Named(_) => return None,
            Journey::Copied(Copied::Unknown) => Why::Unknown,
            Journey::Copied(Copied::Elsewhere) => Why::Elsewhere,
            Journey::Copied(Copied::TurnedOff) => Why::TurnedOff,
            Journey::Copied(Copied::Blocked) => Why::Blocked,
            Journey::Duplicate => Why::Duplicate,
        })
    }
}

/// What a pack would do with one of the instance's content files.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileJourney {
    pub kind: ContentKind,
    /// The enabled name, which is how the mods list keys a file.
    pub file_name: String,
    pub disabled: bool,
    pub size: u64,
    pub travels: Travels,
    /// Which platform the pack names it on, when it names one.
    pub provider: Option<String>,
    /// Why its bytes travel, when they do.
    pub why: Option<Why>,
}

/// Everything the dialog needs about one format, from the pass the export
/// itself will make.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPlan {
    /// Which groups this format can carry as references rather than bytes.
    pub carries: Vec<GroupCarry>,
    /// Whether a reference spares the pack the bytes at all. False for a Prism
    /// instance zip, where every file travels either way and a reference buys
    /// the recipient a name and an update path instead of a smaller download.
    pub spares_bytes: bool,
    /// What the pack would do with every content file, in the order the writer
    /// walks them.
    pub content: Vec<FileJourney>,
    /// The counts the export will report, counted from this very pass.
    pub report: ExportReport,
}

/// Ask the platforms, then work out what the pack would do.
///
/// The same [`identity::for_instance`] the export runs, with the same [`Ask`],
/// so the dialog's numbers are the export's numbers. It costs what the export's
/// own identify pass costs — a hash of every content file, and one request per
/// platform — which is why it is per format and cached by the caller rather
/// than refetched.
pub async fn plan(
    ask: Ask<'_>,
    paths: &Paths,
    instance: &Instance,
    format: PackFormat,
    selection: &Selection,
    progress: &Progress,
) -> Result<ExportPlan> {
    let catalogue = identity::for_instance(ask, paths, instance, progress).await?;
    Ok(plan_from(instance, format, selection, &catalogue))
}

/// The same, from a catalogue already built — which is what the tests plan
/// through, and what proves the dry run and the writer agree.
///
/// The selection is part of the question, not a filter laid over the answer: a
/// group left out travels in no channel, so a file in one is in neither the
/// list this returns nor the counts beside it.
pub fn plan_from(
    instance: &Instance,
    format: PackFormat,
    selection: &Selection,
    catalogue: &Catalogue,
) -> ExportPlan {
    // Each writer's own planning pass, never a second opinion about it: the
    // reasons a file ends up as bytes are not all `travel`'s to give, and a
    // CurseForge manifest's duplicate ids and a Prism index's missing folder
    // are exactly the ones a preview would otherwise miss.
    let journeys: Vec<(&Identity, Journey<'_>)> = match format {
        PackFormat::Geodepack | PackFormat::Mrpack => {
            crate::instance::modpack::plan_pack(catalogue, format.carry(), selection)
        }
        PackFormat::Curseforge => crate::instance::modpack::plan_cfpack(catalogue, selection)
            .into_iter()
            .map(|planned| (planned.ident, planned.journey))
            .collect(),
        PackFormat::Prism => prism::plan_instance(catalogue, instance, selection)
            .into_iter()
            .map(|planned| (planned.ident, planned.journey))
            .collect(),
    };

    let mut report = ExportReport {
        trouble: catalogue.trouble.clone(),
        ..Default::default()
    };
    let mut content = Vec::with_capacity(journeys.len());
    for (ident, journey) in &journeys {
        report.count(journey);
        content.push(FileJourney {
            kind: ident.kind,
            file_name: ident.file_name.clone(),
            disabled: ident.disabled,
            size: ident.size,
            travels: match journey {
                Journey::Linked(_) => Travels::Linked,
                Journey::Named(_) => Travels::Named,
                Journey::Copied(_) | Journey::Duplicate => Travels::Copied,
            },
            provider: match journey {
                Journey::Linked(reference) | Journey::Named(reference) => {
                    Some(reference.provider.clone())
                }
                // Not the platform that *nearly* named it: the pack is not
                // naming this file, and printing a provider beside bytes that
                // travelled reads as a link the recipient can follow.
                Journey::Copied(_) | Journey::Duplicate => None,
            },
            why: Why::of(journey),
        });
    }

    ExportPlan {
        carries: ALL
            .iter()
            .map(|group| GroupCarry {
                group: *group,
                can_reference: group.can_reference(format),
            })
            .collect(),
        spares_bytes: format.carry().spares_bytes,
        content,
        report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::Loader;
    use crate::paths::Scratch;

    /// Write the fixture out through the real writer for that format, so what
    /// the plan promised can be held against what the export reports.
    async fn export_as(
        paths: &Paths,
        inst: &Instance,
        format: PackFormat,
        selection: &Selection,
        catalogue: &Catalogue,
    ) -> ExportReport {
        use crate::instance::modpack;

        let dest = paths.root.join(format!("planned-{format:?}.pack"));
        let progress = Progress::silent("t");
        match format {
            PackFormat::Geodepack => {
                crate::share::pack::export_with(paths, inst, dest, selection, catalogue, &progress)
                    .await
            }
            PackFormat::Mrpack => {
                modpack::write_instance_pack(
                    paths,
                    inst,
                    dest,
                    selection,
                    modpack::GeodeHalf::default(),
                    catalogue,
                    &progress,
                )
                .await
            }
            PackFormat::Curseforge => {
                modpack::write_cfpack(
                    modpack::instance_input(paths, inst, selection),
                    "Somebody",
                    dest,
                    catalogue,
                    &progress,
                )
                .await
            }
            PackFormat::Prism => {
                prism::write_instance(paths, inst, dest, selection, catalogue, &progress).await
            }
        }
        .unwrap()
    }

    /// An instance on disk holding one file in each named place, with a
    /// catalogue of whatever its content folders turned out to have.
    async fn fixture(tag: &str, files: &[(&str, &[u8])]) -> (Scratch, Instance, Catalogue) {
        let scratch = Scratch::new(&format!("export-{tag}"));
        let mut inst = Instance::new(
            "Kitchen Sink".into(),
            "1.21.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        );
        inst.id = format!("export-{tag}");
        let game = scratch.paths.game_dir(&inst.id);
        for (rel, bytes) in files {
            let path = game.join(rel);
            tokio::fs::create_dir_all(path.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(path, bytes).await.unwrap();
        }
        let catalogue =
            identity::for_instance(Ask::Nobody, &scratch.paths, &inst, &Progress::silent("t"))
                .await
                .unwrap();
        (scratch, inst, catalogue)
    }

    fn counted(tallies: &[GroupTally], group: Group) -> (usize, u64) {
        let found = tallies
            .iter()
            .find(|tally| tally.group == group)
            .unwrap_or_else(|| panic!("no row for {group:?}"));
        (found.files, found.bytes)
    }

    /// The groups are the sharer's view of their own instance, so a file goes
    /// where they would look for it: a world under worlds, a screenshot beside
    /// the logs, and `options.txt` in what is left.
    #[tokio::test]
    async fn an_instance_is_grouped_the_way_its_owner_reads_it() {
        let (scratch, inst, _) = fixture(
            "grouped",
            &[
                ("mods/create.jar", b"a mod"),
                ("mods/jei.jar.disabled", b"a mod turned off"),
                ("config/create/common.toml", b"config"),
                ("resourcepacks/faithful.zip", b"pack"),
                ("shaderpacks/complementary.zip", b"shader"),
                ("datapacks/vanilla-tweaks.zip", b"datapack"),
                ("saves/Hardcore/level.dat", b"world"),
                ("saves/Hardcore/region/r.0.0.mca", b"region"),
                ("screenshots/one.png", b"shot"),
                ("logs/latest.log", b"log"),
                ("crash-reports/crash.txt", b"crash"),
                ("options.txt", b"options"),
            ],
        )
        .await;

        let tallies = tally(scratch.paths.game_dir(&inst.id)).await.unwrap();
        assert_eq!(counted(&tallies, Group::Mods), (2, 21));
        assert_eq!(counted(&tallies, Group::Config), (1, 6));
        assert_eq!(counted(&tallies, Group::ResourcePacks), (1, 4));
        assert_eq!(counted(&tallies, Group::ShaderPacks), (1, 6));
        assert_eq!(counted(&tallies, Group::DataPacks), (1, 8));
        // Both files of the world, however deep they sit.
        assert_eq!(counted(&tallies, Group::Saves), (2, 11));
        // One thing to decide about, not three.
        assert_eq!(counted(&tallies, Group::Logs), (3, 12));
        assert_eq!(counted(&tallies, Group::Other), (1, 7));
    }

    /// A cache is nobody's file to send. `.fabric` and `versions` are rebuilt
    /// on first launch, and letting them fall into "everything else" would ship
    /// a gigabyte to somebody who asked for their own options file.
    #[tokio::test]
    async fn a_rebuilt_cache_is_in_no_group_at_all() {
        let (scratch, inst, _) = fixture(
            "caches",
            &[
                (".fabric/remappedJars/x.jar", b"cache"),
                ("versions/1.21.1/1.21.1.json", b"a copy of the game"),
                ("options.txt", b"options"),
            ],
        )
        .await;

        let tallies = tally(scratch.paths.game_dir(&inst.id)).await.unwrap();
        assert_eq!(counted(&tallies, Group::Other), (1, 7));
        assert_eq!(group_of(".fabric"), None);
        assert_eq!(group_of("versions"), None);

        // The ones a NeoForge instance with Sinytra Connector writes, which
        // the list was not written for: 51 MB of the 380 MB instance this was
        // measured against, `.cache` alone being 47 MB of it.
        for cache in [
            ".cache",
            ".sable",
            ".mixin.out",
            "dynamic-resource-pack-cache",
            "dynamic-data-pack-cache",
        ] {
            assert_eq!(group_of(cache), None, "{cache} would travel");
        }

        // And the line that is not crossed. `pfm/` on that same instance is
        // 28 MB and holds nothing but a `cache/`, and it still travels: it is
        // a mod's own directory, and dropping a player's files out of a pack
        // is a worse failure than a pack that is large. Beside it, two more
        // mod directories that are nothing but the player's own work.
        for theirs in ["pfm", "immersive_furniture", "xaero"] {
            assert_eq!(group_of(theirs), Some(Group::Other), "{theirs}");
        }
    }

    /// Every format an instance exports as.
    const FORMATS: [PackFormat; 4] = [
        PackFormat::Geodepack,
        PackFormat::Mrpack,
        PackFormat::Curseforge,
        PackFormat::Prism,
    ];

    /// What a format can reference is the format's own answer, and the one
    /// that trips people up is CurseForge: it names two integers and no
    /// folder, so a shader pack referenced there would arrive in `mods/` doing
    /// nothing.
    #[test]
    fn a_format_that_names_no_folder_can_only_reference_mods() {
        for group in [Group::ResourcePacks, Group::ShaderPacks] {
            assert!(
                group.can_reference(PackFormat::Geodepack),
                "{group:?} in a .geodepack"
            );
            assert!(
                group.can_reference(PackFormat::Mrpack),
                "{group:?} in a .mrpack"
            );
            assert!(
                group.can_reference(PackFormat::Prism),
                "{group:?} in a Prism instance"
            );
            assert!(
                !group.can_reference(PackFormat::Curseforge),
                "{group:?} in a CurseForge pack"
            );
        }
        for format in FORMATS {
            assert!(Group::Mods.can_reference(format), "a mod in {format:?}");
        }
    }

    /// A Prism instance keeps its packwiz index in a folder per kind and keeps
    /// none for a data pack, so a data pack there has nowhere for its
    /// provenance to go — a limit `Carry::PRISM` cannot state, since all it
    /// can say is that there is an index. The row and the mod list below it
    /// are read together, so one promising a name while the other says the
    /// bytes travel is two contradictory sentences on one screen.
    #[test]
    fn a_prism_instance_names_no_data_pack() {
        assert!(!Group::DataPacks.can_reference(PackFormat::Prism));
        // The formats whose one index names any path do name one.
        assert!(Group::DataPacks.can_reference(PackFormat::Geodepack));
        assert!(Group::DataPacks.can_reference(PackFormat::Mrpack));
        assert!(!Group::DataPacks.can_reference(PackFormat::Curseforge));
    }

    /// Nothing names a world or a config file, so no format can do anything
    /// but carry the bytes — which is what the dialog has to say rather than
    /// leave the sharer to guess.
    #[test]
    fn nobody_can_reference_a_world_or_a_config_file() {
        for group in [Group::Config, Group::Saves, Group::Logs, Group::Other] {
            for format in FORMATS {
                assert!(!group.can_reference(format), "{group:?} in {format:?}");
            }
        }
    }

    /// The whole point of the dry run: the numbers shown before the export are
    /// the numbers the export then reports. Asserted per format, because each
    /// one has reasons of its own that `travel` does not decide — and per
    /// selection, because a group left out is carried by no channel of any of
    /// them, and a file counted as having travelled is a file the sharer was
    /// told went into a pack it is not in.
    #[tokio::test]
    async fn the_dry_run_counts_what_the_export_then_reports() {
        let asked = [
            ("everything the default carries", Selection::default()),
            (
                "the mods left out",
                Selection::of(vec![
                    Group::Config,
                    Group::ResourcePacks,
                    Group::ShaderPacks,
                    Group::DataPacks,
                    Group::Other,
                ]),
            ),
        ];
        for format in FORMATS {
            for (what, selection) in &asked {
                let (scratch, inst, mut catalogue) = fixture(
                    &format!("agree-{format:?}-{}", what.replace(' ', "-")),
                    &[
                        ("mods/create.jar", b"create"),
                        ("mods/create-copy.jar", b"create"),
                        ("mods/jei.jar", b"jei"),
                        ("mods/secret.jar", b"nobody has this"),
                        ("mods/off.jar.disabled", b"turned off"),
                        ("resourcepacks/faithful.zip", b"faithful"),
                        ("datapacks/tweaks.zip", b"tweaks"),
                    ],
                )
                .await;
                catalogue.pretend_modrinth_holds(&[
                    "jei.jar",
                    "off.jar",
                    "faithful.zip",
                    "tweaks.zip",
                ]);
                catalogue.pretend_curseforge_holds(&[
                    ("create.jar", 328085, 4593548, true),
                    ("create-copy.jar", 328085, 4593548, true),
                ]);

                let planned = plan_from(&inst, format, selection, &catalogue);
                let written = export_as(&scratch.paths, &inst, format, selection, &catalogue).await;

                assert_eq!(
                    tallied(&planned.report),
                    tallied(&written),
                    "{format:?} with {what} was previewed as one thing and exported as another"
                );
                assert_eq!(
                    planned.content.len(),
                    tallied(&planned.report).iter().sum::<usize>(),
                    "{format:?} with {what}: every file the plan lists is counted once"
                );
            }
        }
    }

    /// A group the sharer unticked travels in no channel at all. The two
    /// halves of getting that wrong are both worse than leaving the file out:
    /// a mod still named in the index is one the recipient's launcher
    /// downloads although the sharer excluded it, and a mod nothing can name
    /// is counted as "bundled in" although its bytes are nowhere in the zip.
    #[tokio::test]
    async fn a_group_left_out_is_in_neither_the_pack_nor_the_counts() {
        use std::io::Read;

        let (scratch, inst, mut catalogue) = fixture(
            "left-out",
            &[
                ("mods/jei.jar", b"jei"),
                ("mods/homemade.jar", b"nobody has this"),
                ("config/x.toml", b"config"),
            ],
        )
        .await;
        catalogue.pretend_modrinth_holds(&["jei.jar"]);

        // The dialog with its Mods box unticked, and nothing else to carry but
        // the config.
        let selection = Selection::of(vec![Group::Config]);
        let planned = plan_from(&inst, PackFormat::Mrpack, &selection, &catalogue);
        assert!(
            planned.content.is_empty(),
            "a file of an unticked group was planned: {:?}",
            planned.content
        );
        assert_eq!(tallied(&planned.report), [0; 6]);

        let written = export_as(
            &scratch.paths,
            &inst,
            PackFormat::Mrpack,
            &selection,
            &catalogue,
        )
        .await;
        assert_eq!(tallied(&written), [0; 6]);

        let dest = scratch.paths.root.join("planned-Mrpack.pack");
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let mut names: Vec<String> = zip.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(names, ["modrinth.index.json", "overrides/config/x.toml"]);

        let mut raw = String::new();
        zip.by_name("modrinth.index.json")
            .unwrap()
            .read_to_string(&mut raw)
            .unwrap();
        let index: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            index["files"],
            serde_json::json!([]),
            "the index named a file the pack was asked not to carry"
        );
    }

    /// Every count a report carries, so a mismatch names itself.
    fn tallied(report: &ExportReport) -> [usize; 6] {
        [
            report.referenced,
            report.embedded_unknown,
            report.embedded_elsewhere,
            report.embedded_turned_off,
            report.embedded_blocked,
            report.embedded_duplicate,
        ]
    }

    /// Why a file's bytes travel is the part a sharer can act on, and the
    /// reasons are not interchangeable: a mod nothing has is nobody's fault, a
    /// mod this format cannot link is answered by picking another format.
    #[tokio::test]
    async fn the_plan_says_why_each_file_travels_as_bytes() {
        let (_scratch, inst, mut catalogue) = fixture(
            "reasons",
            &[
                ("mods/jei.jar", b"jei"),
                ("mods/secret.jar", b"nobody has this"),
                ("mods/off.jar.disabled", b"turned off"),
                ("mods/blocked.jar", b"blocked"),
            ],
        )
        .await;
        catalogue.pretend_modrinth_holds(&["jei.jar", "off.jar"]);
        catalogue.pretend_curseforge_holds(&[("blocked.jar", 1, 2, false)]);

        let plan = plan_from(&inst, PackFormat::Mrpack, &Selection::default(), &catalogue);
        let said = |name: &str| {
            let found = plan
                .content
                .iter()
                .find(|file| file.file_name == name)
                .unwrap_or_else(|| panic!("{name} is not in the plan"));
            (found.travels, found.why)
        };

        assert_eq!(said("jei.jar"), (Travels::Linked, None));
        assert_eq!(said("secret.jar"), (Travels::Copied, Some(Why::Unknown)));
        // A .mrpack has nowhere to say a mod is off, so a reference would
        // arrive switched on.
        assert_eq!(said("off.jar"), (Travels::Copied, Some(Why::TurnedOff)));
        // Only CurseForge names it, and its author turned third-party
        // downloads off: nobody's format gets around that.
        assert_eq!(said("blocked.jar"), (Travels::Copied, Some(Why::Blocked)));
    }

    /// A platform is only shown beside a file the pack is actually naming. The
    /// case that matters is the middle one: Modrinth holds `off.jar`, and a
    /// `.mrpack` still has to carry its bytes because it cannot say a mod is
    /// turned off. Printing "modrinth" beside it would read as a link the
    /// recipient can follow, and there is none in the pack.
    #[tokio::test]
    async fn only_a_file_the_pack_names_is_shown_with_a_platform() {
        let (_scratch, inst, mut catalogue) = fixture(
            "providers",
            &[
                ("mods/jei.jar", b"jei"),
                ("mods/off.jar.disabled", b"turned off"),
                ("mods/secret.jar", b"secret"),
            ],
        )
        .await;
        catalogue.pretend_modrinth_holds(&["jei.jar", "off.jar"]);

        let plan = plan_from(&inst, PackFormat::Mrpack, &Selection::default(), &catalogue);
        for file in &plan.content {
            match file.file_name.as_str() {
                "jei.jar" => assert_eq!(file.provider.as_deref(), Some("modrinth")),
                // Named by Modrinth, carried as bytes all the same.
                "off.jar" => assert_eq!(
                    (file.travels, file.provider.as_deref()),
                    (Travels::Copied, None)
                ),
                "secret.jar" => assert_eq!(file.provider, None),
                other => panic!("unexpected {other}"),
            }
        }
    }

    /// The boolean every writer used to take, kept true for the callers that
    /// still hand one over: a pack's worlds are a choice and its logs never
    /// were.
    #[test]
    fn the_old_boolean_still_means_what_it_meant() {
        let without = Selection::with_saves(false);
        assert!(without.travels(Group::Mods));
        assert!(without.travels(Group::Config));
        assert!(without.travels(Group::Other));
        assert!(!without.travels(Group::Saves));
        assert!(!without.travels(Group::Logs));

        assert!(Selection::with_saves(true).travels(Group::Saves));
        assert!(!Selection::with_saves(true).travels(Group::Logs));
        assert_eq!(Selection::default(), Selection::with_saves(false));
    }
}

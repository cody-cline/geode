//! Moving an installed instance to a different Minecraft version or loader.
//!
//! The same question [`crate::plan::build`] answers for a new instance, asked
//! of one that already exists: given a loader and a game version, which file
//! does each mod want? The answer comes from the same place —
//! [`crate::plan::build::file_for`], so the Connector and Quilt rules live in
//! one copy — and lands through the same installer, which replaces a project's
//! previous file rather than leaving two builds of one mod in the folder.
//!
//! What is different here is everything the instance already holds, and all
//! three differences are safety rules rather than features:
//!
//! * **A world is a one-way change.** Minecraft upgrades a world's data format
//!   the first time a newer version opens it and will not take it back down,
//!   so changing the Minecraft version under an instance with worlds in it
//!   cannot be undone the way changing its mods can. So the change lands on a
//!   copy by default when there is anything to lose, and the result says which
//!   happened and why — [`why_copy`].
//! * **A mod with nowhere to go is turned off, never deleted.** Nobody asked
//!   for their mod to be thrown away because a version bump could not find a
//!   build for it, and the jar on disk is the only copy of it there may be.
//! * **A file nothing can be looked up for is named, not assumed.** A jar
//!   somebody dropped into `mods/` by hand has no project to ask about, and a
//!   report that said nothing about it would be claiming a clean move over a
//!   folder it never examined.
//!
//! Nothing is taken away until the downloads have landed, which is the rule
//! `server/transfer.rs` states for a server moving between machines. Here it
//! falls out of two things: every step that can fail for reasons outside this
//! machine runs before anything on disk changes, and the installer fetches a
//! mod's new file before it removes the old one. A failure after a copy has
//! been made takes the copy with it, so there is no plausible-looking instance
//! left in the library whose mods are built for a version it is not.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::connector::SupportMatrix;
use crate::error::{Context, Error, Result};
use crate::meta::{Loader, ManifestCache};
use crate::net::Progress;
use crate::paths::Paths;
use crate::plan::resolve::Fit;
use crate::providers::{Project, Version};

use super::content::{self, ContentKind, ContentRecord, Origin};
use super::Instance;

/// One mod now pointing at a file built for the new target.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Repointed {
    pub name: String,
    /// The file on disk, which is also what its record says.
    pub file_name: String,
    pub version_number: String,
    /// A Fabric build, running on (Neo)Forge through Sinytra Connector.
    pub through_connector: bool,
    /// Dependencies of this mod that did not land, with the reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// One mod the new target has no file for.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stranded {
    pub name: String,
    pub file_name: String,
    /// Why nothing could be pointed at, in words a person can read.
    pub reason: String,
    /// The jar is off: renamed to `.disabled`, or already was, or gone. The
    /// game will not load a build for the version this instance no longer is.
    ///
    /// False only when the rename itself failed, which is the one case here a
    /// person has to do something about.
    pub off: bool,
}

/// One file in `mods/` that nothing could be looked up for.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unchecked {
    pub file_name: String,
    pub reason: String,
}

/// What a re-target did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetargetReport {
    /// The instance the new target landed on: the one asked about, or the copy.
    pub instance_id: String,
    pub instance_name: String,
    pub mc_version: String,
    pub loader: String,
    pub loader_version: String,
    /// The change landed on a copy, and the instance asked about is exactly as
    /// it was.
    pub applied_to_copy: bool,
    /// Why a copy was the right idea, when it was Geode's own choice rather
    /// than something the caller asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copied_because: Option<String>,
    pub repointed: Vec<Repointed>,
    /// Mods with no file for the new target. Turned off, never deleted.
    pub stranded: Vec<Stranded>,
    /// Files left exactly as they are, because there was nothing to ask about
    /// them. Never a claim that they are fine.
    pub unchecked: Vec<Unchecked>,
    /// Sinytra Connector and its companions, when the new target needs them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connector: Vec<String>,
}

/// Move an instance to a different Minecraft version or loader, and re-point
/// its mods at files built for it.
///
/// `to_copy` is the caller's answer to "which instance does this land on":
/// `Some(true)` for a copy, `Some(false)` for this one, and `None` to leave it
/// to [`why_copy`]. Whichever way it is decided, the report says what happened.
#[allow(clippy::too_many_arguments)] // one target's coordinates, travelling together
pub async fn retarget(
    client: &reqwest::Client,
    paths: &Paths,
    manifests: &ManifestCache,
    matrix: &SupportMatrix,
    id: &str,
    target_loader: &str,
    game_version: &str,
    to_copy: Option<bool>,
    java_override: Option<&Path>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<RetargetReport> {
    let loader = Loader::parse(target_loader)
        .filter(|l| l.supports_mods())
        .ok_or_else(|| Error::msg(format!("'{target_loader}' is not a mod loader")))?;
    let source = super::load(paths, id).await?;

    let why = why_copy(paths, &source).await;
    let to_copy = to_copy.unwrap_or(why.is_some());

    // A joined instance plays what its server runs, and its next Play brings
    // it back to the server's pack — so re-targeting it in place is a change
    // that would be undone, having moved every world in it on the way. Onto a
    // copy it is exactly the right thing: `duplicate` makes a copy that is not
    // a join, so the copy is nobody's but this machine's to re-target.
    if !to_copy && source.joined.is_some() {
        return Err(Error::msg(format!(
            "{} plays on a shared server, and the Minecraft version and loader it runs are the \
             server's to choose. Apply this to a copy instead, which is nobody's but yours.",
            source.name
        )));
    }

    // Resolved before anything is copied: a loader with no build for this
    // Minecraft version is a dead target, and finding that out should not cost
    // a copy of somebody's worlds first.
    let loader_version =
        crate::plan::build::newest_loader_build(client, loader, game_version, progress).await?;

    let mut inst = if to_copy {
        progress.stage("Copying the instance", 1).await;
        let name = format!("{} ({} {})", source.name, loader.display_name(), game_version);
        let copy = super::duplicate(paths, id, name)
            .await
            .ctx("copying the instance before changing it")?;
        progress.advance(1, &copy.name).await;
        copy
    } else {
        source
    };

    let applied = apply(
        client,
        paths,
        manifests,
        matrix,
        &mut inst,
        loader,
        &loader_version,
        game_version,
        java_override,
        curseforge_key,
        progress,
    )
    .await;

    match applied {
        Ok(mut report) => {
            report.applied_to_copy = to_copy;
            report.copied_because = to_copy.then_some(why).flatten().map(str::to_string);
            Ok(report)
        }
        // The copy goes with the failure. A copy left half-re-targeted looks
        // like a working instance in the library and is not one, and the rule
        // every other one-way move here holds to is that the destination is
        // absent rather than plausible.
        Err(e) => {
            if to_copy {
                let _ = super::delete(paths, &inst.id).await;
            }
            Err(e)
        }
    }
}

/// Do the move, on whichever instance it has been decided to land on.
///
/// Every step that can fail for a reason outside this machine comes before the
/// first one that changes anything, so a failure returns with the instance as
/// it was. After that nothing fails the whole operation: a mod that cannot be
/// moved is reported and turned off, which is a finished answer rather than a
/// half-done one.
#[allow(clippy::too_many_arguments)] // one target's coordinates, travelling together
async fn apply(
    client: &reqwest::Client,
    paths: &Paths,
    manifests: &ManifestCache,
    matrix: &SupportMatrix,
    inst: &mut Instance,
    loader: Loader,
    loader_version: &str,
    game_version: &str,
    java_override: Option<&Path>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<RetargetReport> {
    let dir = content::content_dir(paths, &inst.id, ContentKind::Mod);
    let (tracked, unchecked) = sort_the_mods_folder(&dir, &inst.content).await?;

    // Every mod's answer before any of them is installed. Two things need the
    // whole picture up front: the Connector runtime goes in before the first
    // Fabric jar does, and a mod with nowhere to go has to be turned off
    // rather than left loading a build for the version this instance is about
    // to stop being.
    progress
        .stage("Finding files for the new target", tracked.len() as u64)
        .await;
    let mut placements = Vec::with_capacity(tracked.len());
    for record in tracked {
        progress.check_cancelled()?;
        progress.advance(1, &record.name).await;
        let placement = placement(
            client,
            matrix,
            loader.as_str(),
            game_version,
            &record,
            curseforge_key,
        )
        .await;
        placements.push((record, placement));
    }

    let mut report = RetargetReport {
        instance_id: inst.id.clone(),
        instance_name: inst.name.clone(),
        mc_version: game_version.to_string(),
        loader: loader.as_str().to_string(),
        loader_version: loader_version.to_string(),
        applied_to_copy: false,
        copied_because: None,
        repointed: Vec::new(),
        stranded: Vec::new(),
        unchecked,
        connector: Vec::new(),
    };

    // The last thing that can fail the whole move, and the reason it runs here
    // rather than at the end: a Forge installer that will not run or a Java
    // that will not install should leave every mod where it was.
    crate::install::install_version(
        client,
        paths,
        manifests,
        game_version,
        loader,
        loader_version,
        java_override,
        progress,
    )
    .await
    .ctx("installing Minecraft for the new target")?;

    // The instance is the new target from here on, in memory. `ensure_stack`
    // and every dependency walk below read these three fields off it, so they
    // have to move before the installs do; the one save at the end is what
    // makes it permanent.
    inst.mc_version = game_version.to_string();
    inst.loader = loader;
    inst.loader_version = loader_version.to_string();

    let needs_connector = placements
        .iter()
        .any(|(_, p)| matches!(p, Placement::Found(f) if f.through_connector));
    if needs_connector {
        // Before any Fabric jar lands, for `plan::build`'s reason: a Fabric
        // mod in a NeoForge mods folder does nothing on its own, and a failure
        // here should stop before the folder fills up with jars that cannot
        // load.
        report.connector = crate::connector::ensure_stack(client, paths, inst, progress)
            .await
            .ctx("installing Sinytra Connector for the new target")?;
    }

    // Mods whose record is about to be wrong, so the jar they name can be
    // turned off once the installs that might replace it are done.
    let mut stranded: Vec<(ContentRecord, String)> = Vec::new();

    progress
        .stage("Re-pointing mods", placements.len() as u64)
        .await;
    // A cancellation stops the loop rather than the function, so the records
    // for everything already moved are still written below. Dropping them
    // would leave those jars untracked — the worst of both answers, since the
    // files have moved and nothing would know it.
    let mut cancelled = false;
    for (record, placement) in placements {
        if progress.check_cancelled().is_err() {
            cancelled = true;
            break;
        }
        progress.advance(1, &record.name).await;

        let found = match placement {
            Placement::Found(found) => *found,
            Placement::Nowhere => {
                stranded.push((
                    record,
                    format!(
                        "nothing published runs on {} {game_version}",
                        loader.display_name()
                    ),
                ));
                continue;
            }
            // Nobody answered, so nothing is known and nothing is changed —
            // not the file, not the record, not whether it is on. Saying so is
            // the whole of what can honestly be done about it.
            Placement::Unanswered(reason) => {
                report.unchecked.push(Unchecked {
                    file_name: record.file_name.clone(),
                    reason,
                });
                continue;
            }
        };

        match crate::install::content::install_with_dependencies(
            client,
            paths,
            inst,
            &found.version,
            &found.project,
            // Dependencies follow the mod, not the instance: a Fabric mod
            // running on NeoForge through Connector needs its Fabric ones.
            game_version,
            found.loader,
            curseforge_key,
            progress,
        )
        .await
        {
            Ok(installed) => {
                let landed = installed.installed.first().cloned().unwrap_or_default();
                // Read back off the record that was written rather than off
                // the version that was asked for. A file CurseForge will not
                // serve is installed from its author's own Modrinth release,
                // and the record says so — a report built from the request
                // would name a file that is not the one on disk.
                let written = inst
                    .content
                    .iter()
                    .find(|r| r.file_name == landed && r.kind == ContentKind::Mod);
                report.repointed.push(Repointed {
                    name: written.map(|r| r.name.clone()).unwrap_or(found.project.name),
                    file_name: landed,
                    version_number: written.map(|r| r.version_number.clone()).unwrap_or_default(),
                    through_connector: found.through_connector,
                    skipped: installed.skipped,
                });
            }
            // The download landed nothing, so the build this instance had is
            // still the file on disk and still what its record says. Turned
            // off for the same reason as a mod with nowhere to go: a jar built
            // for the version this instance no longer is will stop the game
            // starting rather than be politely ignored.
            Err(e) => stranded.push((record, e.to_string())),
        }
    }

    progress
        .stage("Turning off what cannot come along", stranded.len() as u64)
        .await;
    for (record, reason) in stranded {
        progress.advance(1, &record.name).await;
        report.stranded.push(turn_off(&dir, &record, reason).await);
    }
    // Appended to above rather than sorted in place, so the report reads in one
    // order however an entry got into it.
    report.unchecked.sort_by_key(|u| u.file_name.to_lowercase());

    // Last, because nothing before it is worth keeping on its own, and before
    // a cancellation is answered, because what has already moved on disk has
    // to be what the instance says it holds.
    inst.save(paths)
        .await
        .ctx("saving the instance's new target")?;
    if cancelled {
        return Err(Error::Cancelled);
    }
    Ok(report)
}

/// The file one mod wants at the new target: the project it belongs to, the
/// version to install, and the loader that version is built for.
struct Found {
    project: Project,
    version: Version,
    loader: &'static str,
    through_connector: bool,
}

/// One mod's answer at the new target.
enum Placement {
    /// Boxed because a project and a version together are most of a kilobyte,
    /// while the other two answers are a word apiece.
    Found(Box<Found>),
    /// Asked, and nothing that would run here is published.
    Nowhere,
    /// Could not ask at all: a CurseForge mod with no key for it, a project the
    /// platform no longer answers about, a connection that dropped.
    Unanswered(String),
}

/// Which file one mod wants at the new target, or why there is none.
///
/// Native first, always: a mod that ships a build for this loader is never put
/// through a compatibility layer. Only when there is no native build does
/// Connector come into it, and only where Connector itself publishes a file
/// for *this* pair — a project's `loaders` and `game_versions` are independent
/// unions, so their product promises builds that do not exist. The matrix is
/// read from Connector's own files, and it only ever holds the loaders
/// Connector publishes for, so asking it is the whole test.
async fn placement(
    client: &reqwest::Client,
    matrix: &SupportMatrix,
    target_loader: &str,
    game_version: &str,
    record: &ContentRecord,
    curseforge_key: Option<&str>,
) -> Placement {
    let ask = |fit| {
        crate::plan::build::file_for(
            client,
            fit,
            target_loader,
            &record.provider,
            &record.project_id,
            game_version,
            curseforge_key,
        )
    };

    fn found(
        (project, version, loader): (Project, Version, &'static str),
        through_connector: bool,
    ) -> Placement {
        Placement::Found(Box::new(Found {
            project,
            version,
            loader,
            through_connector,
        }))
    }

    match ask(Fit::Native).await {
        Ok(Some(native)) => return found(native, false),
        Ok(None) => {}
        Err(e) => return Placement::Unanswered(e.to_string()),
    }

    if !matrix.supports(game_version, target_loader) {
        return Placement::Nowhere;
    }
    match ask(Fit::Connector).await {
        Ok(Some(through)) => found(through, true),
        Ok(None) => Placement::Nowhere,
        Err(e) => Placement::Unanswered(e.to_string()),
    }
}

/// Split the mods folder into what can be looked up and what cannot.
///
/// Disk is the source of truth, as everywhere else that reads an instance's
/// content: a record whose file is gone is nothing to re-point, and a jar
/// somebody dropped in by hand has no project to ask about. Both kinds of file
/// that cannot be asked about are named rather than passed over, because a
/// report that mentioned only the mods it moved would read as a clean move
/// over a folder it never examined.
async fn sort_the_mods_folder(
    dir: &Path,
    records: &[ContentRecord],
) -> Result<(Vec<ContentRecord>, Vec<Unchecked>)> {
    let mut tracked: Vec<ContentRecord> = Vec::new();
    let mut unchecked: Vec<Unchecked> = Vec::new();
    // A mod can be on disk under both its names at once, which is one mod.
    let mut seen: HashSet<String> = HashSet::new();

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((tracked, unchecked)),
        Err(e) => return Err(Error::from(e)).ctx("reading the instance's mods folder"),
    };

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !content::is_content_file(&file_name, ContentKind::Mod) {
            continue;
        }
        // Records are keyed by the enabled name, so a mod that is off is still
        // the mod its record describes.
        let base = content::strip_disabled(&file_name).0.to_string();
        if !seen.insert(base.clone()) {
            continue;
        }

        let record = records
            .iter()
            .find(|r| r.file_name == base && r.kind == ContentKind::Mod);
        let reason = match record {
            // A server's mod updates when the server's do, never from here —
            // the same rule that keeps an update check away from them. The
            // version it runs at is the server's to choose.
            Some(r) if r.origin == Origin::Server => {
                "came from a server's pack, so the server decides which version it runs"
            }
            Some(r) if r.provider.is_empty() || r.project_id.is_empty() => {
                "nothing records which project this file is, so there is nothing to look up"
            }
            Some(r) => {
                tracked.push(r.clone());
                continue;
            }
            None => "was added by hand, so there is no project to look a new build up from",
        };
        unchecked.push(Unchecked {
            file_name: base,
            reason: reason.to_string(),
        });
    }

    tracked.sort_by_key(|r| r.name.to_lowercase());
    unchecked.sort_by_key(|u| u.file_name.to_lowercase());
    Ok((tracked, unchecked))
}

/// Turn one mod off, for a mod the new target has no file for.
///
/// Renamed to `.disabled` — the convention every other launcher uses, so the
/// jar is still there and still recognisable, and turning it back on is one
/// click once there is a build to turn it on for. Nothing is deleted: a
/// version bump that could not find a build is no reason to throw away
/// somebody's mod, and the jar on disk may be the only copy of it left.
async fn turn_off(dir: &Path, record: &ContentRecord, reason: String) -> Stranded {
    let off = match content::either_name_in(dir, &record.file_name).await {
        Some((on_disk, true)) => content::set_enabled_in(dir, &on_disk, false).await.is_ok(),
        // Already off, or the record has outlived its file. Either way there
        // is nothing here for the game to load.
        _ => true,
    };
    Stranded {
        name: record.name.clone(),
        file_name: record.file_name.clone(),
        reason,
        off,
    }
}

/// Why a copy is the default for this instance, if it is.
///
/// Both answers are one-way changes nobody asked for. A world's data format is
/// upgraded the first time a newer Minecraft opens it and is never taken back
/// down, so an instance with worlds in it cannot simply be moved back. And a
/// joined instance's version is its server's to choose, so moving it would be
/// undone by the next Play — having converted every world on the way.
///
/// `None` is an instance with nothing to lose: its mods can be moved and moved
/// back, which is what makes doing it in place the right default there.
async fn why_copy(paths: &Paths, inst: &Instance) -> Option<&'static str> {
    if inst.joined.is_some() {
        return Some(
            "it plays on a shared server, whose Minecraft version and loader are the server's \
             to choose",
        );
    }
    if has_worlds(paths, &inst.id).await {
        return Some(
            "it has worlds in it, and Minecraft upgrades a world the first time a newer version \
             opens it and never puts it back",
        );
    }
    None
}

/// Whether there is anything in this instance's `saves`.
///
/// Anything at all, rather than only what looks like a world: the cost of
/// being wrong one way is a copy nobody needed, and the other way it is
/// somebody's world converted by a version they did not choose to move to.
async fn has_worlds(paths: &Paths, id: &str) -> bool {
    let saves = paths.game_dir(id).join("saves");
    let Ok(mut entries) = tokio::fs::read_dir(&saves).await else {
        return false;
    };
    matches!(entries.next_entry().await, Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    fn record(file: &str, name: &str) -> ContentRecord {
        ContentRecord {
            file_name: file.into(),
            kind: ContentKind::Mod,
            provider: "modrinth".into(),
            project_id: "P".into(),
            version_id: "v1".into(),
            name: name.into(),
            version_number: "1.0".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: None,
            side: None,
            chosen_side: None,
            keep_on_server: false,
            server_off: false,
        }
    }

    async fn write(dir: &Path, name: &str) {
        tokio::fs::create_dir_all(dir).await.unwrap();
        tokio::fs::write(dir.join(name), b"PK").await.unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }

    /// Everything in the folder is accounted for, and the three kinds of file
    /// nothing can be asked about are named rather than quietly left out of
    /// the answer. None of them is touched by the sorting itself.
    #[tokio::test]
    async fn a_file_nothing_can_be_looked_up_for_is_named_and_left_alone() {
        let scratch = Scratch::new("retarget-sort");
        let dir = scratch.paths.root.join("mods");
        for file in [
            "sodium.jar",
            "jei.jar.disabled",
            "by-hand.jar",
            "anticheat.jar",
            "override.jar",
            "notes.txt",
        ] {
            write(&dir, file).await;
        }

        let records = vec![
            record("sodium.jar", "Sodium"),
            // A mod that is off is still the mod its record describes.
            record("jei.jar", "JEI"),
            ContentRecord {
                origin: Origin::Server,
                ..record("anticheat.jar", "Server Anticheat")
            },
            ContentRecord {
                provider: String::new(),
                project_id: String::new(),
                ..record("override.jar", "override.jar")
            },
            // A record whose file is gone is nothing to re-point.
            record("deleted.jar", "Deleted"),
        ];

        let (tracked, unchecked) = sort_the_mods_folder(&dir, &records).await.unwrap();

        assert_eq!(
            tracked.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["JEI", "Sodium"],
        );
        assert_eq!(
            unchecked
                .iter()
                .map(|u| u.file_name.as_str())
                .collect::<Vec<_>>(),
            vec!["anticheat.jar", "by-hand.jar", "override.jar"],
        );
        // Each one says why, so nothing reads as "checked and fine".
        assert!(unchecked[0].reason.contains("server"), "{unchecked:?}");
        assert!(unchecked[1].reason.contains("by hand"), "{unchecked:?}");
        assert!(unchecked[2].reason.contains("nothing records"), "{unchecked:?}");
        // And the folder is exactly as it was: this only reads.
        assert_eq!(
            names(&dir),
            vec![
                "anticheat.jar",
                "by-hand.jar",
                "jei.jar.disabled",
                "notes.txt",
                "override.jar",
                "sodium.jar",
            ],
        );
    }

    /// The rule a version bump must never break: a mod with nowhere to go is
    /// turned off, and its jar is still there afterwards. Deleting it would
    /// throw away what may be the only copy of a mod somebody chose.
    #[tokio::test]
    async fn a_mod_with_nowhere_to_go_is_turned_off_and_never_deleted() {
        let scratch = Scratch::new("retarget-off");
        let dir = scratch.paths.root.join("mods");
        write(&dir, "oldmod.jar").await;
        write(&dir, "already-off.jar.disabled").await;

        let reason = "nothing published runs on NeoForge 1.21.1".to_string();
        let turned = turn_off(&dir, &record("oldmod.jar", "Old Mod"), reason.clone()).await;
        assert!(turned.off);
        assert_eq!(turned.name, "Old Mod");
        assert_eq!(turned.reason, reason);

        // Already off, so there is nothing to rename and nothing to claim.
        let was_off = turn_off(&dir, &record("already-off.jar", "Already Off"), reason.clone()).await;
        assert!(was_off.off);

        // A record that has outlived its file: reported, and no file invented
        // for it.
        let gone = turn_off(&dir, &record("gone.jar", "Gone"), reason).await;
        assert!(gone.off);

        assert_eq!(
            names(&dir),
            vec!["already-off.jar.disabled", "oldmod.jar.disabled"],
            "a jar was deleted rather than turned off",
        );
        assert_eq!(
            tokio::fs::read(dir.join("oldmod.jar.disabled")).await.unwrap(),
            b"PK",
            "the jar that was turned off is not the one that was there",
        );
    }

    /// A copy is the default when there is something a one-way change would
    /// cost, and only then.
    #[tokio::test]
    async fn a_copy_is_the_default_when_there_are_worlds_or_a_server() {
        let scratch = Scratch::new("retarget-why");
        let paths = scratch.paths.clone();
        let inst = super::super::create(
            &paths,
            "Survival".into(),
            "1.20.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        )
        .await
        .unwrap();

        // `create` makes an empty `saves`, which is an instance with nothing
        // to lose.
        assert!(why_copy(&paths, &inst).await.is_none());

        tokio::fs::create_dir_all(paths.game_dir(&inst.id).join("saves").join("home"))
            .await
            .unwrap();
        assert!(has_worlds(&paths, &inst.id).await);
        let because = why_copy(&paths, &inst).await.expect("worlds are a reason");
        assert!(because.contains("worlds"), "{because}");

        // A joined instance is the server's to version, and that answer comes
        // first: it is true whether or not there is a world yet.
        let joined = Instance {
            joined: Some(
                crate::share::link::Link::parse(&format!(
                    "geode://join?addr=203.0.113.7:25590&code=GEODE-ABCDE-FGHJK&pin={}",
                    "ab".repeat(32)
                ))
                .unwrap(),
            ),
            ..inst
        };
        let because = why_copy(&paths, &joined)
            .await
            .expect("a joined instance is a reason");
        assert!(because.contains("shared server"), "{because}");
    }

    /// A joined instance is not moved out from under its server in place, and
    /// the refusal says what to do instead.
    #[tokio::test]
    async fn a_joined_instance_is_not_re_targeted_in_place() {
        let scratch = Scratch::new("retarget-joined");
        let paths = scratch.paths.clone();
        let mut inst = super::super::create(
            &paths,
            "On A Server".into(),
            "1.20.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        )
        .await
        .unwrap();
        inst.joined = Some(
            crate::share::link::Link::parse(&format!(
                "geode://join?addr=203.0.113.7:25590&code=GEODE-ABCDE-FGHJK&pin={}",
                "ab".repeat(32)
            ))
            .unwrap(),
        );
        inst.save(&paths).await.unwrap();

        let err = retarget(
            &offline(),
            &paths,
            &ManifestCache::new(),
            &SupportMatrix::default(),
            &inst.id,
            "neoforge",
            "1.21.1",
            Some(false),
            None,
            None,
            &Progress::silent("t"),
        )
        .await
        .expect_err("a joined instance's version is its server's to choose");
        assert!(err.to_string().contains("shared server"), "{err}");
        // Refused before the network was reached at all, so nothing about the
        // instance moved.
        let saved = super::super::load(&paths, &inst.id).await.unwrap();
        assert_eq!(saved.mc_version, "1.20.1");
    }

    /// The distinction the safety of this whole thing rests on: a request that
    /// failed is not an answer. A mod nobody could be asked about is reported
    /// and left exactly as it is, because turning it off on the strength of a
    /// dropped connection would be deciding something nothing here knows.
    #[tokio::test]
    async fn a_request_that_failed_is_not_an_answer() {
        let ask = |record| async move {
            placement(
                &offline(),
                &SupportMatrix::default(),
                "fabric",
                "1.21.1",
                &record,
                None,
            )
            .await
        };

        assert!(
            matches!(ask(record("sodium.jar", "Sodium")).await, Placement::Unanswered(_)),
            "a mod the provider could not be asked about was given a verdict",
        );

        // Nor does a record with nothing to look up become one.
        let nameless = ContentRecord {
            provider: String::new(),
            ..record("override.jar", "override.jar")
        };
        assert!(
            matches!(ask(nameless).await, Placement::Unanswered(_)),
            "a record with no provider was given a verdict",
        );
    }

    /// Nothing points anywhere: a client whose every request fails.
    fn offline() -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap()
    }

    /// A run that cannot finish leaves the instance exactly as it was and no
    /// copy behind — whether it got as far as making one or not, the library
    /// holds one instance afterwards and it is the one that was there.
    #[tokio::test]
    async fn a_failure_leaves_the_instance_as_it_was_and_no_copy_behind() {
        let scratch = Scratch::new("retarget-fail");
        let paths = scratch.paths.clone();
        let inst = super::super::create(
            &paths,
            "Survival".into(),
            "1.20.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        )
        .await
        .unwrap();
        let dir = content::content_dir(&paths, &inst.id, ContentKind::Mod);
        write(&dir, "sodium.jar").await;
        // A world, so a copy is what this would land on by default.
        tokio::fs::create_dir_all(paths.game_dir(&inst.id).join("saves").join("home"))
            .await
            .unwrap();

        for to_copy in [None, Some(true)] {
            retarget(
                &offline(),
                &paths,
                &ManifestCache::new(),
                &SupportMatrix::default(),
                &inst.id,
                "neoforge",
                "1.21.1",
                to_copy,
                None,
                None,
                &Progress::silent("t"),
            )
            .await
            .expect_err("nothing here can reach a loader's version list");

            let instances = super::super::list(&paths).await.unwrap();
            assert_eq!(
                instances.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
                vec![inst.id.clone()],
                "a copy was left behind by a run that did not finish",
            );
            let saved = &instances[0];
            assert_eq!(saved.mc_version, "1.20.1");
            assert_eq!(saved.loader, Loader::Fabric);
            assert_eq!(saved.loader_version, "0.16.9");
            assert_eq!(
                names(&dir),
                vec!["sodium.jar"],
                "a mod was moved or turned off by a run that failed",
            );
        }
    }
}

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
//! left in the library whose mods are built for a version it is not. In place
//! there is no copy to take away, so from the first file that moves nothing
//! fails the move at all — it is absorbed into the report — bar two things.
//! One is writing the instance's own file at the end, which is
//! [`not_written_down`]. The other is somebody cancelling: that is not an
//! answer about any mod, so it is never absorbed into the report as one.
//! The move stops where it stands, what already moved is written down so the
//! instance says what its folder holds, and the account of it goes back in the
//! error instead of a report — [`stopped_part_way`].

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::connector::SupportMatrix;
use crate::error::{Context, Error, Result};
use crate::meta::{Loader, ManifestCache};
use crate::net::Progress;
use crate::paths::Paths;
use crate::plan::resolve::{self, Fit, SupportCache};
use crate::providers::{Project, Version};

use super::content::{self, ContentKind, ContentRecord, Origin};
use super::Instance;

/// One mod now pointing at a file built for the new target.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepointedMod {
    pub name: String,
    /// The file on disk, which is also what its record says.
    pub file_name: String,
    pub version_number: String,
    /// A Fabric build, running on (Neo)Forge through Sinytra Connector.
    pub through_connector: bool,
    /// The Minecraft version this build is for, when it is not the instance's
    /// new one — the neighbouring patch the preview named. Said rather than
    /// rounded off to the target, because "3 mods now built for 1.21.1" over a
    /// jar built for 1.21.3 is the same lie in a quieter voice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_version: Option<String>,
    /// Dependencies of this mod that did not land, with the reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// One mod the new target has no file for.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrandedMod {
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
pub struct UncheckedFile {
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
    pub repointed: Vec<RepointedMod>,
    /// Mods with no file for the new target. Turned off, never deleted.
    pub stranded: Vec<StrandedMod>,
    /// Files left exactly as they are, because there was nothing to ask about
    /// them. Never a claim that they are fine.
    pub unchecked: Vec<UncheckedFile>,
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
    support: &SupportCache,
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
        support,
        &mut inst,
        to_copy,
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
///
/// `to_copy` is not used to decide anything about a mod — `inst` is already
/// whichever instance this lands on. It is what the two failures that cannot be
/// absorbed say, because on a copy the caller takes the destination away and
/// there is nothing left for a person to go and fix.
#[allow(clippy::too_many_arguments)] // one target's coordinates, travelling together
async fn apply(
    client: &reqwest::Client,
    paths: &Paths,
    manifests: &ManifestCache,
    matrix: &SupportMatrix,
    support: &SupportCache,
    inst: &mut Instance,
    to_copy: bool,
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
            support,
            loader,
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

    // What the instance said before this line, for the one failure below that
    // cannot be undone or absorbed — see [`not_written_down`].
    let was = format!("{} {}", inst.loader.display_name(), inst.mc_version);

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
        match crate::connector::ensure_stack(client, paths, inst, progress).await {
            Ok(added) => report.connector = added,
            // Absorbed rather than returned, which is this function's own rule
            // and was not being kept here. `ensure_stack` installs jar by jar,
            // so a failure part-way has already put files in the folder — and
            // in place there is no copy to delete, so returning left an
            // instance whose `instance.json` named the old target over a
            // folder that no longer matched it. The move finishes instead, and
            // what it tells the mods that needed the layer depends on which
            // failure this was — see [`without_connector`], which is where
            // that stopped being one answer for all three.
            Err(e) => without_connector(&mut placements, &e, loader, game_version),
        }
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
            Placement::Nowhere(reason) => {
                stranded.push((record, reason));
                continue;
            }
            // Nobody answered, so nothing is known and nothing is changed —
            // not the file, not the record, not whether it is on. Saying so is
            // the whole of what can honestly be done about it.
            Placement::Unanswered(reason) => {
                report.unchecked.push(UncheckedFile {
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
            // running on NeoForge through Connector needs its Fabric ones, and
            // a mod carried over on a neighbouring patch's build needs the
            // dependencies that build was published against.
            &found.game_version,
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
                report.repointed.push(RepointedMod {
                    name: written.map(|r| r.name.clone()).unwrap_or(found.project.name),
                    file_name: landed,
                    version_number: written.map(|r| r.version_number.clone()).unwrap_or_default(),
                    through_connector: found.through_connector,
                    via_version: (found.game_version != game_version).then_some(found.game_version),
                    skipped: installed.skipped,
                });
            }
            // A cancel lands here when it arrives mid-download, and it is no
            // more an answer about this mod than one caught at the top of the
            // loop is: same flag, same break, and nothing turned off over it.
            // Without this a cancelled download disabled the jar it was
            // replacing and then called it a mod with no build.
            Err(Error::Cancelled) => {
                cancelled = true;
                break;
            }
            // Anything else and the build this instance had is still the file
            // on disk and still what its record says. Turned off for the same
            // reason as a mod with nowhere to go: a jar built for the version
            // this instance no longer is will stop the game starting rather
            // than be politely ignored. The reason travels with it, because
            // "nothing is published for the new target" and "the download did
            // not land" are the difference between going to find another mod
            // and simply running this again.
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
    if let Err(e) = inst.save(paths).await {
        return Err(not_written_down(
            &inst.name, to_copy, &was, loader, game_version, e,
        ));
    }
    if cancelled {
        return Err(stopped_part_way(
            &report,
            to_copy,
            &was,
            loader,
            game_version,
        ));
    }
    Ok(report)
}

/// The one failure here that cannot be rolled back, absorbed into the report,
/// or retried on the caller's behalf: the jars in `mods/` are the new target's
/// and `instance.json` could not be made to say so.
///
/// `server/transfer.rs` states the rule this module holds to — every failure
/// leaves the destination absent rather than plausible — and that is kept
/// everywhere a step before this one can fail: nothing outside this machine is
/// asked after the first file moves, a mod that cannot come along is turned off
/// and reported, and Connector failing costs the mods that needed it rather
/// than the move. Writing one small file to the instance's own directory is
/// what is left, and a disk that refuses it cannot be undone by writing
/// something else to the same disk — putting the old jars back would need the
/// network that is no longer being asked.
///
/// So it is reported, in full and with the one thing to do about it. Both
/// targets are named because the two halves now disagree and a person has to
/// know which is which: the files are the new one, the file that describes them
/// is the old one, and setting the version and loader in the instance's
/// settings is what puts them back in step.
///
/// All of that is the in-place path. On a copy this failure *can* be rolled
/// back, and is: the caller deletes the copy, so the plain failure goes back
/// and nothing is left for anybody to fix. Advice about an instance's settings
/// would be advice about a copy that stopped existing a moment later — the name
/// here is whichever instance the move landed on, which on that path is the
/// copy.
fn not_written_down(
    name: &str,
    to_copy: bool,
    was: &str,
    loader: Loader,
    game_version: &str,
    e: Error,
) -> Error {
    if to_copy {
        return Error::msg(format!("writing the copy's own instance file: {e}"));
    }
    Error::msg(format!(
        "{name}'s mods are now built for {} {game_version}, but Geode could not write that down: \
         {e}. It still says {was}, so set its Minecraft version and loader to {} {game_version} in \
         its settings — or run this again once there is room to save it.",
        loader.display_name(),
        loader.display_name(),
    ))
}

/// What a move somebody stopped part way comes back as.
///
/// The report is the only account of what it had already done, and a
/// cancellation has nowhere to put one: the caller asked for a target and is
/// getting an error instead. So the account goes in the error, because the one
/// thing a person has to know is that the instance is the new target and some
/// of its jars are not. Running the move again finishes it, which is said.
///
/// Nothing in here was turned off by the cancellation — cancelling is not an
/// answer about a mod, so what this names is only what the move genuinely did:
/// the mods it had re-pointed, and the ones it found no build for on the way.
///
/// On a copy there is nothing to account for, the same way there is nothing to
/// fix: the caller takes the copy away, so a plain cancellation is the whole
/// truth and naming a copy about to stop existing would not be.
fn stopped_part_way(
    report: &RetargetReport,
    to_copy: bool,
    was: &str,
    loader: Loader,
    game_version: &str,
) -> Error {
    if to_copy {
        return Error::Cancelled;
    }
    let to = format!("{} {game_version}", loader.display_name());
    Error::msg(format!(
        "Moving {} to {to} was stopped part way through. {} of its mods had been re-pointed at a \
         build for {to} and {} turned off, each either having no build for {to} or having had \
         its download stop; the rest are still built for {was}. \
         The instance itself now says {to}, so run this again to finish the move.",
        report.instance_name,
        report.repointed.len(),
        report.stranded.len(),
    ))
}

/// Take the Connector option away from every mod the ladder placed on one,
/// with Connector's own failure as the reason.
///
/// A Fabric jar in a (Neo)Forge mods folder does nothing without the layer
/// under it, so a mod whose chosen fit was a Connector one has no file here
/// after all. Everything else keeps the placement it had — a native build is a
/// native build whether Connector installed or not.
///
/// The rungs below the Connector one are **not** climbed again, and that is a
/// gap rather than a principle. A mod that also had a native build on a
/// neighbouring patch is turned off here for a reason that was never about it.
/// This is reached before the first file moves, so re-climbing would be safe;
/// what it would cost is another round of provider calls for those mods. The
/// comment that used to stand here claimed this module asks nothing outside
/// the machine by now, which is plainly untrue — the loop below asks a
/// provider for every mod it re-points. Re-climbing is the better answer and
/// is not built yet; until it is, the reason travels with the mod so a person
/// can see that Connector, not their mod, is what failed.
///
/// Which answer those mods get is the whole of this function, and it is not one
/// answer for three failures. Being told otherwise is how a dropped connection
/// came to turn mods off again:
///
/// * [`Error::Unpublished`] — a project in the stack publishes nothing for this
///   pair. An answer, as much as the support matrix read off Connector's own
///   files is, so the mod has nowhere to go and is turned off.
/// * Anything else — a provider that did not answer, a download that dropped.
///   That is an answer about nothing, and a mod is never turned off because
///   something could not be asked. Reported unchecked, jar untouched.
/// * [`Error::Cancelled`] — not an answer about any mod at all, so no mod's
///   placement is touched. [`apply`]'s own mod loop reads the same flag, breaks
///   before the first mod, and [`stopped_part_way`] says what the move had
///   done: cancelling here is cancelling at the first mod, and there is no
///   honest second reading of it.
fn without_connector(
    placements: &mut [(ContentRecord, Placement)],
    failure: &Error,
    loader: Loader,
    game_version: &str,
) {
    if matches!(failure, Error::Cancelled) {
        return;
    }
    let answered = matches!(failure, Error::Unpublished(_));
    let reason = if answered {
        format!(
            "a Fabric build needs Sinytra Connector to run on {} {game_version}, and {failure}",
            loader.display_name()
        )
    } else {
        format!(
            "Sinytra Connector could not be installed, so whether a Fabric build runs on {} \
             {game_version} was never settled: {failure}",
            loader.display_name()
        )
    };

    for (_, placement) in placements.iter_mut() {
        if matches!(placement, Placement::Found(f) if f.through_connector) {
            *placement = if answered {
                Placement::Nowhere(reason.clone())
            } else {
                Placement::Unanswered(reason.clone())
            };
        }
    }
}

/// The file one mod wants at the new target: the project it belongs to, the
/// version to install, and the loader that version is built for.
struct Found {
    project: Project,
    version: Version,
    loader: &'static str,
    through_connector: bool,
    /// The Minecraft version this file is built for, which is the instance's
    /// new one except where the only build is from a neighbouring patch. Its
    /// dependencies are resolved against this rather than the instance's, for
    /// [`crate::plan::build`]'s reason: a dependency follows the mod.
    game_version: String,
}

/// One mod's answer at the new target.
enum Placement {
    /// Boxed because a project and a version together are most of a kilobyte,
    /// while the other two answers are a sentence apiece.
    Found(Box<Found>),
    /// Asked, and nothing that would run here can be had — with the reason in
    /// words, because "nothing is published for this pair" and "the layer that
    /// would have run it could not be installed" are the same outcome for two
    /// different reasons, and the mod is being turned off over it.
    Nowhere(String),
    /// Could not ask at all: a CurseForge mod with no key for it, a project the
    /// platform no longer answers about, a connection that dropped.
    Unanswered(String),
}

/// Which file one mod wants at the new target, or why there is none.
///
/// Every fit the preview ranked, in the order it ranked them —
/// [`crate::plan::resolve::ladder`] is that order, and this walks the whole of
/// it rather than its first entry. Native first, always: a mod that ships a
/// build for this loader is never put through a compatibility layer, and only
/// where Connector itself publishes a file for *this* pair does that option
/// exist at all — the matrix is read from Connector's own files and holds only
/// the loaders it publishes for, so asking it is the whole test. Then the
/// compromises the cost model rates as usable — a build from a neighbouring
/// patch, and both at once.
///
/// This is the bug this function was rewritten for. It used to ask for two of
/// the five fits, both at the target's own version, so a mod the screen had
/// just shown as fitting "using its 1.21.3 build" was reported as having
/// nowhere to go and turned off. The preview was the honest half: the resolver
/// deliberately rates a near-version jar as usable, cheap on Fabric and dear
/// on the Forge side, and a screen must never claim what the code did not do.
///
/// The ladder is read off the project's whole file list, out of the cache the
/// preview filled, so the two cannot answer differently.
#[allow(clippy::too_many_arguments)] // one mod's coordinates, travelling together
async fn placement(
    client: &reqwest::Client,
    matrix: &SupportMatrix,
    support: &SupportCache,
    loader: Loader,
    game_version: &str,
    record: &ContentRecord,
    curseforge_key: Option<&str>,
) -> Placement {
    let target_loader = loader.as_str();
    let table = match support
        .support(client, &record.provider, &record.project_id, curseforge_key)
        .await
    {
        Ok(table) => table,
        Err(e) => return Placement::Unanswered(e.to_string()),
    };

    // A project whose file list holds nothing usable at all is the one empty
    // answer that is more likely a gap in what a platform said than a fact
    // about the mod — the resolver puts exactly this case in `unresolved`
    // rather than giving it a verdict at a target. A gap must not turn
    // somebody's mod off, so it is reported and the jar is left alone.
    if table.is_empty() {
        return Placement::Unanswered(
            "this platform lists no downloadable file for it at all, so there was nothing to \
             compare with the new target"
                .to_string(),
        );
    }

    climb(
        resolve::ladder(&table, target_loader, game_version, matrix),
        game_version,
        &format!(
            "nothing published runs on {} {game_version}",
            loader.display_name()
        ),
        |fit, at| async move {
            crate::plan::build::file_for(
                client,
                fit,
                target_loader,
                &record.provider,
                &record.project_id,
                &at,
                curseforge_key,
            )
            .await
        },
    )
    .await
}

/// Ask for each rung of one mod's ladder in turn, and take the first file that
/// comes back.
///
/// `ask` is [`crate::plan::build::file_for`] in every real call; it is a
/// parameter so the whole of this decision — which rungs are asked, at which
/// version, and what a failed request does to the answer — is a test that needs
/// no provider. The three answers it can give are each load-bearing somewhere
/// else in this file, and two of them were wrong before this existed.
async fn climb<F, Fut>(
    rungs: Vec<(Fit, Option<String>)>,
    game_version: &str,
    nowhere: &str,
    ask: F,
) -> Placement
where
    F: Fn(Fit, String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<(Project, Version, &'static str)>>>,
{
    // A request that failed is not an answer, and it outlives a later rung
    // answering "nothing published": the mod must never be turned off because
    // something could not be asked. Kept rather than returned on, so a rung
    // further down still gets its chance to find a file.
    let mut unanswered: Option<String> = None;

    for (fit, via) in rungs {
        let at = via.unwrap_or_else(|| game_version.to_string());
        match ask(fit, at.clone()).await {
            Ok(Some((project, version, loader))) => {
                return Placement::Found(Box::new(Found {
                    project,
                    version,
                    loader,
                    through_connector: matches!(fit, Fit::Connector | Fit::ConnectorNear),
                    game_version: at,
                }))
            }
            Ok(None) => {}
            Err(e) => unanswered = Some(e.to_string()),
        }
    }

    match unanswered {
        // No rungs at all lands here too, and rightly: the file list was in
        // hand and held nothing that runs at this target.
        None => Placement::Nowhere(nowhere.to_string()),
        Some(reason) => Placement::Unanswered(reason),
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
) -> Result<(Vec<ContentRecord>, Vec<UncheckedFile>)> {
    let mut tracked: Vec<ContentRecord> = Vec::new();
    let mut unchecked: Vec<UncheckedFile> = Vec::new();
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
        unchecked.push(UncheckedFile {
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
async fn turn_off(dir: &Path, record: &ContentRecord, reason: String) -> StrandedMod {
    let off = match content::either_name_in(dir, &record.file_name).await {
        Some((on_disk, true)) => content::set_enabled_in(dir, &on_disk, false).await.is_ok(),
        // Already off, or the record has outlived its file. Either way there
        // is nothing here for the game to load.
        _ => true,
    };
    StrandedMod {
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
    let Ok(mut entries) = tokio::fs::read_dir(paths.instance_saves(id)).await else {
        return false;
    };
    matches!(entries.next_entry().await, Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    /// The text of one function in this file, for the test below.
    ///
    /// A shape rather than a value, the way `commands.rs`'s own `body_of` is
    /// and for the same reason: reaching the arm this pins needs a provider
    /// that answers, a download that starts, and a cancel part way through it,
    /// and a unit test has none of the three. What must not happen is worth
    /// more than nothing being able to check it. The first definition wins, so
    /// this reads the real function rather than the literal asking for it.
    fn body_of(signature: &str) -> &'static str {
        let here = include_str!("retarget.rs");
        let after = here
            .split(signature)
            .nth(1)
            .unwrap_or_else(|| panic!("{signature} is not in retarget.rs"));
        &after[..after.find("\n}\n").unwrap_or(after.len())]
    }

    /// A cancel that lands mid-download decides nothing about a mod.
    ///
    /// The loop checks for a cancel at the top of each turn, but
    /// `install_with_dependencies` raises `Error::Cancelled` of its own when a
    /// download is stopped part way, and that used to fall through to the arm
    /// that strands: the jar being replaced was turned off, and the account of
    /// the move called it a mod with no build for the new target. Both halves
    /// were untrue, and one of them was destructive.
    ///
    /// Order is the whole of it. The arm below the cancel one catches every
    /// error there is, so a cancel reaches the arm written for it only while
    /// that arm comes first.
    #[test]
    fn a_cancel_mid_download_is_not_a_verdict_on_a_mod() {
        let body = body_of("async fn apply(");

        let cancel = body.find("Err(Error::Cancelled) => {").expect(
            "the install loop no longer tells a cancel apart from a failure, so stopping a \
             download turns off the jar it was replacing and reports it as having no build",
        );
        let catch_all = body
            .find("Err(e) => stranded.push(")
            .expect("the install loop no longer strands a mod whose download failed");
        assert!(
            cancel < catch_all,
            "the catch-all arm comes first, so a cancel is stranded by it and never reaches \
             the arm written for it",
        );
    }
    use super::*;
    use crate::paths::Scratch;
    use crate::providers::{Provider, ProjectKind, VersionFile};
    use std::cell::RefCell;

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

        tokio::fs::create_dir_all(paths.instance_saves(&inst.id).join("home"))
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
            &SupportCache::new(),
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
        let ask = |record: ContentRecord| async move {
            placement(
                &offline(),
                &SupportMatrix::default(),
                &SupportCache::new(),
                Loader::Fabric,
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

        // Nor a project whose file list came back holding nothing usable,
        // which the resolver calls unresolved rather than missing. Read from
        // the cache here, so this is the decision and not a request.
        let empty = crate::plan::resolve::SupportCache::new();
        empty.seed("modrinth", "P", support(&[])).await;
        let placed = placement(
            &offline(),
            &SupportMatrix::default(),
            &empty,
            Loader::Fabric,
            "1.21.1",
            &record("sodium.jar", "Sodium"),
            None,
        )
        .await;
        assert!(
            matches!(placed, Placement::Unanswered(_)),
            "a mod whose platform listed no file at all was turned off over it",
        );

        // Where the list is there and rules the target out, it is a verdict:
        // the mod is stranded, and nothing was asked to find that out — this
        // client could not have asked.
        let elsewhere = crate::plan::resolve::SupportCache::new();
        elsewhere
            .seed("modrinth", "P", support(&[("forge", "1.12.2")]))
            .await;
        let placed = placement(
            &offline(),
            &SupportMatrix::default(),
            &elsewhere,
            Loader::Fabric,
            "1.21.1",
            &record("sodium.jar", "Sodium"),
            None,
        )
        .await;
        let Placement::Nowhere(reason) = placed else {
            panic!("a mod with nothing for this target was not stranded");
        };
        assert!(reason.contains("Fabric 1.21.1"), "{reason}");
    }

    /// One mod's whole published history, as pairs.
    fn support(pairs: &[(&str, &str)]) -> crate::plan::resolve::Support {
        crate::plan::resolve::Support::from_pairs(
            pairs
                .iter()
                .map(|(l, g)| (l.to_string(), g.to_string())),
        )
    }

    /// Connector covering NeoForge on 1.21.1 only, like the real matrix.
    fn connector_matrix() -> SupportMatrix {
        SupportMatrix::from_pairs(vec![crate::connector::SupportPair {
            loader: "neoforge".into(),
            game_version: "1.21.1".into(),
        }])
    }

    /// What a provider answers with when it has a file: a project, the version
    /// built for `game`, and the loader that file is for.
    fn published(game: &str, loader: &'static str) -> (Project, Version, &'static str) {
        let project = Project {
            provider: Provider::Modrinth,
            id: "P".into(),
            slug: "sodium".into(),
            name: "Sodium".into(),
            summary: String::new(),
            icon_url: None,
            downloads: 0,
            follows: 0,
            author: String::new(),
            kind: ProjectKind::Mod,
            categories: vec![],
            loaders: vec![],
            game_versions: vec![],
            updated: None,
            page_url: String::new(),
            banner_url: None,
            distribution_allowed: true,
            client_side: None,
            server_side: None,
            also_on: vec![],
        };
        let version = Version {
            provider: Provider::Modrinth,
            id: "v".into(),
            project_id: "P".into(),
            name: game.into(),
            version_number: "1.0".into(),
            game_versions: vec![game.to_string()],
            loaders: vec![loader.to_string()],
            release_type: "release".into(),
            published: None,
            downloads: 0,
            files: vec![VersionFile {
                url: "https://example.invalid/a.jar".into(),
                filename: "sodium.jar".into(),
                sha1: None,
                sha512: None,
                size: None,
                primary: true,
            }],
            dependencies: vec![],
            blocked_reason: None,
            blocked_sha1: None,
        };
        (project, version, loader)
    }

    /// **The rule a re-target exists to keep.** The preview ranks a mod whose
    /// only build is for a neighbouring patch as fitting, and says so on screen
    /// — "other version", "Using its 1.21.3 build". Applying that target has to
    /// install that build.
    ///
    /// It did not. Two of the five fits were asked for, both at the target's
    /// own version, so this mod came back with nowhere to go, landed in
    /// `stranded`, and had its jar renamed `.disabled`. The screen promised the
    /// target worked and applying it turned the mod off.
    #[tokio::test]
    async fn a_mod_whose_only_build_is_next_door_is_re_pointed_and_never_turned_off() {
        let asked = RefCell::new(Vec::new());
        let placed = climb(
            resolve::ladder(
                &support(&[("fabric", "1.21.3")]),
                "fabric",
                "1.21.1",
                &SupportMatrix::default(),
            ),
            "1.21.1",
            "nothing published runs on Fabric 1.21.1",
            |fit, at| {
                asked.borrow_mut().push((fit, at.clone()));
                std::future::ready(Ok((at == "1.21.3").then(|| published("1.21.3", "fabric"))))
            },
        )
        .await;

        let Placement::Found(found) = placed else {
            panic!("a mod the preview showed as fitting was given nowhere to go, and would be turned off");
        };
        assert_eq!(
            found.game_version, "1.21.3",
            "the build asked for was not the one the preview named",
        );
        assert!(!found.through_connector);
        assert_eq!(
            asked.into_inner(),
            vec![(Fit::NearVersion, "1.21.3".to_string())],
            "the only rung on this mod's ladder was never asked for",
        );
    }

    /// Every fit the preview ranks is asked for, in the order it ranks them —
    /// and a mod that genuinely has no file for this target is still stranded,
    /// which is the half that was right.
    #[tokio::test]
    async fn every_fit_the_preview_ranks_is_asked_for_in_that_order() {
        let asked = RefCell::new(Vec::new());
        // A Fabric build for the target, another a patch along, and a NeoForge
        // build a patch along — on a NeoForge target Connector covers. Nothing
        // answers, so the whole ladder is walked.
        let placed = climb(
            resolve::ladder(
                &support(&[
                    ("fabric", "1.21.1"),
                    ("fabric", "1.21.3"),
                    ("neoforge", "1.21.4"),
                ]),
                "neoforge",
                "1.21.1",
                &connector_matrix(),
            ),
            "1.21.1",
            "nothing published runs on NeoForge 1.21.1",
            |fit, at| {
                asked.borrow_mut().push((fit, at));
                std::future::ready(Ok(None))
            },
        )
        .await;

        assert_eq!(
            asked.into_inner(),
            vec![
                (Fit::Connector, "1.21.1".to_string()),
                (Fit::NearVersion, "1.21.4".to_string()),
                (Fit::ConnectorNear, "1.21.3".to_string()),
            ],
        );
        // Asked everywhere and published nowhere: stranded, with the reason.
        let Placement::Nowhere(reason) = placed else {
            panic!("a mod with no file at this target was not stranded");
        };
        assert!(reason.contains("NeoForge 1.21.1"), "{reason}");

        // And a mod whose file list rules this target out has no rungs, so
        // nothing is asked for it at all — even here, where anything asked
        // would be answered with a file.
        let placed = climb(
            resolve::ladder(
                &support(&[("forge", "1.12.2")]),
                "fabric",
                "1.21.1",
                &SupportMatrix::default(),
            ),
            "1.21.1",
            "nothing published runs on Fabric 1.21.1",
            |_, at| std::future::ready(Ok(Some(published(&at, "fabric")))),
        )
        .await;
        assert!(matches!(placed, Placement::Nowhere(_)));
    }

    /// A Connector fit is installed as the Fabric build it is, and says so.
    #[tokio::test]
    async fn a_connector_fit_is_marked_as_one() {
        let placed = climb(
            resolve::ladder(
                &support(&[("fabric", "1.21.1")]),
                "neoforge",
                "1.21.1",
                &connector_matrix(),
            ),
            "1.21.1",
            "nothing published runs on NeoForge 1.21.1",
            |_, at| std::future::ready(Ok(Some(published(&at, "fabric")))),
        )
        .await;

        let Placement::Found(found) = placed else {
            panic!("a Fabric-only mod was not carried onto NeoForge");
        };
        assert!(found.through_connector);
        assert_eq!(found.loader, "fabric");
        assert_eq!(found.game_version, "1.21.1");
    }

    /// The distinction the safety of the whole thing rests on, now that more
    /// than one rung is asked for: a request that failed is not an answer, and
    /// it outlives a later rung saying "nothing published". Turning a mod off
    /// because a connection dropped is the one mistake here that cannot be
    /// undone from the screen that caused it.
    #[tokio::test]
    async fn a_request_that_failed_never_strands_a_mod() {
        let rungs = || {
            resolve::ladder(
                &support(&[("fabric", "1.21.1"), ("fabric", "1.21.3")]),
                "quilt",
                "1.21.1",
                &SupportMatrix::default(),
            )
        };
        let dropped = || Error::msg("network request failed");

        // The first rung could not be asked; the second answers. The file wins.
        let placed = climb(rungs(), "1.21.1", "nowhere", |_, at| {
            std::future::ready(if at == "1.21.1" {
                Err(dropped())
            } else {
                Ok(Some(published(&at, "fabric")))
            })
        })
        .await;
        assert!(matches!(placed, Placement::Found(_)), "a rung after a failed request was not tried");

        // The first rung could not be asked and the second publishes nothing.
        // That is not "nothing runs here" — nobody knows — so nothing is
        // turned off.
        let placed = climb(rungs(), "1.21.1", "nowhere", |_, at| {
            std::future::ready(if at == "1.21.1" {
                Err(dropped())
            } else {
                Ok(None)
            })
        })
        .await;
        assert!(
            matches!(placed, Placement::Unanswered(_)),
            "a mod was turned off because a request failed",
        );
    }

    /// Three mods with three placements, for the three ways the Connector
    /// runtime can fail to go in.
    fn connector_placements() -> Vec<(ContentRecord, Placement)> {
        let found = |through_connector| {
            let (project, version, loader) = published("1.21.1", "fabric");
            Placement::Found(Box::new(Found {
                project,
                version,
                loader,
                through_connector,
                game_version: "1.21.1".into(),
            }))
        };
        vec![
            (record("sodium.jar", "Sodium"), found(false)),
            (record("fabric-only.jar", "Fabric Only"), found(true)),
            (
                record("by-hand.jar", "By Hand"),
                Placement::Unanswered("nobody answered".into()),
            ),
        ]
    }

    /// Connector failing costs the mods that needed it, not the move.
    ///
    /// In place there is no copy to delete, and `ensure_stack` installs jar by
    /// jar — so returning here left an instance whose `instance.json` named the
    /// old target over a folder that had already changed.
    #[test]
    fn connector_failing_strands_only_the_mods_that_needed_it() {
        let mut placements = connector_placements();
        without_connector(
            &mut placements,
            &Error::Unpublished("Connector Extras has no build for Minecraft 1.21.1".into()),
            Loader::NeoForge,
            "1.21.1",
        );

        assert!(matches!(placements[0].1, Placement::Found(ref f) if !f.through_connector));
        match &placements[1].1 {
            Placement::Nowhere(reason) => assert!(reason.contains("Connector"), "{reason}"),
            _ => panic!("a mod that needed Connector was left pointing at a Fabric build"),
        }
        assert!(matches!(placements[2].1, Placement::Unanswered(_)));
    }

    /// **The rule the previous fix broke putting it back.** `ensure_stack` is
    /// three Modrinth calls and an install, so a dropped connection comes out
    /// of it — and every failure was being read as "Connector is not available
    /// for this pair", which is what turns a mod off. A request that failed is
    /// not an answer here either.
    ///
    /// The mod is reported unchecked instead, which is what leaves its jar
    /// alone: the loop that would have renamed it `.disabled` only sees
    /// `Nowhere`.
    #[test]
    fn a_connector_request_that_failed_never_strands_a_mod() {
        let mut placements = connector_placements();
        without_connector(
            &mut placements,
            &Error::msg("network request failed: connection closed before message completed"),
            Loader::NeoForge,
            "1.21.1",
        );

        assert!(matches!(placements[0].1, Placement::Found(ref f) if !f.through_connector));
        match &placements[1].1 {
            Placement::Unanswered(reason) => {
                assert!(reason.contains("never settled"), "{reason}");
                assert!(reason.contains("NeoForge 1.21.1"), "{reason}");
            }
            Placement::Nowhere(reason) => {
                panic!("a mod was turned off because a request failed: {reason}")
            }
            _ => panic!("a native build lost its placement"),
        }
        assert!(matches!(placements[2].1, Placement::Unanswered(_)));
    }

    /// Cancelling is not an answer about any mod, so no mod's placement
    /// changes: not to nowhere, which turns it off, and not to unchecked
    /// either, which would report a mod nobody asked about as examined.
    ///
    /// What happens to the move instead is
    /// [`a_cancelled_move_says_what_it_already_did`].
    #[test]
    fn cancelling_while_connector_installs_decides_nothing_about_a_mod() {
        let mut placements = connector_placements();
        let before: Vec<&'static str> = placements
            .iter()
            .map(|(_, p)| match p {
                Placement::Found(_) => "found",
                Placement::Nowhere(_) => "nowhere",
                Placement::Unanswered(_) => "unanswered",
            })
            .collect();

        without_connector(&mut placements, &Error::Cancelled, Loader::NeoForge, "1.21.1");

        let after: Vec<&'static str> = placements
            .iter()
            .map(|(_, p)| match p {
                Placement::Found(_) => "found",
                Placement::Nowhere(_) => "nowhere",
                Placement::Unanswered(_) => "unanswered",
            })
            .collect();
        assert_eq!(
            after, before,
            "a mod was given a verdict because somebody pressed cancel",
        );
        assert!(matches!(placements[1].1, Placement::Found(ref f) if f.through_connector));
    }

    /// A cancellation is not absorbed into the report as though it were an
    /// answer, and it does not throw away what the move had already done
    /// either: the instance is the new target and some of its jars are not, so
    /// the error says so and says how to finish it.
    ///
    /// On a copy there is nothing to say. The caller deletes the copy, so
    /// naming it — or telling somebody to run the move again on it — would be
    /// about an instance that no longer exists.
    #[test]
    fn a_cancelled_move_says_what_it_already_did() {
        let mut report = RetargetReport {
            instance_id: "i".into(),
            instance_name: "Survival".into(),
            mc_version: "1.21.1".into(),
            loader: "neoforge".into(),
            loader_version: "21.1.80".into(),
            applied_to_copy: false,
            copied_because: None,
            repointed: Vec::new(),
            stranded: Vec::new(),
            unchecked: Vec::new(),
            connector: Vec::new(),
        };
        report.repointed.push(RepointedMod {
            name: "Sodium".into(),
            file_name: "sodium.jar".into(),
            version_number: "1.0".into(),
            through_connector: false,
            via_version: None,
            skipped: Vec::new(),
        });
        report.stranded.push(StrandedMod {
            name: "Old Mod".into(),
            file_name: "oldmod.jar".into(),
            reason: "nothing published runs on NeoForge 1.21.1".into(),
            off: true,
        });

        let text = stopped_part_way(&report, false, "Fabric 1.20.1", Loader::NeoForge, "1.21.1")
            .to_string();
        assert!(text.contains("Survival"), "{text}");
        assert!(text.contains("NeoForge 1.21.1"), "{text}");
        assert!(text.contains("Fabric 1.20.1"), "{text}");
        // What it had done, counted off the report rather than guessed at.
        assert!(
            text.contains("1 of its mods had been re-pointed"),
            "the one mod that moved went unaccounted for: {text}",
        );
        assert!(
            text.contains("1 turned off"),
            "the one mod turned off went unaccounted for: {text}",
        );
        assert!(
            text.contains("run this again"),
            "a half-finished move said nothing about finishing it: {text}",
        );

        // On a copy: the copy goes, so there is nothing to account for and
        // nothing to tell anybody to do.
        let on_copy = stopped_part_way(&report, true, "Fabric 1.20.1", Loader::NeoForge, "1.21.1");
        assert!(
            matches!(on_copy, Error::Cancelled),
            "a cancellation named a copy the caller was about to delete: {on_copy}",
        );
    }

    /// The one failure that cannot be rolled back says so, and names both
    /// halves: what the files are now, and what the instance still claims.
    ///
    /// Only in place, because only there is it unrollbackable. On the copy path
    /// — which this is reached on too — the caller deletes the copy, and the
    /// message was being built from the copy's own name and then telling
    /// somebody to open its settings, moments before it stopped existing.
    #[test]
    fn a_move_that_could_not_be_written_down_says_what_to_do_about_it() {
        let err = not_written_down(
            "Survival",
            false,
            "Fabric 1.20.1",
            Loader::NeoForge,
            "1.21.1",
            Error::msg("no space left on device"),
        );
        let text = err.to_string();
        assert!(text.contains("Survival"), "{text}");
        assert!(text.contains("NeoForge 1.21.1"), "{text}");
        assert!(text.contains("Fabric 1.20.1"), "{text}");
        assert!(text.contains("no space left on device"), "{text}");
        assert!(text.contains("settings"), "{text}");

        let on_copy = not_written_down(
            "Survival (NeoForge 1.21.1)",
            true,
            "Fabric 1.20.1",
            Loader::NeoForge,
            "1.21.1",
            Error::msg("no space left on device"),
        )
        .to_string();
        assert!(on_copy.contains("no space left on device"), "{on_copy}");
        assert!(
            !on_copy.contains("settings"),
            "a copy that is about to be deleted was something to go and fix: {on_copy}",
        );
        assert!(
            !on_copy.contains("Survival (NeoForge 1.21.1)"),
            "a copy that is about to be deleted was named: {on_copy}",
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
    ///
    /// This is the failure before anything moves, which is the only one a test
    /// with no network can reach: everything after it needs a loader's version
    /// list and a provider's files. What happens past that point is each
    /// decision on its own —
    /// [`connector_failing_strands_only_the_mods_that_needed_it`] for the
    /// failure that used to return mid-move, and
    /// [`a_move_that_could_not_be_written_down_says_what_to_do_about_it`] for
    /// the one that cannot be absorbed.
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
        tokio::fs::create_dir_all(paths.instance_saves(&inst.id).join("home"))
            .await
            .unwrap();

        for to_copy in [None, Some(true)] {
            retarget(
                &offline(),
                &paths,
                &ManifestCache::new(),
                &SupportMatrix::default(),
                &SupportCache::new(),
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

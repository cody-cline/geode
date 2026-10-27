//! Writing and reading a `.geodepack`.
//!
//! The file is an ordinary `.mrpack` with a `geode/` directory beside the
//! index. Writing it is the mrpack writer plus a few more entries; reading it
//! is the mrpack reader plus a pass that applies what the index had nowhere to
//! record. Neither half duplicates the other — see [`super::manifest`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::install::identity::{self, travel, Ask, Carry, Catalogue, Travel};
use crate::instance::export::Selection;
use crate::instance::modpack::ExportReport;
use crate::instance::multiplayer::Wrote;
use crate::instance::{self, Instance};
use crate::net::Progress;
use crate::paths::Paths;

use super::manifest::{
    format_for, FileNote, Forbidden, Kind, Listing, Manifest, Override, Policy, Profile, Settings,
    SourceRef, SyncedPath, EXTENSION, FORMAT,
};

/// Build the Geode half of a pack for an instance.
///
/// The catalogue comes in rather than being built here because the two halves
/// must agree: a file the index is carrying must not also be named a source
/// here, or the recipient would fetch it twice — once by URL and once by id,
/// the second needing a key for a file Modrinth hosts.
pub async fn manifest_for(
    paths: &Paths,
    instance: &Instance,
    selection: &Selection,
    catalogue: &Catalogue,
) -> Manifest {
    let media = paths.instance_media(&instance.id);
    let files = notes_for(
        &paths.game_dir(&instance.id),
        &instance.content.iter().collect::<Vec<_>>(),
        selection,
        catalogue,
    )
    .await;

    Manifest {
        geode_format: format_for(&files),
        kind: Kind::Instance,
        profile: Profile {
            name: instance.name.clone(),
            group: instance.group.clone(),
            notes: instance.notes.clone(),
            memory_mb: instance.overrides.memory_mb,
            java_args: instance.overrides.java_args.clone(),
            java_path_never_shared: (),
            width: instance.overrides.width,
            height: instance.overrides.height,
            fullscreen: instance.overrides.fullscreen,
            server_kind: None,
        },
        icon: artwork(&media, instance.icon.as_ref()),
        banner: artwork(&media, instance.banner.as_ref()),
        source: instance.pack.clone(),
        files,
        // An instance's pack has no revision: only a server's is checked for
        // changes.
        revision: None,
        // Nor a listing: there is no server behind a personal pack to put in
        // anybody's multiplayer list.
        listing: None,
        // Nor anything synced: a person hands a pack over once, and what the
        // recipient makes of its files afterwards is between them and their
        // own instance.
        settings: None,
        // Nor a rule about what the recipient may run. A pack is handed over;
        // nobody stays behind it to have one.
        forbidden: Vec::new(),
    }
}

/// Where a picture goes in the zip, when the file it names is really there.
fn artwork(media: &Path, name: Option<&String>) -> Option<String> {
    name.filter(|name| media.join(name).is_file())
        .map(|name| media_entry(name))
}

fn media_entry(file_name: &str) -> String {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    format!("geode/media/{}", sanitise(file_name, ext))
}

/// Keep the extension and throw away everything else about the name.
///
/// The name inside the zip need only be stable and safe; the original is a
/// local file name that means nothing to a reader.
fn sanitise(file_name: &str, ext: &str) -> String {
    let stem = if file_name.starts_with("banner") {
        "banner"
    } else {
        "icon"
    };
    format!("{stem}.{ext}")
}

/// What each file declared about itself, and where a recipient can get it.
///
/// Two things earn a file a note. The **side** is what the jar said about where
/// it belongs, carried across so an importer need not re-read every jar. The
/// **source** is how a file the index cannot point at gets fetched anyway — by
/// project and version id, resolved through the recipient's own credentials,
/// rather than by us putting the jar in the zip.
///
/// For an instance's pack, which follows links like the rest of its walk. A
/// server's pack follows none, and writes its notes with [`server_notes`].
///
/// Only files the pack carries get a note. A note is this half's channel for
/// naming a source, so writing one for a group the sharer left out would have
/// the recipient fetch a file the index and `overrides/` both left behind.
async fn notes_for(
    game_dir: &Path,
    records: &[&instance::content::ContentRecord],
    selection: &Selection,
    catalogue: &Catalogue,
) -> Vec<FileNote> {
    let mut out = Vec::new();

    for record in records {
        let rel = PathBuf::from(record.kind.folder()).join(&record.file_name);
        if !selection.carries(&rel) {
            continue;
        }

        // A record is provenance, not proof. Deleting a file outside the
        // launcher leaves its record behind forever — the module says as much,
        // and nothing prunes it — so a reference written from the record alone
        // would promise the recipient a mod the sharer does not actually have.
        let Some((full, disabled)) = on_disk(game_dir, &rel).await else {
            continue;
        };

        let side = tokio::task::spawn_blocking(move || instance::jar::read(&full))
            .await
            .ok()
            .flatten()
            .and_then(|meta| meta.side);
        // `Carry::GEODEPACK` because these notes *are* the Geode half: writing
        // them is what makes the pack one, and `export_with` tells the index
        // writer the same.
        let travel = catalogue.travel(record.kind, &record.file_name, Carry::GEODEPACK);
        let source = source_of(travel.as_ref());

        // A file the recipient fetches rather than unpacks arrives fresh from
        // its platform, switched on, so one that is turned off has to be told.
        // A file carried as bytes keeps its `.disabled` name on the way
        // through and says it that way.
        let fetched = matches!(travel, Some(Travel::Named(_) | Travel::Linked(_)));
        let turned_off = disabled && fetched;

        // Only files with something to say are recorded. A pack whose mods all
        // come from Modrinth and all run on both sides carries no notes at all,
        // which is the difference between a manifest that annotates and one
        // that repeats.
        if side.is_none() && source.is_none() && !turned_off {
            continue;
        }
        out.push(FileNote {
            path: rel.to_string_lossy().replace('\\', "/"),
            side,
            disabled: turned_off,
            source,
        });
    }

    // What the files say, which the records do not always know. A jar dropped
    // into `mods/` by hand has no record at all, so the loop above never
    // reaches it:
    //
    // * Its source was therefore never written, and this half's note is the
    //   only channel that carries one. `referenced()` reads the notes, so the
    //   file was left out of the referenced set and the `overrides/` walk
    //   copied its bytes — while the report the sharer reads counted it as a
    //   reference, because that count is taken from the catalogue. The two
    //   disagreed, and the one that was right was the zip.
    // * Its off state was never written either — `set_enabled` only renames
    //   the file — and the index carries it under its enabled name, so it
    //   would arrive switched on. Worse than not sending it: the sharer
    //   turned it off for a reason.
    //
    // Merged into whatever the records already said rather than replacing it,
    // so a side read from the jar above survives.
    for ident in catalogue.identities() {
        if !selection.carries(&ident.disk_rel) {
            continue;
        }
        let travel = travel(ident, Carry::GEODEPACK);
        let source = source_of(Some(&travel));
        // As above: a file the recipient fetches arrives switched on, so one
        // that is off has to say so. A file carried as bytes keeps its
        // `.disabled` name and needs no note.
        let turned_off =
            ident.disabled && matches!(travel, Travel::Named(_) | Travel::Linked(_));
        if source.is_none() && !turned_off {
            continue;
        }
        let path = format!("{}/{}", ident.kind.folder(), ident.file_name);
        match out.iter_mut().find(|note| note.path == path) {
            Some(note) => {
                if note.source.is_none() {
                    note.source = source;
                }
                note.disabled |= turned_off;
            }
            None => out.push(FileNote {
                path,
                side: None,
                disabled: turned_off,
                source,
            }),
        }
    }
    out
}

/// The file a record refers to, and whether it is turned off.
///
/// Records are keyed by the enabled name, so a mod that has been turned off is
/// `foo.jar` in the record and `foo.jar.disabled` on disk. Both count as having
/// the file.
///
/// Follows links, unlike [`instance::content::is_regular_file`]: an instance's
/// own walk does, and a mod the player symlinked in is still theirs to share.
async fn on_disk(game_dir: &Path, rel: &Path) -> Option<(PathBuf, bool)> {
    let enabled = game_dir.join(rel);
    if tokio::fs::metadata(&enabled).await.is_ok() {
        return Some((enabled, false));
    }

    let disabled = game_dir.join(disabled_rel(rel)?);
    tokio::fs::metadata(&disabled)
        .await
        .is_ok()
        .then_some((disabled, true))
}

/// `mods/foo.jar` → `mods/foo.jar.disabled`.
fn disabled_rel(rel: &Path) -> Option<PathBuf> {
    let mut name = rel.file_name()?.to_os_string();
    name.push(instance::content::DISABLED_SUFFIX);
    Some(rel.with_file_name(name))
}

/// Where a recipient should fetch a file, when the index cannot say.
///
/// Modrinth content is deliberately absent: the index already carries it as a
/// URL and a pair of hashes, and the rule this format rests on is that the same
/// fact never lives in both halves. Which half a file belongs to is decided
/// once, in [`crate::install::identity::travel`] — what it calls `Linked` is
/// the index's, and only what it calls `Named` is named here. A jar nothing can
/// name gets nothing, and so stays an embedded override, which is the one case
/// where there is genuinely no link to give.
fn source_of(travel: Option<&Travel<'_>>) -> Option<SourceRef> {
    match travel {
        Some(Travel::Named(reference)) => Some(SourceRef {
            provider: reference.provider.clone(),
            project_id: reference.project_id.clone(),
            version_id: reference.version_id.clone(),
        }),
        _ => None,
    }
}

/// Everything that goes in `geode/`, ready for the mrpack writer.
pub async fn extras_for(
    paths: &Paths,
    instance: &Instance,
    manifest: &Manifest,
) -> Result<crate::instance::modpack::Extras> {
    extras_from(
        &paths.instance_media(&instance.id),
        instance.icon.as_ref(),
        instance.banner.as_ref(),
        manifest,
    )
    .await
}

/// The same, for artwork kept in any media folder.
async fn extras_from(
    media: &Path,
    icon: Option<&String>,
    banner: Option<&String>,
    manifest: &Manifest,
) -> Result<crate::instance::modpack::Extras> {
    let mut extras = vec![(
        Manifest::PATH.to_string(),
        serde_json::to_vec_pretty(manifest)?,
    )];

    for (entry, source) in [
        (manifest.icon.as_ref(), icon),
        (manifest.banner.as_ref(), banner),
    ] {
        let (Some(entry), Some(source)) = (entry, source) else {
            continue;
        };
        // Artwork is the one thing that is always embedded rather than linked.
        // It is small, it is the author's own, and a pack that arrives without
        // its picture looks broken.
        if let Ok(bytes) = tokio::fs::read(media.join(source)).await {
            extras.push((entry.clone(), bytes));
        }
    }
    Ok(extras)
}

/// Write a pack that another launcher can open and Geode can open exactly.
pub async fn export(
    ask: Ask<'_>,
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    selection: &Selection,
    progress: &Progress,
) -> Result<ExportReport> {
    let catalogue = identity::for_instance(ask, paths, instance, progress).await?;
    export_with(paths, instance, dest, selection, &catalogue, progress).await
}

/// The same, from a catalogue already built.
///
/// Both halves are written from the one catalogue, which is what stops them
/// disagreeing about which files the recipient is expected to fetch. It is
/// also the seam a preview uses: the counts it shows come from the very
/// catalogue the export then writes from, so it cannot promise one thing and
/// do another.
pub async fn export_with(
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    selection: &Selection,
    catalogue: &Catalogue,
    progress: &Progress,
) -> Result<ExportReport> {
    let manifest = manifest_for(paths, instance, selection, catalogue).await;
    let half = instance::modpack::GeodeHalf {
        extras: extras_for(paths, instance, &manifest).await?,
        referenced: referenced(&manifest),
        carry: Carry::GEODEPACK,
    };

    instance::modpack::write_instance_pack(
        paths,
        instance,
        dest,
        selection,
        half,
        catalogue,
        progress,
    )
    .await
}

/// The files a pack names instead of carrying.
///
/// Read off the manifest rather than worked out a second time. A file is left
/// out of `overrides/` if and only if the manifest actually carries a reference
/// for it, so the two halves cannot come to disagree about which files the
/// recipient is expected to fetch — which is the same rule the format applies
/// to download URLs, for the same reason.
fn referenced(manifest: &Manifest) -> HashSet<PathBuf> {
    manifest
        .files
        .iter()
        .filter(|note| note.source.is_some())
        // Split on the separator the manifest uses rather than handing the
        // string to `PathBuf`, so a path written on one platform still matches
        // the walk's own on another.
        .map(|note| note.path.split('/').collect::<PathBuf>())
        .collect()
}

/// Read the Geode half out of a pack, if it has one.
///
/// `Ok(None)` means a plain `.mrpack`, which is a perfectly good thing to be
/// handed and is imported as one.
pub fn read_manifest(archive: &Path) -> Result<Option<Manifest>> {
    // An instance folder somebody pointed at is not a zip and has no Geode
    // half to find — the import path is shared with a pack file, and this is
    // the one step that would refuse a folder for not being an archive.
    if archive.is_dir() {
        return Ok(None);
    }
    let mut zip = instance::modpack::open_pack(archive)?;
    let Ok(entry) = zip.by_name(Manifest::PATH) else {
        return Ok(None);
    };

    // Held to the same ceiling as the index beside it: read whole and then
    // parsed, it is otherwise a way to exhaust memory from a small file.
    let raw = instance::modpack::read_json_entry(entry, "Geode manifest")?;
    let manifest: Manifest = serde_json::from_slice(&raw)
        .map_err(|e| Error::msg(format!("this pack's Geode manifest could not be read: {e}")))?;

    if !manifest.readable() {
        return Err(Error::msg(format!(
            "this pack was written by a newer Geode (format {}, this build reads {FORMAT}) \
             — update before importing it",
            manifest.geode_format
        )));
    }
    Ok(Some(manifest))
}

/// What a pack file is, both halves read.
///
/// [`instance::modpack::preview`] answers for the half other launchers read,
/// and knows nothing of the Geode half — so a `.geodepack` whose notes name
/// CurseForge files used to report that it needed no key and then discover the
/// need inside [`resolve_references`], after the rest of the import had
/// already run, reporting those mods as missing. Asked here instead, the
/// question is answered before an instance exists.
pub async fn preview(archive: &Path) -> Result<instance::modpack::PackPreview> {
    let mut described = instance::modpack::preview(archive.to_path_buf()).await?;
    let probe = archive.to_path_buf();
    let manifest = tokio::task::spawn_blocking(move || read_manifest(&probe))
        .await
        .map_err(|e| Error::msg(format!("reading the pack's manifest panicked: {e}")))??;
    described.needs_curseforge_key |= manifest.as_ref().is_some_and(needs_curseforge_key);
    Ok(described)
}

/// Whether finishing this pack's import will need the recipient's own
/// CurseForge key.
///
/// A note's source is ids rather than a URL precisely so the recipient
/// resolves it themselves, and CurseForge is the one provider
/// [`resolve_references`] can resolve — so a note naming one is a note that
/// needs a key.
fn needs_curseforge_key(manifest: &Manifest) -> bool {
    manifest.files.iter().any(|note| {
        note.source
            .as_ref()
            .is_some_and(|source| source.provider == "curseforge")
    })
}

/// Apply the annotations to an instance the mrpack half already created.
///
/// Only ever fills in what the pack actually carried: a manifest that recorded
/// no memory size leaves the importer's default alone rather than writing a
/// zero over it.
///
/// A server's pack never sets a launcher setting — memory, Java arguments,
/// the window size or fullscreen — whatever it carries. Geode never writes one
/// into a server's pack, so a server pack that has them was written by
/// something else. Java arguments are a way to run code (`-javaagent:`) that no
/// mod count in the join preview would show; memory is sized for the server's
/// own heap, and the window is the player's own screen, which no server knows
/// anything about. None of the five is named in the join preview either, so a
/// player accepting a pack is not accepting any of them. A person's pack is
/// still trusted with all five.
pub fn apply(manifest: &Manifest, instance: &mut Instance) {
    let p = &manifest.profile;
    if !p.name.trim().is_empty() {
        instance.name = p.name.clone();
    }
    if p.group.is_some() {
        instance.group = p.group.clone();
    }
    if !p.notes.is_empty() {
        instance.notes = p.notes.clone();
    }
    let personal = manifest.kind != Kind::Server;
    if personal && p.memory_mb.is_some() {
        instance.overrides.memory_mb = p.memory_mb;
    }
    if personal && p.java_args.is_some() {
        instance.overrides.java_args = p.java_args.clone();
    }
    if personal && p.width.is_some() {
        instance.overrides.width = p.width;
    }
    if personal && p.height.is_some() {
        instance.overrides.height = p.height;
    }
    if personal && p.fullscreen.is_some() {
        instance.overrides.fullscreen = p.fullscreen;
    }
    if manifest.source.is_some() {
        instance.pack = manifest.source.clone();
    }
    // Not a launcher setting and not the player's to choose: it is how far
    // this instance has caught up with the sender's settings pushes, and the
    // files this pack just wrote whole are the ones it names. Without it they
    // would be written whole again on the next update, over whatever the
    // player had changed in the meantime.
    if let Some(settings) = &manifest.settings {
        instance.settings_at = Some(settings.at);
    }
    // A Java path is never carried, so there is nothing to apply and nothing
    // that could point at a directory on somebody else's machine.
}

/// How a pack's settings files land in this instance: the one place a policy
/// turns into an action.
///
/// A path the pack says nothing about is written whole and a write that fails
/// fails the import, which is what importing a pack has always done.
///
/// [`Policy::Enforce`] is that behaviour too, and the only thing it does not
/// share is the failure: a settings file the player has made read-only is a
/// line in their report rather than a launch they cannot have.
/// [`Policy::ShipOnce`] and [`Policy::Merge`] arrive whole while the player is
/// behind the push the path's behaviour was chosen at — on a first join there
/// is no file there at all, so they arrive then too — and are the player's
/// from then on, which is the whole of keeping their own changes.
///
/// `owed` is the paths a run before this one could not write, which are
/// offered again however their behaviour reads: the player never received the
/// sender's copy of them, and "present and caught up" would otherwise be true
/// of a file that never landed, so nothing would ever send it again.
fn landings(
    settings: Option<&Settings>,
    applied: u32,
    owed: &[String],
    allowed: bool,
) -> instance::modpack::Overrides {
    use instance::modpack::Landing;

    let settings = settings.cloned().unwrap_or_default();
    let owed: Vec<PathBuf> = owed.iter().map(PathBuf::from).collect();
    instance::modpack::Overrides::decided(move |rel, present| {
        let Some(named) = settings.covering(rel) else {
            return Landing::Replace;
        };
        // The player's own switch, and it has no exceptions: a file replaced
        // every time stops being replaced too. Their files are not touched
        // when they turn it off — nothing here is undoable — and because
        // nothing records where they got to while it is off, turning it back
        // on applies everything at once.
        if !allowed {
            return Landing::Keep;
        }
        landing_for(named, applied, &owed, rel, present)
    })
}

/// A pack's settings half, held to every ceiling and every path rule before
/// anything acts on it — and a line for the recipient's report when it goes
/// past one.
///
/// Past a ceiling the half is dropped whole rather than trimmed, and the pack
/// is still imported. That is the safe answer and not a lenient one: with no
/// settings half, every file the pack carries is written whole and a write
/// that fails stops the import — exactly what every Geode did before the field
/// existed, and what a pack leaving the field out gets today. Refusing the
/// pack instead would cost the recipient every mod in it over a list they
/// cannot see, and trimming it would apply half of what a stranger sent.
///
/// Geode cannot build a half this refuses — `settings::push::push` checks the
/// same ceilings before writing one — so a refusal here means the pack came
/// from something else.
fn settings_half(manifest: Option<&Manifest>) -> (Option<&Settings>, Vec<String>) {
    let Some(half) = manifest.and_then(|m| m.settings.as_ref()) else {
        return (None, Vec::new());
    };
    match half.checked() {
        Ok(()) => (Some(half), Vec::new()),
        Err(e) => (
            None,
            vec![format!(
                "Geode did not use what this pack says about its settings files, and wrote each \
                 of them whole instead: {e}"
            )],
        ),
    }
}

/// How one synced file lands, as one rule in one place.
///
/// Two things ask it and must never disagree: the extraction, which decides
/// whether the pack's copy of a file is written at all, and
/// [`pushes_to_splice`], which decides whether the sender's individual
/// settings have to be spliced into the copy the player kept. A file written
/// whole already holds every one of those settings, so splicing it too would
/// be work at best and, if the two rules ever drifted, a file written whole
/// and then edited again.
fn landing_for(
    named: &SyncedPath,
    applied: u32,
    owed: &[PathBuf],
    rel: &Path,
    present: bool,
) -> instance::modpack::Landing {
    use instance::modpack::Landing;

    match named.policy {
        Policy::Enforce => Landing::Offer,
        Policy::ShipOnce | Policy::Merge => {
            if present && applied >= named.from && !owed.iter().any(|path| path == rel) {
                Landing::Keep
            } else {
                Landing::Offer
            }
        }
    }
}

/// Which of the sender's settings this update has to splice into files the
/// player keeps, worked out **before** anything is written.
///
/// Before, because the rule turns on whether the player already has the file,
/// and after the extraction every synced file is there. A file that lands
/// whole is left out: it arrives holding every setting the sender owns, so
/// there is nothing to splice into it.
///
/// Only a [`Policy::Merge`] path. A path sent once is never touched again,
/// even if the sender owns a setting inside it — "sent once" has to mean that
/// or it means nothing — and a path replaced every time has just been
/// replaced.
///
/// Grouped by file, so each file is read, spliced and written once however
/// many settings landed in it.
fn pushes_to_splice(
    settings: &Settings,
    applied: u32,
    owed: &[String],
    game_dir: &Path,
) -> Vec<(String, Vec<Override>)> {
    use instance::modpack::Landing;

    let owed: Vec<PathBuf> = owed.iter().map(PathBuf::from).collect();
    let mut plan: Vec<(String, Vec<Override>)> = Vec::new();
    for entry in &settings.overrides {
        // Everything above what this instance has caught up with, and nothing
        // at or below it: a setting the player has already been sent is never
        // applied again, which is what makes their own change back stick.
        if entry.at <= applied || !entry.allowed() {
            continue;
        }
        let rel = Path::new(&entry.path);
        let Some(named) = settings.covering(rel) else {
            continue;
        };
        if named.policy != Policy::Merge {
            continue;
        }
        let present = game_dir.join(rel).exists();
        if landing_for(named, applied, &owed, rel, present) != Landing::Keep {
            continue;
        }
        match plan.iter_mut().find(|(path, _)| *path == entry.path) {
            Some((_, group)) => group.push(entry.clone()),
            None => plan.push((entry.path.clone(), vec![entry.clone()])),
        }
    }
    plan
}

/// Splice the sender's settings into the files the player kept, and say what
/// moved and what could not be written.
///
/// A file that cannot be spliced — read-only, or not an ordinary file, or in a
/// format Geode cannot read one setting at a time — is named in the player's
/// report and left exactly as it is. Not retried on the next Play either, and
/// that is deliberate: the player has their own copy of it, so there is
/// nothing owed to them, and offering the file whole instead would replace the
/// very work keeping it was for. The sender's next push tries again.
async fn splice_the_pushes(
    paths: &Paths,
    instance: &Instance,
    plan: &[(String, Vec<Override>)],
) -> (Vec<(String, crate::settings::Applied)>, Vec<String>) {
    let game_dir = paths.game_dir(&instance.id);
    let mut landed = Vec::new();
    let mut skipped = Vec::new();
    for (rel, overrides) in plan {
        match crate::settings::splice_file(&game_dir.join(rel), overrides).await {
            Ok(applied) => {
                landed.extend(applied.into_iter().map(|one| (rel.clone(), one)));
            }
            Err(e) => skipped.push(format!("{rel} — {e}")),
        }
    }
    (landed, skipped)
}

/// Import a pack into a freshly created instance — both halves, in order.
///
/// The mrpack half runs first and builds a working instance any launcher would
/// recognise; the Geode half then applies what `.mrpack` has nowhere to put.
/// This is the one sequence the app runs, so it is also the one the tests run.
///
/// `Ok(vec![])` means everything the pack named is now on disk. Anything in the
/// returned list is a file the manifest pointed at that the recipient has to
/// fetch themselves — which is the design working rather than a shortfall: the
/// pack said where each mod lives instead of carrying somebody else's jar, and
/// a named list is more actionable than an instance that is quietly incomplete.
///
/// On any exit, including an error or a cancel, whatever landed from a server's
/// pack has already been claimed for the server, and turned on or off as it
/// should be. The caller saves a partial import rather than discarding it, so
/// the ownership has to be right before the failure is reported, not only on
/// success.
///
/// A server's pack also leaves the instance naming no modpack. The mrpack half
/// labels every import as the pack its index names, which for a server is the
/// server's own name beside the version of whatever pack the server was built
/// from — a pack this instance is not. A joined instance says where it came
/// from through its link instead.
pub async fn import(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    let probe = archive.to_path_buf();
    let manifest = tokio::task::spawn_blocking(move || read_manifest(&probe))
        .await
        .map_err(|e| Error::msg(format!("reading the pack's manifest panicked: {e}")))??;

    // Read before anything lands, so what the server claims afterwards is what
    // the pack itself names and nothing else that happens to be on disk.
    let arrived = match manifest.as_ref().filter(|m| m.kind == Kind::Server) {
        Some(manifest) => {
            let probe = archive.to_path_buf();
            let manifest = manifest.clone();
            Some(
                tokio::task::spawn_blocking(move || arrivals(&probe, &manifest))
                    .await
                    .map_err(|e| Error::msg(format!("reading the pack's files panicked: {e}")))??,
            )
        }
        None => None,
    };

    let from_a_server = manifest.as_ref().is_some_and(|m| m.kind == Kind::Server);
    // The pack's settings half, held to every ceiling before a byte of it is
    // acted on — and treated as absent, with a line for the report, when it
    // goes past one. Absent is the safe answer: it is what every Geode from
    // before the field existed does, which is to write each file whole.
    let (half, refused) = settings_half(manifest.as_ref());
    // How the pack's own settings files land, worked out before anything is
    // written. A fresh instance has caught up with nothing, so on a first
    // join every one of them arrives whole — which is what a default config
    // being shipped means.
    let how = landings(
        half,
        instance.settings_at.unwrap_or(0),
        &instance.settings_owed,
        !instance.server_settings_off,
    );

    let outcome = async {
        if from_a_server {
            check_server_downloads(archive).await?;
        }
        // Only a server's pack is held to Modrinth — and with it everything
        // else the import does over the network, the lookup that turns its
        // files into names included. A `.mrpack` the player chose themselves
        // names whatever hosts that format allows, and a CurseForge one is
        // fetched from wherever the CurseForge API answers with.
        let http = if from_a_server {
            server_pack_client()
        } else {
            client.clone()
        };
        // Both halves name what the user has to fetch themselves, and both
        // lists are the same list: a CurseForge file nobody may download and a
        // reference the recipient's own key could not resolve are the same
        // problem to whoever reads the report. A settings file that could not
        // be written joins them, for the same reason: it is one more line
        // naming a file and what to do about it.
        let imported = instance::modpack::import_into_at(
            &http,
            crate::providers::curseforge::API,
            crate::providers::modrinth::API,
            paths,
            instance,
            archive,
            curseforge_key,
            &how,
            progress,
        )
        .await?;
        let mut missing = imported.left_out;
        missing.extend(imported.not_written.into_iter().map(|file| file.why));
        missing.extend(refused.clone());
        // A plain `.mrpack` has no Geode half.
        if let Some(manifest) = &manifest {
            missing.extend(
                apply_pack(client, paths, instance, archive, manifest, curseforge_key, progress)
                    .await?,
            );
        }
        Ok::<_, Error>(missing)
    }
    .await;

    if let Some(arrived) = &arrived {
        instance.pack = None;
        settle_server_files(paths, instance, arrived, &PlayerOff::default()).await?;
    }
    // Last, and after the server has claimed what it sent: a jar that arrived
    // as bytes gets its record from `settle_server_files`, so naming it before
    // that ran would mean naming nothing at all. Only once the import worked —
    // a failure is already a sentence the player has to read, and a minute of
    // hashing on the way to it is not an improvement.
    //
    // The general client even for a server's pack, which is no widening: the
    // only addresses this reaches are Modrinth's own API, written down in
    // `providers::modrinth` and never read out of the pack. What the
    // restricted client is for is a pack naming where to download from, and
    // this asks nothing of the pack at all — the same reason
    // `update_from_server` resolves its references through it.
    if outcome.is_ok() {
        crate::install::content::name_what_arrived(client, paths, instance, progress).await;
    }
    outcome
}

/// What an update from a server did that the player should hear about.
#[derive(Debug, Default)]
pub struct UpdateReport {
    /// Files the pack pointed at that the player has to fetch themselves, as
    /// [`import`] returns them.
    pub missing: Vec<String>,
    /// The player's own mods that are off because the server sends the same
    /// mod, by name.
    pub turned_off: Vec<String>,
    /// Settings files the server sent that could not be written, so the
    /// player's own are still there, each line naming the file and why.
    ///
    /// Reported rather than fatal, and only for the paths the server syncs. A
    /// read-only config used to fail the import, which failed the update,
    /// which stopped the launch — on that Play and, because the revision was
    /// never recorded, on every Play after it. A mod the player is short of
    /// stops them joining; a settings file they kept their own copy of
    /// usually does not.
    ///
    /// Each of these is also left owed on the instance, so a player who makes
    /// the file writable and presses Play gets the server's copy, which is
    /// what the notice tells them. Without that the file would be skipped for
    /// good: the revision is recorded, so nothing would fetch a pack again,
    /// and a synced file that is present and caught up is one nothing sends.
    pub not_written: Vec<String>,
    /// The player's own mods that are off because the server does not allow
    /// them, each with the admin's reason. Said again on every update: a ban
    /// is a standing rule, not a change that happened once.
    pub banned: Vec<Banned>,
    /// The revision the pack itself carried, when it carried one: what the
    /// instance now holds, to keep instead of the revision of a description
    /// read before the pack was built. A pack from an admin's Geode older
    /// than this has none, and the caller keeps the description's.
    pub revision: Option<String>,
    /// What the server's own settings pushes did to the player's files: the
    /// settings that moved, how many of them the player had changed
    /// themselves, and any file a change could not be written to.
    ///
    /// `None` when nothing moved, which is every update from a server that
    /// has pushed nothing. Already written to this instance's own
    /// `last-sync.json` by the time this is returned — the copy here is so
    /// the launch can say one sentence and point at the panel that holds the
    /// rest.
    pub settings: Option<crate::settings::report::LastSync>,
}

/// One mod of the player's own that is off because the server does not allow
/// it.
///
/// Ordered by name, so the sentence the player reads does not shuffle between
/// updates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Banned {
    /// What to call it to them: their record's name, else the file's.
    pub name: String,
    /// The admin's own words for why. Never empty — a rule with no reason is a
    /// bug report waiting to happen, which is why `Forbidden::checked` refuses
    /// one.
    pub reason: String,
}

/// The revision a pack says it holds, when it says one.
pub fn pack_revision(manifest: &Manifest) -> Option<String> {
    manifest
        .revision
        .clone()
        .filter(|revision| !revision.trim().is_empty())
}

/// Bring an instance joined from a server up to a newer pack from that server,
/// in place.
///
/// The instance is the player's, and only the server's part of it changes. In
/// order:
///
/// 1. The pack must be a server's, and may download from nowhere but Modrinth.
///    Either refusal comes before anything is touched.
/// 2. What the pack brings is read off it, as [`import`] reads it.
/// 3. What the player turned off among the server's files is noted, by name and
///    by project. A file off because the server had it off, or because the
///    player keeps server-side mods off, is not the player's own choice and is
///    applied again from the pack instead.
/// 4. Each file the player already has moves to the name the pack writes it
///    under, so a mod they keep off is not fetched all over again — see
///    [`take_the_packs_name`].
/// 5. The mrpack half runs, and the references are resolved again. The rest of
///    the Geode half does not: the instance's name, group, notes, window
///    settings, icon and banner stay the player's. Each settings file the pack
///    carries lands here, as the behaviour its admin chose for it says, worked
///    out before anything is written from what this instance has already
///    caught up with — see [`landings`]. One that could not be written is
///    noted and the player's own left in place, rather than failing the
///    update.
/// 6. On every exit, including an error or a cancel, what landed is settled as
///    the server's, with each file the player turned off kept off — under a new
///    file name too, when it is the same project.
/// 7. The server's files the pack no longer brings are deleted, turned off or
///    not: the server decides what it has. One the player had turned off waits
///    instead, when the update failed before the new version of that project
///    landed to hold the choice in its place.
/// 8. A mod of the player's own that is the same mod as one the server now has
///    on, under another file name, is turned off — two copies of one mod is a
///    game that does not start — but not deleted, and it stays the player's.
///    The same Modrinth or CurseForge project by their record, or the same id
///    declared by both jars when a jar they dropped in by hand has no record
///    to go by. Its name is reported, on this update and on any that follows
///    while it is off.
/// 9. A mod of the player's own the server does not allow is turned off, with
///    the admin's reason, and not deleted: the file keeps its bytes and its
///    record stays theirs. Named by identity — a provider and project one of
///    their records holds, or an id one of their jars declares — never by a
///    path the pack wrote. This one re-applies on every update, because it is
///    a standing rule and not a change that happened once, and it is
///    client-side only: nothing here is enforcement.
/// 10. Only once all of that finished does the instance take the pack's
///     Minecraft version, loader and loader version, and the push its settings
///     half was at — with the paths this run could not write left owed, since
///     that number says the push landed and for those files it did not. A
///     failed update is saved by the caller as far as it got, and runs again
///     next time; until then the game it starts is the one it had.
///
/// Everything else in the instance is left alone: the player's own records and
/// untracked files, worlds, options, resource packs, shaders, memory, Java, and
/// whether server-side mods are on. Config files the server sends land as the
/// behaviour its admin chose for them says — replaced every time, sent once and
/// the player's from then on, or kept in sync, where the file is the player's
/// and only the settings the admin pushed are spliced into it
/// (`super::manifest::Settings`); config files it stopped sending stay,
/// whichever it was. One that could not be written whole is reported and left
/// owed, so the next run offers it again however its behaviour reads. One a
/// setting could not be spliced into is reported and **not** owed: the player
/// has their own copy of it, so offering the file whole instead would replace
/// the work keeping it was for.
///
/// A player who has turned the server's settings off gets none of this and
/// keeps where they had got to, so turning it back on applies the backlog in
/// one pass.
pub async fn update(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<UpdateReport> {
    use instance::content::content_dir;

    let probe = archive.to_path_buf();
    let manifest = tokio::task::spawn_blocking(move || read_manifest(&probe))
        .await
        .map_err(|e| Error::msg(format!("reading the pack's manifest panicked: {e}")))??
        .filter(|manifest| manifest.kind == Kind::Server)
        .ok_or_else(|| {
            Error::msg("that address did not send a server's pack, so nothing was changed")
        })?;
    check_server_downloads(archive).await?;
    let described = instance::modpack::preview(archive.to_path_buf()).await?;
    let arrived = {
        let (probe, manifest) = (archive.to_path_buf(), manifest.clone());
        tokio::task::spawn_blocking(move || arrivals(&probe, &manifest))
            .await
            .map_err(|e| Error::msg(format!("reading the pack's files panicked: {e}")))??
    };

    let player_off = players_own_off(paths, instance).await;
    let allowed = !instance.server_settings_off;
    // The pack's settings half, held to every ceiling before anything acts on
    // it — see [`settings_half`].
    let (half, refused) = settings_half(Some(&manifest));
    // How the pack's own settings files land, worked out before anything is
    // written — and from what this instance has caught up with, which is why
    // a file the player has already been sent once is not sent again.
    let how = landings(
        half,
        instance.settings_at.unwrap_or(0),
        &instance.settings_owed,
        allowed,
    );
    // And which of the sender's individual settings have to be spliced into
    // the files this player keeps. Worked out here, before the extraction,
    // because the rule turns on whether they already have the file.
    let plan = match half.filter(|_| allowed) {
        Some(half) => pushes_to_splice(
            half,
            instance.settings_at.unwrap_or(0),
            &instance.settings_owed,
            &paths.game_dir(&instance.id),
        ),
        None => Vec::new(),
    };
    let mut not_written: Vec<instance::modpack::NotWritten> = Vec::new();

    // What the pack's index says each file it names is, so only that very file
    // is moved to the name it will be written under.
    let indexed = {
        let probe = archive.to_path_buf();
        let index = tokio::task::spawn_blocking(move || instance::modpack::read_index(&probe))
            .await
            .map_err(|e| Error::msg(format!("reading the pack's index panicked: {e}")))??;
        index
            .into_iter()
            .flat_map(|index| index.files)
            .filter_map(|file| {
                let (kind, base, _) = content_file(&file.path)?;
                Some(((kind, base), file.hashes.get("sha1").cloned()))
            })
            .collect::<HashMap<_, _>>()
    };

    let outcome = async {
        for ((kind, name), arrival) in &arrived {
            let dir = content_dir(paths, &instance.id, *kind);
            let named = indexed
                .get(&(*kind, name.clone()))
                .and_then(|sha1| sha1.as_deref());
            take_the_packs_name(&dir, name, arrival.off, named).await?;
        }

        // The pack's own downloads go through the client that holds them to
        // Modrinth; the references do not, since those URLs come from the
        // provider's API rather than from the pack.
        let http = server_pack_client();
        // One list, for the reason `import` gives: what either half could not
        // install is the same thing to the player reading it. A settings file
        // that could not be written is not on that list — it is already on
        // disk, as the player's own copy, and says so in its own line.
        let imported = instance::modpack::import_into_at(
            &http,
            crate::providers::curseforge::API,
            crate::providers::modrinth::API,
            paths,
            instance,
            archive,
            curseforge_key,
            &how,
            progress,
        )
        .await?;
        not_written = imported.not_written;
        let mut missing = imported.left_out;
        missing.extend(
            resolve_references(
                client,
                crate::providers::curseforge::API,
                crate::providers::modrinth::API,
                paths,
                instance,
                &manifest,
                curseforge_key,
                progress,
            )
            .await?,
        );
        Ok::<_, Error>(missing)
    }
    .await;

    // The mrpack half labels every import as the pack its index names, which
    // for a server is a pack this instance is not — see `import`.
    instance.pack = None;
    settle_server_files(paths, instance, &arrived, &player_off).await?;
    remove_departed(paths, instance, &arrived, &player_off).await?;
    let turned_off = turn_off_the_players_duplicates(paths, instance, &arrived).await?;
    let banned = enforce_the_servers_bans(paths, instance, &arrived, &manifest.forbidden).await?;

    // Named after the settling, for the reason `import` gives: a mod the
    // server sent as bytes has no record until it has been claimed. Through
    // the general client, for the reason `import` gives for that too.
    if outcome.is_ok() {
        crate::install::content::name_what_arrived(client, paths, instance, progress).await;
    }

    // Settled before the outcome is asked for, and not a line sooner: a half
    // that failed or was cancelled still left the server's files on disk, and
    // they are the server's however it ended.
    let mut missing = outcome?;
    missing.extend(refused);

    // The settings the admin pushed, spliced into the files this player kept —
    // after the extraction, so the files are as this update leaves them, and
    // only once it has otherwise finished, since a run that stopped partway
    // leaves `settings_at` where it was and this would run again next time.
    //
    // At Play time and never while the game is up: `bring_up_to_date` is the
    // only caller, and both Minecraft and a mod rewrite their config from
    // memory when they exit, over anything written under them.
    let (landed, could_not) = splice_the_pushes(paths, instance, &plan).await;

    instance.mc_version = described.mc_version;
    instance.loader = described.loader;
    instance.loader_version = described.loader_version;
    // Last, with the version and the loader, and for the same reason: a run
    // that did not finish leaves this where it was, so the next Play decides
    // every synced path again from the same place rather than from half of
    // this one.
    //
    // Nothing of it moves while the player keeps the server's settings off:
    // none of it was written, so a number saying it had been would lose the
    // whole backlog the moment they turned it back on.
    if allowed {
        if let Some(settings) = &manifest.settings {
            instance.settings_at = Some(settings.at);
        }
        // And the paths this run could not write, which that number cannot
        // speak for: it says the push landed, and for these files it did not.
        // Assigned rather than added to, so a path that went in this time — or
        // one the server has stopped sending — drops out of it.
        instance.settings_owed = not_written.iter().map(|file| file.path.clone()).collect();
    }

    // What the player is told about their own settings, in a file beside
    // `instance.json` rather than in a toast that is gone in four seconds.
    // Written only when there is something in it, so an update that moved no
    // setting leaves the last one that did where they can still read it.
    let settings = instance
        .settings_at
        .map(|at| crate::settings::report::from_applied(at, &landed, could_not))
        .filter(|report| report.worth_saying());
    if let Some(report) = &settings {
        // Not worth failing an update over: every setting in it is already on
        // disk, and the player hears the same thing in the launch notice.
        let _ = crate::settings::report::save(paths, &instance.id, report).await;
    }

    Ok(UpdateReport {
        missing,
        turned_off,
        not_written: not_written.into_iter().map(|file| file.why).collect(),
        banned,
        revision: pack_revision(&manifest),
        settings,
    })
}

/// What the player themselves turned off among the server's files, by name and
/// by project, read before the pack lands.
///
/// A file off because the server has it off, or because the player keeps
/// server-side mods off, is not their own choice, and is left to the pack to
/// decide again. Projects as well as names, because the same mod comes back
/// under a new file name when its version changes.
async fn players_own_off(paths: &Paths, instance: &Instance) -> PlayerOff {
    use instance::content::{content_dir, disabled_name, is_regular_file, Origin};

    let mut player_off = PlayerOff::default();
    for record in &instance.content {
        let not_the_players = record.origin != Origin::Server
            || record.server_off
            || (instance.server_side_mods_off && record.side == Some(instance::jar::Side::Server));
        if not_the_players {
            continue;
        }
        let dir = content_dir(paths, &instance.id, record.kind);
        if !is_regular_file(&dir.join(disabled_name(&record.file_name))).await {
            continue;
        }
        player_off
            .names
            .insert((record.kind, record.file_name.clone()));
        if !record.provider.is_empty() {
            player_off
                .projects
                .insert((record.provider.clone(), record.project_id.clone()));
        }
    }
    player_off
}

/// Delete the server's files the pack no longer brings — step 7 of [`update`],
/// which states the rule.
///
/// After the import, not before it: a mod the player turned off whose new
/// version comes under a new file name has nothing but its old file holding
/// that choice until the new one has landed. Deleted first, an update that
/// then failed — or finished with that file left to fetch by hand — would
/// forget it and bring the mod back on next time.
async fn remove_departed(
    paths: &Paths,
    instance: &mut Instance,
    arrived: &HashMap<(instance::content::ContentKind, String), Arrival>,
    player_off: &PlayerOff,
) -> Result<()> {
    use instance::content::{content_dir, disabled_name, either_name_in, Origin};

    let departed: Vec<instance::content::ContentRecord> = instance
        .content
        .iter()
        .filter(|r| r.origin == Origin::Server)
        .filter(|r| !arrived.contains_key(&(r.kind, r.file_name.clone())))
        .cloned()
        .collect();
    for record in departed {
        let dir = content_dir(paths, &instance.id, record.kind);
        let mut waiting = false;
        if !record.provider.is_empty()
            && player_off
                .names
                .contains(&(record.kind, record.file_name.clone()))
        {
            let project = (record.provider.clone(), record.project_id.clone());
            for ((kind, name), arrival) in arrived {
                if arrival.project.as_ref() != Some(&project) {
                    continue;
                }
                let to = content_dir(paths, &instance.id, *kind);
                if either_name_in(&to, name).await.is_none() {
                    waiting = true;
                    break;
                }
            }
        }
        if waiting {
            continue;
        }
        // A record names one file in its folder. One naming anything else
        // was not written by Geode, and nothing is deleted on its word.
        let one_file = Path::new(&record.file_name);
        if crate::paths::is_safe_relative(one_file) && one_file.components().count() == 1 {
            for file in [record.file_name.clone(), disabled_name(&record.file_name)] {
                match tokio::fs::remove_file(dir.join(file)).await {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
        }
        instance.content.retain(|r| {
            !(r.origin == Origin::Server
                && r.kind == record.kind
                && r.file_name == record.file_name)
        });
    }
    Ok(())
}

/// Turn off the player's own copy of a mod the server now sends under another
/// name, and name what ended up off.
///
/// Once the server's copies are where they are going to be. Two copies of one
/// mod is a game that does not start, but only when both are on: a copy of the
/// server's that is off, or that never landed, leaves the player's own the one
/// they have. What is reported is what ended up off, so an update that runs
/// again after a failure says the same thing again.
///
/// Recognised two ways, because a record is not always there to recognise it
/// by. A mod the player installed through Geode carries the provider and
/// project it came from, and that is matched against what the pack says each
/// file it brings is. A jar dropped into the folder by hand carries nothing at
/// all — so the ids the two jars *declare* are compared instead, which is what
/// the loader itself goes by and the only thing left that says they are one
/// mod. Without that second way, a hand-dropped copy of a mod the server now
/// ships left both copies on, and the game that then would not start read to
/// the player as the update having broken their instance.
pub(crate) async fn turn_off_the_players_duplicates(
    paths: &Paths,
    instance: &Instance,
    arrived: &HashMap<(instance::content::ContentKind, String), Arrival>,
) -> Result<Vec<String>> {
    use instance::content::{
        content_dir, either_name_in, is_regular_file, set_enabled_in, ContentKind, Origin,
    };

    // Each of the player's own files to turn off, by kind and the name its
    // record is keyed by, with what to call it to them. One file matched both
    // ways is one mod, and is said once.
    let mut duplicates: HashMap<(ContentKind, String), String> = HashMap::new();
    // Read once, and only when a jar the pack brings could match one: reading
    // every jar in a folder is what `jar::MetaCache` exists to avoid doing
    // twice, and a player with nothing of their own has nothing to compare.
    let theirs = if arrived.keys().any(|(kind, _)| *kind == ContentKind::Mod) {
        the_players_own_jars(paths, instance, arrived).await
    } else {
        Vec::new()
    };

    for ((kind, name), arrival) in arrived {
        let dir = content_dir(paths, &instance.id, *kind);
        if !is_regular_file(&dir.join(name)).await {
            continue;
        }
        if let Some((provider, project_id)) = &arrival.project {
            for record in &instance.content {
                let own_copy = record.origin == Origin::User
                    && record.kind == *kind
                    && &record.provider == provider
                    && &record.project_id == project_id
                    && &record.file_name != name;
                if own_copy {
                    duplicates.insert((*kind, record.file_name.clone()), record.name.clone());
                }
            }
        }
        // Only mods: an id is a thing a mod jar declares, and two copies of one
        // mod is the failure this is about.
        if *kind != ContentKind::Mod || theirs.is_empty() {
            continue;
        }
        let ours = declared_ids(&dir.join(name)).await;
        for jar in &theirs {
            if &jar.file == name {
                continue;
            }
            let same_mod = jar
                .ids
                .iter()
                .any(|id| ours.iter().any(|ours| ours.eq_ignore_ascii_case(id)));
            if same_mod {
                duplicates.insert((*kind, jar.file.clone()), jar.name.clone());
            }
        }
    }

    let mut turned_off = Vec::new();
    for ((kind, file), name) in duplicates {
        let dir = content_dir(paths, &instance.id, kind);
        let Some((_, enabled)) = either_name_in(&dir, &file).await else {
            continue;
        };
        if enabled {
            set_enabled_in(&dir, &file, false).await?;
        }
        turned_off.push(name);
    }
    // Sorted because what they are read out of is a map: the player reads this
    // sentence again on every update, and it should not shuffle.
    turned_off.sort();
    Ok(turned_off)
}

/// Turn off the player's own mods the server does not allow, and say which and
/// why.
///
/// A standing rule rather than a change, and the one asymmetry with everything
/// else an update does: it runs again on every update, so a mod turned back on
/// is off again after the next one. Every update and no more than that — what
/// reaches this is a pack, so a Play that fetches none reaches none of it. Two
/// things make a Play fetch one, and `commands::fetch_due` is both: a revision
/// the instance does not hold, and a settings file an earlier run could not
/// write and still owes. So a player who owes one has the rule re-applied on
/// their very next Play, with the revision exactly where it was. What is
/// reported is what is off, with the admin's own reason, so the player reads
/// the same sentence every time rather than wondering what happened to their
/// mod.
///
/// Never a delete. The file keeps its bytes under the `.disabled` name and its
/// record stays the player's, so deleting it is still their decision and
/// nothing of theirs is ever lost to a rule. The existing refusal in
/// `content::remove_in` is untouched, because it is about the *server's* files
/// and this touches none of them.
///
/// What the pack may name is an identity, never a path: a provider and project
/// one of the player's own records names — by its id or by the slug that same
/// record holds — or an id one of their own jars declares. All of them are read
/// from this instance and these files — the same discipline [`content_file`]
/// applies to everything else a pack claims — so an entry holding a path, a
/// `..`, or a file name resolves to nothing at all and a stranger's pack cannot
/// use the list to reach a file it was never allowed to name.
///
/// The list itself is checked here rather than trusted, by the same
/// [`Forbidden::checked`] the admin's own typing goes through: this one arrived
/// over the network, and the reason is read straight into the notice the player
/// sees.
///
/// The server's own files are not touched. The rule is about what the player
/// brings; a server that ships a mod it forbids is contradicting itself, and
/// turning its own mod off would only leave the next update to put it back.
///
/// Nothing is written to the instance, which is what keeps the rule to the
/// joined instance and to the time it is joined: a duplicate of a joined
/// instance is not a join, never updates, and so inherits no rule.
async fn enforce_the_servers_bans(
    paths: &Paths,
    instance: &Instance,
    arrived: &HashMap<(instance::content::ContentKind, String), Arrival>,
    forbidden: &[Forbidden],
) -> Result<Vec<Banned>> {
    use instance::content::{content_dir, either_name_in, set_enabled_in, ContentKind, Origin};

    // Held to the same rules as what the admin types, because this list came
    // off the network: a pack nobody's Geode built can carry a reason of any
    // length, over any number of lines, and that reason is interpolated into
    // the launch notice. An entry that does not pass is dropped rather than
    // failing the update — refusing the pack whole would cost the player every
    // mod in it, which is the answer `reconcile_listing` gives for a listing it
    // will not write either.
    let forbidden: Vec<Forbidden> = forbidden
        .iter()
        .filter_map(|rule| rule.checked().ok())
        .collect();
    if forbidden.is_empty() {
        return Ok(Vec::new());
    }

    // Each file to turn off, by kind and the name its record is keyed by, with
    // what to call it and the reason to show. A mod matched both ways is one
    // mod, and is said once.
    let mut banned: HashMap<(ContentKind, String), (String, String)> = HashMap::new();

    // By what the player's own record says the file is. A record with no
    // provider names no project, and matches nothing.
    for record in &instance.content {
        if record.origin != Origin::User
            || record.provider.is_empty()
            || record.project_id.is_empty()
        {
            continue;
        }
        // By the project's id, or by the slug the same record holds. The slug
        // is the only id on the project's own page and in its URL, so it is
        // what an admin copies, and a rule that matched nothing would say so
        // nowhere: there is no channel back from the players to tell them.
        let rule = forbidden.iter().find(|rule| {
            !rule.provider.is_empty()
                && rule.provider == record.provider
                && (rule.project_id == record.project_id
                    || record
                        .slug
                        .as_deref()
                        .is_some_and(|slug| slug.eq_ignore_ascii_case(&rule.project_id)))
        });
        if let Some(rule) = rule {
            banned.insert(
                (record.kind, record.file_name.clone()),
                (record.name.clone(), rule.reason.clone()),
            );
        }
    }

    // By the ids the player's own jars declare, which is what is left for a jar
    // they dropped in by hand — and what names the mod rather than the build of
    // it somebody happens to have.
    if forbidden.iter().any(|rule| !rule.mod_id.is_empty()) {
        for jar in the_players_own_jars(paths, instance, arrived).await {
            let rule = forbidden.iter().find(|rule| {
                !rule.mod_id.is_empty()
                    && jar
                        .ids
                        .iter()
                        .any(|id| id.eq_ignore_ascii_case(&rule.mod_id))
            });
            if let Some(rule) = rule {
                banned.insert(
                    (ContentKind::Mod, jar.file.clone()),
                    (jar.name.clone(), rule.reason.clone()),
                );
            }
        }
    }

    let mut off = Vec::new();
    for ((kind, file), (name, reason)) in banned {
        let dir = content_dir(paths, &instance.id, kind);
        // Their file under whichever of its two names it is on disk. Gone
        // since, there is nothing to turn off and nothing to tell them.
        let Some((_, enabled)) = either_name_in(&dir, &file).await else {
            continue;
        };
        if enabled {
            set_enabled_in(&dir, &file, false).await?;
        }
        off.push(Banned { name, reason });
    }
    // Sorted because what they are read out of is a map, and the player reads
    // this on every update.
    off.sort();
    Ok(off)
}

/// One of the player's own mod jars, and what its jar says it is.
struct PlayersJar {
    /// The enabled file name, which is the one a record is keyed by.
    file: String,
    /// What to call it to them: their record's name, else the file's.
    name: String,
    /// The ids the jar declares for itself.
    ids: Vec<String>,
}

/// The player's own mod jars in a joined instance, each with the ids its jar
/// declares.
///
/// Read off the folder rather than off the records, because the files with no
/// record are exactly the ones this is for. What is left out is the server's:
/// anything the pack brought, and anything a record of the server's names. A
/// mod of the server's is the server's to decide on, and was never the player's
/// to turn off.
///
/// A jar that declares nothing we can read is left out too — there is nothing
/// to recognise it by, and guessing from a file name is how a pack gets to name
/// a file it was not allowed to.
///
/// A mod on disk under both of its names is one mod, and the **enabled** copy
/// is the one read, because that is the copy a rule acts on.
async fn the_players_own_jars(
    paths: &Paths,
    instance: &Instance,
    arrived: &HashMap<(instance::content::ContentKind, String), Arrival>,
) -> Vec<PlayersJar> {
    use instance::content::{
        content_dir, either_name_in, is_content_file, strip_disabled, ContentKind, Origin,
    };

    let dir = content_dir(paths, &instance.id, ContentKind::Mod);
    let mut out: Vec<PlayersJar> = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !is_content_file(&file_name, ContentKind::Mod) {
            continue;
        }
        let (base, _) = strip_disabled(&file_name);
        if arrived.contains_key(&(ContentKind::Mod, base.to_string())) {
            continue;
        }
        let record = instance
            .content
            .iter()
            .find(|r| r.kind == ContentKind::Mod && r.file_name == base);
        if record.is_some_and(|r| r.origin != Origin::User) {
            continue;
        }
        // On and off at once is two files and one mod, read once — and read
        // from the copy a rule acts on, which is the enabled one, never from
        // whichever name `read_dir` happened to yield first. The two can be
        // different builds declaring different ids, so deciding from one and
        // turning off the other decides nothing about either, and the answer
        // would change with the order a folder happens to be in.
        if out.iter().any(|jar| jar.file == base) {
            continue;
        }
        let Some((on_disk, _)) = either_name_in(&dir, base).await else {
            continue;
        };
        let ids = declared_ids(&dir.join(on_disk)).await;
        if ids.is_empty() {
            continue;
        }
        out.push(PlayersJar {
            file: base.to_string(),
            name: record
                .map(|r| r.name.clone())
                .unwrap_or_else(|| base.to_string()),
            ids,
        });
    }
    out
}

/// The ids a jar declares for itself, or none when it declares nothing this
/// build can read.
///
/// Its own id and its aliases, never what it bundles: a mod carrying a copy of
/// a library is not that library, and reading the two as one would turn off a
/// mod the loader was happily using because the server ships something that
/// packs it.
///
/// Blocking inside, like every other read of a jar.
async fn declared_ids(path: &Path) -> Vec<String> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || instance::jar::read(&owned))
        .await
        .ok()
        .flatten()
        .map(|meta| meta.primary)
        .unwrap_or_default()
}

/// Move the player's copy of a file a pack brings to the name the pack writes
/// it under, when their copy is under the other one.
///
/// A pack writes each file under the name the server has it by: a mod the
/// server has on as `x.jar`, one it has off as `x.jar.disabled`. A player who
/// keeps a mod off has it under the other name, where the import does not look
/// — so it is fetched all over again, and a Modrinth that cannot be reached, or
/// a version taken down since, fails the whole update over a file the player
/// already has. Moved first it is found, and [`settle_server_files`] puts the
/// player's choice back afterwards, because what they turned off was read
/// before any of this.
///
/// Only their copy of the very file the pack names is moved: where the index
/// says which file that is, it has to be that one, or it stays where it is and
/// is fetched. A pack that names no hash carries the bytes itself, or fetches
/// them by id against a hash of its own, and either lands over whatever is
/// there.
pub(crate) async fn take_the_packs_name(
    dir: &Path,
    name: &str,
    off: bool,
    named: Option<&str>,
) -> Result<()> {
    use instance::content::{disabled_name, is_regular_file, set_enabled_in};

    let disabled = disabled_name(name);
    let (written, other) = if off {
        (disabled.as_str(), name)
    } else {
        (name, disabled.as_str())
    };
    if is_regular_file(&dir.join(written)).await || !is_regular_file(&dir.join(other)).await {
        return Ok(());
    }
    if let Some(sha1) = named {
        let same = matches!(
            crate::net::sha1_file(&dir.join(other)).await,
            Ok(actual) if actual.eq_ignore_ascii_case(sha1)
        );
        if !same {
            return Ok(());
        }
    }
    set_enabled_in(dir, other, !off).await?;
    Ok(())
}

/// One content file a server's pack brings, however it brings it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Arrival {
    /// The server had it turned off.
    pub off: bool,
    /// How the server classified it, when its pack said.
    pub side: Option<instance::jar::Side>,
    /// The provider and project it is, when the pack names one.
    pub project: Option<(String, String)>,
}

/// Every content file a server's pack brings, by kind and enabled file name.
///
/// Read off the pack rather than the disk after importing, because a joined
/// instance holds more than the server's files: a player's own mod beside
/// them is theirs, and must not be claimed for the server just for being in
/// the same folder. So only what the pack names counts — its index entries a
/// client installs, its references, and the files it carries as bytes directly
/// in a content folder — and the notes then say which side each belongs on.
///
/// A mod the pack brings both on and off is one mod, and arrives on: that is
/// the copy a server's pack names.
///
/// Blocking: it reads the zip.
pub(crate) fn arrivals(
    archive: &Path,
    manifest: &Manifest,
) -> Result<HashMap<(instance::content::ContentKind, String), Arrival>> {
    let mut found: HashMap<(instance::content::ContentKind, String), Arrival> = HashMap::new();
    let mut add = |rel: &str, off: bool, project: Option<(String, String)>| {
        let Some((kind, base, disabled)) = content_file(rel) else {
            return;
        };
        found
            .entry((kind, base))
            .and_modify(|a| {
                a.off &= off || disabled;
                a.project = a.project.take().or(project.clone());
            })
            .or_insert(Arrival {
                off: off || disabled,
                side: None,
                project,
            });
    };

    for file in instance::modpack::read_index(archive)?
        .into_iter()
        .flat_map(|index| index.files)
    {
        // What an index says a client does not install never lands.
        if file
            .env
            .as_ref()
            .is_some_and(|env| env.client == "unsupported")
        {
            continue;
        }
        let project = file
            .downloads
            .iter()
            .find_map(|url| crate::install::content::ids_from_modrinth_url(url))
            .map(|(project, _)| ("modrinth".to_string(), project));
        add(&file.path, false, project);
    }
    for note in &manifest.files {
        if let Some(source) = &note.source {
            let project = (source.provider.clone(), source.project_id.clone());
            add(&note.path, note.disabled, Some(project));
        }
    }
    let zip = instance::modpack::open_pack(archive)?;
    for name in zip.file_names() {
        if let Some(rel) = ["overrides/", "client-overrides/"]
            .iter()
            .find_map(|root| name.strip_prefix(root))
        {
            add(rel, false, None);
        }
    }

    for note in &manifest.files {
        if let Some((kind, base, _)) = content_file(&note.path) {
            if let Some(arrival) = found.get_mut(&(kind, base)) {
                arrival.side = note.side;
            }
        }
    }
    Ok(found)
}

/// A pack path naming a content file directly inside its folder: the kind,
/// the enabled name, and whether the name is turned off.
///
/// Anything deeper, or anywhere else, is not a file a content folder lists,
/// so it is nothing to claim.
pub(crate) fn content_file(rel: &str) -> Option<(instance::content::ContentKind, String, bool)> {
    let path = Path::new(rel);
    if !crate::paths::is_safe_relative(path) || path.components().count() != 2 {
        return None;
    }
    let kind = kind_for(path)?;
    let name = path.file_name()?.to_str()?;
    if !instance::content::is_content_file(name, kind) {
        return None;
    }
    let (base, enabled) = instance::content::strip_disabled(name);
    Some((kind, base.to_string(), !enabled))
}

/// What a player turned off themselves among a server's files, to keep off
/// when the server sends them again.
#[derive(Debug, Default)]
pub(crate) struct PlayerOff {
    /// By kind and enabled file name.
    pub names: HashSet<(instance::content::ContentKind, String)>,
    /// By provider and project, so a new version under a new name stays off.
    pub projects: HashSet<(String, String)>,
}

/// Make what a server's pack brought the server's, in the state it should be.
///
/// Tagging records alone is not enough, and was the first version's mistake: a
/// jar with no provenance travels as an override, lands with no record at all,
/// and so could never be tagged — which left exactly the mods a server most
/// needs to keep, the ones with no public download, as the only ones a player
/// could delete. So every file the pack brought gets a record here, and one
/// that had none gets a record that says nothing except whose it is. Files the
/// pack did not bring are left alone, record or not: they are the player's.
///
/// Each file ends up on disk once, on or off. It is off when the server had it
/// off, when the player turned it off before, or when it is a server-side mod
/// and the player keeps those off; its record remembers the server's side and
/// whether the server had it off, so the player's choices can be told apart
/// from the server's later.
///
/// Safe to run on a failed import too, and should be: a server pack that
/// stopped halfway is still the server's, and a file that did not land is
/// simply skipped.
pub(crate) async fn settle_server_files(
    paths: &Paths,
    instance: &mut Instance,
    arrivals: &HashMap<(instance::content::ContentKind, String), Arrival>,
    player_off: &PlayerOff,
) -> Result<()> {
    use instance::content::{
        content_dir, disabled_name, is_regular_file, set_enabled_in, ContentRecord, Origin,
    };

    for ((kind, name), arrival) in arrivals {
        let dir = content_dir(paths, &instance.id, *kind);
        let disabled = disabled_name(name);
        let (fresh, stale) = if arrival.off {
            (&disabled, name)
        } else {
            (name, &disabled)
        };
        let (has_fresh, has_stale) = (
            is_regular_file(&dir.join(fresh)).await,
            is_regular_file(&dir.join(stale)).await,
        );
        if !has_fresh && !has_stale {
            continue;
        }
        // Both on and off is one mod twice, and the one the pack just wrote is
        // the one it named.
        if has_fresh && has_stale {
            tokio::fs::remove_file(dir.join(stale)).await?;
        }
        let current = if has_fresh { fresh } else { stale };

        let off = arrival.off
            || player_off.names.contains(&(*kind, name.clone()))
            || arrival
                .project
                .as_ref()
                .is_some_and(|project| player_off.projects.contains(project))
            || (instance.server_side_mods_off && arrival.side == Some(instance::jar::Side::Server));
        if (current == &disabled) != off {
            set_enabled_in(&dir, current, !off).await?;
        }

        match instance
            .content
            .iter_mut()
            .find(|r| r.kind == *kind && &r.file_name == name)
        {
            Some(record) => {
                record.origin = Origin::Server;
                record.side = arrival.side;
                record.server_off = arrival.off;
            }
            None => instance.content.push(ContentRecord {
                file_name: name.clone(),
                kind: *kind,
                // Nothing is known about where it came from, and nothing is
                // pretended: no provider means no update check and no source.
                provider: String::new(),
                project_id: String::new(),
                version_id: String::new(),
                name: name.clone(),
                version_number: String::new(),
                slug: None,
                icon_url: None,
                dependency: false,
                origin: Origin::Server,
                requires: None,
                side: arrival.side,
                chosen_side: None,
                keep_on_server: false,
                server_off: arrival.off,
            }),
        }
    }
    Ok(())
}

/// Refuse a server's pack whose index would download from anywhere but
/// Modrinth, before anything is fetched.
///
/// A server's pack comes from a machine the player was only given an address
/// for, and its index is a list of URLs fetched without asking. Honoured from
/// anywhere, it could have the player's Geode request any address — their own
/// router included — over plain HTTP, and write whatever came back for as long
/// as it kept coming. Modrinth over HTTPS is the only place [`export_server`]
/// names, so nothing a real server sends is lost.
pub async fn check_server_downloads(archive: &Path) -> Result<()> {
    let probe = archive.to_path_buf();
    let index = tokio::task::spawn_blocking(move || instance::modpack::read_index(&probe))
        .await
        .map_err(|e| Error::msg(format!("reading the pack's index panicked: {e}")))??;
    // A pack with no Modrinth index is a CurseForge one, and there is no index
    // here to check: its files are ids the CurseForge API turns into downloads.
    // [`export_server`] only ever writes a Modrinth index, so nothing a real
    // server sends arrives this way.
    let Some(index) = index else {
        return Err(Error::msg(
            "this server's pack is in CurseForge's format, not Modrinth's, so it was not installed",
        ));
    };
    let elsewhere = index
        .files
        .iter()
        .find(|file| !file.downloads.iter().all(|url| from_modrinth(url)));
    match elsewhere {
        Some(file) => Err(Error::msg(format!(
            "this server's pack would download {} from somewhere other than Modrinth, so it was not installed",
            file.path
        ))),
        None => Ok(()),
    }
}

/// The client a server's pack is downloaded through.
///
/// [`check_server_downloads`] reads the URLs the index names, and the general
/// client would then follow up to ten redirects from them to any host and any
/// scheme — so the promise that check makes would rest on Modrinth's CDN never
/// answering with a `Location`. Here a hop is followed only to where the index
/// was allowed to point in the first place, through the same predicate, and a
/// plain HTTP URL is refused outright rather than requested.
///
/// The pack's files are not all that rides on it: the import that follows also
/// asks Modrinth's API what the files it just fetched belong to, and that host
/// is not one [`from_modrinth`] allows, so a redirect on that call is stopped
/// and the pack's contents show under their file names.
///
/// Built once: a client holds its own connection pool, and these downloads
/// arrive ten at a time.
fn server_pack_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                // Held to the general client's numbers: the largest download
                // the app makes is no place to be more impatient than the rest.
                .user_agent(crate::net::USER_AGENT)
                .timeout(crate::net::REQUEST_TIMEOUT)
                .connect_timeout(crate::net::CONNECT_TIMEOUT)
                .https_only(true)
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if may_follow(attempt.previous().len(), attempt.url().as_str()) {
                        attempt.follow()
                    } else {
                        attempt.stop()
                    }
                }))
                .build()
                .expect("http client")
        })
        .clone()
}

/// The most hops a server pack's download may be sent through.
///
/// What reqwest's own limit would have been, restated because a custom policy
/// replaces it rather than adding to it: nothing but the policy counts the
/// chain, so two Modrinth URLs that redirect to each other would be followed
/// for as long as they kept answering.
const MAX_REDIRECTS: usize = 10;

/// Whether the client above may follow a redirect it was handed.
///
/// Past the cap the hop is stopped rather than made, which hands the 30x back
/// to the caller — and a reply that is not a success is already a failed
/// download, so the chain ends there.
fn may_follow(hops_so_far: usize, url: &str) -> bool {
    hops_so_far <= MAX_REDIRECTS && from_modrinth(url)
}

/// `https://cdn.modrinth.com/…`, and nothing dressed up to look like it.
///
/// Both what the index may name and where a redirect may lead — see
/// [`server_pack_client`].
fn from_modrinth(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("cdn.modrinth.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

/// Apply the Geode half to the instance the mrpack half just built.
async fn apply_pack(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    manifest: &Manifest,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    apply(manifest, instance);
    extract_media(paths, instance, archive, manifest).await?;
    add_to_the_multiplayer_list(paths, instance, manifest).await;
    let missing = resolve_references(
        client,
        crate::providers::curseforge::API,
        crate::providers::modrinth::API,
        paths,
        instance,
        manifest,
        curseforge_key,
        progress,
    )
    .await?;
    switch_off_what_the_notes_say(paths, instance, manifest).await?;
    Ok(missing)
}

/// Put the server in this instance's multiplayer list, and point Play at it.
///
/// The whole point of joining a server is to play on it, and until now an
/// import left the player at a title screen with an empty Multiplayer list and
/// the only address they had ever been shown being the pack port — which is
/// not the game's and does not work there.
///
/// Only a server's pack has anybody to list. A pack from a Geode too old to
/// carry a listing, or an admin who has not set one, adds no row and says
/// nothing: that is the graceful degrade, and it is why the manifest format is
/// not bumped for this.
///
/// A failure is not reported and does not stop the import. The address is only
/// recorded once the row is really there, so the next Play — which reconciles
/// this from the server's own description, and has somewhere to put a notice —
/// tries again and tells the player if it still cannot.
async fn add_to_the_multiplayer_list(paths: &Paths, instance: &mut Instance, manifest: &Manifest) {
    if manifest.kind != Kind::Server {
        return;
    }
    // Checked rather than trusted: this came off the network, and a name that
    // would garble somebody's server list is better dropped than written.
    let Some(listing) = manifest.listing.as_ref().and_then(|l| l.checked().ok()) else {
        return;
    };
    let game_dir = paths.game_dir(&instance.id);
    let previous = our_row(instance).map(str::to_string);
    if let Ok(wrote) = instance::multiplayer::ensure(
        &game_dir,
        &listing.name,
        &listing.address,
        previous.as_deref(),
    )
    .await
    {
        listed(instance, &listing.address, wrote);
    }
}

/// Record the address this instance's row stands at and whose row it is, and
/// send Play straight there on the versions of the game that have the flag.
pub(crate) fn listed(instance: &mut Instance, address: &str, wrote: Wrote) {
    instance.joined_listing_address = Some(address.to_string());
    instance.joined_listing_claimed = wrote == Wrote::Claimed;
    instance.overrides.quick_play_server = Some(address.to_string());
}

/// The row [`instance::multiplayer::ensure`] should find again for this
/// instance, and nothing when that row is the player's own.
///
/// A row they wrote keeps the name they gave it, so it is never handed back as
/// one of ours to rename — it is claimed again by its address, every Play.
pub(crate) fn our_row(instance: &Instance) -> Option<&str> {
    if instance.joined_listing_claimed {
        return None;
    }
    instance.joined_listing_address.as_deref()
}

/// Turn off the files the pack said were turned off.
///
/// A file carried as bytes arrives under its own `.disabled` name and needs
/// nothing, and a reference resolved by id is written straight to the off name.
/// What is left is a file the index carried: it arrives fresh from its
/// platform, switched on, and the note is the only thing that remembers the
/// sharer had it off.
///
/// A file the note names that is not there is nothing to do — the import may
/// have been told to fetch it and failed — and so is one already off.
async fn switch_off_what_the_notes_say(
    paths: &Paths,
    instance: &Instance,
    manifest: &Manifest,
) -> Result<()> {
    use instance::content::{content_dir, either_name_in, set_enabled_in};

    for note in manifest.files.iter().filter(|note| note.disabled) {
        let Some((kind, base, _)) = content_file(&note.path) else {
            continue;
        };
        let dir = content_dir(paths, &instance.id, kind);
        if let Some((name, true)) = either_name_in(&dir, &base).await {
            set_enabled_in(&dir, &name, false).await?;
        }
    }
    Ok(())
}

/// Copy the pack's artwork out of `geode/` and point the instance at it.
///
/// Overrides are extracted by path prefix and `geode/` matches none of them, so
/// without this the artwork is not merely unused — it never lands on disk.
async fn extract_media(
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    manifest: &Manifest,
) -> Result<()> {
    let wanted: Vec<(String, bool)> = [(manifest.icon.as_ref(), true), (manifest.banner.as_ref(), false)]
        .into_iter()
        .filter_map(|(entry, is_icon)| entry.map(|e| (e.clone(), is_icon)))
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }

    let media = paths.instance_media(&instance.id);
    tokio::fs::create_dir_all(&media).await?;

    let archive = archive.to_path_buf();
    let media_dir = media.clone();
    let written = tokio::task::spawn_blocking(move || -> Result<Vec<(String, bool)>> {
        use std::io::Read;
        let mut zip = instance::modpack::open_pack(&archive)?;
        let mut out = Vec::new();
        for (entry, is_icon) in wanted {
            let Ok(mut source) = zip.by_name(&entry) else {
                continue;
            };
            // The name on disk is ours, never the pack's. Taking the entry's
            // file name looks safe and is not: on Windows `C:evil.dll` is a
            // single component that `join` treats as a drive-relative path and
            // writes wherever the process happens to be running. Only the
            // extension is taken, and only one the app would accept from its
            // own image picker.
            let Some(ext) = Path::new(&entry)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif"))
            else {
                continue;
            };
            let name = format!("{}.{ext}", if is_icon { "icon" } else { "banner" });

            // The same ceiling as a picture chosen by hand, and enforced on the
            // bytes rather than the header, which a crafted zip can understate.
            const MAX_BYTES: u64 = 8 * 1024 * 1024;
            let mut dest = std::fs::File::create(media_dir.join(&name))?;
            let copied = std::io::copy(&mut (&mut source).take(MAX_BYTES + 1), &mut dest)?;
            if copied > MAX_BYTES {
                drop(dest);
                let _ = std::fs::remove_file(media_dir.join(&name));
                continue;
            }
            out.push((name, is_icon));
        }
        Ok(out)
    })
    .await
    .map_err(|e| Error::msg(format!("extracting pack artwork panicked: {e}")))??;

    for (name, is_icon) in written {
        if is_icon {
            instance.icon = Some(name);
        } else {
            instance.banner = Some(name);
        }
    }
    Ok(())
}

/// Fetch everything the manifest pointed at rather than carried.
///
/// Each reference is resolved live through the recipient's own credentials, so
/// an author who has since turned third-party downloads off is respected rather
/// than worked around — which is the whole reason these are ids and not URLs.
///
/// Both API bases are parameters for the reason `curseforge::check_key_at` is
/// one: what this does is decided entirely by what CurseForge answers about a
/// reference and, for one its author has blocked, what Modrinth answers about
/// the same bytes. A test serves both itself rather than reaching the real
/// APIs with a key no test has.
#[allow(clippy::too_many_arguments)] // two bases and one import's coordinates
async fn resolve_references(
    client: &reqwest::Client,
    cf_base: &str,
    modrinth_base: &str,
    paths: &Paths,
    instance: &mut Instance,
    manifest: &Manifest,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    let refs: Vec<(&FileNote, &SourceRef)> = manifest
        .files
        .iter()
        .filter_map(|note| note.source.as_ref().map(|source| (note, source)))
        .collect();
    if refs.is_empty() {
        return Ok(Vec::new());
    }

    let key = curseforge_key.filter(|k| !k.trim().is_empty());
    let game_dir = paths.game_dir(&instance.id);
    let mut downloads = Vec::new();
    let mut records = Vec::new();
    let mut missing = Vec::new();

    progress.stage("Resolving pack references", refs.len() as u64).await;

    for (note, source) in refs {
        progress.check_cancelled()?;
        progress.advance(1, &note.path).await;

        let rel = Path::new(&note.path);
        if !crate::paths::is_safe_relative(rel) {
            // Refused, and said so. An empty list has to mean everything the
            // pack named arrived — and a pack naming `../../` is exactly the
            // one somebody most needs to hear about.
            missing.push(format!("{} (not a safe path, so it was not fetched)", note.path));
            continue;
        }
        let Some(kind) = kind_for(rel) else {
            missing.push(format!("{} (unrecognised folder)", note.path));
            continue;
        };

        // Only CurseForge is ever written as a reference today — Modrinth goes
        // in the index, and nothing else is installed by id.
        if source.provider != "curseforge" {
            missing.push(format!("{} (from {}, which Geode cannot fetch by id)", note.path, source.provider));
            continue;
        }
        let Some(key) = key else {
            missing.push(format!("{} (needs your CurseForge API key)", note.path));
            continue;
        };
        let (Ok(project), Ok(file_id)) = (
            source.project_id.parse::<u32>(),
            source.version_id.parse::<u32>(),
        ) else {
            missing.push(format!("{} (unreadable CurseForge ids)", note.path));
            continue;
        };

        let version =
            match crate::providers::curseforge::file_at(client, cf_base, project, file_id, key)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    missing.push(format!("{} ({e})", note.path));
                    continue;
                }
            };
        // The author turned third-party downloads off. Exactly one thing is
        // tried instead, and only the hashes can authorise it: that same
        // author's own Modrinth release of the identical bytes. The reference
        // itself is still resolved through the recipient's own key, which is
        // why these are ids and not URLs. See `install::blocked`.
        let mut version = version;
        let file = match version
            .primary_file()
            .filter(|_| version.blocked_reason.is_none())
            .cloned()
        {
            Some(file) => file,
            None => {
                let by_hand = format!("drop it into the instance's {} folder", kind.folder());
                match crate::install::blocked::instead_of(
                    client,
                    &version,
                    &by_hand,
                    crate::install::blocked::Fallback::at(modrinth_base, progress),
                )
                .await
                {
                    crate::install::blocked::Instead::Modrinth {
                        version: authors_own,
                        file,
                    } => {
                        version = authors_own;
                        file
                    }
                    crate::install::blocked::Instead::Refused(because) => {
                        missing.push(format!("{} — {} ({because})", note.path, version.name));
                        continue;
                    }
                }
            }
        };

        // Arrives in whichever state the sharer had it. The record stays on the
        // enabled name either way, which is how every record is keyed.
        let dest = if note.disabled {
            match disabled_rel(rel) {
                Some(off) => game_dir.join(off),
                None => game_dir.join(rel),
            }
        } else {
            game_dir.join(rel)
        };
        downloads.push(
            crate::net::Download::new(file.url.clone(), dest.clone())
                .sha1(file.sha1.clone())
                .size(file.size)
                .label(note.path.clone()),
        );
        records.push((dest, instance::content::ContentRecord {
            file_name: rel
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| note.path.clone()),
            kind,
            // Whichever platform the bytes came from. For nearly every
            // reference that is CurseForge; for one whose author blocked it
            // there and published it on Modrinth themselves, Modrinth.
            provider: version.provider.as_str().to_string(),
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            name: version.name.clone(),
            version_number: version.version_number.clone(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: None,
            side: None,
            chosen_side: None,
            keep_on_server: false,
            server_off: false,
        }));
    }

    progress.stage("Downloading referenced content", downloads.len() as u64).await;
    let fetched = crate::net::fetch_all(client, downloads, progress, 8).await;

    // Record what arrived even if something else did not. A download writes to
    // a `.part` file and renames only on success, so a destination that exists
    // is one that completed. Dropping every record because one of five failed
    // would leave four working jars as untracked files — and a CurseForge jar
    // cannot be re-identified by hash later, so they would stay that way.
    put_records(
        &mut instance.content,
        records
            .into_iter()
            .filter(|(dest, _)| dest.is_file())
            .map(|(_, r)| r),
    );
    fetched?;

    Ok(missing)
}

/// Keep a record for each file that arrived, in place of any the instance
/// already had for the same file.
///
/// An update resolves a server's references again into an instance that has
/// records for them from last time, and two records for one file disagree
/// about which version it is.
fn put_records(
    content: &mut Vec<instance::content::ContentRecord>,
    arrived: impl IntoIterator<Item = instance::content::ContentRecord>,
) {
    for record in arrived {
        content.retain(|r| !(r.kind == record.kind && r.file_name == record.file_name));
        content.push(record);
    }
}

/// Which content folder a manifest path names, if any.
fn kind_for(rel: &Path) -> Option<instance::content::ContentKind> {
    let folder = rel.parent()?.file_name()?.to_str()?;
    [
        instance::content::ContentKind::Mod,
        instance::content::ContentKind::ResourcePack,
        instance::content::ContentKind::ShaderPack,
        instance::content::ContentKind::DataPack,
        instance::content::ContentKind::Plugin,
    ]
    .into_iter()
    .find(|k| k.folder() == folder)
}

// ---------------------------------------------------------------------------
// A server's pack, for its players
// ---------------------------------------------------------------------------

/// The loader and loader version a player's client needs for this server.
///
/// Fabric, Forge and NeoForge servers record their loader version directly.
/// Plugin servers — Paper, Folia, Purpur, Spigot — and vanilla need a vanilla
/// client and no mods. Proxies (Velocity, Waterfall) have no Minecraft version
/// of their own and are refused, and so are the kinds whose client loader
/// version Geode does not record today (Quilt, Mohist, Arclight, Banner) —
/// with a message saying so, rather than a pack that names a version that
/// does not exist.
pub fn client_loader(server: &crate::server::Server) -> Result<(crate::meta::Loader, String)> {
    use crate::meta::Loader;
    use crate::server::ServerKind;

    // Every kind by name and no catch-all, so a kind added later fails to
    // compile here instead of quietly getting somebody else's answer.
    let loader = match server.kind {
        ServerKind::Fabric => Loader::Fabric,
        ServerKind::NeoForge => Loader::NeoForge,
        ServerKind::Forge => Loader::Forge,
        ServerKind::Paper
        | ServerKind::Folia
        | ServerKind::Purpur
        | ServerKind::Spigot
        | ServerKind::Vanilla => return Ok((Loader::Vanilla, String::new())),
        ServerKind::Velocity | ServerKind::Waterfall => {
            return Err(Error::msg(format!(
                "{} is a proxy, which has no Minecraft version of its own to build a pack \
                 for — share one of the servers behind it instead",
                server.kind.display_name()
            )));
        }
        // What `build` holds for these is an installer version, a build number
        // or a file name — never the loader version a client needs.
        ServerKind::Quilt | ServerKind::Mohist | ServerKind::Arclight | ServerKind::Banner => {
            return Err(Error::msg(format!(
                "Geode does not record which loader version a {} server's players need yet, \
                 so it cannot build them a pack",
                server.kind.display_name()
            )));
        }
    };

    // For these three the build is the loader version. A server whose install
    // never finished has none, and a pack naming an empty loader version is one
    // no launcher can install.
    if server.build.trim().is_empty() {
        return Err(Error::msg(format!(
            "this {} server has no loader installed yet, so there is no version to give players",
            server.kind.display_name()
        )));
    }
    Ok((loader, server.build.clone()))
}

/// Which of a server's mods go to its players.
struct ServerMods<'a> {
    /// Records for the mods that go. Only these are indexed or referenced.
    shared: Vec<&'a instance::content::ContentRecord>,
    /// Every mod file that goes, by its path from the run directory as it is
    /// named on disk, whether the pack ends up indexing it, referencing it or
    /// carrying its bytes. The walk carries nothing else from `mods/` or
    /// `client-mods/`, so this is also the count a player is shown.
    carried: HashSet<PathBuf>,
    /// Where each carried file belongs, by the same path as `carried`.
    sides: HashMap<PathBuf, instance::jar::Side>,
}

impl<'a> ServerMods<'a> {
    /// What a carried path is: the folder it comes out of, its name with any
    /// `.disabled` taken off, whether it is the copy that is on, and the side
    /// it goes to a player with.
    ///
    /// The name is worked out once, here, so a caller that wants both halves of
    /// it cannot end up asking two different questions.
    fn about<'p>(
        &self,
        rel: &'p Path,
    ) -> (
        instance::content::ContentKind,
        &'p str,
        bool,
        instance::jar::Side,
    ) {
        use instance::content::{strip_disabled, ContentKind};

        let kind = if rel.starts_with(ContentKind::ClientMod.folder()) {
            ContentKind::ClientMod
        } else {
            ContentKind::Mod
        };
        let (base, enabled) = rel
            .file_name()
            .and_then(|n| n.to_str())
            .map_or(("", true), strip_disabled);
        let side = self
            .sides
            .get(rel)
            .copied()
            .unwrap_or(instance::jar::Side::Both);
        (kind, base, enabled, side)
    }

    /// The record that goes with a carried file, when one does.
    ///
    /// Only the records that go are asked. A record whose file is a link, or is
    /// gone, names nothing the pack sends and is not in `shared` — which is
    /// what makes this a different question from the lookup [`server_mods`]
    /// runs over every record.
    fn shared_record_for(
        &self,
        kind: instance::content::ContentKind,
        base: &str,
    ) -> Option<&'a instance::content::ContentRecord> {
        self.shared
            .iter()
            .copied()
            .find(|r| r.kind == kind && r.file_name == base)
    }

    /// Every carried file as the catalogue wants to be told about it.
    ///
    /// Built from the walk rather than from the records, because the walk is
    /// what decides what the pack sends: a jar dropped in by hand is a mod,
    /// and a record whose file is a link or is gone names nothing.
    fn candidates(&self) -> Vec<identity::Candidate> {
        self.carried
            .iter()
            .map(|rel| {
                let (kind, base, enabled, _) = self.about(rel);
                identity::Candidate {
                    kind,
                    disk_rel: rel.clone(),
                    file_name: base.to_string(),
                    disabled: !enabled,
                }
            })
            .collect()
    }
}

/// List what of a server's `mods/` and `client-mods/` goes to its players:
/// every jar in either, each mod once.
///
/// Read from the folders rather than the records, because disk is the source
/// of truth: a jar dropped in by hand has no record and is still a mod, and a
/// record whose jar is gone is not.
///
/// Every regular file directly inside either folder with a jar's name goes,
/// turned off or not and with a record or without one — jars the admin added
/// by hand, and jars that declare themselves server-side, included. Only what a
/// loader would not load stays: a `.bak` of a jar, a folder of old tools, a
/// link to somewhere else. None of that shows in the admin's Content tab, and
/// the pack must not send what they cannot see. A folder that is itself a link
/// shares nothing, for the reason [`instance::modpack::Walk::Only`] gives.
///
/// A player's game has one mods folder, so a mod the server has in both goes
/// once, from `mods/`: a client mod with the name of a jar in `mods/`, or with
/// the Modrinth or CurseForge project of a mod in `mods/` that goes, stays.
async fn server_mods<'a>(
    run: &Path,
    records: &'a [instance::content::ContentRecord],
) -> Result<ServerMods<'a>> {
    use instance::content::{is_content_file, side_of, strip_disabled, ContentKind};

    type Found = (ContentKind, String, Option<instance::jar::Side>);
    let run_dir = run.to_path_buf();
    let mut found = tokio::task::spawn_blocking(move || -> Result<Vec<Found>> {
        let mut found = Vec::new();
        for kind in [ContentKind::Mod, ContentKind::ClientMod] {
            let dir = run_dir.join(kind.folder());
            match std::fs::symlink_metadata(&dir) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            }
            for entry in std::fs::read_dir(&dir)?.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if is_content_file(&name, kind) {
                    // Only a regular file gets this far, so reading what it
                    // declares follows no link.
                    let declared = instance::jar::read(&entry.path()).and_then(|meta| meta.side);
                    found.push((kind, name, declared));
                }
            }
        }
        Ok(found)
    })
    .await
    .map_err(|e| Error::msg(format!("reading the server's mods panicked: {e}")))??;

    let record_for = |kind: ContentKind, base: &str| {
        records
            .iter()
            .find(|r| r.kind == kind && r.file_name == base)
    };

    // Everything the admin is keeping here drops out before anything else is
    // asked about it, so there is no route by which a pack can carry it: not
    // the index, not a reference, not its bytes, not a note naming where it
    // came from, not the revision, and not the count a player is shown. The
    // walk below is the only thing that puts a file in `carried`, and
    // `carried` is the only thing the writer is given.
    //
    // It also stops being a reason to leave its twin in `client-mods/` behind:
    // the rule there is that one mod reaches a player once, and this one is no
    // longer reaching them at all.
    found.retain(|(kind, name, _)| {
        !record_for(*kind, strip_disabled(name).0).is_some_and(|r| r.keep_on_server)
    });

    let project = |r: &'a instance::content::ContentRecord| {
        (!r.provider.is_empty() && !r.project_id.is_empty())
            .then_some((r.provider.as_str(), r.project_id.as_str()))
    };
    let in_mods: HashSet<&str> = found
        .iter()
        .filter(|(kind, ..)| *kind == ContentKind::Mod)
        .map(|(_, name, _)| strip_disabled(name).0)
        .collect();
    let projects_in_mods: HashSet<(&str, &str)> = in_mods
        .iter()
        .filter_map(|base| record_for(ContentKind::Mod, base).and_then(project))
        .collect();

    let mut mods = ServerMods {
        shared: Vec::new(),
        carried: HashSet::new(),
        sides: HashMap::new(),
    };
    for (kind, name, declared) in &found {
        let base = strip_disabled(name).0;
        // Records follow the files. One whose file is a link, or is gone, names
        // nothing the pack sends, so it must not be indexed or referenced
        // either — or the pack would carry a mod the count left out.
        let record = record_for(*kind, base);
        let twin_in_mods = *kind == ContentKind::ClientMod
            && (in_mods.contains(base)
                || record
                    .and_then(project)
                    .is_some_and(|p| projects_in_mods.contains(&p)));
        if twin_in_mods {
            continue;
        }
        let rel = PathBuf::from(kind.folder()).join(name);
        // The answer alone. A pack has one field for a side and no way to say
        // nobody gave one, which a player's Geode reads back as the server
        // having said nothing — see [`server_notes`].
        let side = side_of(
            *kind,
            record.and_then(|r| r.chosen_side),
            *declared,
            record.and_then(|r| r.side),
        )
        .side;
        mods.sides.insert(rel.clone(), side);
        mods.carried.insert(rel);
        // A mod on disk both on and off is two files and one record, which is
        // indexed or referenced once.
        if let Some(record) = record.filter(|r| !mods.shared.iter().any(|s| std::ptr::eq(*s, *r))) {
            mods.shared.push(record);
        }
    }
    Ok(mods)
}

/// What a server's pack says about each mod it sends.
///
/// Every mod that belongs on one side only is noted with that side, whatever
/// carries it, so a player's Geode knows which mods only the server needs.
/// Every mod the index cannot point at is noted with where to get it, and
/// whether the server had it turned off. A mod for both sides that travels in
/// the index or as bytes has nothing to say.
fn server_notes(mods: &ServerMods, catalogue: &Catalogue) -> Vec<FileNote> {
    use instance::content::ContentKind;
    use instance::jar::Side;

    let mut carried: Vec<&PathBuf> = mods.carried.iter().collect();
    carried.sort();

    let mut out = Vec::new();
    for path in carried {
        let (kind, base, enabled, side) = mods.about(path);
        // A carried path is a file in a folder, so it always has a name. One
        // that somehow does not names no mod anyone could be sent.
        if base.is_empty() {
            continue;
        }
        // A mod on disk both on and off is one mod to a player, and is noted
        // once, as the copy that is on.
        if !enabled && mods.carried.contains(&path.with_file_name(base)) {
            continue;
        }
        let source = source_of(catalogue.travel(kind, base, Carry::SERVER_PACK).as_ref());
        if side == Side::Both && source.is_none() {
            continue;
        }
        let referenced = source.is_some();
        out.push(FileNote {
            // Where a player's game keeps it, which for a client mod is not
            // where the server does.
            path: format!("{}/{base}", ContentKind::Mod.folder()),
            side: (side != Side::Both).then_some(side),
            source,
            // Only a reference needs telling; bytes keep their `.disabled`.
            disabled: !enabled && referenced,
        });
    }
    out
}

/// What a player is shown before importing, without building the pack.
pub async fn server_pack_info(
    paths: &Paths,
    server: &crate::server::Server,
) -> Result<super::host::PackInfo> {
    let (loader, loader_version) = client_loader(server)?;
    let run = paths.server_run(&server.id);
    let mods = server_mods(&run, &server.content).await?;
    let include_config = sends_config(paths, &server.id).await?;
    let forbidden = forbidden_for(paths, &server.id).await?;
    let settings = synced_settings(paths, &server.id).await?;
    let sent = settings_sent(paths, &server.id).await?;
    let revision = server_revision(
        paths, server, &mods, include_config, &forbidden, &settings, &sent.files,
    )
    .await?;
    // The same list the revision describes and the pack carries, so the number
    // a player is shown is the number of files they get — see
    // [`settings_sent`].
    let carried: Vec<&Sent> = if include_config {
        sent.files.iter().filter(|row| row.sent).collect()
    } else {
        Vec::new()
    };
    // Off the files themselves and not off the folder's behaviour, because the
    // admin picks per file: a share that keeps one file and replaces another
    // cannot be summed up by what it chose for `config/`. Nothing of the
    // player's survives unless nothing is replaced, which is the cautious
    // reading of a mixed set and the only one that cannot overpromise.
    let config_kept = !carried.is_empty() && carried.iter().all(|row| row.policy != Policy::Enforce);
    Ok(super::host::PackInfo {
        name: server.name.clone(),
        mc_version: server.mc_version.clone(),
        loader: loader.as_str().to_string(),
        loader_version,
        mods: mods.carried.len(),
        server_mods: mods
            .sides
            .values()
            .filter(|side| **side == instance::jar::Side::Server)
            .count(),
        server_kind: server.kind.as_str().to_string(),
        config_files: carried.len(),
        // The only thing a player asks about those files, answered here
        // rather than by naming the behaviour: a bool cannot fail to read the
        // way a policy name a newer Geode invented would, and a description
        // that would not parse leaves a player unable to check for changes at
        // all.
        config_kept,
        // The other half of the same question, for the one behaviour a single
        // bool could not describe: the files are the player's, *and* what the
        // admin changes later arrives as the settings they changed.
        config_pushed: config_kept && carried.iter().any(|row| row.policy == Policy::Merge),
        revision,
        server_id: server.id.clone(),
        // Read here as well as written into the pack, and on purpose: this is
        // the endpoint a joined instance asks on every Play, so a rename or a
        // move reaches its players with nothing downloaded. The manifest is
        // authority at import — it is in the bytes the player received, and
        // it is the only carrier a `.geodepack` handed over as a file has —
        // and this is authority afterwards, because it is current.
        listing: listing_for(paths, &server.id).await?,
    })
}

/// Whether a server's pack sends its `config/` folder.
///
/// From the share file, read now: an admin who just stopped sending config is
/// not second-guessed by a copy of anything loaded earlier. A server that is
/// not shared, now or ever, sends none.
async fn sends_config(paths: &Paths, id: &str) -> Result<bool> {
    Ok(crate::server::share::load(paths, id)
        .await?
        .is_some_and(|share| share.shared && share.include_config))
}

/// What a server's pack says about the settings files it sends: which paths a
/// player keeps their own changes to, and how far the admin's pushes have got.
///
/// From the share file, read now, for the same reason as [`sends_config`]. A
/// server that is not shared syncs nothing: its pack reaches nobody, and an
/// empty answer is the one that means "every file is replaced", which is what
/// a share from before any of this has always done.
async fn synced_settings(paths: &Paths, id: &str) -> Result<Settings> {
    Ok(crate::server::share::load(paths, id)
        .await?
        .filter(|share| share.shared)
        .map(|share| share.settings)
        .unwrap_or_default())
}

/// How this server should appear in its players' multiplayer list, if the
/// admin has said.
///
/// Read fresh from the share file for the same reason as `sends_config`, and
/// checked again on the way out rather than trusted: the file is on disk, and
/// a listing that would garble a player's list is better dropped here than
/// sent to everybody.
pub(crate) async fn listing_for(paths: &Paths, id: &str) -> Result<Option<Listing>> {
    Ok(crate::server::share::load(paths, id)
        .await?
        .and_then(|share| share.listing)
        .and_then(|listing| listing.checked().ok()))
}

/// The two folders a server's settings files can live in, and the only two a
/// pack carries one out of.
///
/// An allowlist rather than a list of what to keep out, for the reason
/// [`instance::modpack::Walk::Only`] gives: the rest of a server's run
/// directory is `server.properties` with its RCON password in it, the lists of
/// who may play and who is banned, other people's names and addresses in
/// `usercache.json`, the logs, the worlds and the plugins, and a list of names
/// to keep out is only ever one forgotten name from shipping one of them.
/// Nobody looks at this pack before it goes.
const SETTINGS_FOLDERS: [&str; 2] = ["config", instance::modpack::CLIENT_FILES];

/// One settings file on a server's disk, and what its pack does with it.
#[derive(Debug, Clone)]
pub(crate) struct Sent {
    /// Where it is, from the server's run directory.
    pub rel: PathBuf,
    /// Where it lands in a player's instance: the same path, except for a
    /// client file, which has no folder of its own there — see
    /// [`instance::modpack::pack_rel`].
    pub lands: PathBuf,
    /// Read by the admin's picker, which is the app's. The headless host has
    /// no picker, so nothing there asks how big a settings file is.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub size: u64,
    /// Whether this server's pack carries it at all.
    pub sent: bool,
    /// What happens to a player's own changes to it, as chosen for it or for a
    /// folder above it — or [`default_policy`] when nobody has chosen.
    pub policy: Policy,
}

/// One path as every list a player or an admin reads spells it: forward
/// slashes, whatever this machine uses.
///
/// One function because a path is matched by text on the other side — in a
/// revision line, against a pack's settings half, against a row the admin
/// ticked — so two spellings of one file are two files.
fn shown(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// What a settings file nobody chose a behaviour for does.
///
/// [`Policy::Enforce`] under `config/`, which is what sending a config folder
/// has always done and what every share made before there was anything to
/// choose goes on doing.
///
/// [`Policy::ShipOnce`] for a client file, and that is not a matter of taste.
/// Minecraft rewrites `options.txt` in full on every exit, in its own key
/// order, so a copy replaced on every update would move a player's keybinds
/// under them between one session and the next — the worst thing this feature
/// could do to somebody. A client file nobody chose for is sent to a player
/// who has none and is theirs from then on.
fn default_policy(rel: &Path) -> Policy {
    if rel.starts_with(instance::modpack::CLIENT_FILES) {
        Policy::ShipOnce
    } else {
        Policy::Enforce
    }
}

/// One path the admin picked that no pack of this server's will carry, and
/// what to tell them about it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Refused {
    pub path: String,
    pub why: String,
}

/// Every settings file a server has, and every pick that will never be one.
pub(crate) struct Sends {
    pub files: Vec<Sent>,
    /// Shown to the admin beside their picks, so only the app reads it.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub refused: Vec<Refused>,
    /// Whether the admin has picked anything at all. False means this server's
    /// `config/` folder whole, a file a mod writes for the first time next
    /// week included — what sending a config folder has always meant.
    ///
    /// The picker's question, so the app asks it and the host does not.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub whole_config: bool,
}

/// Which of a server's own settings files its pack carries, and which of the
/// admin's picks it will not.
///
/// One function because four things count on this set and must never disagree.
/// [`server_revision`] describes these files, so they are what a player's
/// update is triggered by; [`server_pack_info`] counts them, so they are what
/// the join preview promises is coming; `settings::push` reviews them, so they
/// are what the admin can push a setting out of; and [`export_server`] walks
/// exactly this list, so they are what the pack holds. A second walk that
/// drifted would tell a player a number the pack does not carry, or offer the
/// admin a setting inside a file nobody is sent.
///
/// What the admin picked is held to [`SETTINGS_FOLDERS`] here and not where it
/// was written, because a bug in a picker must not be able to ship
/// `ops.json`. The picks are a list in a file on disk; this is the code that
/// decides, and it is the same code for the listing the admin reads and the
/// pack a stranger downloads.
///
/// The caller decides whether settings files are sent at all —
/// [`sends_config`] — so this answers only "which files", never "whether".
pub(crate) async fn settings_sent(paths: &Paths, id: &str) -> Result<Sends> {
    let (picked, settings) = match crate::server::share::load(paths, id).await? {
        Some(share) => (share.picked, share.settings),
        None => (Vec::new(), Settings::default()),
    };
    let run = paths.server_run(id);

    let mut files = Vec::new();
    for (rel, size) in settings_candidates(&run).await? {
        files.push(Sent {
            sent: picked_out(&picked, &rel),
            policy: settings
                .covering(&rel)
                .map_or_else(|| default_policy(&rel), |named| named.policy),
            lands: instance::modpack::pack_rel(&rel),
            rel,
            size,
        });
    }

    let mut refused = Vec::new();
    for path in &picked {
        if let Some(why) = refusal(&run, Path::new(path)).await {
            refused.push(Refused {
                path: path.clone(),
                why,
            });
        }
    }
    Ok(Sends {
        files,
        refused,
        whole_config: picked.is_empty(),
    })
}

/// Whether the admin's picks carry this file.
///
/// No picks means the whole `config/` folder, which is what sending a config
/// folder has always meant and what every share made before there was a
/// picker goes on doing — files a mod writes for the first time next week
/// included. A pick names a file or a folder above one, matched the way a
/// behaviour chosen for a folder is matched, because they are the same kind of
/// claim on the same kind of path.
fn picked_out(picked: &[String], rel: &Path) -> bool {
    use super::manifest::covers;

    if picked.is_empty() {
        return covers(Path::new("config"), rel);
    }
    picked
        .iter()
        .any(|path| covers(Path::new(path), rel) && in_settings_folder(Path::new(path)))
}

fn in_settings_folder(rel: &Path) -> bool {
    rel.components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
        .is_some_and(|top| SETTINGS_FOLDERS.contains(&top))
}

/// Why a path can never be a settings file of this server's, however the disk
/// looks — or `None` when it could be.
///
/// The two rules that can never come true, kept apart from the rest because
/// they are the two worth refusing at the moment the admin asks for them:
/// [`crate::commands::set_server_settings_picks`] turns these into the error
/// on their screen, so a path the pack could never name is never written down
/// as a pick at all. The others are facts about the disk right now, and belong
/// on a row rather than in a refusal to save.
pub(crate) fn never_nameable(rel: &Path) -> Option<String> {
    if !crate::paths::is_safe_relative(rel) {
        return Some("That is not a path inside this server's own folder.".to_string());
    }
    if !in_settings_folder(rel) {
        return Some(format!(
            "Players are sent this server's {} folder and the {} folder beside it, and nothing \
             else — the rest of a server's folder holds its own password, the lists of who may \
             play, and its worlds.",
            SETTINGS_FOLDERS[0], SETTINGS_FOLDERS[1]
        ));
    }
    None
}

/// Why a path the admin picked will never travel, or `None` when it can.
///
/// Said as a reason rather than left out of a listing, because a row that
/// silently does nothing is how an admin concludes sharing is broken — and
/// because two of these are worth knowing: a pick is refused for what it is,
/// not for where it sits, so a file that was a settings file last week and is
/// a link today stops travelling and says so.
async fn refusal(run: &Path, rel: &Path) -> Option<String> {
    if let Some(why) = never_nameable(rel) {
        return Some(why);
    }
    // Every step of the way, because an allowlist of folders says nothing
    // about where a link inside one of them points — the same reason the pack's
    // own walk refuses one wherever it sits. `symlink_metadata` is the link
    // itself rather than what it points at, which is what makes that check the
    // one that holds.
    let steps = rel.components().count();
    let mut so_far = run.to_path_buf();
    for (nth, part) in rel.components().enumerate() {
        so_far.push(part);
        let Ok(meta) = tokio::fs::symlink_metadata(&so_far).await else {
            return Some("There is no such file on this server any more.".to_string());
        };
        if meta.is_symlink() {
            return Some(
                "Geode sends nothing through a link: a link inside a folder it sends says \
                 nothing about where it points."
                    .to_string(),
            );
        }
        if nth + 1 == steps {
            if !meta.is_file() && !meta.is_dir() {
                return Some("That is not an ordinary file, so nothing of it travels.".to_string());
            }
        } else if !meta.is_dir() {
            return Some("There is no such folder on this server any more.".to_string());
        }
    }
    None
}

/// Every regular file in the folders a server's settings files live in, with
/// its size, found the way the pack's own walk finds them: through no link,
/// and nothing at all from a folder that is itself one.
async fn settings_candidates(run: &Path) -> Result<Vec<(PathBuf, u64)>> {
    let run = run.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        for folder in SETTINGS_FOLDERS {
            let top = run.join(folder);
            // A folder that is itself a link is not sent, so it is nothing
            // here either.
            if !std::fs::symlink_metadata(&top).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            let mut stack = vec![top];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    match entry.file_type() {
                        Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                        Ok(kind) if kind.is_file() => {
                            let Ok(rel) = entry.path().strip_prefix(&run).map(Path::to_path_buf)
                            else {
                                continue;
                            };
                            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                            files.push((rel, size));
                        }
                        _ => {}
                    }
                }
            }
        }
        files.sort();
        files
    })
    .await
    .map_err(|e| Error::msg(format!("reading the server's settings files panicked: {e}")))
}

/// One row of the picker: a settings file, what it would do, and whether it
/// can do the one thing a format has to be readable for.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsChoice {
    /// Where it is, from the server's run directory.
    pub path: String,
    /// Where it lands in a player's instance.
    pub lands: String,
    pub size: u64,
    pub sent: bool,
    /// What happens to a player's own changes to it — **as it would actually
    /// be applied**, which for a file kept in sync whose format Geode cannot
    /// read one setting at a time is that it is sent once.
    ///
    /// Not the choice as stored, on purpose. Keeping a file in sync is
    /// splicing in the settings the admin pushes, and nothing can be pushed
    /// out of a file no setting can be found inside — so a row under a label
    /// reading "only the settings I change" that quietly did something else is
    /// the one thing this listing must not show.
    pub policy: Policy,
    /// Whether keeping this file in sync is a behaviour it can honour.
    pub can_sync: bool,
}

/// What the admin picks from: every settings file this server has, and every
/// pick it will not carry.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPicker {
    pub files: Vec<SettingsChoice>,
    pub refused: Vec<Refused>,
    /// Whether the whole `config/` folder travels, a file a mod writes for the
    /// first time next week included. True until the admin picks anything.
    pub whole_config: bool,
}

/// The listing the admin picks from, which is the same set of files the pack
/// carries — [`settings_sent`] decides both.
///
/// This reads each file, which the pack's own walk does not have to: a format
/// is what a file holds and never what it is called, so saying whether a row
/// can be kept in sync means looking inside it. That is work for a listing
/// somebody asked for and not for a pack a stranger downloads, which is why it
/// is here and not in [`settings_sent`].
///
/// Only the app calls this — it is what fills the admin's picker — so in a
/// headless build the function is compiled and never reached. It stays
/// compiled rather than gated out so its tests run in both builds, which is
/// the point of the rule this file lives by: what a pack sends is decided in
/// one place, and that place is checked wherever it is built.
#[cfg_attr(not(feature = "desktop"), allow(dead_code))]
pub(crate) async fn settings_picker(paths: &Paths, id: &str) -> Result<SettingsPicker> {
    let sends = settings_sent(paths, id).await?;
    let run = paths.server_run(id);

    let mut files = Vec::new();
    for row in sends.files {
        let can_sync = match tokio::fs::read(run.join(&row.rel)).await {
            Ok(bytes) => crate::settings::sniff(&bytes).is_some(),
            Err(_) => false,
        };
        files.push(SettingsChoice {
            path: shown(&row.rel),
            lands: shown(&row.lands),
            size: row.size,
            sent: row.sent,
            policy: match row.policy {
                Policy::Merge if !can_sync => Policy::ShipOnce,
                policy => policy,
            },
            can_sync,
        });
    }
    Ok(SettingsPicker {
        files,
        refused: sends.refused,
        whole_config: sends.whole_config,
    })
}

/// One row of the picker as the admin's choice comes back: a settings file,
/// and what they chose to happen to a player's own changes to it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pick {
    /// The file or folder, from the server's run directory, spelled as
    /// [`SettingsChoice::path`] spelled it.
    pub path: String,
    pub policy: Policy,
}

/// The mods this server does not allow its players to run, if the admin has
/// named any.
///
/// Read fresh from the share file for the same reason as `sends_config`, and
/// checked again on the way out rather than trusted: an entry a hand-edited
/// file holds that names anything but a mod is dropped here rather than sent to
/// everybody. A server that is not shared forbids nothing, because it sends no
/// pack to carry a rule in.
pub(crate) async fn forbidden_for(paths: &Paths, id: &str) -> Result<Vec<Forbidden>> {
    Ok(crate::server::share::load(paths, id)
        .await?
        .map(|share| share.forbidden)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry.checked().ok())
        .collect())
}

/// The SHA-1 of a file, or nothing when it is no longer there to read.
///
/// A running server rewrites files under it while it runs — a mod saving its
/// config, a database writing and renaming its own files — so one listed a
/// moment ago can be gone by the time it is hashed. Leaving it out of the
/// revision costs at most one extra update later; failing instead would make
/// the server's description unreadable, and a player pressing Play would be
/// told it could not be reached.
async fn sha1_if_there(path: &Path) -> Result<Option<String>> {
    match crate::net::sha1_file(path).await {
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        other => other.map(Some),
    }
}

/// One value for everything a server's players receive, which changes exactly
/// when that does.
///
/// Worked out from what the files hold, never from when they were written. The
/// setup comes first — the Minecraft version, the client loader and its
/// version, and whether config is sent — and then a line for each mod the pack
/// carries: its path as it is on disk, `.disabled` and all, the side it is sent
/// with, and what it is. A mod with a record naming its provider, project and
/// version is that and its size, so a pack of a few hundred Modrinth mods is
/// described without hashing one of them; any other is the SHA-1 of its bytes,
/// since nothing else says which build of a jar added by hand it is. When
/// config is sent, every regular file under `config/` is its path and SHA-1,
/// found the way the pack's own walk finds them, through no link.
///
/// The behaviour chosen for each path the server syncs is a line of its own,
/// because a change to it changes what players get and nothing else here would
/// say so: it lives in `share.json`, which nothing in this walk reads. A file
/// the admin lets players keep their own copy of is named without its hash —
/// its bytes are not what a player receives, so hashing them would send
/// everybody a pack over an edit that reaches none of them, which is what a
/// mod rewriting its own config on every boot would otherwise do. Named, and
/// not left out, because a player who has no copy of it is still sent one, and
/// this is the only thing that tells their Geode to come and look.
///
/// A mod the server does not allow is a line too, with the admin's reason: it
/// reaches the player's game as surely as a jar does, and changing the reason
/// changes what they are shown.
///
/// Left out on purpose: the server's name, notes and artwork, which an update
/// does not apply to a player's instance; its worlds, logs and properties,
/// which never travel; and every modification time, so a restart that writes
/// the same configs again does not send every player an update.
///
/// A file that is gone by the time it is read is left out rather than failing
/// the whole description — see [`sha1_if_there`].
async fn server_revision(
    paths: &Paths,
    server: &crate::server::Server,
    mods: &ServerMods<'_>,
    include_config: bool,
    forbidden: &[Forbidden],
    settings: &Settings,
    sent: &[Sent],
) -> Result<String> {
    use instance::jar::Side;
    use sha2::{Digest, Sha256};

    let (loader, loader_version) = client_loader(server)?;
    let run = paths.server_run(&server.id);

    let mut lines = Vec::new();
    for rel in &mods.carried {
        let (kind, base, _, side) = mods.about(rel);
        let side = match side {
            Side::Client => "client",
            Side::Server => "server",
            Side::Both => "both",
        };
        let full = run.join(rel);
        // Only a record that names all three says which build this is; any
        // other file is hashed instead.
        let record = mods.shared_record_for(kind, base).filter(|r| {
            !r.provider.is_empty() && !r.project_id.is_empty() && !r.version_id.is_empty()
        });
        let identity = match record {
            Some(r) => match tokio::fs::symlink_metadata(&full).await {
                Ok(meta) => format!(
                    "{}:{}:{}:{}",
                    r.provider,
                    r.project_id,
                    r.version_id,
                    meta.len()
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            },
            None => match sha1_if_there(&full).await? {
                Some(hash) => format!("sha1:{hash}"),
                None => continue,
            },
        };
        lines.push(format!("mod {} {side} {identity}", shown(rel)));
    }

    for rule in forbidden {
        lines.push(format!(
            "forbid {}:{}:{} {}",
            rule.provider, rule.project_id, rule.mod_id, rule.reason
        ));
    }

    if include_config {
        // Only a path whose files actually travel. A behaviour chosen for a
        // folder this pack does not send changes nothing for anybody, and a
        // revision that moved for it would send every player a pack over a
        // choice none of them can see.
        for named in &settings.paths {
            lines.push(format!(
                "settings {} {} {}",
                named.path,
                named.policy.as_str(),
                named.from
            ));
        }
        // Each setting the admin owns, by the push it went out at. The push
        // is enough on its own — a value cannot change without one, since
        // every push takes the next number and stamps it on every entry it
        // writes — and it is what a player compares against what they have
        // caught up with. Only a path whose files are kept in sync: a setting
        // inside a file players are sent whole, or sent once, reaches nobody,
        // and a revision that moved for it would send every player a pack for
        // nothing.
        //
        // The key is written as the list it is, quoted and escaped, so two
        // different keys cannot be spelled the same way here.
        for entry in &settings.overrides {
            if settings.policy_for(Path::new(&entry.path)) != Policy::Merge {
                continue;
            }
            lines.push(format!(
                "setting {} {} {:?}",
                entry.path, entry.at, entry.key
            ));
        }
        // Only the files the pack carries. A settings file on disk the admin
        // did not pick reaches nobody, so a revision that moved for it would
        // send every player a pack over a file none of them will ever see —
        // and a file that stops being picked loses its line here, which is
        // how they are told it has stopped coming.
        for row in sent.iter().filter(|row| row.sent) {
            let rel = row.rel.as_path();
            if row.policy != Policy::Enforce {
                // Its bytes are not what a player receives, so they are not
                // here — but *that it exists* is: a player with no copy of it
                // is sent one, and nothing else in this walk would ever say it
                // had appeared. A line with no hash in it is the difference
                // between announcing a new settings file and sending everybody
                // a pack every time a mod rewrites its own config.
                lines.push(format!("config-file {} sent", shown(rel)));
                continue;
            }
            let Some(hash) = sha1_if_there(&run.join(rel)).await? else {
                continue;
            };
            lines.push(format!("config-file {} sha1:{hash}", shown(rel)));
        }
    }
    lines.sort();

    let header = [
        "geode-revision 1".to_string(),
        format!("minecraft {}", server.mc_version),
        format!("loader {} {loader_version}", loader.as_str()),
        format!("config {include_config}"),
    ];
    let all: Vec<String> = header.into_iter().chain(lines).collect();
    Ok(hex::encode(Sha256::digest(all.join("\n").as_bytes())))
}

/// Write the pack a server hands to its players.
///
/// The same writer as an instance's pack, walking the server's run directory
/// by **allowlist** — `mods/`, `client-mods/`, and `config/` only when the
/// server's share says so — never by denylist. A server's run directory holds
/// `server.properties` with its RCON password, `ops.json`, the whitelist, the
/// ban lists, every world and a few hundred megabytes of libraries; a denylist
/// is one forgotten entry from shipping any of them.
///
/// From `mods/` and `client-mods/` it takes only the jars sitting directly
/// inside them, and from any folder it takes nothing through a link, so an
/// allowed folder name cannot be a way to reach a file outside it. Unlike an
/// instance's pack, which follows links, nobody reviews this one before it
/// goes.
///
/// Every jar in `mods/` goes: one from a provider as a reference, and the rest
/// — jars added by hand, and jars that declare themselves server-side — as
/// copies. Client mods go the same way, as mods in the player's `mods/`, and
/// each mod's side goes with it. Only links, subfolders and files that are not
/// jars stay. The manifest is `Kind::Server`, so importing it claims every file
/// for the server, and it carries the revision of what this very pack holds, so
/// a player keeps the revision of what they received.
pub async fn export_server(
    paths: &Paths,
    server: &crate::server::Server,
    dest: PathBuf,
    progress: &Progress,
) -> Result<()> {
    let (loader, loader_version) = client_loader(server)?;
    let run = paths.server_run(&server.id);
    let mods = server_mods(&run, &server.content).await?;
    let include_config = sends_config(paths, &server.id).await?;
    let forbidden = forbidden_for(paths, &server.id).await?;
    let settings = synced_settings(paths, &server.id).await?;
    let sent = settings_sent(paths, &server.id).await?;
    // Worked out from the very files this pack is about to be written from,
    // and before it is written: a file that changes while the pack is built
    // makes the revision it carries the older of the two, which the next check
    // finds changed. The other way round would be a change nobody ever sees.
    let revision = server_revision(
        paths, server, &mods, include_config, &forbidden, &settings, &sent.files,
    )
    .await?;

    // Records only, and so no client and no network. This runs inside the share
    // host's download handler under `MAX_BUILDS`, and `server_revision` is
    // computed from the same records both here and in `server_pack_info`, which
    // several tests assert must agree. A lookup that succeeded in one and not
    // the other would advertise a revision the pack does not have and push an
    // update to every player — and every online/offline flip would do it again.
    // A server gains references at `link_untracked_server_content`, where
    // records are saved.
    let catalogue =
        identity::identify(&run, mods.candidates(), &mods.shared, Ask::Nobody, progress).await?;

    let media = paths.server_media(&server.id);
    let files = server_notes(&mods, &catalogue);
    let manifest = Manifest {
        geode_format: format_for(&files),
        kind: Kind::Server,
        profile: Profile {
            name: server.name.clone(),
            notes: server.notes.clone(),
            server_kind: Some(server.kind.as_str().to_string()),
            // No memory and no Java arguments. A server's are sized for its
            // own heap, and carried across they would replace the player's.
            ..Default::default()
        },
        icon: artwork(&media, server.icon.as_ref()),
        banner: artwork(&media, server.banner.as_ref()),
        // Not the modpack the server was built from, even when it was: the
        // server's mods are the admin's to change since, and naming the
        // published pack here would make a joined instance show — and Browse
        // treat — it as that pack when it is not. A joined instance says where
        // it came from through `joined` instead.
        source: None,
        files,
        revision: Some(revision),
        // Carried so a player who imports this pack from a file — with no
        // server to ask afterwards — still gets the row. A pack served over
        // the wire is kept in step by the description instead.
        listing: listing_for(paths, &server.id).await?,
        // Only when the folder those paths are in actually travels — and then
        // always, with the choice nobody made spelled out, so the player's
        // Geode has one rule for every file it is sent. See
        // [`Settings::spelling_out`]; it is also what tells a settings file
        // apart from every other file in the pack, which is what makes one
        // that cannot be written a line in a report instead of a failed
        // update.
        // Spelled as the paths the files land at, because that is what the
        // player's Geode matches against their own disk: a server's
        // `client-files/options.txt` is their `options.txt`, at the top of
        // the game directory. See [`Settings::for_players`].
        settings: include_config.then(|| {
            let mut half = settings.spelling_out("config").for_players();
            // And every file this pack carries that the half still says
            // nothing about — a client file, which lands where no folder can
            // name it. Left unsaid it would be written whole on every update,
            // which is the one thing `options.txt` must never be.
            for row in sent.files.iter().filter(|row| row.sent) {
                half.spell_out(&shown(&row.lands), row.policy);
            }
            half
        }),
        forbidden,
    };
    let extras = extras_from(
        &media,
        server.icon.as_ref(),
        server.banner.as_ref(),
        &manifest,
    )
    .await?;

    // Mods and client mods by name, exactly the files `server_mods` read and
    // counted, and the settings files the admin picked when the share says to
    // send any — exactly the list `settings_sent` named, so what this pack
    // holds is what the revision described and what the admin was shown.
    // Nothing follows a link.
    let mut carried = mods.carried;
    if include_config {
        carried.extend(
            sent.files
                .into_iter()
                .filter(|row| row.sent)
                .map(|row| row.rel),
        );
    }

    let input = instance::modpack::PackInput {
        root: run,
        name: &server.name,
        notes: &server.notes,
        version: server
            .pack
            .as_ref()
            .map(|p| p.version.clone())
            .unwrap_or_else(|| "1.0.0".into()),
        mc_version: &server.mc_version,
        loader,
        loader_version: &loader_version,
        walk: instance::modpack::Walk::Only { files: carried },
    };
    let half = instance::modpack::GeodeHalf {
        extras,
        referenced: referenced(&manifest),
        carry: Carry::SERVER_PACK,
    };
    // The report is for somebody choosing what to send. Nobody is here: this
    // pack is built on demand for whoever presented the share code.
    instance::modpack::write_mrpack(input, dest, half, &catalogue, progress)
        .await
        .map(|_| ())
}

/// `My Pack 1.2` becomes `My Pack 1.2.geodepack`.
pub fn file_name(stem: &str) -> String {
    let cleaned: String = stem
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '-' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    let stem = if trimmed.is_empty() { "pack" } else { trimmed };
    format!("{stem}.{EXTENSION}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::jar::tests::fabric_jar;
    use crate::instance::InstanceOverrides;
    use crate::paths::Scratch;

    fn manifest() -> Manifest {
        Manifest {
            geode_format: FORMAT,
            kind: Kind::Instance,
            profile: Profile {
                name: "Shared Pack".into(),
                group: Some("Friends".into()),
                notes: "read me".into(),
                memory_mb: Some(6144),
                java_args: Some("-XX:+UseG1GC".into()),
                java_path_never_shared: (),
                width: Some(1600),
                height: Some(900),
                fullscreen: Some(false),
                server_kind: None,
            },
            icon: None,
            banner: None,
            source: None,
            files: Vec::new(),
            revision: None,
            listing: None,
            settings: None,
            forbidden: Vec::new(),
        }
    }

    fn instance() -> Instance {
        let mut i = Instance::new(
            "Local name".into(),
            "1.21.1".into(),
            crate::meta::Loader::Fabric,
            "0.16.0".into(),
        );
        i.overrides = InstanceOverrides {
            memory_mb: Some(2048),
            java_path: Some("/usr/lib/jvm/java-21/bin/java".into()),
            ..Default::default()
        };
        i
    }

    #[test]
    fn importing_applies_what_the_pack_carried() {
        let mut got = instance();
        apply(&manifest(), &mut got);
        assert_eq!(got.name, "Shared Pack");
        assert_eq!(got.group.as_deref(), Some("Friends"));
        assert_eq!(got.notes, "read me");
        assert_eq!(got.overrides.memory_mb, Some(6144));
        assert_eq!(got.overrides.java_args.as_deref(), Some("-XX:+UseG1GC"));
        assert_eq!(got.overrides.width, Some(1600));
    }

    /// The other half of the same rule: what the pack did not carry must be
    /// left alone. Writing a `None` over a setting the importer chose would
    /// make "this pack says nothing about memory" and "this pack wants the
    /// default" the same thing, and they are not.
    #[test]
    fn importing_leaves_alone_what_the_pack_did_not_carry() {
        let mut m = manifest();
        m.profile.memory_mb = None;
        m.profile.java_args = None;
        m.profile.group = None;
        m.profile.notes = String::new();

        let mut got = instance();
        got.notes = "my own notes".into();
        apply(&m, &mut got);

        assert_eq!(got.overrides.memory_mb, Some(2048), "kept the local size");
        assert_eq!(got.notes, "my own notes");
        assert!(got.group.is_none());
    }

    /// A server's launcher settings are not the player's. Its heap is sized
    /// for its own machine, its window is nothing it knows about, and Java
    /// arguments are a way to run code the join preview never mentions — so
    /// all five are refused, and not one of them is in the preview a player
    /// agreed to. Geode never writes them into a server pack, so one that has
    /// them is not to be believed.
    #[test]
    fn a_server_pack_never_sets_a_launcher_setting() {
        let mut server = manifest();
        server.kind = Kind::Server;
        server.profile.java_args = Some("-javaagent:mods/agent.jar".into());
        server.profile.memory_mb = Some(256);
        server.profile.fullscreen = Some(true);

        let mut joined = instance();
        joined.overrides.width = Some(1280);
        joined.overrides.height = Some(720);
        joined.overrides.fullscreen = Some(false);
        apply(&server, &mut joined);
        assert_eq!(joined.overrides.memory_mb, Some(2048), "kept the player's size");
        assert_eq!(joined.overrides.java_args, None);
        assert_eq!(joined.overrides.width, Some(1280), "kept the player's window");
        assert_eq!(joined.overrides.height, Some(720));
        assert_eq!(joined.overrides.fullscreen, Some(false));
        // The rest of the profile still arrives.
        assert_eq!(joined.name, "Shared Pack");
        assert_eq!(joined.notes, "read me");

        // A person's pack is still trusted with all five.
        let mut personal = server.clone();
        personal.kind = Kind::Instance;
        let mut got = instance();
        got.overrides.width = Some(1280);
        got.overrides.height = Some(720);
        got.overrides.fullscreen = Some(false);
        apply(&personal, &mut got);
        assert_eq!(got.overrides.memory_mb, Some(256));
        assert_eq!(got.overrides.java_args.as_deref(), Some("-javaagent:mods/agent.jar"));
        assert_eq!(got.overrides.width, Some(1600));
        assert_eq!(got.overrides.height, Some(900));
        assert_eq!(got.overrides.fullscreen, Some(true));
    }

    /// A Java path is the author's machine, not the pack's. It is never
    /// exported, so importing can never point somebody's launcher at a
    /// directory that does not exist on their disk.
    #[test]
    fn importing_never_touches_the_java_path() {
        let mut got = instance();
        let before = got.overrides.java_path.clone();
        apply(&manifest(), &mut got);
        assert_eq!(got.overrides.java_path, before);
    }

    /// The invariant the whole format rests on: everything Geode adds lives
    /// under `geode/`, so deleting that directory leaves a `.mrpack` other
    /// launchers still open correctly. If the Geode half could write anywhere
    /// else it could overwrite the index, and the pack would stop being a
    /// modpack the moment it gained a Geode feature.
    #[tokio::test]
    async fn everything_geode_adds_stays_inside_its_own_directory() {
        let scratch = Scratch::new("extras");
        let paths = scratch.paths.clone();
        let mut inst = instance();
        inst.icon = Some("icon.png".into());
        inst.banner = Some("banner.jpg".into());

        let mut m = manifest();
        m.icon = Some(media_entry("icon.png"));
        m.banner = Some(media_entry("banner.jpg"));

        let extras = extras_for(&paths, &inst, &m).await.unwrap();
        assert!(!extras.is_empty());
        for (name, _) in &extras {
            assert!(
                name.starts_with("geode/"),
                "{name} is written outside geode/"
            );
        }
        assert!(extras.iter().all(|(n, _)| n != "modrinth.index.json"));
    }

    #[test]
    fn artwork_keeps_its_own_format_rather_than_being_renamed_to_png() {
        // A jpg banner stays a jpg; calling it .png would leave a file no
        // decoder opens by name.
        assert_eq!(media_entry("banner-93ab.jpg"), "geode/media/banner.jpg");
        assert_eq!(media_entry("icon-12cd.png"), "geode/media/icon.png");
    }

    // -----------------------------------------------------------------------
    // Referencing rather than copying
    // -----------------------------------------------------------------------

    use crate::instance::content::{ContentKind, ContentRecord};

    fn record(provider: &str, file_name: &str, kind: ContentKind) -> ContentRecord {
        ContentRecord {
            file_name: file_name.into(),
            kind,
            provider: provider.into(),
            project_id: if provider.is_empty() { String::new() } else { "proj-1".into() },
            version_id: if provider.is_empty() { String::new() } else { "ver-1".into() },
            name: file_name.into(),
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

    /// An instance on disk, with each named file actually written.
    async fn fixture(tag: &str, files: &[(&str, &str)]) -> (Scratch, Instance) {
        let scratch = Scratch::new(&format!("ref-{tag}"));
        let mut inst = instance();
        inst.id = format!("inst-{tag}");
        let game = scratch.paths.game_dir(&inst.id);
        let _ = tokio::fs::remove_dir_all(&game).await;
        for (dir, name) in files {
            let d = game.join(dir);
            tokio::fs::create_dir_all(&d).await.unwrap();
            // Real zip bytes would let `jar::read` find a side; empty is fine
            // here, because every assertion below is about paths, not sides.
            tokio::fs::write(d.join(name), b"not really a jar").await.unwrap();
        }
        (scratch, inst)
    }

    /// The catalogue an export builds when nothing is asked: the records are
    /// the only answer, which is what every export had before a matcher
    /// existed, and so is the baseline these tests measure against.
    async fn from_records(paths: &Paths, inst: &Instance) -> Catalogue {
        identity::for_instance(Ask::Nobody, paths, inst, &Progress::silent("t"))
            .await
            .unwrap()
    }

    /// Everything inside an exported pack, by entry name.
    fn entries(archive: &Path) -> Vec<String> {
        let file = std::fs::File::open(archive).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect()
    }

    /// The point of the whole format: a jar that came from somewhere is named,
    /// not copied. Sending the bytes is what makes a shared pack an act of
    /// redistribution rather than a description of one.
    #[tokio::test]
    async fn a_provider_backed_jar_is_referenced_instead_of_embedded() {
        let (scratch, mut inst) =
            fixture("cf", &[("mods", "create.jar"), ("config", "create.toml")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        let manifest = manifest_for(&paths, &inst, &Selection::default(), &from_records(&paths, &inst).await).await;
        let note = manifest.note("mods/create.jar").expect("a note was written");
        assert_eq!(
            note.source,
            Some(SourceRef {
                provider: "curseforge".into(),
                project_id: "proj-1".into(),
                version_id: "ver-1".into(),
            })
        );

        let dest = paths.root.join("out.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t")).await.unwrap();
        let names = entries(&dest);

        assert!(
            !names.iter().any(|n| n.contains("create.jar")),
            "the jar was embedded anyway: {names:?}"
        );
        // The config is the author's own work and has no link to give, so it
        // still travels as bytes. That is what `overrides/` is for.
        assert!(names.contains(&"overrides/config/create.toml".to_string()), "{names:?}");
    }

    /// The same jar, turned off.
    ///
    /// Records are keyed by the enabled name while disk carries `.disabled`, so
    /// a naive path comparison misses every disabled file — and misses it in
    /// the worst direction, writing the jar into `overrides/` while the
    /// manifest says it was left out. Both halves wrong at once.
    #[tokio::test]
    async fn a_disabled_provider_backed_jar_is_not_embedded_either() {
        let (scratch, mut inst) = fixture("cfoff", &[("mods", "create.jar.disabled")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        let manifest = manifest_for(&paths, &inst, &Selection::default(), &from_records(&paths, &inst).await).await;
        assert!(
            manifest.note("mods/create.jar").is_some_and(|n| n.source.is_some()),
            "a disabled file still has a source: {:?}",
            manifest.files
        );

        let dest = paths.root.join("off.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t")).await.unwrap();
        let names = entries(&dest);
        assert!(
            !names.iter().any(|n| n.contains("create.jar")),
            "the disabled jar was embedded: {names:?}"
        );
    }

    /// A note is this half's channel for naming a source, so a group the
    /// sharer unticked must have none. A note naming a mod whose bytes stayed
    /// at home is the recipient fetching the very file that was excluded —
    /// which is worse than leaving it out, because the index and `overrides/`
    /// both honoured the choice.
    #[tokio::test]
    async fn a_group_left_out_is_named_in_no_note() {
        let (scratch, mut inst) =
            fixture("left-out", &[("mods", "create.jar"), ("config", "create.toml")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];
        let selection = Selection::of(vec![instance::export::Group::Config]);

        let manifest =
            manifest_for(&paths, &inst, &selection, &from_records(&paths, &inst).await).await;
        assert!(
            manifest.note("mods/create.jar").is_none(),
            "a file the pack was asked not to carry was named anyway: {:?}",
            manifest.files
        );

        let dest = paths.root.join("left-out.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &selection, &Progress::silent("t"))
            .await
            .unwrap();
        let names = entries(&dest);
        assert!(!names.iter().any(|n| n.contains("create.jar")), "{names:?}");
        assert!(names.contains(&"overrides/config/create.toml".to_string()), "{names:?}");
    }

    /// A jar somebody dragged in has no link to give, so it is still carried.
    ///
    /// This is the case the format's own rule names, and the tempting mistake
    /// is to drop it for symmetry — which would silently lose a mod the author
    /// wrote themselves, with nothing anywhere saying where it went.
    #[tokio::test]
    async fn a_jar_with_no_provenance_is_still_embedded() {
        let (scratch, mut inst) = fixture(
            "hand",
            &[("mods", "homemade.jar"), ("mods", "no-record-at-all.jar")],
        )
        .await;
        let paths = scratch.paths.clone();
        // Two ways a jar has no link to give: a record with nothing in it, and
        // — the ordinary case — a file somebody dropped in, which Geode never
        // writes a record for at all.
        inst.content = vec![record("", "homemade.jar", ContentKind::Mod)];

        let manifest = manifest_for(&paths, &inst, &Selection::default(), &from_records(&paths, &inst).await).await;
        for path in ["mods/homemade.jar", "mods/no-record-at-all.jar"] {
            assert!(
                manifest.note(path).is_none_or(|n| n.source.is_none()),
                "{path} must not claim a source"
            );
        }

        let dest = paths.root.join("hand.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t")).await.unwrap();
        let names = entries(&dest);
        for path in ["mods/homemade.jar", "mods/no-record-at-all.jar"] {
            assert!(
                names.contains(&format!("overrides/{path}")),
                "{path} was dropped from the pack: {names:?}"
            );
        }
    }

    /// Disk is the source of truth; a record is only provenance. Deleting a
    /// file outside the launcher leaves its record behind forever, and a
    /// reference written from that record alone would promise the recipient a
    /// mod the sharer does not have.
    #[tokio::test]
    async fn a_record_whose_file_is_gone_is_not_referenced() {
        let (scratch, mut inst) = fixture("stale", &[("mods", "present.jar")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![
            record("curseforge", "present.jar", ContentKind::Mod),
            record("curseforge", "deleted-by-hand.jar", ContentKind::Mod),
        ];

        let manifest = manifest_for(&paths, &inst, &Selection::default(), &from_records(&paths, &inst).await).await;
        assert!(manifest.note("mods/present.jar").is_some());
        assert!(
            manifest.note("mods/deleted-by-hand.jar").is_none(),
            "referenced a file that is not there: {:?}",
            manifest.files
        );
    }

    /// The index already says where a Modrinth file comes from. Saying it again
    /// here is how the two halves come to disagree.
    #[tokio::test]
    async fn modrinth_content_is_never_given_a_source() {
        let (scratch, mut inst) = fixture("mr", &[("mods", "sodium.jar")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("modrinth", "sodium.jar", ContentKind::Mod)];

        let manifest = manifest_for(&paths, &inst, &Selection::default(), &from_records(&paths, &inst).await).await;
        assert!(manifest.files.iter().all(|n| n.source.is_none()), "{:?}", manifest.files);
    }

    /// The invariant that keeps a pack honest: a file is left out of
    /// `overrides/` exactly when the manifest carries a reference for it. If
    /// these two could drift, a pack would either redistribute a jar it claimed
    /// not to, or omit one nothing tells the recipient how to get.
    #[tokio::test]
    async fn nothing_is_omitted_without_the_manifest_saying_where_to_get_it() {
        let (scratch, mut inst) = fixture(
            "inv",
            &[
                ("mods", "from-cf.jar"),
                ("mods", "from-mr.jar"),
                ("mods", "homemade.jar"),
                ("config", "settings.toml"),
            ],
        )
        .await;
        let paths = scratch.paths.clone();
        inst.content = vec![
            record("curseforge", "from-cf.jar", ContentKind::Mod),
            record("modrinth", "from-mr.jar", ContentKind::Mod),
            record("", "homemade.jar", ContentKind::Mod),
        ];

        let manifest = manifest_for(&paths, &inst, &Selection::default(), &from_records(&paths, &inst).await).await;
        let dest = paths.root.join("inv.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t")).await.unwrap();
        let names = entries(&dest);
        let index = index_of(&dest);

        for (dir, file) in [
            ("mods", "from-cf.jar"),
            ("mods", "from-mr.jar"),
            ("mods", "homemade.jar"),
            ("config", "settings.toml"),
        ] {
            let path = format!("{dir}/{file}");
            let embedded = names.contains(&format!("overrides/{path}"));
            let referenced = manifest.note(&path).is_some_and(|n| n.source.is_some());
            // Read off the pack rather than assumed, because which files the
            // index carries is no longer a fact about the records.
            let indexed = index.files.iter().any(|f| f.path == path);
            assert!(
                [embedded, referenced, indexed].iter().filter(|x| **x).count() == 1,
                "{path}: embedded={embedded} referenced={referenced} indexed={indexed}"
            );
        }
    }

    /// A plain `.mrpack` written from a catalogue the test built. No Geode
    /// half, so the index is the only place this pack can name anything.
    async fn mrpack_from(
        paths: &Paths,
        inst: &Instance,
        dest: &Path,
        catalogue: &Catalogue,
    ) -> ExportReport {
        instance::modpack::write_instance_pack(
            paths,
            inst,
            dest.to_path_buf(),
            &Selection::default(),
            instance::modpack::GeodeHalf::default(),
            catalogue,
            &Progress::silent("t"),
        )
        .await
        .unwrap()
    }

    /// What the report says travelled as bytes, by reason: nothing named it,
    /// something did but this format cannot link to it, it was turned off, and
    /// its author turned third-party downloads off. Every reason there is, so
    /// that a file counted under a new one cannot pass for a file that did not
    /// travel at all.
    fn bundled(report: &ExportReport) -> (usize, usize, usize, usize) {
        (
            report.embedded_unknown,
            report.embedded_elsewhere,
            report.embedded_turned_off,
            report.embedded_blocked,
        )
    }

    /// The index a pack carries.
    fn index_of(archive: &Path) -> instance::modpack::MrIndex {
        serde_json::from_slice(&read_entry(archive, "modrinth.index.json")).unwrap()
    }

    /// A plain `.mrpack` has nowhere to record a reference by id, so a mod only
    /// CurseForge has must keep travelling as bytes — leaving it out of one
    /// loses the mod with nothing anywhere to say so.
    #[tokio::test]
    async fn a_plain_mrpack_still_embeds_what_it_cannot_reference() {
        let (scratch, mut inst) = fixture("plain", &[("mods", "create.jar")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        // Asked and not found, which is the case: this jar is on CurseForge
        // and nowhere else.
        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_modrinth_holds(&[]);

        let dest = paths.root.join("plain.mrpack");
        let report = mrpack_from(&paths, &inst, &dest, &catalogue).await;

        assert!(
            entries(&dest).contains(&"overrides/mods/create.jar".to_string()),
            "{:?}",
            entries(&dest)
        );
        assert!(index_of(&dest).files.is_empty(), "nothing can be indexed");
        // And the reason the sharer is given is the format's, not the mod's:
        // CurseForge has this jar, an `.mrpack` just cannot say so.
        assert_eq!((report.referenced, bundled(&report)), (0, (0, 1, 0, 0)));
    }

    /// The owner's request, in one test: a jar Geode knows as CurseForge
    /// content travels as a reference because Modrinth holds the same bytes.
    /// Its record used to be the gate, and so a pack imported from CurseForge
    /// shipped every jar it had.
    #[tokio::test]
    async fn a_curseforge_jar_modrinth_also_hosts_is_linked_not_embedded() {
        let (scratch, mut inst) = fixture("both", &[("mods", "create.jar")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["create.jar"]);

        let dest = paths.root.join("both.geodepack");
        let report = export_with(&paths, &inst, dest.clone(), &Selection::default(), &catalogue, &Progress::silent("t"))
            .await
            .unwrap();

        let index = index_of(&dest);
        assert_eq!(
            index.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["mods/create.jar"]
        );
        let names = entries(&dest);
        assert!(
            !names.contains(&"overrides/mods/create.jar".to_string()),
            "the jar was embedded as well: {names:?}"
        );
        // And not named in both halves: the recipient would fetch it twice,
        // the second time needing a CurseForge key for a file Modrinth hosts.
        let manifest = read_manifest(&dest).unwrap().unwrap();
        assert!(
            manifest.note("mods/create.jar").is_none_or(|n| n.source.is_none()),
            "{:?}",
            manifest.files
        );
        assert_eq!((report.referenced, bundled(&report)), (1, (0, 0, 0, 0)));
    }

    /// A record is no longer the gate. A jar somebody dropped in by hand is
    /// linked too, as long as the bytes are published somewhere.
    #[tokio::test]
    async fn a_jar_with_no_record_at_all_is_linked_when_its_hash_matches() {
        let (scratch, inst) = fixture("norecord", &[("mods", "sodium.jar")]).await;
        let paths = scratch.paths.clone();
        assert!(inst.content.is_empty());

        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar"]);

        let dest = paths.root.join("norecord.mrpack");
        mrpack_from(&paths, &inst, &dest, &catalogue).await;

        let index = index_of(&dest);
        assert_eq!(
            index.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["mods/sodium.jar"]
        );
        // The matched file's own URL. There is no record here to build one
        // from, and even with one the uploaded name need not be the name on
        // disk — which is what made the constructed URL worth replacing.
        assert_eq!(
            index.files[0].downloads,
            vec!["https://cdn.modrinth.com/data/mr-project/versions/mr-version/uploaded-sodium.jar"]
        );
        assert!(
            !entries(&dest).contains(&"overrides/mods/sodium.jar".to_string()),
            "{:?}",
            entries(&dest)
        );
    }

    /// A jar with no record at all, named by CurseForge's by-id channel.
    ///
    /// Modrinth content rides the index, so the `.mrpack` case above already
    /// worked. CurseForge has no index entry: a note is the only channel this
    /// format has for naming it. Those notes were written from
    /// `instance.content`, so a jar nobody had a record for got none — and
    /// `referenced()` reads the notes, so the file was left out of the
    /// referenced set and the `overrides/` walk copied its bytes. Meanwhile
    /// the report the sharer reads counts from the catalogue, which knew
    /// perfectly well it was a reference. The two disagreed, and the zip was
    /// the one telling the truth.
    ///
    /// That report is what the sharer reads to confirm nothing travelled that
    /// should not have, and sending bytes rather than a link is a licensing
    /// position here, not a size optimisation.
    #[tokio::test]
    async fn a_geodepack_references_a_jar_with_no_record_rather_than_copying_it() {
        let (scratch, inst) = fixture("norecord-geode", &[("mods", "create.jar")]).await;
        let paths = scratch.paths.clone();
        assert!(inst.content.is_empty());

        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_curseforge_holds(&[("create.jar", 731, 4242, true)]);

        let manifest = manifest_for(&paths, &inst, &Selection::default(), &catalogue).await;
        let note = manifest
            .note("mods/create.jar")
            .expect("a note naming where the jar came from");
        assert_eq!(
            note.source.as_ref().map(|s| s.provider.as_str()),
            Some("curseforge"),
            "the Geode half named no source, so the walk would copy the bytes"
        );

        // And the link that was actually broken: `referenced` is what the
        // `overrides/` walk subtracts, and it reads the notes. A full export
        // cannot stand in for this here, because `export` builds its own
        // catalogue and a pretended answer cannot reach it.
        assert!(
            referenced(&manifest).contains(&PathBuf::from("mods/create.jar")),
            "the jar was not in the referenced set, so the walk would copy it: {:?}",
            referenced(&manifest)
        );
    }

    /// A record Modrinth has disowned — the jar was patched, replaced or
    /// renamed since — must not become an index entry. The entry's hashes come
    /// from the local bytes while its URL would come from the record, so every
    /// launcher reading the pack would fail its checksum.
    #[tokio::test]
    async fn a_stale_record_is_embedded_rather_than_indexed() {
        let (scratch, mut inst) = fixture("stale-mr", &[("mods", "patched.jar")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("modrinth", "patched.jar", ContentKind::Mod)];

        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_modrinth_holds(&[]);

        let dest = paths.root.join("stale.mrpack");
        let report = mrpack_from(&paths, &inst, &dest, &catalogue).await;

        assert!(index_of(&dest).files.is_empty(), "the stale record was indexed");
        assert!(
            entries(&dest).contains(&"overrides/mods/patched.jar".to_string()),
            "{:?}",
            entries(&dest)
        );
        assert_eq!((report.referenced, bundled(&report)), (0, (1, 0, 0, 0)));
    }

    /// Nothing asked, nothing concluded: an export with no lookup writes the
    /// pack the records describe, with the URL built from their ids, which is
    /// exactly what every export did before the matcher existed. An offline
    /// export must still produce a pack that works.
    #[tokio::test]
    async fn an_offline_export_still_writes_a_valid_pack() {
        let (scratch, mut inst) =
            fixture("offline", &[("mods", "sodium.jar"), ("mods", "create.jar")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![
            record("modrinth", "sodium.jar", ContentKind::Mod),
            record("curseforge", "create.jar", ContentKind::Mod),
        ];

        // The client is the one that reaches a port nothing listens on, so the
        // lookup fails rather than being skipped — the harder case of the two,
        // and the one a user with no internet actually hits.
        let dest = paths.root.join("offline.geodepack");
        let catalogue = identity::for_instance(
            Ask::Platforms {
                client: &offline(),
                want: &["modrinth"],
                curseforge_key: None,
                index: true,
                spares_bytes: true,
            },
            &paths,
            &inst,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        let report = export_with(&paths, &inst, dest.clone(), &Selection::default(), &catalogue, &Progress::silent("t"))
            .await
            .unwrap();

        let index = index_of(&dest);
        assert_eq!(index.files.len(), 1, "{:?}", index.files);
        assert_eq!(index.files[0].path, "mods/sodium.jar");
        assert_eq!(
            index.files[0].downloads,
            vec!["https://cdn.modrinth.com/data/proj-1/versions/ver-1/sodium.jar"]
        );
        // The CurseForge jar still travels as ids in the Geode half.
        let manifest = read_manifest(&dest).unwrap().unwrap();
        assert!(manifest.note("mods/create.jar").is_some_and(|n| n.source.is_some()));
        assert_eq!(report.referenced, 2);
        assert_eq!(
            report.trouble.len(),
            1,
            "the user is told Modrinth could not be asked: {:?}",
            report.trouble
        );
    }

    /// An index that stays uploadable to Modrinth and openable in Prism as it
    /// grows: both hashes on every file, a size, an `env` naming both sides, a
    /// safe relative path, and a canonical URL on an allowlisted host.
    #[tokio::test]
    async fn an_exported_index_is_still_one_modrinth_would_accept() {
        let (scratch, mut inst) = fixture(
            "valid",
            &[("mods", "sodium.jar"), ("mods", "matched.jar"), ("config", "x.toml")],
        )
        .await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("modrinth", "sodium.jar", ContentKind::Mod)];

        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["sodium.jar", "matched.jar"]);

        let dest = paths.root.join("valid.mrpack");
        mrpack_from(&paths, &inst, &dest, &catalogue).await;

        let index = index_of(&dest);
        assert_eq!(index.format_version, 1);
        assert_eq!(index.game, "minecraft");
        assert_eq!(index.files.len(), 2, "{:?}", index.files);
        for file in &index.files {
            assert!(file.hashes.contains_key("sha1"), "{file:?}");
            assert!(file.hashes.contains_key("sha512"), "{file:?}");
            assert!(file.file_size.is_some(), "{file:?}");
            let env = file.env.as_ref().expect("an env pair");
            assert!(!env.client.is_empty() && !env.server.is_empty(), "{env:?}");
            let path = Path::new(&file.path);
            assert!(crate::paths::is_safe_relative(path), "{}", file.path);
            assert!(!file.path.starts_with('/') && !file.path.contains('\\'));
            let url = file.downloads.first().expect("a download");
            let parsed = url::Url::parse(url).expect("a URL");
            assert_eq!(parsed.as_str(), url, "not canonical: {url}");
            assert!(
                matches!(
                    parsed.host_str(),
                    Some("cdn.modrinth.com" | "github.com" | "raw.githubusercontent.com" | "gitlab.com")
                ),
                "off Modrinth's allowlist: {url}"
            );
        }
    }

    /// A file whose name is not text cannot be an index path and cannot be a
    /// zip entry either. It is named to the sharer rather than written as
    /// U+FFFD, which would be a name no reader could ever match back.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_utf8_path_is_reported_rather_than_mangled() {
        use std::os::unix::ffi::OsStrExt;

        let (scratch, inst) = fixture("badname", &[("mods", "fine.jar")]).await;
        let paths = scratch.paths.clone();
        let mods = paths.game_dir(&inst.id).join("mods");
        let name = std::ffi::OsStr::from_bytes(b"sodium\xff.jar");
        tokio::fs::write(mods.join(name), b"not really a jar").await.unwrap();

        let dest = paths.root.join("badname.mrpack");
        let report = mrpack_from(&paths, &inst, &dest, &from_records(&paths, &inst).await).await;

        assert_eq!(report.unnameable.len(), 1, "{:?}", report.unnameable);
        let names = entries(&dest);
        assert!(
            names.iter().all(|n| !n.contains('\u{fffd}')),
            "a mangled name went into the pack: {names:?}"
        );
        // And the rest of the pack still went, including its neighbour.
        assert!(names.contains(&"overrides/mods/fine.jar".to_string()), "{names:?}");
    }

    /// A turned-off mod travels per what each format can say. A `.geodepack`
    /// indexes it under its enabled name and switches it back off from its
    /// note, so it costs no bytes. A plain `.mrpack` has nowhere to say "off",
    /// and arriving switched on is worse than arriving as bytes.
    #[tokio::test]
    async fn a_turned_off_mod_is_referenced_in_a_geodepack_and_embedded_in_an_mrpack() {
        let (scratch, mut inst) = fixture("off", &[("mods", "heavy.jar.disabled")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("modrinth", "heavy.jar", ContentKind::Mod)];

        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["heavy.jar"]);

        let geode = paths.root.join("off.geodepack");
        export_with(&paths, &inst, geode.clone(), &Selection::default(), &catalogue, &Progress::silent("t"))
            .await
            .unwrap();
        assert_eq!(
            index_of(&geode).files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["mods/heavy.jar"],
            "indexed under the name it will have once it is on"
        );
        let manifest = read_manifest(&geode).unwrap().unwrap();
        assert!(
            manifest.note("mods/heavy.jar").is_some_and(|n| n.disabled),
            "nothing says to switch it back off"
        );
        // And the pack says which layout reads it, because a Geode that
        // predates this would fetch the mod from the index and leave it on.
        assert_eq!(manifest.geode_format, 2);
        assert!(
            entries(&geode).iter().all(|n| !n.contains("heavy.jar")),
            "{:?}",
            entries(&geode)
        );

        let plain = paths.root.join("off.mrpack");
        let report = mrpack_from(&paths, &inst, &plain, &catalogue).await;
        assert!(index_of(&plain).files.is_empty(), "an mrpack cannot say 'off'");
        assert!(
            entries(&plain).contains(&"overrides/mods/heavy.jar.disabled".to_string()),
            "{:?}",
            entries(&plain)
        );
        // And the sharer hears the real reason: Modrinth has this mod.
        assert_eq!((report.referenced, bundled(&report)), (0, (0, 0, 1, 0)));
    }

    /// The off state is a fact about the files, not about the records. A jar
    /// dropped into `mods/` by hand and switched off has no record at all —
    /// switching a mod off only renames the file — and the index carries it
    /// under its enabled name regardless, so without a note of its own it
    /// would arrive switched on.
    #[tokio::test]
    async fn a_turned_off_jar_with_no_record_is_noted_as_off_too() {
        let (scratch, inst) = fixture("offhand", &[("mods", "heavy.jar.disabled")]).await;
        let paths = scratch.paths.clone();
        assert!(inst.content.is_empty(), "nothing wrote this jar down");

        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["heavy.jar"]);

        let dest = paths.root.join("offhand.geodepack");
        export_with(
            &paths,
            &inst,
            dest.clone(),
            &Selection::default(),
            &catalogue,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert_eq!(
            index_of(&dest)
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["mods/heavy.jar"]
        );
        assert!(
            read_manifest(&dest)
                .unwrap()
                .unwrap()
                .note("mods/heavy.jar")
                .is_some_and(|n| n.disabled),
            "nothing says to switch it back off: {:?}",
            read_manifest(&dest).unwrap().unwrap().files
        );
        // The bytes stayed out, so the note is the only thing that can.
        assert!(
            entries(&dest).iter().all(|n| !n.contains("heavy.jar")),
            "{:?}",
            entries(&dest)
        );
    }

    /// The other half of the same rule: a mod indexed under its enabled name
    /// arrives switched on unless something turns it off, and the note is the
    /// only thing that remembers.
    #[tokio::test]
    async fn a_mod_the_pack_had_turned_off_arrives_turned_off() {
        let (scratch, mut inst) = fixture("offtrip", &[("mods", "heavy.jar.disabled")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("modrinth", "heavy.jar", ContentKind::Mod)];
        let mut catalogue = from_records(&paths, &inst).await;
        catalogue.pretend_modrinth_holds(&["heavy.jar"]);

        let archive = paths.root.join("offtrip.geodepack");
        export_with(&paths, &inst, archive.clone(), &Selection::default(), &catalogue, &Progress::silent("t"))
            .await
            .unwrap();

        // The index entry is a download, which this test does not do; what it
        // proves is that a file that landed switched on ends up off.
        let mut into = fresh("offtrip-in");
        let mods = paths.game_dir(&into.id).join("mods");
        tokio::fs::create_dir_all(&mods).await.unwrap();
        tokio::fs::write(mods.join("heavy.jar"), b"not really a jar").await.unwrap();

        let manifest = read_manifest(&archive).unwrap().unwrap();
        apply_pack(
            &offline(),
            &paths,
            &mut into,
            &archive,
            &manifest,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert!(mods.join("heavy.jar.disabled").is_file(), "it arrived switched on");
        assert!(!mods.join("heavy.jar").exists());
    }

    // -----------------------------------------------------------------------
    // The round trip
    // -----------------------------------------------------------------------

    use crate::instance::content::Origin;

    /// The whole point of the format: what goes out comes back. Before this,
    /// Geode could write a `.geodepack` and then silently drop its own name,
    /// notes, memory and artwork on the way back in.
    #[tokio::test]
    async fn a_pack_carries_the_instance_back_to_another_machine() {
        let (scratch, mut inst) =
            fixture("round", &[("mods", "create.jar"), ("config", "x.toml")]).await;
        let paths = scratch.paths.clone();
        inst.name = "My Pack".into();
        inst.notes = "read me".into();
        inst.overrides.memory_mb = Some(6144);
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        // Artwork lives beside the instance rather than in the game directory.
        let media = paths.instance_media(&inst.id);
        tokio::fs::create_dir_all(&media).await.unwrap();
        tokio::fs::write(media.join("icon.png"), b"PNG").await.unwrap();
        inst.icon = Some("icon.png".into());

        let dest = paths.root.join("round.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();

        // A fresh instance, standing in for somebody else's machine.
        let mut arrived = fresh("arrived");
        let missing = import_offline(&paths, &mut arrived, &dest).await.unwrap();

        assert_eq!(arrived.name, "My Pack");
        assert_eq!(arrived.notes, "read me");
        assert_eq!(arrived.overrides.memory_mb, Some(6144));
        assert_eq!(arrived.icon.as_deref(), Some("icon.png"));
        assert!(
            paths.instance_media("arrived").join("icon.png").is_file(),
            "the artwork never landed on disk"
        );

        // The CurseForge mod was named rather than carried, so with no key
        // configured the recipient gets a list to act on — not an instance
        // that is quietly one mod short.
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("create.jar"), "{missing:?}");
        assert!(missing[0].contains("CurseForge API key"), "{missing:?}");
    }

    /// Both halves together, in the order the app runs them. This is what
    /// proves `geode/` entries do not confuse the override extractor, that
    /// nothing Geode adds leaks into the game directory, and that a config the
    /// author wrote actually arrives.
    #[tokio::test]
    async fn a_pack_imports_through_the_same_path_the_app_uses() {
        let (scratch, mut inst) = fixture(
            "full",
            &[("mods", "homemade.jar"), ("config", "author.toml")],
        )
        .await;
        let paths = scratch.paths.clone();
        inst.name = "Full Round".into();
        inst.overrides.java_args = Some("-XX:+UseG1GC".into());
        inst.content = vec![record("", "homemade.jar", ContentKind::Mod)];

        let dest = paths.root.join("full.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();

        let mut arrived = fresh("full-arrived");
        let missing = import_offline(&paths, &mut arrived, &dest).await.unwrap();

        assert!(missing.is_empty(), "nothing here needed fetching: {missing:?}");
        assert_eq!(arrived.name, "Full Round");
        assert_eq!(arrived.overrides.java_args.as_deref(), Some("-XX:+UseG1GC"));

        let game = paths.game_dir("full-arrived");
        assert!(game.join("config").join("author.toml").is_file(), "the author's config did not arrive");
        assert!(game.join("mods").join("homemade.jar").is_file(), "the jar with no link did not arrive");
        assert!(!game.join("geode").exists(), "the Geode half was unpacked into the game directory");
    }

    /// The bug the first version shipped. A jar with no public download travels
    /// as an override and lands with no record — so tagging records alone left
    /// exactly the mods a server most needs to keep, its private ones, as the
    /// only ones a player could delete.
    #[tokio::test]
    async fn a_server_pack_owns_even_the_jars_that_arrive_with_no_record() {
        let (scratch, mut inst) = fixture("srv", &[("mods", "private-anticheat.jar")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("", "private-anticheat.jar", ContentKind::Mod)];

        let dest = paths.root.join("server.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(&dest, |m| m.kind = Kind::Server, &[]);

        let mut arrived = fresh("srv-arrived");
        import_offline(&paths, &mut arrived, &dest).await.unwrap();

        let owned = arrived
            .content
            .iter()
            .find(|r| r.file_name == "private-anticheat.jar")
            .expect("the override-carried jar was given no record");
        assert_eq!(owned.origin, Origin::Server);

        let mods = paths.game_dir("srv-arrived").join("mods");
        let err = crate::instance::content::remove_in(
            &mods,
            &mut arrived.content,
            ContentKind::Mod,
            "private-anticheat.jar",
        )
        .await
        .expect_err("a server's private jar must not be deletable");
        assert!(err.to_string().contains("cannot be removed"), "{err}");
        assert!(mods.join("private-anticheat.jar").is_file());
    }

    /// The same through the whole import, from a server pack Geode would never
    /// write: one that sets a Java agent, a heap and a window, and names a
    /// modpack. None of it lands, while the same pack from a person still
    /// carries every bit of it.
    #[tokio::test]
    async fn a_server_pack_cannot_set_the_jvm_or_claim_to_be_a_modpack() {
        let (scratch, mut inst) = fixture("jvm", &[("mods", "agent.jar")]).await;
        let paths = scratch.paths.clone();
        inst.overrides.java_args = Some("-javaagent:mods/agent.jar".into());
        inst.overrides.memory_mb = Some(256);
        inst.overrides.width = Some(3840);
        inst.overrides.height = Some(2160);
        inst.overrides.fullscreen = Some(true);
        inst.pack = Some(crate::instance::PackSource::local(
            "Upstream Pack".into(),
            "4.2.0".into(),
            Some("modrinth".into()),
        ));

        let personal = paths.root.join("jvm-personal.geodepack");
        export(Ask::Nobody, &paths, &inst, personal.clone(), &Selection::default(), &Progress::silent("t")).await.unwrap();
        let hostile = paths.root.join("jvm-server.geodepack");
        std::fs::copy(&personal, &hostile).unwrap();
        rewrite_pack(&hostile, |m| m.kind = Kind::Server, &[]);

        let mut joined = fresh("jvm-joined");
        joined.overrides.width = Some(1280);
        joined.overrides.height = Some(720);
        joined.overrides.fullscreen = Some(false);
        import_offline(&paths, &mut joined, &hostile).await.unwrap();
        assert_eq!(joined.overrides.java_args, None);
        assert_eq!(joined.overrides.memory_mb, Some(2048));
        // The window a server pack names is not applied either, and nothing in
        // the join preview mentions one.
        assert_eq!(joined.overrides.width, Some(1280));
        assert_eq!(joined.overrides.height, Some(720));
        assert_eq!(joined.overrides.fullscreen, Some(false));
        assert!(joined.pack.is_none(), "labelled as a modpack: {:?}", joined.pack);

        let mut shared = fresh("jvm-shared");
        import_offline(&paths, &mut shared, &personal).await.unwrap();
        assert_eq!(shared.overrides.java_args.as_deref(), Some("-javaagent:mods/agent.jar"));
        assert_eq!(shared.overrides.memory_mb, Some(256));
        assert_eq!(shared.overrides.width, Some(3840));
        assert_eq!(shared.overrides.height, Some(2160));
        assert_eq!(shared.overrides.fullscreen, Some(true));
        assert_eq!(shared.pack.as_ref().map(|p| p.version.as_str()), Some("4.2.0"));
    }

    /// A server pack that stops halfway is still the server's. The caller saves
    /// a partial import rather than throwing it away, so ownership has to be
    /// settled before the failure is reported — not only when all went well.
    #[tokio::test]
    async fn a_cancelled_server_import_still_belongs_to_the_server() {
        let scratch = Scratch::new("cancel");
        let paths = scratch.paths.clone();
        tokio::fs::create_dir_all(&paths.root).await.unwrap();

        // Both of the pack's files already on the recipient's disk, standing
        // for a run that was interrupted after they landed: a cancel is heard
        // at the first entry of the extraction now, so a run cancelled from the
        // start brings nothing in of its own — deterministic, and offline.
        let mut arrived = fresh("cancel-arrived");
        let mods = paths.game_dir("cancel-arrived").join("mods");
        tokio::fs::create_dir_all(&mods).await.unwrap();
        tokio::fs::write(mods.join("indexed.jar"), b"indexed").await.unwrap();
        tokio::fs::write(mods.join("private.jar"), b"private").await.unwrap();
        let sha1 = crate::net::sha1_file(&mods.join("indexed.jar")).await.unwrap();

        let index = serde_json::json!({
            "formatVersion": 1, "game": "minecraft", "versionId": "1", "name": "S",
            "files": [{
                "path": "mods/indexed.jar",
                "hashes": { "sha1": sha1, "sha512": "unused" },
                "env": { "client": "required", "server": "required" },
                "downloads": ["https://cdn.modrinth.com/data/never/versions/fetched/indexed.jar"],
                "fileSize": 7
            }],
            "dependencies": { "minecraft": "1.21.1" }
        });
        let manifest = Manifest {
            geode_format: FORMAT,
            kind: Kind::Server,
            profile: Profile { name: "Survival".into(), ..Default::default() },
            icon: None,
            banner: None,
            source: None,
            files: Vec::new(),
            revision: None,
            listing: None,
            settings: None,
            forbidden: Vec::new(),
        };
        let dest = paths.root.join("cancel.geodepack");
        write_pack(&dest, &index, &manifest, &[("overrides/mods/private.jar", b"private")]);

        let progress = Progress::silent("t");
        progress.cancel_flag().store(true, std::sync::atomic::Ordering::Relaxed);
        let result = import(&reqwest::Client::new(), &paths, &mut arrived, &dest, None, &progress).await;

        assert!(result.is_err(), "the import was meant to be cancelled");
        for file in ["indexed.jar", "private.jar"] {
            let record = arrived
                .content
                .iter()
                .find(|r| r.file_name == file)
                .unwrap_or_else(|| panic!("{file} landed but was left unowned: {:?}", arrived.content));
            assert_eq!(record.origin, Origin::Server, "{file}");
        }
    }

    /// A pack from a person, not a server, leaves the recipient in charge of
    /// their own mods — they can delete any of them.
    #[tokio::test]
    async fn a_personal_pack_leaves_its_content_the_recipients_own() {
        let (scratch, mut inst) = fixture("pers", &[("mods", "homemade.jar")]).await;
        let paths = scratch.paths.clone();
        inst.content = vec![record("", "homemade.jar", ContentKind::Mod)];

        let dest = paths.root.join("personal.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();

        let mut arrived = fresh("pers-arrived");
        import_offline(&paths, &mut arrived, &dest).await.unwrap();

        assert!(
            arrived.content.iter().all(|r| r.origin == Origin::User),
            "a personal pack must not lock the recipient out of their own mods"
        );
        let mods = paths.game_dir("pers-arrived").join("mods");
        crate::instance::content::remove_in(&mods, &mut arrived.content, ContentKind::Mod, "homemade.jar")
            .await
            .unwrap();
    }

    /// A reference is fetched fresh, so a mod the sharer turned off has to say
    /// so or it arrives switched on — the one they turned off because it
    /// crashed, now crashing for somebody else.
    #[tokio::test]
    async fn a_disabled_reference_carries_its_state() {
        let (scratch, mut inst) = fixture(
            "offref",
            &[("mods", "crashes.jar.disabled"), ("mods", "fine.jar")],
        )
        .await;
        let paths = scratch.paths.clone();
        inst.content = vec![
            record("curseforge", "crashes.jar", ContentKind::Mod),
            record("curseforge", "fine.jar", ContentKind::Mod),
        ];

        let manifest = manifest_for(&paths, &inst, &Selection::default(), &from_records(&paths, &inst).await).await;
        assert!(manifest.note("mods/crashes.jar").is_some_and(|n| n.disabled), "{:?}", manifest.files);
        assert!(manifest.note("mods/fine.jar").is_some_and(|n| !n.disabled), "{:?}", manifest.files);
        assert_eq!(disabled_rel(Path::new("mods/crashes.jar")), Some(PathBuf::from("mods/crashes.jar.disabled")));
    }

    /// The name artwork is written under is Geode's, never the pack's. On
    /// Windows a name like `C:evil.png` joins as a drive-relative path and lands
    /// outside the media folder entirely; this cannot reproduce that on Linux,
    /// but it does pin that the pack's chosen name never reaches the disk.
    #[tokio::test]
    async fn a_pack_cannot_choose_where_its_artwork_is_written() {
        let (scratch, inst) = fixture("art", &[("config", "x.toml")]).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("art.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(
            &dest,
            |m| {
                m.icon = Some("geode/media/C:evil.png".into());
                m.banner = Some("geode/media/payload.dll".into());
            },
            &[
                ("geode/media/C:evil.png", b"PNG".to_vec()),
                ("geode/media/payload.dll", b"MZ".to_vec()),
            ],
        );

        let mut arrived = fresh("art-arrived");
        import_offline(&paths, &mut arrived, &dest).await.unwrap();

        assert_eq!(arrived.icon.as_deref(), Some("icon.png"));
        assert_eq!(arrived.banner, None, "a non-image banner must be refused");
        let names: Vec<String> = std::fs::read_dir(paths.instance_media("art-arrived"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["icon.png".to_string()], "{names:?}");
    }

    /// An "icon" is not a way to fill somebody's disk. Held to the same eight
    /// megabytes as a picture chosen by hand.
    #[tokio::test]
    async fn oversized_artwork_is_refused() {
        let (scratch, inst) = fixture("big", &[("config", "x.toml")]).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("big.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(
            &dest,
            |m| m.icon = Some("geode/media/icon.png".into()),
            &[("geode/media/icon.png", vec![0u8; 9 * 1024 * 1024])],
        );

        let mut arrived = fresh("big-arrived");
        import_offline(&paths, &mut arrived, &dest).await.unwrap();
        assert_eq!(arrived.icon, None);
        assert!(!paths.instance_media("big-arrived").join("icon.png").exists());
    }

    /// The Geode manifest is read whole and parsed too, so it is held to the
    /// same ceiling as the index. Padded with spaces, which is valid JSON, so
    /// nothing but its size can be what refuses it.
    #[test]
    fn an_oversized_geode_manifest_is_refused() {
        use std::io::Write;
        let scratch = Scratch::new("bigmanifest");
        let dir = scratch.paths.root.clone();
        std::fs::create_dir_all(&dir).unwrap();

        let json = serde_json::to_vec(&manifest()).unwrap();
        for (spaces, readable) in [(1024, true), (9 * 1024 * 1024, false)] {
            let archive = dir.join(format!("{spaces}.geodepack"));
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file(Manifest::PATH, options).unwrap();
            zip.write_all(&json).unwrap();
            zip.write_all(&vec![b' '; spaces]).unwrap();
            zip.finish().unwrap();

            match (read_manifest(&archive), readable) {
                (Ok(Some(m)), true) => assert_eq!(m.profile.name, "Shared Pack"),
                (Err(e), false) => assert!(e.to_string().contains("not read"), "{e}"),
                (got, _) => panic!("{spaces} spaces of padding: {got:?}"),
            }
        }
    }

    /// Refusing a path is not enough on its own; the refusal has to be said.
    /// An empty list means everything the pack named arrived, and a pack that
    /// names `../../` is exactly the one somebody most needs to hear about.
    #[tokio::test]
    async fn an_unsafe_reference_is_reported_rather_than_skipped() {
        let (scratch, inst) = fixture("unsafe", &[("config", "x.toml")]).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("unsafe.geodepack");
        export(Ask::Nobody, &paths, &inst, dest.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(
            &dest,
            |m| {
                m.files.push(FileNote {
                    path: "../../mods/escape.jar".into(),
                    side: None,
                    source: Some(SourceRef {
                        provider: "curseforge".into(),
                        project_id: "1".into(),
                        version_id: "2".into(),
                    }),
                    disabled: false,
                })
            },
            &[],
        );

        let mut arrived = fresh("unsafe-arrived");
        let missing = import_offline(&paths, &mut arrived, &dest).await.unwrap();
        assert!(missing.iter().any(|m| m.contains("not a safe path")), "{missing:?}");
    }

    /// What the admin sets travels in the pack, and in the description beside
    /// it — but only for a server, because a personal pack has no server
    /// behind it to put in anybody's list.
    #[tokio::test]
    async fn an_exported_server_pack_carries_the_admins_listing_and_an_instance_pack_never_does() {
        let (scratch, server) = server_fixture("listing", false).await;
        let paths = scratch.paths.clone();

        // Nothing set yet: no listing in either carrier, which is what an
        // older admin's Geode also sends.
        let dest = paths.root.join("plain.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();
        assert_eq!(read_manifest(&dest).unwrap().unwrap().listing, None);
        assert_eq!(server_pack_info(&paths, &server).await.unwrap().listing, None);

        // Set, and in both.
        let mut share = crate::server::share::load(&paths, &server.id).await.unwrap().unwrap();
        let listing = Listing {
            name: "Survival Club".into(),
            address: "play.example.com:25566".into(),
        };
        share.listing = Some(listing.clone());
        crate::server::share::save(&paths, &server.id, &share).await.unwrap();

        let dest = paths.root.join("listed.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();
        let manifest = read_manifest(&dest).unwrap().unwrap();
        assert_eq!(manifest.listing, Some(listing.clone()));
        assert_eq!(
            server_pack_info(&paths, &server).await.unwrap().listing,
            Some(listing)
        );
        // And it is an addition, not a new layout: a pack carrying one still
        // claims the format an older Geode reads.
        assert_eq!(manifest.geode_format, 1);

        // A listing that would garble somebody's list is dropped on the way
        // out rather than sent to every player.
        share.listing = Some(Listing {
            name: "Bad\nName".into(),
            address: "play.example.com".into(),
        });
        crate::server::share::save(&paths, &server.id, &share).await.unwrap();
        let dest = paths.root.join("garbled.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();
        assert_eq!(read_manifest(&dest).unwrap().unwrap().listing, None);
        assert_eq!(server_pack_info(&paths, &server).await.unwrap().listing, None);

        // A personal pack never carries one.
        let instance_manifest = manifest_for(&paths, &instance(), &Selection::default(), &Catalogue::default()).await;
        assert_eq!(instance_manifest.listing, None);
    }

    /// The whole point of the feature, through the app's own import sequence:
    /// the server is in the instance's multiplayer list when the import
    /// finishes, and Play is pointed at the same address.
    ///
    /// The instance's pack is turned into a server's the way the other import
    /// tests here do it, so nothing is fetched. Put the share address in the
    /// listing instead of the game address and the pack port lands in the
    /// player's list, which the last assertion is watching for.
    #[tokio::test]
    async fn importing_a_server_pack_adds_the_row_and_points_quick_play_at_it() {
        let (scratch, inst) = fixture("listed", &[("mods", "homemade.jar")]).await;
        let paths = scratch.paths.clone();
        let personal = paths.root.join("listed-personal.geodepack");
        export(Ask::Nobody, &paths, &inst, personal.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();

        // No listing: no row, and Play points nowhere. This is also what an
        // older admin's Geode sends, and what an admin who has not set one
        // sends — the graceful degrade, rather than an error.
        let unlisted = paths.root.join("unlisted.geodepack");
        std::fs::copy(&personal, &unlisted).unwrap();
        rewrite_pack(&unlisted, |m| m.kind = Kind::Server, &[]);
        let mut plain = fresh("unlisted-joined");
        import_offline(&paths, &mut plain, &unlisted).await.unwrap();
        assert_eq!(plain.joined_listing_address, None);
        assert_eq!(plain.overrides.quick_play_server, None);
        assert!(
            !instance::multiplayer::file(&paths.game_dir(&plain.id)).exists(),
            "a pack with no listing wrote a multiplayer list"
        );

        // With one: the row, and Play pointed at it.
        let listed_pack = paths.root.join("listed-server.geodepack");
        std::fs::copy(&personal, &listed_pack).unwrap();
        rewrite_pack(
            &listed_pack,
            |m| {
                m.kind = Kind::Server;
                m.listing = Some(Listing {
                    name: "Survival Club".into(),
                    address: "play.example.com:25566".into(),
                });
            },
            &[],
        );

        let mut joined = fresh("listed-joined");
        import_offline(&paths, &mut joined, &listed_pack).await.unwrap();
        assert_eq!(
            joined.joined_listing_address.as_deref(),
            Some("play.example.com:25566")
        );
        assert_eq!(
            joined.overrides.quick_play_server.as_deref(),
            Some("play.example.com:25566")
        );

        let list = tokio::fs::read(instance::multiplayer::file(&paths.game_dir(&joined.id)))
            .await
            .expect("no multiplayer list was written");
        let text = String::from_utf8_lossy(&list);
        assert!(text.contains("Survival Club"), "the name is not in the list");
        assert!(text.contains("play.example.com:25566"), "the address is not in the list");
        // The pack port is Geode's own and the game has nothing listening on
        // it: it must never reach a player's list.
        assert!(!text.contains("25590"), "the pack port reached the list");

        // And a personal pack carrying one adds nothing: there is no server
        // behind it, so a listing in it was not written by Geode.
        let pretend = paths.root.join("listed-personal-claim.geodepack");
        std::fs::copy(&personal, &pretend).unwrap();
        rewrite_pack(
            &pretend,
            |m| {
                m.listing = Some(Listing {
                    name: "Not A Server".into(),
                    address: "play.example.com".into(),
                })
            },
            &[],
        );
        let mut personal_import = fresh("personal-joined");
        import_offline(&paths, &mut personal_import, &pretend).await.unwrap();
        assert_eq!(personal_import.joined_listing_address, None);
        assert!(
            !instance::multiplayer::file(&paths.game_dir(&personal_import.id)).exists(),
            "a personal pack wrote a multiplayer list"
        );
    }

    /// A row the player already had at that address is claimed by the import
    /// and recorded as theirs, which is what keeps the name they gave it
    /// through the Plays after it.
    ///
    /// Break it by recording a claimed row as one Geode wrote and the next
    /// reconcile cannot tell it from ours: it renames their row to the
    /// admin's.
    #[tokio::test]
    async fn importing_claims_a_row_the_player_already_had_and_records_it_as_theirs() {
        let (scratch, inst) = fixture("claimed", &[("mods", "homemade.jar")]).await;
        let paths = scratch.paths.clone();
        let pack = paths.root.join("claimed-server.geodepack");
        export(Ask::Nobody, &paths, &inst, pack.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(
            &pack,
            |m| {
                m.kind = Kind::Server;
                m.listing = Some(Listing {
                    name: "Survival Club".into(),
                    address: "play.example.com".into(),
                });
            },
            &[],
        );

        // Theirs, added by hand before the import, spelled the way they typed
        // it rather than the way the admin's listing spells it.
        let mut joined = fresh("claimed-joined");
        instance::multiplayer::ensure(
            &paths.game_dir(&joined.id),
            "My Server",
            "PLAY.example.com:25565",
            None,
        )
        .await
        .unwrap();

        import_offline(&paths, &mut joined, &pack).await.unwrap();
        let list = tokio::fs::read(instance::multiplayer::file(&paths.game_dir(&joined.id)))
            .await
            .expect("their list went missing");
        let text = String::from_utf8_lossy(&list);
        assert!(text.contains("My Server"), "the import took their name: {text}");
        assert!(!text.contains("Survival Club"), "the import took their name: {text}");
        assert!(
            joined.joined_listing_claimed,
            "their row was recorded as one of Geode's, so the next Play renames it"
        );
        // Still the row this instance is reached by, and Play still goes there.
        assert_eq!(joined.joined_listing_address.as_deref(), Some("play.example.com"));
        assert_eq!(
            joined.overrides.quick_play_server.as_deref(),
            Some("play.example.com")
        );
    }

    // -----------------------------------------------------------------------
    // Helpers for building and bending packs
    // -----------------------------------------------------------------------

    fn fresh(id: &str) -> Instance {
        let mut i = instance();
        i.id = id.into();
        i
    }

    /// The app's own import sequence, with no key and a client that cannot
    /// reach anything. None of these fixtures download a file — and the pass
    /// that names what arrived does ask Modrinth, so a client that fails is
    /// what proves these fixtures do not depend on the answer.
    async fn import_offline(paths: &Paths, inst: &mut Instance, archive: &Path) -> Result<Vec<String>> {
        import(&offline(), paths, inst, archive, None, &Progress::silent("t")).await
    }

    /// Rewrite an exported pack in place: edit its manifest, add entries.
    ///
    /// For building packs Geode would never write itself — a server pack, a
    /// hostile one — out of one it did, so the rest stays realistic.
    fn rewrite_pack(archive: &Path, edit: impl FnOnce(&mut Manifest), extra: &[(&str, Vec<u8>)]) {
        use std::io::{Read, Write};
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
            for i in 0..zip.len() {
                let mut e = zip.by_index(i).unwrap();
                let mut bytes = Vec::new();
                e.read_to_end(&mut bytes).unwrap();
                entries.push((e.name().to_string(), bytes));
            }
        }
        let mut manifest: Manifest = entries
            .iter()
            .find(|(n, _)| n == Manifest::PATH)
            .map(|(_, b)| serde_json::from_slice(b).unwrap())
            .expect("an exported pack has a manifest");
        edit(&mut manifest);

        let mut zip = zip::ZipWriter::new(std::fs::File::create(archive).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for (name, bytes) in entries.iter().filter(|(n, _)| n != Manifest::PATH) {
            zip.start_file(name.as_str(), options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.start_file(Manifest::PATH, options).unwrap();
        zip.write_all(&serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        for (name, bytes) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A pack built from parts, for the cases no export produces.
    fn write_pack(dest: &Path, index: &serde_json::Value, manifest: &Manifest, extra: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("modrinth.index.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(index).unwrap()).unwrap();
        zip.start_file(Manifest::PATH, options).unwrap();
        zip.write_all(&serde_json::to_vec_pretty(manifest).unwrap()).unwrap();
        for (name, bytes) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    // -----------------------------------------------------------------------
    // A server's pack
    // -----------------------------------------------------------------------

    use crate::server::share::ServerShare;
    use crate::server::{Server, ServerKind};

    /// A shared Fabric server holding nothing but the files named.
    async fn small_server(
        tag: &str,
        files: &[(&str, Vec<u8>)],
        include_config: bool,
    ) -> (Scratch, Server) {
        let scratch = Scratch::new(&format!("srv-{tag}"));
        let paths = scratch.paths.clone();
        let mut server = Server::new("Survival Club".into(), ServerKind::Fabric, "1.21.1".into());
        server.id = format!("srv-{tag}");
        server.build = "0.16.9".into();

        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(run.join("mods")).await.unwrap();
        for (rel, bytes) in files {
            let path = run.join(rel);
            tokio::fs::create_dir_all(path.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(path, bytes).await.unwrap();
        }
        let media = paths.server_media(&server.id);
        tokio::fs::create_dir_all(&media).await.unwrap();
        tokio::fs::write(media.join("icon.png"), b"PNG")
            .await
            .unwrap();
        server.icon = Some("icon.png".into());

        crate::server::share::save(
            &paths,
            &server.id,
            &ServerShare {
                code: crate::share::code::Code::generate(),
                include_config,
                settings: Default::default(),
                picked: Vec::new(),
                listing: None,
                shared: true,
                forbidden: Vec::new(),
            },
        )
        .await
        .unwrap();
        (scratch, server)
    }

    /// A Fabric server laid out the way a real one is after it has run: the
    /// secrets, the player lists, the world and the libraries sitting beside
    /// the mods, which is exactly what its pack must not pick up.
    async fn server_fixture(tag: &str, include_config: bool) -> (Scratch, Server) {
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("server.properties", b"enable-rcon=true\nrcon.password=secret\n".to_vec()),
            ("ops.json", br#"[{"name":"admin","level":4}]"#.to_vec()),
            ("whitelist.json", br#"[{"name":"friend"}]"#.to_vec()),
            ("banned-players.json", br#"[{"name":"griefer"}]"#.to_vec()),
            ("usercache.json", br#"[{"name":"friend"}]"#.to_vec()),
            ("eula.txt", b"eula=true\n".to_vec()),
            ("logs/latest.log", b"[Server] 203.0.113.9 logged in\n".to_vec()),
            ("libraries/x.jar", b"library".to_vec()),
            ("world/level.dat", b"level".to_vec()),
            ("world/datapacks/dp.zip", b"datapack".to_vec()),
            ("plugins/p.jar", b"plugin".to_vec()),
            ("config/secret.toml", b"token = \"secret\"\n".to_vec()),
            // Indexed: a Modrinth record.
            ("mods/sodium-mr.jar", b"from modrinth".to_vec()),
            // Referenced: a CurseForge record.
            ("mods/create-cf.jar", b"from curseforge".to_vec()),
            // Carried as bytes: no record, and it runs on both sides.
            ("mods/homemade.jar", fabric_jar("homemade", "*")),
            // Once held back from players, with a CurseForge record.
            ("mods/anticheat.jar", b"private".to_vec()),
            // Once held back from players, with a Modrinth record.
            ("mods/excluded-mr.jar", b"private".to_vec()),
            // The admin's own, turned off, with no record.
            ("mods/private-tools.jar.disabled", b"private".to_vec()),
            // Declares itself server-side, with no record.
            ("mods/server-side.jar", fabric_jar("serverside", "server")),
            // Declares itself server-side, with a Modrinth record.
            ("mods/srv-mr.jar", fabric_jar("srvmr", "server")),
        ];
        let (scratch, mut server) = small_server(tag, &files, include_config).await;
        server.notes = "be nice".into();
        server.settings.memory_mb = 8192;
        server.settings.java_args = Some("-XX:+UseZGC".into());
        server.content = vec![
            record("modrinth", "sodium-mr.jar", ContentKind::Mod),
            record("curseforge", "create-cf.jar", ContentKind::Mod),
            record("curseforge", "anticheat.jar", ContentKind::Mod),
            record("modrinth", "excluded-mr.jar", ContentKind::Mod),
            record("modrinth", "srv-mr.jar", ContentKind::Mod),
            // A plugin the index could point at, if the pack looked past mods/.
            record("modrinth", "p.jar", ContentKind::Plugin),
        ];
        (scratch, server)
    }

    fn read_entry(archive: &Path, name: &str) -> Vec<u8> {
        use std::io::Read;
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        let mut bytes = Vec::new();
        zip.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
        bytes
    }

    fn sorted(mut names: Vec<String>) -> Vec<String> {
        names.sort();
        names
    }

    /// A server's pack is built from records alone: no client reaches
    /// `export_server`, so there is nothing for it to look up. Its index names
    /// the Modrinth mods with the URL built from their ids, and the revision it
    /// carries is the one `server_pack_info` advertises. A lookup that
    /// succeeded in one and not the other would advertise a revision the pack
    /// does not have and push an update to every player — and every
    /// online/offline flip would do it again. What the pack holds is pinned by
    /// `a_server_pack_carries_every_mod_and_nothing_else`.
    #[tokio::test]
    async fn a_servers_pack_is_built_without_touching_the_network() {
        let (scratch, server) = server_fixture("norecon", false).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("norecon.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();

        let index = index_of(&dest);
        let sodium = index
            .files
            .iter()
            .find(|f| f.path == "mods/sodium-mr.jar")
            .expect("the Modrinth mod is indexed");
        assert_eq!(
            sodium.downloads,
            vec!["https://cdn.modrinth.com/data/proj-1/versions/ver-1/sodium-mr.jar"]
        );

        let info = server_pack_info(&paths, &server).await.unwrap();
        let manifest = read_manifest(&dest).unwrap().expect("a Geode pack");
        assert_eq!(manifest.revision, Some(info.revision));
    }

    /// The allowlist, end to end. Every jar in `mods/` goes — the admin's own,
    /// one turned off, one that only runs on a server — and everything else in
    /// the run directory stays where it is: the RCON password, the player
    /// lists, the world, the libraries, the plugins and the configs.
    #[tokio::test]
    async fn a_server_pack_carries_every_mod_and_nothing_else() {
        let (scratch, server) = server_fixture("only", false).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();

        assert_eq!(
            sorted(entries(&dest)),
            vec![
                "geode/manifest.json",
                "geode/media/icon.png",
                "modrinth.index.json",
                "overrides/mods/homemade.jar",
                "overrides/mods/private-tools.jar.disabled",
                "overrides/mods/server-side.jar",
            ],
        );

        // Nothing secret travels under any name, in any entry.
        for name in entries(&dest) {
            let bytes = String::from_utf8_lossy(&read_entry(&dest, &name)).to_string();
            assert!(!bytes.contains("secret"), "{name} carries a secret: {bytes}");
        }

        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
        let indexed = sorted(index.files.iter().map(|f| f.path.clone()).collect());
        assert_eq!(
            indexed,
            vec![
                "mods/excluded-mr.jar",
                "mods/sodium-mr.jar",
                "mods/srv-mr.jar"
            ],
            "every Modrinth mod is indexed"
        );
        // A server-only jar does nothing on a client, and is not refused there.
        let env = index
            .files
            .iter()
            .find(|f| f.path == "mods/srv-mr.jar")
            .and_then(|f| f.env.as_ref())
            .expect("an env for the server-side mod");
        assert_eq!(
            (env.client.as_str(), env.server.as_str()),
            ("optional", "required")
        );
        assert_eq!(index.name, "Survival Club");
        assert_eq!(index.dependencies.get("minecraft").map(String::as_str), Some("1.21.1"));
        assert_eq!(index.dependencies.get("fabric-loader").map(String::as_str), Some("0.16.9"));

        let manifest = read_manifest(&dest).unwrap().expect("a Geode pack");
        assert_eq!(manifest.kind, Kind::Server);
        assert_eq!(manifest.profile.name, "Survival Club");
        assert_eq!(manifest.profile.notes, "be nice");
        assert_eq!(manifest.profile.server_kind.as_deref(), Some("fabric"));
        assert_eq!(manifest.profile.memory_mb, None, "the server's heap is not the player's");
        assert_eq!(manifest.profile.java_args, None, "the server's JVM flags are not the player's");
        assert_eq!(manifest.icon.as_deref(), Some("geode/media/icon.png"));

        let referenced = sorted(
            manifest
                .files
                .iter()
                .filter(|n| n.source.is_some())
                .map(|n| n.path.clone())
                .collect(),
        );
        assert_eq!(referenced, vec!["mods/anticheat.jar", "mods/create-cf.jar"]);
        assert!(
            manifest.files.iter().all(|n| !n.path.contains("p.jar")),
            "the manifest mentions the plugin: {:?}",
            manifest.files
        );
    }

    /// A side the admin set by hand is the side the pack sends, in both of the
    /// places a pack says one: the index's `env` pair and the manifest's note.
    /// They are written by different code from different inputs, and a pack
    /// whose index called a mod client-only while its note called it
    /// server-side would install it in one place and hide it in the other.
    #[tokio::test]
    async fn a_side_the_admin_set_is_the_side_the_whole_pack_sends() {
        use crate::instance::jar::Side;

        let (scratch, mut server) = server_fixture("chosen-side", false).await;
        let paths = scratch.paths.clone();

        // Declares both sides and has no record at all until now.
        server.content.push(ContentRecord {
            chosen_side: Some(Side::Server),
            ..record("", "homemade.jar", ContentKind::Mod)
        });
        // Listed as client only, which is wrong: the admin says it runs here.
        for r in &mut server.content {
            if r.file_name == "excluded-mr.jar" {
                r.side = Some(Side::Client);
                r.chosen_side = Some(Side::Both);
            }
        }

        let dest = paths.root.join("chosen.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let manifest = read_manifest(&dest).unwrap().expect("a Geode pack");
        assert_eq!(
            manifest.note("mods/homemade.jar").and_then(|n| n.side),
            Some(Side::Server),
            "the admin's answer beat the jar's own declaration: {:?}",
            manifest.files
        );
        assert_eq!(
            manifest.note("mods/excluded-mr.jar").and_then(|n| n.side),
            None,
            "a mod for both sides is noted with no side at all"
        );

        let env = index_of(&dest)
            .files
            .iter()
            .find(|f| f.path == "mods/excluded-mr.jar")
            .and_then(|f| f.env.clone())
            .expect("the Modrinth mod is indexed");
        assert_eq!(
            (env.client.as_str(), env.server.as_str()),
            ("required", "required"),
            "the index agrees with the note rather than with the listing"
        );
    }

    /// A mod the admin is keeping on this server reaches a pack by no route at
    /// all. One of each kind is held back — the Modrinth mod the index would
    /// name, the CurseForge mod the manifest would reference, and the jar added
    /// by hand whose bytes would travel — and the pack must carry none of
    /// them, mention none of them, and count none of them. This is a licence
    /// and a privacy promise as much as a convenience, so it is checked one
    /// carrier at a time rather than by the total.
    #[tokio::test]
    async fn a_mod_kept_on_this_server_reaches_the_pack_by_no_route() {
        use instance::content::ContentKind;

        let (scratch, mut server) = server_fixture("kept-here", false).await;
        let paths = scratch.paths.clone();
        let held = ["sodium-mr.jar", "create-cf.jar", "homemade.jar"];
        for file in held {
            // Through the admin's own command, so the jar added by hand gets
            // the record the choice has to live on.
            crate::server::set_mod_kept(&paths, &mut server, ContentKind::Mod, file, true)
                .await
                .unwrap();
        }
        // Saved and read again, which is what happens between the press and
        // the next player's download: the host loads the server itself. For
        // the jar added by hand that is the whole test, since its record holds
        // nothing but this choice and a load drops records that say nothing.
        server.save(&paths).await.unwrap();
        let server = crate::server::load(&paths, &server.id).await.unwrap();

        let dest = paths.root.join("kept.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        // Its bytes.
        assert_eq!(
            sorted(entries(&dest)),
            vec![
                "geode/manifest.json",
                "geode/media/icon.png",
                "modrinth.index.json",
                "overrides/mods/private-tools.jar.disabled",
                "overrides/mods/server-side.jar",
            ],
        );

        let index = index_of(&dest);
        let manifest = read_manifest(&dest).unwrap().expect("a Geode pack");
        for file in held {
            // The index.
            assert!(
                !index.files.iter().any(|f| f.path.contains(file)),
                "{file} is indexed: {:?}",
                index.files.iter().map(|f| &f.path).collect::<Vec<_>>()
            );
            // A reference, a side, a disabled flag — any note at all, since a
            // note names where a file came from.
            assert!(
                !manifest.files.iter().any(|n| n.path.contains(file)),
                "{file} is named in the manifest: {:?}",
                manifest.files
            );
        }

        // And the count a player is shown, which has to be the number of files
        // the pack actually carries.
        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.mods, 5);
        assert_eq!(info.mods, mods_in(&dest));
    }

    /// Holding a jar back changes what every player receives, so the server's
    /// description has to change with it. Without that, a player who already
    /// has the pack would check for changes, find none, and keep the jar for
    /// ever.
    #[tokio::test]
    async fn keeping_a_mod_here_changes_what_players_check_for() {
        use instance::content::ContentKind;

        let (scratch, mut server) = server_fixture("kept-revision", false).await;
        let paths = scratch.paths.clone();
        let before = server_pack_info(&paths, &server).await.unwrap().revision;

        crate::server::set_mod_kept(&paths, &mut server, ContentKind::Mod, "sodium-mr.jar", true)
            .await
            .unwrap();
        let after = server_pack_info(&paths, &server).await.unwrap();

        assert_ne!(before, after.revision);
        // And the pack built from that same state carries the new one, so the
        // player who downloads it is not told again that it has changed.
        let dest = paths.root.join("kept-revision.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();
        assert_eq!(
            read_manifest(&dest).unwrap().unwrap().revision,
            Some(after.revision)
        );
    }

    /// A mod an admin held back while that could be done is an ordinary mod
    /// now. Its record still says so in `server.json`, and still reads.
    #[tokio::test]
    async fn a_mod_once_held_back_is_shared_now() {
        let (scratch, server) = server_fixture("held-back", false).await;
        let paths = scratch.paths.clone();
        server.save(&paths).await.unwrap();
        let file = paths.server(&server.id).join("server.json");
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        for record in json["content"].as_array_mut().unwrap() {
            if record["fileName"] == "excluded-mr.jar" {
                record["sharing"] = serde_json::json!({ "excluded": true });
            }
        }
        std::fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();

        let server = crate::server::load(&paths, &server.id).await.unwrap();
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
        assert!(
            index.files.iter().any(|f| f.path == "mods/excluded-mr.jar"),
            "the mod once held back was left out: {:?}",
            index.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.mods, 8);
        assert_eq!(info.mods, mods_in(&dest));
    }

    /// `config/` is the one thing beyond mods a server may send, and only when
    /// its admin said so.
    #[tokio::test]
    async fn a_server_sends_its_config_only_when_asked() {
        let (scratch, server) = server_fixture("config", true).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();

        assert_eq!(
            sorted(entries(&dest)),
            vec![
                "geode/manifest.json",
                "geode/media/icon.png",
                "modrinth.index.json",
                "overrides/config/secret.toml",
                "overrides/mods/homemade.jar",
                "overrides/mods/private-tools.jar.disabled",
                "overrides/mods/server-side.jar",
            ],
        );

        // A server whose sharing is stopped keeps the choice for when it is
        // shared again, and sends no config meanwhile.
        crate::server::share::stop(&paths, &server.id)
            .await
            .unwrap();
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();
        assert!(
            entries(&dest).iter().all(|name| !name.contains("config/")),
            "a stopped share sent config: {:?}",
            entries(&dest)
        );
    }

    /// Choose a behaviour for this server's config folder, the way the admin's
    /// control does: on, and this behaviour for the whole folder.
    async fn choose_config(paths: &Paths, id: &str, policy: Policy) {
        let mut share = crate::server::share::load(paths, id)
            .await
            .unwrap()
            .expect("a shared server");
        share.include_config = true;
        share.settings.choose("config", policy);
        crate::server::share::save(paths, id, &share).await.unwrap();
    }

    /// A share made before any of this — config on, no behaviour chosen —
    /// carries no settings half at all, and choosing the behaviour it already
    /// has changes nothing either. So every existing share's pack is the pack
    /// it was, its revision is the revision it was, and no player downloads
    /// anything over a feature nobody asked for.
    #[tokio::test]
    async fn a_share_that_chose_nothing_sends_the_pack_it_always_sent() {
        let (scratch, server) = server_fixture("migrate", true).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();

        let before = sorted(entries(&dest));
        let revision = revision_of(&paths, &server).await;
        // The pack spells out the choice nobody made, which is the one sending
        // a config folder has always made: every file replaced, in force since
        // before there was anything counting.
        let spelled = crate::share::manifest::Settings {
            paths: vec![crate::share::manifest::SyncedPath {
                path: "config".into(),
                policy: Policy::Enforce,
                from: 0,
            }],
            overrides: Vec::new(),
            at: 0,
        };
        assert_eq!(
            read_manifest(&dest).unwrap().unwrap().settings,
            Some(spelled.clone())
        );

        // "Always replace" is what sending a config folder has always done, so
        // picking it is picking what is already in force.
        choose_config(&paths, &server.id, Policy::Enforce).await;
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();
        assert_eq!(sorted(entries(&dest)), before);
        assert_eq!(
            read_manifest(&dest).unwrap().unwrap().settings,
            Some(spelled)
        );
        assert_eq!(
            revision_of(&paths, &server).await,
            revision,
            "picking the behaviour already in force moved the revision"
        );
        // And the one thing a player is told about those files: whether they
        // are theirs once they have them. They are not, under either of these.
        assert!(!server_pack_info(&paths, &server).await.unwrap().config_kept);

        // And the other two are real choices, so they do move it — a player
        // who is never told cannot act on any of this.
        choose_config(&paths, &server.id, Policy::ShipOnce).await;
        let sent_once = revision_of(&paths, &server).await;
        assert_ne!(sent_once, revision, "choosing Send once told nobody");
        let info = server_pack_info(&paths, &server).await.unwrap();
        assert!(
            info.config_kept,
            "a player is still told the server will replace what it sent once"
        );
        assert_eq!(info.config_files, 1, "{info:?}");
        let manifest = {
            export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();
            read_manifest(&dest).unwrap().unwrap()
        };
        let settings = manifest.settings.expect("a settings half");
        assert_eq!(settings.paths.len(), 1, "{:?}", settings.paths);
        assert_eq!(settings.paths[0].path, "config");
        assert_eq!(settings.paths[0].policy, Policy::ShipOnce);
        assert_eq!(settings.at, settings.paths[0].from);
        // The pack still carries the files themselves: the behaviour decides
        // what the player's Geode does with them, not whether they travel.
        assert!(entries(&dest).contains(&"overrides/config/secret.toml".to_string()));
        // And the two agree, as they must with a settings half as without one.
        assert_eq!(
            manifest.revision.as_deref(),
            Some(sent_once.as_str()),
            "the pack and the description disagree once a behaviour is chosen"
        );

        choose_config(&paths, &server.id, Policy::Merge).await;
        assert_ne!(revision_of(&paths, &server).await, sent_once);
        assert!(server_pack_info(&paths, &server).await.unwrap().config_kept);

        // Not sending the folder at all says nothing about it either way.
        let mut share = crate::server::share::load(&paths, &server.id)
            .await
            .unwrap()
            .unwrap();
        share.include_config = false;
        crate::server::share::save(&paths, &server.id, &share).await.unwrap();
        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!((info.config_files, info.config_kept), (0, false), "{info:?}");
    }

    /// Changing a config file the server replaces still reaches every player,
    /// because its bytes are what they receive. Changing one they were sent
    /// once reaches nobody — so hashing it would send everybody a pack over an
    /// edit none of them gets, which is what a mod rewriting its own config on
    /// boot does on every restart.
    #[tokio::test]
    async fn the_revision_follows_a_config_file_the_server_replaces_and_not_one_it_sent_once() {
        let (scratch, server) = server_fixture("rev-policy", true).await;
        let paths = scratch.paths.clone();
        let config = paths.server_run(&server.id).join("config/secret.toml");

        choose_config(&paths, &server.id, Policy::Enforce).await;
        let replaced = revision_of(&paths, &server).await;
        tokio::fs::write(&config, b"token = \"rotated\"\n").await.unwrap();
        assert_ne!(
            revision_of(&paths, &server).await,
            replaced,
            "a file the server replaces changed and nobody was told"
        );

        for policy in [Policy::ShipOnce, Policy::Merge] {
            choose_config(&paths, &server.id, policy).await;
            let settled = revision_of(&paths, &server).await;
            tokio::fs::write(&config, format!("token = \"{policy:?}\"\n"))
                .await
                .unwrap();
            assert_eq!(
                revision_of(&paths, &server).await,
                settled,
                "{policy:?} sent every player a pack over an edit none of them gets"
            );
        }

        // Adding a mod still does, whatever is chosen for config: the two are
        // different halves of what a player receives.
        tokio::fs::write(
            paths.server_run(&server.id).join("mods/added.jar"),
            b"added",
        )
        .await
        .unwrap();
        assert_ne!(revision_of(&paths, &server).await, replaced);
    }

    /// A config file the player has no copy of is one the server sends whatever
    /// behaviour is chosen for it — `landings` says so — so the revision has to
    /// say the file exists, or nothing ever tells that player's Geode to come
    /// and fetch it. Its bytes are still not part of the revision: an edit to a
    /// file that is already the player's reaches nobody.
    #[tokio::test]
    async fn a_new_config_file_the_server_sends_once_moves_the_revision() {
        let (scratch, server) = server_fixture("rev-added", true).await;
        let paths = scratch.paths.clone();
        choose_config(&paths, &server.id, Policy::ShipOnce).await;
        let before = revision_of(&paths, &server).await;

        let added = paths.server_run(&server.id).join("config/newmod-client.toml");
        tokio::fs::write(&added, b"first = 1\n").await.unwrap();
        let announced = revision_of(&paths, &server).await;
        assert_ne!(
            announced, before,
            "a settings file no player has a copy of was never announced to them"
        );

        // And its bytes are still not in it, which is the whole reason the
        // line carries no hash.
        tokio::fs::write(&added, b"first = 2\n").await.unwrap();
        assert_eq!(
            revision_of(&paths, &server).await,
            announced,
            "an edit none of them receives sent every player a pack"
        );

        // Taking it away moves it back, because the walk is the pack's own: the
        // files it names are the files it carries.
        tokio::fs::remove_file(&added).await.unwrap();
        assert_eq!(revision_of(&paths, &server).await, before);
    }

    /// What arrives is the server's: every mod, including the one carried as
    /// bytes with no record, is claimed for the server on the player's side.
    #[tokio::test]
    async fn a_server_pack_arrives_as_the_servers_own() {
        let (scratch, mut server) = server_fixture("round", false).await;
        let paths = scratch.paths.clone();
        // Built from a published pack, whose version the index carries.
        server.pack = Some(crate::instance::PackSource::local(
            "Upstream Pack".into(),
            "4.2.0".into(),
            Some("modrinth".into()),
        ));
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();

        // The indexed jars are already on the player's disk with the right
        // hash, so the download step completes offline. Everything else that
        // would reach the network is sent to a port nothing listens on.
        let mut arrived = fresh("srvpack-round-arrived");
        let game = paths.game_dir(&arrived.id);
        let _ = tokio::fs::remove_dir_all(&game).await;
        seed_indexed_mods(&paths, &server, &game).await;

        let missing = import(&offline(), &paths, &mut arrived, &dest, None, &Progress::silent("t"))
            .await
            .unwrap();

        assert_eq!(missing.len(), 2, "{missing:?}");
        for referenced in ["anticheat.jar", "create-cf.jar"] {
            assert!(
                missing.iter().any(|m| m.contains(referenced)),
                "{missing:?}"
            );
        }
        assert_eq!(arrived.name, "Survival Club");
        assert_eq!(arrived.overrides.memory_mb, Some(2048), "the player's own memory setting stays");
        assert!(arrived.pack.is_none(), "labelled as a modpack: {:?}", arrived.pack);

        for file in ["homemade.jar", "sodium-mr.jar", "private-tools.jar"] {
            let owned = arrived
                .content
                .iter()
                .find(|r| r.file_name == file)
                .unwrap_or_else(|| panic!("{file} arrived unclaimed: {:?}", arrived.content));
            assert_eq!(owned.origin, Origin::Server, "{file}");
        }
        assert!(arrived.content.iter().all(|r| r.origin == Origin::Server), "{:?}", arrived.content);

        let mut on_disk: Vec<String> = Vec::new();
        let mut stack = vec![game.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                if entry.path().is_dir() {
                    stack.push(entry.path());
                } else {
                    let rel = entry.path().strip_prefix(&game).unwrap().to_string_lossy().to_string();
                    on_disk.push(rel);
                }
            }
        }
        assert_eq!(
            sorted(on_disk),
            vec![
                "mods/excluded-mr.jar",
                "mods/homemade.jar",
                "mods/private-tools.jar.disabled",
                "mods/server-side.jar",
                "mods/sodium-mr.jar",
                "mods/srv-mr.jar",
            ]
        );
    }

    /// A client that tries to reach the network reaches a port nothing
    /// listens on, so a test that downloads anything fails instead.
    fn offline() -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap()
    }

    /// Put the fixture's Modrinth-indexed jars into a player's `mods/` with
    /// the bytes the server has, so an import finds them there and downloads
    /// nothing.
    async fn seed_indexed_mods(paths: &Paths, server: &Server, game: &Path) {
        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(game.join("mods")).await.unwrap();
        for jar in ["excluded-mr.jar", "sodium-mr.jar", "srv-mr.jar"] {
            tokio::fs::copy(run.join("mods").join(jar), game.join("mods").join(jar))
                .await
                .unwrap();
        }
    }

    /// A jar that declares it runs only on a server used to stay behind. It
    /// reaches the player now, whether it travels as bytes or through the
    /// index, and it is the server's like everything else the pack brings.
    #[tokio::test]
    async fn a_server_side_jar_reaches_the_player() {
        let (scratch, server) = server_fixture("server-side", false).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let mut arrived = fresh("srvpack-server-side-arrived");
        let game = paths.game_dir(&arrived.id);
        let _ = tokio::fs::remove_dir_all(&game).await;
        seed_indexed_mods(&paths, &server, &game).await;
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        import(
            &offline(),
            &paths,
            &mut arrived,
            &dest,
            None,
            &Progress::new(recorder.clone(), "t"),
        )
        .await
        .unwrap();

        // The jar is on disk already, so what shows the index entry was taken
        // is that the import set out to fetch it along with the other two.
        let fetched = recorder
            .events()
            .into_iter()
            .find(|(_, e)| e["stage"] == "Downloading pack contents")
            .map(|(_, e)| e["total"].as_u64());
        assert_eq!(fetched, Some(Some(3)), "an indexed mod was skipped on import");

        let run = paths.server_run(&server.id);
        for jar in ["server-side.jar", "srv-mr.jar"] {
            assert_eq!(
                std::fs::read(game.join("mods").join(jar)).ok(),
                Some(std::fs::read(run.join("mods").join(jar)).unwrap()),
                "{jar} did not arrive as the server has it"
            );
            let record = arrived
                .content
                .iter()
                .find(|r| r.file_name == jar)
                .unwrap_or_else(|| panic!("{jar} has no record: {:?}", arrived.content));
            assert_eq!(record.origin, Origin::Server, "{jar}");
        }
    }

    /// The preview and the pack must agree, or a player is promised a number of
    /// mods that is not what they get.
    #[tokio::test]
    async fn the_preview_counts_the_mods_the_pack_actually_carries() {
        let (scratch, server) = server_fixture("info", false).await;
        let paths = scratch.paths.clone();
        let info = server_pack_info(&paths, &server).await.unwrap();

        assert_eq!(
            info,
            crate::share::host::PackInfo {
                name: "Survival Club".into(),
                mc_version: "1.21.1".into(),
                loader: "fabric".into(),
                loader_version: "0.16.9".into(),
                mods: 8,
                server_mods: 2,
                server_kind: "fabric".into(),
                // This server does not send config, so no settings file is on
                // its way even though `run/config` has one in it.
                config_files: 0,
                config_kept: false,
                config_pushed: false,
                revision: info.revision.clone(),
                server_id: "srv-info".into(),
                // No listing set on this server, so none is described.
                listing: None,
            }
        );
        assert_eq!(info.revision.len(), 64, "{}", info.revision);
        assert!(
            !paths.root.join("server.geodepack").exists(),
            "a preview must not build the pack"
        );

        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();
        assert_eq!(info.mods, mods_in(&dest));
    }

    /// Config was the one thing a player received — and kept receiving on every
    /// update — that nothing in the preview ever mentioned. The count that says
    /// so comes from the same walk the revision hashes, so neither can promise
    /// a file the other does not see: a link under `config/` is in neither, and
    /// a nested file is in both.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_settings_count_and_the_revision_see_the_same_config_files() {
        use std::os::unix::fs::symlink;

        let (scratch, server) = server_fixture("settings-count", true).await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);

        tokio::fs::create_dir_all(run.join("config/nested")).await.unwrap();
        tokio::fs::write(run.join("config/nested/deep.toml"), b"ok = true\n")
            .await
            .unwrap();
        symlink(run.join("server.properties"), run.join("config/linked.toml")).unwrap();

        // `config/secret.toml` and `config/nested/deep.toml`. Not the link.
        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.config_files, 2, "a link under config/ was counted");

        // What the link points at is not a file the pack sends, so changing it
        // tells no player to update either.
        tokio::fs::write(run.join("server.properties"), b"motd=changed\n")
            .await
            .unwrap();
        let unmoved = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(unmoved.config_files, 2);
        assert_eq!(
            unmoved.revision, info.revision,
            "a link's target moved the revision"
        );

        // The nested file is in both halves of the same walk.
        tokio::fs::write(run.join("config/nested/deep.toml"), b"ok = false\n")
            .await
            .unwrap();
        let moved = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(moved.config_files, 2);
        assert_ne!(
            moved.revision, info.revision,
            "a nested config file left the revision alone"
        );

        // And an admin who has stopped sharing is sending nothing, so there is
        // nothing to tell a player about.
        crate::server::share::stop(&paths, &server.id).await.unwrap();
        let stopped = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(stopped.config_files, 0);
    }

    /// Every mod a pack brings, however it brings it: indexed, referenced, or
    /// carried as bytes, client mods among them.
    fn mods_in(archive: &Path) -> usize {
        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(archive, "modrinth.index.json")).unwrap();
        let manifest = read_manifest(archive).unwrap().unwrap();
        index.files.iter().filter(|f| f.path.starts_with("mods/")).count()
            + manifest
                .files
                .iter()
                .filter(|n| n.source.is_some() && n.path.starts_with("mods/"))
                .count()
            + entries(archive)
                .iter()
                .filter(|n| {
                    n.starts_with("overrides/mods/") || n.starts_with("client-overrides/mods/")
                })
                .count()
    }

    /// What an admin leaves lying in `mods/` is not a mod, and what a link in
    /// either folder points at is not the server's to send.
    ///
    /// The allowlist names folders; before this, a link inside one carried
    /// whatever it pointed at under the allowed name — the run directory's
    /// `server.properties`, an SSH key — and a `.bak` of a jar or a jar parked
    /// in a subfolder went too, none of it visible in the admin's Content tab,
    /// and none of it in the count players were shown.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_server_pack_takes_no_strays_and_follows_no_links() {
        use std::os::unix::fs::symlink;

        let (scratch, mut server) = server_fixture("strays", true).await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);

        // Outside the run directory: secrets, and jars that would pass every
        // other check — both sides, with a record saying where they came from.
        let elsewhere = paths.root.join("elsewhere");
        tokio::fs::create_dir_all(elsewhere.join(".ssh")).await.unwrap();
        tokio::fs::write(elsewhere.join("server.properties"), b"rcon.password=secret\n").await.unwrap();
        tokio::fs::write(elsewhere.join(".ssh/id_ed25519"), b"secret key").await.unwrap();
        tokio::fs::write(elsewhere.join("linked-mr.jar"), fabric_jar("linkedmr", "*")).await.unwrap();
        tokio::fs::write(elsewhere.join("linked-cf.jar"), fabric_jar("linkedcf", "*")).await.unwrap();
        tokio::fs::write(elsewhere.join("linked.jar"), fabric_jar("linked", "*")).await.unwrap();
        server.content.push(record("modrinth", "linked-mr.jar", ContentKind::Mod));
        server.content.push(record("curseforge", "linked-cf.jar", ContentKind::Mod));

        let mods = run.join("mods");
        symlink(&elsewhere, mods.join("backup")).unwrap();
        symlink(elsewhere.join("linked-mr.jar"), mods.join("linked-mr.jar")).unwrap();
        symlink(elsewhere.join("linked-cf.jar"), mods.join("linked-cf.jar")).unwrap();
        symlink(elsewhere.join("linked.jar"), mods.join("linked.jar")).unwrap();
        symlink(run.join("server.properties"), mods.join("properties.jar")).unwrap();
        // A copy of a jar, and a jar that runs on both sides parked where no
        // loader looks.
        tokio::fs::write(mods.join("anticheat.jar.bak"), b"private").await.unwrap();
        tokio::fs::create_dir_all(mods.join("old")).await.unwrap();
        tokio::fs::write(mods.join("old/parked.jar"), fabric_jar("parked", "*")).await.unwrap();

        // Client mods go to players by the same rule, so a link there is no
        // way around it, record or not.
        let client_mods = run.join("client-mods");
        tokio::fs::create_dir_all(&client_mods).await.unwrap();
        symlink(elsewhere.join("linked.jar"), client_mods.join("linked.jar")).unwrap();
        symlink(
            elsewhere.join("linked-cf.jar"),
            client_mods.join("linked-client-cf.jar"),
        )
        .unwrap();
        server.content.push(ContentRecord {
            project_id: "linked-client-cf".into(),
            ..record("curseforge", "linked-client-cf.jar", ContentKind::ClientMod)
        });

        // `config/` goes whole when asked for, but not through a link either.
        symlink(&elsewhere, run.join("config/linked")).unwrap();
        symlink(run.join("server.properties"), run.join("config/linked.toml")).unwrap();
        tokio::fs::create_dir_all(run.join("config/nested")).await.unwrap();
        tokio::fs::write(run.join("config/nested/deep.toml"), b"ok = true\n").await.unwrap();

        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();

        assert_eq!(
            sorted(entries(&dest)),
            vec![
                "geode/manifest.json",
                "geode/media/icon.png",
                "modrinth.index.json",
                "overrides/config/nested/deep.toml",
                "overrides/config/secret.toml",
                "overrides/mods/homemade.jar",
                "overrides/mods/private-tools.jar.disabled",
                "overrides/mods/server-side.jar",
            ],
        );
        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
        let indexed = sorted(index.files.iter().map(|f| f.path.clone()).collect());
        assert_eq!(
            indexed,
            vec![
                "mods/excluded-mr.jar",
                "mods/sodium-mr.jar",
                "mods/srv-mr.jar"
            ],
            "a linked jar was indexed"
        );
        let manifest = read_manifest(&dest).unwrap().unwrap();
        let named = sorted(manifest.files.iter().map(|n| n.path.clone()).collect());
        assert_eq!(
            named,
            vec![
                "mods/anticheat.jar",
                "mods/create-cf.jar",
                "mods/server-side.jar",
                "mods/srv-mr.jar"
            ],
            "a linked jar was named"
        );

        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.mods, 8);
        assert_eq!(info.mods, mods_in(&dest));
        // The settings count comes off the same walk, so what a player is told
        // is coming is what the pack holds — links and strays in neither.
        assert_eq!(
            info.config_files,
            entries(&dest)
                .iter()
                .filter(|name| name.starts_with("overrides/config/"))
                .count(),
        );
        assert_eq!(info.config_files, 2);
    }

    /// Pick which of a server's settings files travel, the way the admin's
    /// picker does: these paths, each with the behaviour chosen for it.
    async fn pick(paths: &Paths, id: &str, picks: &[(&str, Policy)]) {
        let mut share = crate::server::share::load(paths, id)
            .await
            .unwrap()
            .expect("a shared server");
        share.include_config = true;
        let picks: Vec<Pick> = picks
            .iter()
            .map(|(path, policy)| Pick {
                path: (*path).to_string(),
                policy: *policy,
            })
            .collect();
        share.pick(&picks).unwrap();
        crate::server::share::save(paths, id, &share).await.unwrap();
    }

    /// The settings files a pack carries, by the path each one lands at in a
    /// player's game directory — which for a client file is not the path the
    /// server keeps it under.
    fn settings_in(archive: &Path) -> Vec<String> {
        sorted(
            entries(archive)
                .iter()
                .filter_map(|name| {
                    name.strip_prefix("overrides/")
                        .or_else(|| name.strip_prefix("client-overrides/"))
                        .filter(|rel| !rel.starts_with("mods/"))
                        .map(str::to_string)
                })
                .collect(),
        )
    }

    /// The admin picks the files and the folder stops going whole, which is
    /// the only way the things in a real `config/` that are not configuration
    /// stay out of a pack: a window layout of pixel positions that is this
    /// machine's and nobody else's, and the `.java` and `.class` files a
    /// decompiled dev build dumped there.
    ///
    /// A picked file travels on its own, without any folder being named, and
    /// stops travelling the moment it is not there to send — so the listing
    /// the admin reads, the count a player is shown, the revision and the zip
    /// are one answer and cannot drift.
    #[tokio::test]
    async fn a_server_sends_the_settings_files_its_admin_picked_and_no_others() {
        let (scratch, server) = small_server(
            "picked",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/sodium.properties", b"renderDistance=8\n".to_vec()),
                (
                    "config/axiom/imgui.ini",
                    b"[Window][###Tools]\nPos=0,22\n".to_vec(),
                ),
                (
                    "config/create/AllConfigs.java",
                    b"class AllConfigs {}\n".to_vec(),
                ),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);

        // Nothing picked is the folder whole, which is what sending a config
        // folder has always meant — the window layout and the decompiled
        // source along with it.
        let whole = pack_now(&paths, &server, "whole").await;
        assert_eq!(
            settings_in(&whole),
            vec![
                "config/axiom/imgui.ini",
                "config/create/AllConfigs.java",
                "config/sodium.properties",
            ],
        );
        assert!(settings_picker(&paths, &server.id).await.unwrap().whole_config);

        pick(
            &paths,
            &server.id,
            &[("config/sodium.properties", Policy::Merge)],
        )
        .await;
        let picked = pack_now(&paths, &server, "picked").await;
        assert_eq!(settings_in(&picked), vec!["config/sodium.properties"]);
        let picker = settings_picker(&paths, &server.id).await.unwrap();
        assert!(!picker.whole_config);
        assert!(picker.refused.is_empty(), "{:?}", picker.refused);
        // Every file the server has is still a row — the admin cannot pick
        // what they were never shown — and exactly one of them is going.
        assert_eq!(picker.files.len(), 3, "{:?}", picker.files);
        assert_eq!(
            picker
                .files
                .iter()
                .filter(|row| row.sent)
                .map(|row| row.path.clone())
                .collect::<Vec<_>>(),
            vec!["config/sodium.properties"],
        );

        // The count the join preview promises is the number of files in the
        // zip, because one function answered both.
        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.config_files, 1);
        assert_eq!(info.config_files, settings_in(&picked).len());
        let before = info.revision;

        // And a picked file that is no longer there to send stops travelling,
        // says so on its row, and moves the revision — a player holding a
        // copy of a file the server has stopped sending is told by the only
        // thing that tells them anything.
        tokio::fs::remove_file(run.join("config/sodium.properties"))
            .await
            .unwrap();
        let gone = pack_now(&paths, &server, "gone").await;
        assert_eq!(settings_in(&gone), Vec::<String>::new());
        let picker = settings_picker(&paths, &server.id).await.unwrap();
        assert_eq!(picker.refused.len(), 1, "{:?}", picker.refused);
        assert_eq!(picker.refused[0].path, "config/sodium.properties");
        assert!(
            picker.refused[0].why.contains("no such file"),
            "{}",
            picker.refused[0].why
        );
        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.config_files, 0);
        assert_ne!(info.revision, before);
    }

    /// The paths a pack may never carry are refused where the pack is built
    /// and not only where the picking is done, because a bug in a picker must
    /// not be able to ship `ops.json` — and each is a reason on its row
    /// rather than a row that silently does nothing, because an admin whose
    /// pick quietly vanished concludes sharing is broken.
    #[tokio::test]
    async fn a_pick_outside_the_two_settings_folders_ships_nothing_and_says_why() {
        let (scratch, server) = server_fixture("refused", true).await;
        let paths = scratch.paths.clone();
        let never = [
            "server.properties",
            "ops.json",
            "whitelist.json",
            "banned-players.json",
            "banned-ips.json",
            "usercache.json",
            "eula.txt",
            "logs/latest.log",
            "crash-reports",
            "world/level.dat",
            "plugins/p.jar",
            "share.json",
            "../elsewhere/secret.txt",
            "config/../ops.json",
        ];

        // Refused in front of the admin, so none of them is ever written down
        // as a pick in the first place.
        let mut share = crate::server::share::load(&paths, &server.id)
            .await
            .unwrap()
            .expect("a shared server");
        for path in never {
            let err = share
                .pick(&[Pick {
                    path: path.to_string(),
                    policy: Policy::Enforce,
                }])
                .unwrap_err()
                .to_string();
            assert!(err.contains(path), "{path}: {err}");
        }

        // And refused again where the pack is built, so a hand-edited
        // `share.json` naming every one of them ships none of them.
        share.include_config = true;
        share.picked = never.iter().map(|path| path.to_string()).collect();
        crate::server::share::save(&paths, &server.id, &share)
            .await
            .unwrap();
        let dest = pack_now(&paths, &server, "refused").await;
        assert_eq!(settings_in(&dest), Vec::<String>::new());
        assert_eq!(
            sorted(entries(&dest)),
            vec![
                "geode/manifest.json",
                "geode/media/icon.png",
                "modrinth.index.json",
                "overrides/mods/homemade.jar",
                "overrides/mods/private-tools.jar.disabled",
                "overrides/mods/server-side.jar",
            ],
        );
        assert_eq!(server_pack_info(&paths, &server).await.unwrap().config_files, 0);

        let picker = settings_picker(&paths, &server.id).await.unwrap();
        assert_eq!(
            sorted(picker.refused.iter().map(|row| row.path.clone()).collect()),
            sorted(never.iter().map(|path| path.to_string()).collect()),
        );
        for row in &picker.refused {
            assert!(!row.why.trim().is_empty(), "{} has no reason", row.path);
        }
        let ops = picker
            .refused
            .iter()
            .find(|row| row.path == "ops.json")
            .expect("the player list is a row with a reason on it");
        assert!(
            ops.why.contains("config") && ops.why.contains("client-files"),
            "{}",
            ops.why
        );

        // And the folders are an allowlist in their own right, not only
        // because the walk that looks for settings files never offers
        // anything else: a pick outside them carries nothing even when it is
        // handed the path of a file that is really there.
        assert!(!picked_out(
            &["ops.json".to_string()],
            Path::new("ops.json")
        ));
        assert!(picked_out(
            &["config".to_string()],
            Path::new("config/secret.toml")
        ));
        assert!(picked_out(
            &[format!("{}/options.txt", instance::modpack::CLIENT_FILES)],
            Path::new("client-files/options.txt")
        ));
    }

    /// A link anywhere along a picked path is refused, because an allowlist of
    /// folders says nothing about where a link inside one of them points: a
    /// `config/linked` pointing at the directory above would carry
    /// `server.properties` under an allowed name, and nobody looks at this
    /// pack before it goes.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_anywhere_along_a_picked_settings_path_is_refused() {
        use std::os::unix::fs::symlink;

        let (scratch, server) = small_server(
            "picked-link",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/real/ok.properties", b"a=1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        let elsewhere = paths.root.join("elsewhere");
        tokio::fs::create_dir_all(&elsewhere).await.unwrap();
        tokio::fs::write(elsewhere.join("secret.properties"), b"token=hunter2\n")
            .await
            .unwrap();
        // A folder along the way that is a link, and the file itself.
        symlink(&elsewhere, run.join("config/linked")).unwrap();
        symlink(
            elsewhere.join("secret.properties"),
            run.join("config/real/linked.properties"),
        )
        .unwrap();

        pick(
            &paths,
            &server.id,
            &[
                ("config/linked/secret.properties", Policy::Enforce),
                ("config/real/linked.properties", Policy::Enforce),
                ("config/real/ok.properties", Policy::Enforce),
            ],
        )
        .await;

        let dest = pack_now(&paths, &server, "picked-link").await;
        assert_eq!(settings_in(&dest), vec!["config/real/ok.properties"]);
        let picker = settings_picker(&paths, &server.id).await.unwrap();
        assert_eq!(
            sorted(picker.refused.iter().map(|row| row.path.clone()).collect()),
            vec![
                "config/linked/secret.properties",
                "config/real/linked.properties",
            ],
        );
        for row in &picker.refused {
            assert!(row.why.contains("link"), "{}: {}", row.path, row.why);
        }
    }

    /// Minecraft's own settings are the player's files and exist nowhere on a
    /// server's disk, so a server keeps a copy in `client-files/` — a folder
    /// no loader reads, like the one beside it for client mods. The pack
    /// carries it under `client-overrides/`, so a launcher installing this
    /// pack on a server leaves it out, and it lands at the top of a player's
    /// game directory, which is where Minecraft keeps it.
    ///
    /// Sent once, and that is not a matter of taste: the game rewrites the
    /// whole file in its own key order on every exit, so a copy replaced on
    /// every update would move a player's keybinds under them between one
    /// session and the next.
    #[tokio::test]
    async fn minecrafts_own_settings_travel_in_the_folder_no_loader_reads() {
        // A real one's shape: the first colon is the separator and the values
        // have colons of their own.
        let theirs = "version:3955\nfov:70.0\nkey_iris.keybind.reload:key.keyboard.r:CONTROL\n";
        let (scratch, server) = small_server(
            "client-files",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("client-files/options.txt", theirs.as_bytes().to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        pick(
            &paths,
            &server.id,
            &[("client-files/options.txt", Policy::ShipOnce)],
        )
        .await;

        let dest = pack_now(&paths, &server, "client-files").await;
        assert!(
            entries(&dest).contains(&"client-overrides/options.txt".to_string()),
            "{:?}",
            entries(&dest)
        );
        assert_eq!(settings_in(&dest), vec!["options.txt"]);
        // The pack says what happens to it by the path it lands at, because
        // on a player's disk there is no folder above it to name.
        let half = read_manifest(&dest)
            .unwrap()
            .unwrap()
            .settings
            .expect("a settings half");
        let named = half
            .paths
            .iter()
            .find(|named| named.path == "options.txt")
            .expect("the pack says what happens to the file it sends");
        assert_eq!(named.policy, Policy::ShipOnce);
        // Stamped with the push the admin picked it at, which is what sends it
        // to the players already joined: a behaviour in force since before
        // counting began would leave every one of them reading as caught up
        // with a file they have never seen.
        assert!(half.at > 0);
        assert_eq!(named.from, half.at);
        // And the picker tells the admin where it lands, since the path they
        // picked is not the path the player gets.
        let row = settings_picker(&paths, &server.id)
            .await
            .unwrap()
            .files
            .into_iter()
            .find(|row| row.path == "client-files/options.txt")
            .expect("the client file is a row");
        assert_eq!(row.lands, "options.txt");
        assert_eq!(row.size, theirs.len() as u64);

        let mut player = join_now(&paths, &server, "client-files-player", &[]).await;
        let mine = paths.game_dir(&player.id).join("options.txt");
        assert_eq!(std::fs::read_to_string(&mine).unwrap(), theirs);

        // Theirs from then on, however the server's own copy moves.
        tokio::fs::write(&mine, "version:3955\nfov:90.0\n").await.unwrap();
        tokio::fs::write(
            run.join("client-files/options.txt"),
            "version:3955\nfov:30.0\n",
        )
        .await
        .unwrap();
        let pack = pack_now(&paths, &server, "client-files-again").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&mine).unwrap(),
            "version:3955\nfov:90.0\n",
            "the server replaced a client file it had already sent once"
        );

        // And a pack never leaves a client file unsaid, whatever the share
        // file holds: one named as travelling with no behaviour chosen for it
        // still arrives as sent once. Unsaid means written whole on every
        // update, which is the one thing this file must never be.
        let mut share = crate::server::share::load(&paths, &server.id)
            .await
            .unwrap()
            .expect("a shared server");
        share.settings = Default::default();
        share.picked = vec!["client-files/options.txt".to_string()];
        crate::server::share::save(&paths, &server.id, &share)
            .await
            .unwrap();
        let dest = pack_now(&paths, &server, "client-files-unsaid").await;
        let half = read_manifest(&dest)
            .unwrap()
            .unwrap()
            .settings
            .expect("a settings half");
        let named = half
            .paths
            .iter()
            .find(|named| named.path == "options.txt")
            .expect("the pack says what happens to the file it sends");
        assert_eq!((named.policy, named.from), (Policy::ShipOnce, 0));
    }

    /// Keeping a file in sync means splicing in the settings the admin pushes,
    /// and nothing can be pushed out of a file no setting can be found inside.
    /// So the listing says which rows can do it, and a file in a folder kept
    /// in sync whose format Geode cannot read one setting at a time is shown
    /// as what it will actually do — sent once. A row under a label reading
    /// "only the settings I change" that quietly did something else is the one
    /// thing this listing must not show.
    #[tokio::test]
    async fn a_file_no_setting_can_be_found_in_is_not_offered_keep_in_sync() {
        let (scratch, server) = small_server(
            "sniff",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/sodium.properties", b"renderDistance=8\n".to_vec()),
                // A window layout: sections no reader of this takes, and
                // per-machine state nobody else wants anyway.
                (
                    "config/axiom/imgui.ini",
                    b"[Window][###Tools]\nPos=0,22\n".to_vec(),
                ),
                // Not text at all.
                ("config/icon.png", vec![0x89, b'P', b'N', b'G', 0x00, 0xff]),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        choose_config(&paths, &server.id, Policy::Merge).await;

        let picker = settings_picker(&paths, &server.id).await.unwrap();
        let row = |path: &str| {
            picker
                .files
                .iter()
                .find(|row| row.path == path)
                .unwrap_or_else(|| panic!("{path} is not in the listing"))
        };
        assert!(row("config/sodium.properties").can_sync);
        assert_eq!(row("config/sodium.properties").policy, Policy::Merge);
        assert_eq!(row("config/sodium.properties").size, 17);
        for path in ["config/axiom/imgui.ini", "config/icon.png"] {
            assert!(!row(path).can_sync, "{path} was offered keeping in sync");
            assert_eq!(
                row(path).policy,
                Policy::ShipOnce,
                "{path} was shown as something it cannot do"
            );
        }

        // And there is nothing to push out of either of them, said with the
        // reason rather than left out of the admin's screen.
        let review = crate::settings::push::review(&paths, &server.id)
            .await
            .unwrap();
        assert!(review.moved.is_empty(), "{:?}", review.moved);
        assert_eq!(
            sorted(review.unreadable.iter().map(|one| one.path.clone()).collect()),
            vec!["config/axiom/imgui.ini", "config/icon.png"],
        );
    }

    /// A link named like a record, beside a real turned-off copy of that mod:
    /// the pack carries the copy, and says nothing about where the link goes —
    /// not its hash, not its size, not that the mod is on.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_beside_a_turned_off_twin_is_not_read_through() {
        use std::os::unix::fs::symlink;

        let (scratch, mut server) = server_fixture("twin", false).await;
        let paths = scratch.paths.clone();
        let mods = paths.server_run(&server.id).join("mods");
        let elsewhere = paths.root.join("elsewhere");
        tokio::fs::create_dir_all(&elsewhere).await.unwrap();
        tokio::fs::write(elsewhere.join("secret.txt"), b"hunter2\n")
            .await
            .unwrap();
        let secret_sha1 = crate::net::sha1_file(&elsewhere.join("secret.txt"))
            .await
            .unwrap();

        symlink(elsewhere.join("secret.txt"), mods.join("twin-mr.jar")).unwrap();
        tokio::fs::write(mods.join("twin-mr.jar.disabled"), fabric_jar("twinmr", "*"))
            .await
            .unwrap();
        server
            .content
            .push(record("modrinth", "twin-mr.jar", ContentKind::Mod));
        symlink(elsewhere.join("secret.txt"), mods.join("twin-cf.jar")).unwrap();
        tokio::fs::write(mods.join("twin-cf.jar.disabled"), fabric_jar("twincf", "*"))
            .await
            .unwrap();
        server
            .content
            .push(record("curseforge", "twin-cf.jar", ContentKind::Mod));

        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
        assert!(
            index.files.iter().all(|f| f.path != "mods/twin-mr.jar"
                && f.hashes.get("sha1") != Some(&secret_sha1)),
            "the link's target was indexed: {:?}",
            index.files
        );
        assert!(entries(&dest).contains(&"overrides/mods/twin-mr.jar.disabled".to_string()));

        let manifest = read_manifest(&dest).unwrap().unwrap();
        let twin = manifest
            .files
            .iter()
            .find(|n| n.path == "mods/twin-cf.jar")
            .expect("the real copy is named");
        assert!(twin.disabled, "the link made a turned-off mod read as on");

        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.mods, mods_in(&dest), "the count and the pack disagree");
    }

    /// The same rule one level up: a `mods/`, `client-mods/` or `config/` that
    /// is itself a link is not followed, and the count says so rather than
    /// promising its mods.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_server_pack_does_not_follow_a_linked_folder() {
        use std::os::unix::fs::symlink;

        let (scratch, server) = server_fixture("linkdir", true).await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        let elsewhere = paths.root.join("elsewhere");
        tokio::fs::create_dir_all(&elsewhere).await.unwrap();
        tokio::fs::create_dir_all(run.join("client-mods"))
            .await
            .unwrap();
        tokio::fs::write(
            run.join("client-mods/minimap.jar"),
            fabric_jar("minimap", "client"),
        )
        .await
        .unwrap();
        for folder in ["mods", "config", "client-mods"] {
            tokio::fs::rename(run.join(folder), elsewhere.join(folder)).await.unwrap();
            symlink(elsewhere.join(folder), run.join(folder)).unwrap();
        }

        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t")).await.unwrap();

        assert_eq!(
            sorted(entries(&dest)),
            vec!["geode/manifest.json", "geode/media/icon.png", "modrinth.index.json"],
        );
        assert_eq!(mods_in(&dest), 0);
        assert_eq!(server_pack_info(&paths, &server).await.unwrap().mods, 0);
    }

    // -----------------------------------------------------------------------
    // Client, server and both
    // -----------------------------------------------------------------------

    use crate::instance::jar::Side;

    /// Client mods beside the fixture's mods: one added by hand, one from
    /// Modrinth and one from CurseForge, each its own project. Every jar says
    /// it runs on both sides, so only the folder makes them client mods.
    async fn add_client_mods(paths: &Paths, server: &mut Server) {
        let dir = paths.server_run(&server.id).join("client-mods");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        for (name, id) in [
            ("hand.jar", "hand"),
            ("mr-client.jar", "mrclient"),
            ("cf-client.jar", "cfclient"),
        ] {
            tokio::fs::write(dir.join(name), fabric_jar(id, "*"))
                .await
                .unwrap();
        }
        server.content.push(ContentRecord {
            project_id: "mr-client".into(),
            ..record("modrinth", "mr-client.jar", ContentKind::ClientMod)
        });
        server.content.push(ContentRecord {
            project_id: "cf-client".into(),
            ..record("curseforge", "cf-client.jar", ContentKind::ClientMod)
        });
    }

    /// A player's game has no Client mods folder. What the server keeps there
    /// arrives as mods, each carried the way any mod is — bytes under
    /// `client-overrides/`, which a launcher installing on a server skips, an
    /// index entry no server installs, or a reference — and the pack never
    /// names the server's folder.
    #[tokio::test]
    async fn a_server_pack_hands_client_mods_to_players_as_mods() {
        let (scratch, mut server) = server_fixture("client-mods", false).await;
        let paths = scratch.paths.clone();
        add_client_mods(&paths, &mut server).await;
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let names = entries(&dest);
        assert!(
            names.contains(&"client-overrides/mods/hand.jar".to_string()),
            "{names:?}"
        );

        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
        let env = index
            .files
            .iter()
            .find(|f| f.path == "mods/mr-client.jar")
            .and_then(|f| f.env.as_ref())
            .expect("the Modrinth client mod is indexed as a mod");
        assert_eq!(
            (env.client.as_str(), env.server.as_str()),
            ("required", "unsupported")
        );

        let manifest = read_manifest(&dest).unwrap().unwrap();
        let cf = manifest
            .note("mods/cf-client.jar")
            .expect("the CurseForge client mod is referenced as a mod");
        assert!(cf.source.is_some(), "{cf:?}");
        assert_eq!(cf.side, Some(Side::Client));

        let named = names
            .iter()
            .chain(index.files.iter().map(|f| &f.path))
            .chain(manifest.files.iter().map(|n| &n.path));
        for name in named {
            assert!(
                !name.contains("client-mods"),
                "{name} names the server's folder"
            );
        }

        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.mods, 11);
        assert_eq!(info.mods, mods_in(&dest));
        // A client mod is not one only the server needs.
        assert_eq!(info.server_mods, 2);
    }

    /// One mods folder on the player's side means one copy of each mod. The
    /// server's `mods/` wins: a client mod goes only when neither its file
    /// name nor its project is already going from there.
    #[tokio::test]
    async fn a_mod_in_both_folders_is_sent_once() {
        let (scratch, mut server) = server_fixture("both-folders", false).await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(run.join("client-mods"))
            .await
            .unwrap();
        tokio::fs::write(run.join("client-mods/homemade.jar"), b"the client copy")
            .await
            .unwrap();
        tokio::fs::write(run.join("mods/p-2.jar"), b"p two")
            .await
            .unwrap();
        tokio::fs::write(run.join("client-mods/p-1.jar"), b"p one")
            .await
            .unwrap();
        server.content.push(ContentRecord {
            project_id: "P".into(),
            ..record("modrinth", "p-2.jar", ContentKind::Mod)
        });
        server.content.push(ContentRecord {
            project_id: "P".into(),
            ..record("modrinth", "p-1.jar", ContentKind::ClientMod)
        });

        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let names = entries(&dest);
        assert!(
            !names.iter().any(|n| n.starts_with("client-overrides/")),
            "a client mod went beside the mod of the same name: {names:?}"
        );
        assert_eq!(
            read_entry(&dest, "overrides/mods/homemade.jar"),
            std::fs::read(run.join("mods/homemade.jar")).unwrap()
        );
        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
        let indexed: Vec<&str> = index.files.iter().map(|f| f.path.as_str()).collect();
        assert!(indexed.contains(&"mods/p-2.jar"), "{indexed:?}");
        assert!(
            !indexed.contains(&"mods/p-1.jar"),
            "the project went twice: {indexed:?}"
        );

        let info = server_pack_info(&paths, &server).await.unwrap();
        assert_eq!(info.mods, 9);
        assert_eq!(info.mods, mods_in(&dest));
    }

    /// Updating a mod whose file name stays the same, after turning it off,
    /// leaves it on disk both on and off. It is still one mod: indexed once,
    /// referenced once, and noted once, as the copy that is on.
    #[tokio::test]
    async fn a_mod_on_disk_both_on_and_off_is_named_once() {
        let (scratch, server) = server_fixture("on-and-off", false).await;
        let paths = scratch.paths.clone();
        let mods = paths.server_run(&server.id).join("mods");
        for (name, bytes) in [
            ("sodium-mr.jar.disabled", b"older from modrinth".to_vec()),
            ("create-cf.jar.disabled", b"older from curseforge".to_vec()),
            (
                "server-side.jar.disabled",
                fabric_jar("serverside", "server"),
            ),
        ] {
            tokio::fs::write(mods.join(name), bytes).await.unwrap();
        }

        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
        let indexed = index
            .files
            .iter()
            .filter(|f| f.path == "mods/sodium-mr.jar")
            .count();
        assert_eq!(indexed, 1, "{:?}", index.files);

        let manifest = read_manifest(&dest).unwrap().unwrap();
        for path in ["mods/create-cf.jar", "mods/server-side.jar"] {
            let notes: Vec<&FileNote> = manifest.files.iter().filter(|n| n.path == path).collect();
            assert_eq!(notes.len(), 1, "{path}: {:?}", manifest.files);
            assert!(
                !notes[0].disabled,
                "{path} was noted as the copy that is off"
            );
        }
    }

    /// Whatever carries a mod only the server needs, the manifest says so: a
    /// jar that declares it and goes as bytes, and a jar that declares nothing
    /// useful but is listed that way and goes through the index. The preview's
    /// count of them is the same number.
    #[tokio::test]
    async fn a_server_pack_marks_what_only_the_server_needs() {
        let (scratch, mut server) = server_fixture("server-notes", false).await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        tokio::fs::write(
            run.join("mods/listed-srv.jar"),
            fabric_jar("listedsrv", "*"),
        )
        .await
        .unwrap();
        server.content.push(ContentRecord {
            project_id: "listed-srv".into(),
            side: Some(Side::Server),
            ..record("modrinth", "listed-srv.jar", ContentKind::Mod)
        });

        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        assert!(entries(&dest).contains(&"overrides/mods/server-side.jar".to_string()));
        let manifest = read_manifest(&dest).unwrap().unwrap();
        let bytes = manifest
            .note("mods/server-side.jar")
            .expect("the server-side jar carried as bytes has a note");
        assert_eq!(
            (bytes.side, bytes.source.is_none()),
            (Some(Side::Server), true)
        );
        assert_eq!(
            manifest.note("mods/listed-srv.jar").and_then(|n| n.side),
            Some(Side::Server)
        );

        let index: crate::instance::modpack::MrIndex =
            serde_json::from_slice(&read_entry(&dest, "modrinth.index.json")).unwrap();
        let env = index
            .files
            .iter()
            .find(|f| f.path == "mods/listed-srv.jar")
            .and_then(|f| f.env.as_ref())
            .expect("the listed server mod is indexed");
        assert_eq!(
            (env.client.as_str(), env.server.as_str()),
            ("optional", "required")
        );

        let noted = manifest
            .files
            .iter()
            .filter(|n| n.side == Some(Side::Server))
            .count();
        assert_eq!(noted, 3, "{:?}", manifest.files);
        assert_eq!(
            server_pack_info(&paths, &server).await.unwrap().server_mods,
            noted
        );
    }

    /// On the player's side a client mod is simply one of the server's mods,
    /// owned like the rest; one the player has to fetch is named.
    #[tokio::test]
    async fn a_player_gets_client_mods_as_the_servers_own_mods() {
        let (scratch, mut server) = server_fixture("client-mods-arrive", false).await;
        let paths = scratch.paths.clone();
        add_client_mods(&paths, &mut server).await;
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let mut arrived = fresh("srvpack-client-mods-arrived");
        let game = paths.game_dir(&arrived.id);
        let _ = tokio::fs::remove_dir_all(&game).await;
        seed_indexed_mods(&paths, &server, &game).await;
        let run = paths.server_run(&server.id);
        tokio::fs::copy(
            run.join("client-mods/mr-client.jar"),
            game.join("mods/mr-client.jar"),
        )
        .await
        .unwrap();

        let missing = import(
            &offline(),
            &paths,
            &mut arrived,
            &dest,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(
            missing.iter().any(|m| m.contains("cf-client.jar")),
            "{missing:?}"
        );

        for jar in ["hand.jar", "mr-client.jar"] {
            assert!(
                game.join("mods").join(jar).is_file(),
                "{jar} did not arrive in mods/"
            );
            let record = arrived
                .content
                .iter()
                .find(|r| r.file_name == jar)
                .unwrap_or_else(|| panic!("{jar} has no record: {:?}", arrived.content));
            assert_eq!(
                (record.kind, record.origin),
                (ContentKind::Mod, Origin::Server),
                "{jar}"
            );
        }
        assert!(!game.join("client-mods").exists());
    }

    // -----------------------------------------------------------------------
    // A player's choice about server-side mods
    // -----------------------------------------------------------------------

    /// What a server's pack brings is read off the pack: an index entry a
    /// client installs, a reference in the state the server had it, files
    /// carried as bytes on or off and from either overrides folder, and the
    /// side each note gives. Nothing a client never installs, and nothing
    /// outside a content folder, counts.
    #[tokio::test]
    async fn what_a_server_pack_brings_is_read_from_the_pack() {
        let (scratch, mut server) = server_fixture("arrivals", false).await;
        let paths = scratch.paths.clone();
        add_client_mods(&paths, &mut server).await;
        let mods = paths.server_run(&server.id).join("mods");
        tokio::fs::rename(
            mods.join("anticheat.jar"),
            mods.join("anticheat.jar.disabled"),
        )
        .await
        .unwrap();
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();
        let manifest = read_manifest(&dest).unwrap().unwrap();

        let found = arrivals(&dest, &manifest).unwrap();
        let get = |name: &str| {
            found
                .get(&(ContentKind::Mod, name.to_string()))
                .cloned()
                .unwrap_or_else(|| panic!("{name} is not an arrival: {found:?}"))
        };
        let project = |provider: &str| Some((provider.to_string(), "proj-1".to_string()));
        assert_eq!(
            get("sodium-mr.jar"),
            Arrival {
                off: false,
                side: None,
                project: project("modrinth")
            }
        );
        assert_eq!(
            get("anticheat.jar"),
            Arrival {
                off: true,
                side: None,
                project: project("curseforge")
            }
        );
        assert_eq!(
            get("create-cf.jar"),
            Arrival {
                off: false,
                side: None,
                project: project("curseforge")
            }
        );
        assert_eq!(
            get("private-tools.jar"),
            Arrival {
                off: true,
                side: None,
                project: None
            }
        );
        assert_eq!(
            get("homemade.jar"),
            Arrival {
                off: false,
                side: None,
                project: None
            }
        );
        assert_eq!(
            get("hand.jar"),
            Arrival {
                off: false,
                side: Some(Side::Client),
                project: None
            }
        );
        assert_eq!(get("server-side.jar").side, Some(Side::Server));
        assert_eq!(get("srv-mr.jar").side, Some(Side::Server));
        assert_eq!(found.len(), 11, "{found:?}");

        // A pack Geode would not write: an index entry no client installs,
        // and bytes outside a content folder or escaping it.
        let hand_built = paths.root.join("hand-built.geodepack");
        let index = serde_json::json!({
            "formatVersion": 1, "game": "minecraft", "versionId": "1", "name": "S",
            "files": [
                {
                    "path": "mods/server-only.jar",
                    "hashes": { "sha1": "0" },
                    "env": { "client": "unsupported", "server": "required" },
                    "downloads": ["https://cdn.modrinth.com/data/P/versions/V/server-only.jar"],
                },
                {
                    "path": "mods/installed.jar",
                    "hashes": { "sha1": "0" },
                    "downloads": ["https://cdn.modrinth.com/data/Q/versions/V/installed.jar"],
                },
            ],
            "dependencies": { "minecraft": "1.21.1" }
        });
        write_pack(
            &hand_built,
            &index,
            &manifest,
            &[
                ("overrides/somewhere/unknown.jar", b"x"),
                ("overrides/../mods/escape.jar", b"x"),
                ("overrides/mods/nested/deep.jar", b"x"),
                ("overrides/config/mods/config.jar", b"x"),
                ("overrides/mods/notes.txt", b"x"),
            ],
        );
        let found = arrivals(&hand_built, &manifest).unwrap();
        let mut names: Vec<&str> = found.keys().map(|(_, name)| name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "anticheat.jar",
                "cf-client.jar",
                "create-cf.jar",
                "installed.jar"
            ],
            "{found:?}"
        );
        assert_eq!(
            found[&(ContentKind::Mod, "installed.jar".to_string())].project,
            Some(("modrinth".into(), "Q".into()))
        );
    }

    /// A joined instance can hold the player's own mods beside the server's.
    /// Only what the pack brought becomes the server's: an untracked jar the
    /// player dropped in stays untracked, and their own installed mod stays
    /// theirs to delete.
    #[tokio::test]
    async fn a_server_pack_claims_only_what_it_brought() {
        let (scratch, server) = server_fixture("claims-only", false).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let mut arrived = fresh("srvpack-claims-only-arrived");
        let game = paths.game_dir(&arrived.id);
        let _ = tokio::fs::remove_dir_all(&game).await;
        seed_indexed_mods(&paths, &server, &game).await;
        let mods = game.join("mods");
        tokio::fs::write(mods.join("own.jar"), fabric_jar("own", "*"))
            .await
            .unwrap();
        tokio::fs::write(mods.join("own-mr.jar"), fabric_jar("ownmr", "*"))
            .await
            .unwrap();
        arrived
            .content
            .push(record("modrinth", "own-mr.jar", ContentKind::Mod));

        import(
            &offline(),
            &paths,
            &mut arrived,
            &dest,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert!(
            arrived.content.iter().all(|r| r.file_name != "own.jar"),
            "the player's untracked jar was claimed: {:?}",
            arrived.content
        );
        let own = arrived
            .content
            .iter()
            .find(|r| r.file_name == "own-mr.jar")
            .expect("the player's own record is kept");
        assert_eq!(own.origin, Origin::User);
        crate::instance::content::remove_in(
            &mods,
            &mut arrived.content,
            ContentKind::Mod,
            "own-mr.jar",
        )
        .await
        .expect("the player's own mod stays theirs to delete");
    }

    /// Import a server fixture's pack into a fresh instance, offline, with
    /// server-side mods on or off.
    async fn import_choosing(tag: &str, server_side_mods_off: bool) -> (Scratch, Instance) {
        let (scratch, server) = server_fixture(tag, false).await;
        let paths = scratch.paths.clone();
        let dest = paths.root.join("server.geodepack");
        export_server(&paths, &server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();

        let mut arrived = fresh(&format!("srvpack-{tag}-arrived"));
        arrived.server_side_mods_off = server_side_mods_off;
        let game = paths.game_dir(&arrived.id);
        let _ = tokio::fs::remove_dir_all(&game).await;
        seed_indexed_mods(&paths, &server, &game).await;
        import(
            &offline(),
            &paths,
            &mut arrived,
            &dest,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        (scratch, arrived)
    }

    /// A player who keeps server-side mods off still gets them, turned off,
    /// however they travel — and every other mod arrives on. A player who
    /// keeps them gets them on.
    #[tokio::test]
    async fn a_player_who_turned_server_side_mods_off_gets_them_off() {
        for off in [true, false] {
            let (scratch, arrived) = import_choosing(&format!("side-off-{off}"), off).await;
            let paths = scratch.paths.clone();
            let mods = paths.game_dir(&arrived.id).join("mods");

            for jar in ["server-side.jar", "srv-mr.jar"] {
                let disabled = format!("{jar}.disabled");
                assert_eq!(
                    (mods.join(jar).is_file(), mods.join(&disabled).is_file()),
                    (!off, off),
                    "{jar} with server-side mods off: {off}"
                );
                let record = arrived
                    .content
                    .iter()
                    .find(|r| r.file_name == jar)
                    .unwrap_or_else(|| panic!("{jar} has no record: {:?}", arrived.content));
                assert_eq!(
                    (record.origin, record.side, record.server_off),
                    (Origin::Server, Some(Side::Server), false),
                    "{jar}"
                );
            }
            assert!(mods.join("homemade.jar").is_file(), "off: {off}");
            assert!(mods.join("sodium-mr.jar").is_file(), "off: {off}");
        }
    }

    /// A mod the admin turned off is off for the player too, and its record
    /// says the server did it — so turning server-side mods on later does not
    /// undo the admin's choice.
    #[tokio::test]
    async fn a_mod_the_server_turned_off_arrives_off_and_says_so() {
        let (scratch, arrived) = import_choosing("server-off", false).await;
        let paths = scratch.paths.clone();
        let mods = paths.game_dir(&arrived.id).join("mods");

        assert!(mods.join("private-tools.jar.disabled").is_file());
        assert!(!mods.join("private-tools.jar").exists());
        let record = arrived
            .content
            .iter()
            .find(|r| r.file_name == "private-tools.jar")
            .expect("the turned-off jar has a record");
        assert!(record.server_off, "{record:?}");
        assert!(
            arrived
                .content
                .iter()
                .filter(|r| r.file_name != "private-tools.jar")
                .all(|r| !r.server_off),
            "{:?}",
            arrived.content
        );
    }

    // -----------------------------------------------------------------------
    // Revisions, and updating from a server
    // -----------------------------------------------------------------------

    async fn revision_of(paths: &Paths, server: &Server) -> String {
        server_pack_info(paths, server).await.unwrap().revision
    }

    /// A player's Geode updates when the revision changes, so it has to change
    /// for everything that reaches a player's game and for nothing else — or
    /// players miss an admin's change, or download the same pack on every Play.
    #[tokio::test]
    async fn the_revision_moves_only_when_what_players_get_changes() {
        let (scratch, mut server) = server_fixture("revision", false).await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        let share = |include_config| ServerShare {
            code: crate::share::code::Code::generate(),
            include_config,
            settings: Default::default(),
            picked: Vec::new(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
        };

        let mut last = revision_of(&paths, &server).await;
        assert_eq!(
            revision_of(&paths, &server).await,
            last,
            "the same server twice"
        );
        let mut moved = |now: String, what: &str| {
            assert_ne!(now, last, "{what} left the revision alone");
            last = now;
        };

        tokio::fs::write(run.join("mods/added.jar"), b"version one")
            .await
            .unwrap();
        moved(revision_of(&paths, &server).await, "adding a jar by hand");

        tokio::fs::write(run.join("mods/added.jar"), b"version two")
            .await
            .unwrap();
        moved(
            revision_of(&paths, &server).await,
            "new bytes of the same size",
        );

        tokio::fs::rename(
            run.join("mods/homemade.jar"),
            run.join("mods/homemade.jar.disabled"),
        )
        .await
        .unwrap();
        moved(revision_of(&paths, &server).await, "turning a mod off");

        let sodium = server
            .content
            .iter_mut()
            .find(|r| r.file_name == "sodium-mr.jar")
            .unwrap();
        sodium.version_id = "ver-2".into();
        moved(
            revision_of(&paths, &server).await,
            "a new version of a recorded mod",
        );

        tokio::fs::create_dir_all(run.join("client-mods"))
            .await
            .unwrap();
        tokio::fs::rename(
            run.join("mods/added.jar"),
            run.join("client-mods/added.jar"),
        )
        .await
        .unwrap();
        moved(
            revision_of(&paths, &server).await,
            "moving a jar to Client mods",
        );

        crate::server::share::save(&paths, &server.id, &share(true))
            .await
            .unwrap();
        moved(revision_of(&paths, &server).await, "sending config");

        tokio::fs::write(run.join("config/secret.toml"), b"token = \"rotated\"\n")
            .await
            .unwrap();
        moved(
            revision_of(&paths, &server).await,
            "changing a config file that is sent",
        );

        // A mod the server does not allow reaches a player's game as surely as
        // a jar does: without this in the revision, adding a rule reaches
        // nobody until something else on the server happens to change.
        crate::server::share::save(
            &paths,
            &server.id,
            &ServerShare {
                forbidden: vec![Forbidden {
                    mod_id: "xaerominimap".into(),
                    reason: "no x-ray on this server".into(),
                    ..Default::default()
                }],
                ..share(true)
            },
        )
        .await
        .unwrap();
        moved(revision_of(&paths, &server).await, "forbidding a mod");

        // And so does the reason, which is the sentence they read.
        crate::server::share::save(
            &paths,
            &server.id,
            &ServerShare {
                forbidden: vec![Forbidden {
                    mod_id: "xaerominimap".into(),
                    reason: "ask an admin if you want a minimap".into(),
                    ..Default::default()
                }],
                ..share(true)
            },
        )
        .await
        .unwrap();
        moved(
            revision_of(&paths, &server).await,
            "rewording why a mod is not allowed",
        );

        crate::server::share::save(&paths, &server.id, &share(true))
            .await
            .unwrap();
        moved(revision_of(&paths, &server).await, "allowing a mod again");

        // What players do not get, or get nothing new from.
        let unmoved = revision_of(&paths, &server).await;
        tokio::fs::write(run.join("config/secret.toml"), b"token = \"rotated\"\n")
            .await
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(run.join("config/secret.toml"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .unwrap();
        tokio::fs::write(run.join("server.properties"), b"motd=changed\n")
            .await
            .unwrap();
        tokio::fs::write(run.join("world/level.dat"), b"a day later")
            .await
            .unwrap();
        tokio::fs::write(run.join("logs/latest.log"), b"[Server] restarted\n")
            .await
            .unwrap();
        server.name = "Renamed Club".into();
        server.notes = "be nicer".into();
        tokio::fs::write(paths.server_media(&server.id).join("icon.png"), b"NEW PNG")
            .await
            .unwrap();
        // The listing, which reaches a player's game but is not content: it
        // rides the description instead, so a rename or a move lands on their
        // next Play with nothing downloaded. Put it in the revision and every
        // player — hand-added jars and all — re-downloads the pack because the
        // admin corrected a port.
        crate::server::share::save(
            &paths,
            &server.id,
            &ServerShare {
                listing: Some(Listing {
                    name: "Survival Club".into(),
                    address: "play.example.com:25566".into(),
                }),
                ..share(true)
            },
        )
        .await
        .unwrap();
        assert_eq!(revision_of(&paths, &server).await, unmoved);
        crate::server::share::save(
            &paths,
            &server.id,
            &ServerShare {
                listing: Some(Listing {
                    name: "Survival Club 2".into(),
                    address: "moved.example.com".into(),
                }),
                ..share(true)
            },
        )
        .await
        .unwrap();
        assert_eq!(
            revision_of(&paths, &server).await,
            unmoved,
            "renaming or moving the server in players' lists re-downloads the pack"
        );

        crate::server::share::save(&paths, &server.id, &share(false))
            .await
            .unwrap();
        let unsent = revision_of(&paths, &server).await;
        assert_ne!(unsent, unmoved, "no longer sending config");
        tokio::fs::write(run.join("config/secret.toml"), b"token = \"again\"\n")
            .await
            .unwrap();
        assert_eq!(
            revision_of(&paths, &server).await,
            unsent,
            "a config file that is not sent moved the revision"
        );
    }

    /// Resolving a server's references again keeps one record for each file,
    /// the new one, and every record for any other file.
    /// A server rewrites files under itself while it runs — a mod saving its
    /// config, a database renaming its own files — so one listed a moment ago
    /// can be gone by the time it is read. It is left out of the revision,
    /// rather than making the whole description unreadable and every Play say
    /// the server could not be reached.
    #[tokio::test]
    async fn a_file_that_goes_while_the_revision_is_read_is_left_out() {
        let (scratch, server) =
            small_server("ghost", &[("mods/there.jar", b"there".to_vec())], false).await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        let present = server_mods(&run, &server.content).await.unwrap();
        let steady =
            server_revision(&paths, &server, &present, false, &[], &Settings::default(), &[])
            .await
            .unwrap();

        // The same server, listing two files that have gone since: one with a
        // record naming its version, one with nothing but its bytes.
        let recorded = ContentRecord {
            project_id: "G".into(),
            version_id: "v".into(),
            ..record("modrinth", "ghost-mr.jar", ContentKind::Mod)
        };
        let gone = ServerMods {
            shared: vec![&recorded],
            carried: present
                .carried
                .iter()
                .cloned()
                .chain([
                    PathBuf::from("mods/ghost.jar"),
                    PathBuf::from("mods/ghost-mr.jar"),
                ])
                .collect(),
            sides: present.sides.clone(),
        };
        assert_eq!(
            server_revision(&paths, &server, &gone, false, &[], &Settings::default(), &[])
                .await
                .unwrap(),
            steady,
            "a file that is not there says nothing about what players get"
        );

        // The one call the config walk makes for each file it listed.
        assert_eq!(
            sha1_if_there(&run.join("config/gone.toml")).await.unwrap(),
            None
        );
        assert!(sha1_if_there(&run.join("mods/there.jar"))
            .await
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_reference_is_never_recorded_twice() {
        let mut content = vec![
            ContentRecord {
                version_id: "old".into(),
                origin: Origin::Server,
                ..record("curseforge", "create.jar", ContentKind::Mod)
            },
            record("curseforge", "create.jar", ContentKind::ResourcePack),
            record("modrinth", "sodium.jar", ContentKind::Mod),
        ];
        put_records(
            &mut content,
            [ContentRecord {
                version_id: "new".into(),
                ..record("curseforge", "create.jar", ContentKind::Mod)
            }],
        );

        let create: Vec<&ContentRecord> = content
            .iter()
            .filter(|r| r.kind == ContentKind::Mod && r.file_name == "create.jar")
            .collect();
        assert_eq!(create.len(), 1, "{content:?}");
        assert_eq!(create[0].version_id, "new");
        assert_eq!(content.len(), 3, "{content:?}");
    }

    /// The bytes the stub below serves, and the sha1 CurseForge publishes about
    /// its own copy of them. Real, because a download checks it.
    const A_JAR: &[u8] = b"JAR";
    const A_JARS_SHA1: &str = "a7a941dea93a9fd0d1a0a8980df11d40b2cf576c";

    /// A CurseForge and a Modrinth of our own on one listener: file `2001` of
    /// project `1001` with third-party downloads switched off, and the author's
    /// own Modrinth release of the identical bytes.
    ///
    /// Both bases are served because what a reference does is decided entirely
    /// by what the two APIs answer about the one file. The matching file is
    /// listed second and not primary, and the primary one names a hash the
    /// bytes do not have — so a resolve that took the version's primary file
    /// fails its own checksum rather than quietly fetching a sources jar.
    async fn blocked_on_curseforge_and_published_on_modrinth() -> String {
        use http_body_util::Full;
        use hyper::body::{Bytes, Incoming};
        use hyper::service::service_fn;
        use hyper::{Request, Response};
        use hyper_util::rt::TokioIo;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let curseforge = format!(
            r#"{{"data":{{"id":2001,"modId":1001,"displayName":"Cool Mod 1.2.3",
               "fileName":"locked.jar","downloadUrl":null,
               "hashes":[{{"value":"{A_JARS_SHA1}","algo":1}}],
               "gameVersions":["1.21.1","Fabric"]}}}}"#
        );
        let modrinth = format!(
            r#"{{"{A_JARS_SHA1}":{{"id":"MRVER","project_id":"MRPROJ",
               "name":"Cool Mod on Modrinth","version_number":"1.2.3",
               "version_type":"release","game_versions":["1.21.1"],
               "loaders":["fabric"],"downloads":9,"files":[
                 {{"url":"{base}/mr/sources.jar","filename":"sources.jar",
                   "hashes":{{"sha1":"0000000000000000000000000000000000000000"}},
                   "size":1,"primary":true}},
                 {{"url":"{base}/mr/cool-1.2.3.jar","filename":"cool-1.2.3.jar",
                   "hashes":{{"sha1":"{A_JARS_SHA1}"}},"size":3,"primary":false}}
               ]}}}}"#
        );

        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (curseforge, modrinth) = (curseforge.clone(), modrinth.clone());
                tokio::spawn(async move {
                    let service = service_fn(move |r: Request<Incoming>| {
                        let (curseforge, modrinth) = (curseforge.clone(), modrinth.clone());
                        async move {
                            let body = match r.uri().path() {
                                "/v1/mods/1001/files/2001" => Bytes::from(curseforge),
                                path if path.ends_with("/version_files") => Bytes::from(modrinth),
                                _ => Bytes::from_static(A_JAR),
                            };
                            Ok::<_, std::convert::Infallible>(Response::new(Full::new(body)))
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        base
    }

    fn a_note_sourced_from_curseforge(path: &str) -> FileNote {
        FileNote {
            path: path.into(),
            side: None,
            source: Some(SourceRef {
                provider: "curseforge".into(),
                project_id: "1001".into(),
                version_id: "2001".into(),
            }),
            disabled: false,
        }
    }

    /// A shared pack's end of the rule. A reference whose author has since
    /// turned third-party downloads off used to be reported as missing, which
    /// for a pack with one such mod in it is a pack that will not install. When
    /// that same author published the identical bytes on Modrinth it arrives
    /// from their own release instead, and the record says where it came from.
    ///
    /// The file still lands under the path the pack named: a reference says
    /// where a file goes, and that is the sharer's business, not Modrinth's.
    #[tokio::test]
    async fn a_blocked_reference_comes_from_the_authors_own_modrinth_release() {
        let scratch = Scratch::new("ref-blocked");
        let base = blocked_on_curseforge_and_published_on_modrinth().await;
        let mut inst = instance();
        let mut m = manifest();
        m.files.push(a_note_sourced_from_curseforge("mods/locked.jar"));

        let missing = resolve_references(
            &crate::net::client(),
            &format!("{base}/v1"),
            &format!("{base}/v2"),
            &scratch.paths,
            &mut inst,
            &m,
            Some("a key"),
            &Progress::silent("t"),
        )
        .await
        .expect("the pack's one reference resolved");

        assert!(
            missing.is_empty(),
            "the file installed, so there is nothing left for the user to do: {missing:?}"
        );
        let landed = scratch.paths.game_dir(&inst.id).join("mods/locked.jar");
        assert_eq!(std::fs::read(&landed).unwrap(), A_JAR);

        // Modrinth, because that is where the bytes came from.
        assert_eq!(inst.content.len(), 1, "{:?}", inst.content);
        assert_eq!(inst.content[0].file_name, "locked.jar");
        assert_eq!(inst.content[0].provider, "modrinth");
        assert_eq!(inst.content[0].project_id, "MRPROJ");
        assert_eq!(inst.content[0].version_id, "MRVER");
    }

    /// The server's pack as it is now.
    async fn pack_now(paths: &Paths, server: &Server, name: &str) -> PathBuf {
        let dest = paths.root.join(format!("{name}.geodepack"));
        export_server(paths, server, dest.clone(), &Progress::silent("t"))
            .await
            .unwrap();
        dest
    }

    /// Copy jars from the server's `mods/` into a player's, so the index
    /// entries naming them download nothing.
    async fn seed(paths: &Paths, server: &Server, player: &Instance, jars: &[&str]) {
        let from = paths.server_run(&server.id).join("mods");
        let to = paths.game_dir(&player.id).join("mods");
        tokio::fs::create_dir_all(&to).await.unwrap();
        for jar in jars {
            tokio::fs::copy(from.join(jar), to.join(jar)).await.unwrap();
        }
    }

    /// A fresh instance that joined the server's pack as it is now, offline.
    async fn join_now(paths: &Paths, server: &Server, id: &str, seeded: &[&str]) -> Instance {
        let mut joined = fresh(id);
        joined.loader_version = "0.16.9".into();
        let _ = tokio::fs::remove_dir_all(paths.instance(id)).await;
        seed(paths, server, &joined, seeded).await;
        let pack = pack_now(paths, server, &format!("{id}-joined")).await;
        let missing = import(
            &offline(),
            paths,
            &mut joined,
            &pack,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(missing.is_empty(), "{missing:?}");
        joined
    }

    async fn update_offline(
        paths: &Paths,
        player: &mut Instance,
        pack: &Path,
    ) -> Result<UpdateReport> {
        update(
            &offline(),
            paths,
            player,
            pack,
            None,
            &Progress::silent("t"),
        )
        .await
    }

    fn record_of<'a>(player: &'a Instance, file: &str) -> Option<&'a ContentRecord> {
        player.content.iter().find(|r| r.file_name == file)
    }

    /// The server's changes arrive, and nothing of the player's goes: their own
    /// mods, the one of the server's they turned off, their worlds and options,
    /// and what they called the instance and the picture it has.
    #[tokio::test]
    async fn updating_brings_the_servers_changes_and_keeps_the_players_own() {
        let (scratch, mut server) = small_server(
            "keeps",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("mods/kept.jar", b"kept, first".to_vec()),
                ("mods/back-on.jar.disabled", b"back on".to_vec()),
                ("config/c.toml", b"c = 1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-keeps", &[]).await;
        let game = paths.game_dir(&player.id);
        let mods = game.join("mods");
        let icon = std::fs::read(paths.instance_media(&player.id).join("icon.png")).unwrap();

        tokio::fs::write(mods.join("own.jar"), b"the player's own")
            .await
            .unwrap();
        tokio::fs::write(mods.join("mine.jar"), b"installed by the player")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "mine".into(),
            ..record("modrinth", "mine.jar", ContentKind::Mod)
        });
        crate::instance::content::set_enabled_in(&mods, "kept.jar", false)
            .await
            .unwrap();
        player.name = "Mine".into();
        tokio::fs::write(game.join("options.txt"), b"fov:90\n")
            .await
            .unwrap();
        tokio::fs::create_dir_all(game.join("saves/w"))
            .await
            .unwrap();
        tokio::fs::write(game.join("saves/w/level.dat"), b"my world")
            .await
            .unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::remove_file(run.join("mods/a.jar"))
            .await
            .unwrap();
        tokio::fs::write(run.join("mods/b.jar"), b"b")
            .await
            .unwrap();
        tokio::fs::write(run.join("mods/kept.jar"), b"kept, second")
            .await
            .unwrap();
        tokio::fs::rename(
            run.join("mods/back-on.jar.disabled"),
            run.join("mods/back-on.jar"),
        )
        .await
        .unwrap();
        tokio::fs::write(run.join("config/c.toml"), b"c = 2\n")
            .await
            .unwrap();
        server.build = "0.17.0".into();
        server.name = "Renamed Club".into();
        tokio::fs::write(paths.server_media(&server.id).join("icon.png"), b"NEW PNG")
            .await
            .unwrap();
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert!(
            report.missing.is_empty() && report.turned_off.is_empty(),
            "{report:?}"
        );

        assert!(!mods.join("a.jar").exists() && !mods.join("a.jar.disabled").exists());
        assert!(
            record_of(&player, "a.jar").is_none(),
            "{:?}",
            player.content
        );
        assert!(mods.join("b.jar").is_file());
        assert_eq!(
            record_of(&player, "b.jar").map(|r| r.origin),
            Some(Origin::Server)
        );
        assert!(
            !mods.join("kept.jar").exists(),
            "a mod the player turned off came back on"
        );
        assert_eq!(
            std::fs::read(mods.join("kept.jar.disabled")).unwrap(),
            b"kept, second"
        );
        assert!(mods.join("back-on.jar").is_file() && !mods.join("back-on.jar.disabled").exists());
        assert!(!record_of(&player, "back-on.jar").unwrap().server_off);

        assert_eq!(
            std::fs::read(mods.join("own.jar")).unwrap(),
            b"the player's own"
        );
        assert!(
            record_of(&player, "own.jar").is_none(),
            "{:?}",
            player.content
        );
        assert_eq!(
            std::fs::read(mods.join("mine.jar")).unwrap(),
            b"installed by the player"
        );
        assert_eq!(
            record_of(&player, "mine.jar").map(|r| r.origin),
            Some(Origin::User)
        );
        assert_eq!(
            std::fs::read(game.join("options.txt")).unwrap(),
            b"fov:90\n"
        );
        assert_eq!(
            std::fs::read(game.join("saves/w/level.dat")).unwrap(),
            b"my world"
        );
        assert_eq!(player.name, "Mine");
        assert_eq!(player.icon.as_deref(), Some("icon.png"));
        assert_eq!(
            std::fs::read(paths.instance_media(&player.id).join("icon.png")).unwrap(),
            icon
        );
        assert_eq!(
            std::fs::read(game.join("config/c.toml")).unwrap(),
            b"c = 2\n"
        );

        let files: HashSet<(ContentKind, &str)> = player
            .content
            .iter()
            .map(|r| (r.kind, r.file_name.as_str()))
            .collect();
        assert_eq!(files.len(), player.content.len(), "{:?}", player.content);
        assert!(player.pack.is_none(), "{:?}", player.pack);
        assert_eq!(player.loader_version, "0.17.0");
    }

    /// The whole of "ship a default config, let players tweak it, leave it
    /// alone": the file arrives the first time, the player's own version of it
    /// survives every update after, and a player who deletes it gets the
    /// server's copy back.
    #[tokio::test]
    async fn a_config_file_the_server_sends_once_is_the_players_from_then_on() {
        let (scratch, server) = small_server(
            "sent-once",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/sodium.json", b"{\"renderDistance\": 8}".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        choose_config(&paths, &server.id, Policy::ShipOnce).await;

        let mut player = join_now(&paths, &server, "sent-once-player", &[]).await;
        let mine = paths.game_dir(&player.id).join("config/sodium.json");
        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"{\"renderDistance\": 8}",
            "the default config did not arrive on the first join"
        );
        let caught_up = player.settings_at.expect("the join recorded where it got to");

        // The player tunes it, and the admin changes their own copy.
        tokio::fs::write(&mine, b"{\"renderDistance\": 24}").await.unwrap();
        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/sodium.json"), b"{\"renderDistance\": 12}")
            .await
            .unwrap();
        tokio::fs::write(run.join("mods/b.jar"), b"b").await.unwrap();

        let pack = pack_now(&paths, &server, "b").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"{\"renderDistance\": 24}",
            "the server replaced a file it had already sent once"
        );
        assert!(
            paths.game_dir(&player.id).join("mods/b.jar").is_file(),
            "the mod half of the same update did not run"
        );
        assert_eq!(player.settings_at, Some(caught_up));

        // A player who deletes it has no copy, so there is nothing of theirs
        // to keep and the server's current one arrives.
        tokio::fs::remove_file(&mine).await.unwrap();
        let pack = pack_now(&paths, &server, "c").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"{\"renderDistance\": 12}",
            "a player who deleted it did not get it back"
        );
    }

    /// A player who joined before the admin chose anything has had every
    /// config file replaced on every Play, so their copy already holds the
    /// admin's values. They get it whole exactly once more — the update that
    /// carries the choice — and keep their own changes from then on.
    #[tokio::test]
    async fn a_player_who_joined_before_the_choice_gets_the_file_whole_once_more() {
        let (scratch, server) = small_server(
            "catch-up",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/c.toml", b"c = 1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();

        // Joined while the share had chosen nothing, so the only thing to have
        // caught up with is the behaviour that has been in force since before
        // there was anything counting: zero, which is below every choice an
        // admin will ever make.
        let mut player = join_now(&paths, &server, "catch-up-player", &[]).await;
        assert_eq!(player.settings_at, Some(0));
        let mine = paths.game_dir(&player.id).join("config/c.toml");
        tokio::fs::write(&mine, b"c = 99\n").await.unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/c.toml"), b"c = 2\n").await.unwrap();
        choose_config(&paths, &server.id, Policy::ShipOnce).await;
        let pack = pack_now(&paths, &server, "b").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"c = 2\n",
            "the update that carried the choice did not send the file whole"
        );
        let caught_up = player.settings_at.expect("the update recorded where it got to");

        // And from then on it is theirs, with no further choice made.
        tokio::fs::write(&mine, b"c = 99\n").await.unwrap();
        tokio::fs::write(run.join("config/c.toml"), b"c = 3\n").await.unwrap();
        tokio::fs::write(run.join("mods/b.jar"), b"b").await.unwrap();
        let pack = pack_now(&paths, &server, "c").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"c = 99\n",
            "the file was sent whole twice"
        );
        assert_eq!(player.settings_at, Some(caught_up));
    }

    /// A read-only config file used to cost a player their game. `File::create`
    /// on a 0444 file is EACCES, which failed the extraction, which failed the
    /// import, which failed the update, which stopped the launch — and because
    /// the revision was never recorded, it stopped the next launch too, for
    /// ever. For a file the server syncs it is now one line in the report.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_config_the_player_made_read_only_does_not_stop_them_playing() {
        use std::os::unix::fs::PermissionsExt;

        let (scratch, server) = small_server(
            "readonly",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/c.toml", b"c = 1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        // "Always replace" is what sending a config folder has always done, so
        // this share has chosen nothing at all — which is deliberately the
        // case under test. Every share made before any of this is in it, and
        // every one of them has this bug today.
        choose_config(&paths, &server.id, Policy::Enforce).await;
        assert!(
            crate::server::share::load(&paths, &server.id)
                .await
                .unwrap()
                .unwrap()
                .settings
                .paths
                .is_empty(),
            "the share recorded a choice, so this is not the migration case"
        );
        let mut player = join_now(&paths, &server, "readonly-player", &[]).await;

        let mine = paths.game_dir(&player.id).join("config/c.toml");
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o444)).unwrap();
        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/c.toml"), b"c = 2\n").await.unwrap();
        tokio::fs::write(run.join("mods/b.jar"), b"b").await.unwrap();
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(report.not_written.len(), 1, "{report:?}");
        assert!(
            report.not_written[0].contains("config/c.toml"),
            "the player is not told which file: {:?}",
            report.not_written
        );
        // Not among the files to go and fetch: this one is on their disk
        // already, as their own copy.
        assert!(report.missing.is_empty(), "{report:?}");
        // The rest of the update finished and the revision came back, so the
        // instance holds what the pack held — bar the one file, which it owes.
        assert!(report.revision.is_some(), "{report:?}");
        assert_eq!(player.settings_owed, vec!["config/c.toml".to_string()]);
        assert!(paths.game_dir(&player.id).join("mods/b.jar").is_file());
        assert_eq!(std::fs::read(&mine).unwrap(), b"c = 1\n");

        // Writable again, and the server's copy lands on the next update with
        // nothing for the player to do but press Play.
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o644)).unwrap();
        let pack = pack_now(&paths, &server, "c").await;
        let report = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert!(report.not_written.is_empty(), "{report:?}");
        assert_eq!(std::fs::read(&mine).unwrap(), b"c = 2\n");
        assert!(player.settings_owed.is_empty(), "{:?}", player.settings_owed);
    }

    /// A synced file the player's disk would not take is not a file they have.
    /// `settings_at` says the push landed, which for that one file it did not —
    /// and a file that is present and caught up is one `landings` never sends
    /// again. So the paths that did not land are kept beside the number: the
    /// player makes the file writable, presses Play as the notice tells them,
    /// and receives it, while the files they tuned in the meantime are left
    /// exactly as they left them.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sent_once_config_that_could_not_be_written_is_sent_again() {
        use std::os::unix::fs::PermissionsExt;

        let (scratch, server) = small_server(
            "owed",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/pinned.toml", b"pinned = 1\n".to_vec()),
                ("config/tuned.toml", b"tuned = 1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();

        // Joined while the share had chosen nothing, so both files have been
        // replaced on every Play up to now.
        let mut player = join_now(&paths, &server, "owed-player", &[]).await;
        let game = paths.game_dir(&player.id);
        let (pinned, tuned) = (game.join("config/pinned.toml"), game.join("config/tuned.toml"));
        std::fs::set_permissions(&pinned, std::fs::Permissions::from_mode(0o444)).unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/pinned.toml"), b"pinned = 2\n").await.unwrap();
        choose_config(&paths, &server.id, Policy::ShipOnce).await;
        let pack = pack_now(&paths, &server, "b").await;

        // The update that carries the choice sends both files whole once more.
        // One of them lands.
        let report = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(report.not_written.len(), 1, "{report:?}");
        assert!(report.not_written[0].contains("config/pinned.toml"), "{report:?}");
        assert_eq!(player.settings_owed, vec!["config/pinned.toml".to_string()]);
        assert_eq!(std::fs::read(&pinned).unwrap(), b"pinned = 1\n");
        assert_eq!(std::fs::read(&tuned).unwrap(), b"tuned = 1\n");

        // The player does what the notice told them, and tunes the file that
        // is theirs now. The admin changes nothing at all.
        std::fs::set_permissions(&pinned, std::fs::Permissions::from_mode(0o644)).unwrap();
        tokio::fs::write(&tuned, b"tuned = 99\n").await.unwrap();
        let pack = pack_now(&paths, &server, "c").await;
        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert!(report.not_written.is_empty(), "{report:?}");
        assert!(player.settings_owed.is_empty(), "{:?}", player.settings_owed);
        assert_eq!(
            std::fs::read(&pinned).unwrap(),
            b"pinned = 2\n",
            "the file the player made writable never arrived"
        );
        assert_eq!(
            std::fs::read(&tuned).unwrap(),
            b"tuned = 99\n",
            "a file that had already landed was sent again over the player's own changes"
        );
    }


    /// Keep this server's config folder in sync, the way the admin's control
    /// does: the choice, and the snapshot of what is published that the choice
    /// takes with it.
    async fn keep_in_sync(paths: &Paths, id: &str) {
        choose_config(paths, id, Policy::Merge).await;
        crate::settings::push::publish_all(paths, id).await.unwrap();
    }

    /// Press Review and send everything it found, which is what an admin who
    /// leaves every row ticked does. Returns how many settings went.
    async fn push_everything(paths: &Paths, id: &str) -> u32 {
        let review = crate::settings::push::review(paths, id).await.unwrap();
        let wanted: Vec<crate::settings::push::Wanted> = review
            .moved
            .iter()
            .map(|one| crate::settings::push::Wanted {
                path: one.path.clone(),
                key: one.key.clone(),
                value: one.now.clone(),
                was: one.was.clone(),
            })
            .collect();
        if wanted.is_empty() {
            return 0;
        }
        crate::settings::push::push(paths, id, &wanted).await.unwrap()
    }

    /// The whole feature, end to end and offline: the admin changes one
    /// setting in a `.properties` on the server, presses Review and sends it,
    /// and the player's copy gains that one setting with everything they had
    /// tuned still tuned — in a file whose CRLF endings, comments and spacing
    /// are the bytes they were.
    #[tokio::test]
    async fn one_pushed_setting_lands_and_everything_else_the_player_changed_stays() {
        let shipped = "#Sat Oct 17 23:58:04 EDT 2026\r\n\
                       # Range: 0 ~ 32\r\nrenderDistance=8\r\nfov = 70\r\nshader=none\r\n";
        let (scratch, server) = small_server(
            "push-one",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/iris.properties", shipped.as_bytes().to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        keep_in_sync(&paths, &server.id).await;

        let mut player = join_now(&paths, &server, "push-one-player", &[]).await;
        let mine = paths.game_dir(&player.id).join("config/iris.properties");
        assert_eq!(
            std::fs::read_to_string(&mine).unwrap(),
            shipped,
            "the default config did not arrive on the first join"
        );

        // The player tunes two of the three settings.
        let tuned = "#Sat Oct 17 23:58:04 EDT 2026\r\n\
                     # Range: 0 ~ 32\r\nrenderDistance=24\r\nfov = 110\r\nshader=none\r\n";
        tokio::fs::write(&mine, tuned).await.unwrap();

        // The admin changes one setting — and the mod rewrites the file's own
        // timestamp comment while the server runs, which is not a setting and
        // must not be offered as one.
        let run = paths.server_run(&server.id);
        tokio::fs::write(
            run.join("config/iris.properties"),
            "#Mon Oct 19 11:02:55 EDT 2026\r\n\
             # Range: 0 ~ 32\r\nrenderDistance=8\r\nfov = 70\r\nshader=complementary\r\n",
        )
        .await
        .unwrap();
        assert_eq!(
            push_everything(&paths, &server.id).await,
            1,
            "Review offered something other than the one setting that moved"
        );

        let pack = pack_now(&paths, &server, "b").await;
        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(&mine).unwrap(),
            "#Sat Oct 17 23:58:04 EDT 2026\r\n\
             # Range: 0 ~ 32\r\nrenderDistance=24\r\nfov = 110\r\nshader=complementary\r\n",
            "the push did not land as exactly one changed line"
        );
        // And the player is told: one setting moved, and it was not one of
        // theirs — their own two are untouched, so there is nothing to argue
        // about and no row to interrupt them with.
        let settings = report.settings.expect("the update recorded what it changed");
        assert_eq!(settings.rest, 1);
        assert!(settings.yours.is_empty(), "{:?}", settings.yours);
        assert!(settings.skipped.is_empty(), "{:?}", settings.skipped);
        assert_eq!(
            crate::settings::report::load(&paths, &player.id).await.unwrap(),
            Some(settings),
            "the panel reads the same report the launch was told about"
        );
    }

    /// The sentence the whole design rests on: a push is applied once, and a
    /// player who sets the setting back keeps it set back for ever — because
    /// the push it came in never takes a new number.
    #[tokio::test]
    async fn a_push_applies_once_and_a_players_change_back_sticks() {
        let (scratch, server) = small_server(
            "push-twice",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/sodium.properties", b"renderDistance=8\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        keep_in_sync(&paths, &server.id).await;
        let mut player = join_now(&paths, &server, "push-twice-player", &[]).await;
        let mine = paths.game_dir(&player.id).join("config/sodium.properties");

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/sodium.properties"), b"renderDistance=12\n")
            .await
            .unwrap();
        push_everything(&paths, &server.id).await;

        let pack = pack_now(&paths, &server, "b").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(std::fs::read(&mine).unwrap(), b"renderDistance=12\n");

        // The player prefers their own, and says so. Every update after that
        // leaves it alone: the push is below where they have got to.
        tokio::fs::write(&mine, b"renderDistance=24\n").await.unwrap();
        for round in ["c", "d"] {
            tokio::fs::write(run.join(format!("mods/{round}.jar")), b"x").await.unwrap();
            let pack = pack_now(&paths, &server, round).await;
            let report = update_offline(&paths, &mut player, &pack).await.unwrap();
            assert!(
                report.settings.is_none(),
                "an update re-applied a push: {:?}",
                report.settings
            );
            assert_eq!(
                std::fs::read(&mine).unwrap(),
                b"renderDistance=24\n",
                "the server took the player's own change back off them"
            );
        }

        // And the admin pushing a different setting does not drag the old one
        // along with it.
        tokio::fs::write(
            run.join("config/sodium.properties"),
            b"renderDistance=12\nfog=fancy\n",
        )
        .await
        .unwrap();
        assert_eq!(push_everything(&paths, &server.id).await, 1);
        let pack = pack_now(&paths, &server, "e").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"renderDistance=24\nfog=fancy\n",
            "a setting the player never had was not appended, or their own was replaced"
        );
    }

    /// A player who missed three pushes catches up in one pass, in one update,
    /// with the current value of every setting — not a chain of patches. The
    /// three are a setting pushed twice, a setting their file does not have,
    /// and a setting taken out.
    #[tokio::test]
    async fn a_player_who_skipped_three_pushes_gets_all_of_them_at_once() {
        let (scratch, server) = small_server(
            "push-backlog",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/mod.cfg", b"quality = low\nlegacy = 1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        keep_in_sync(&paths, &server.id).await;
        let mut player = join_now(&paths, &server, "push-backlog-player", &[]).await;
        let mine = paths.game_dir(&player.id).join("config/mod.cfg");
        let run = paths.server_run(&server.id);
        let on_the_server = run.join("config/mod.cfg");

        // Push one: a value. Push two: the same setting again, which replaces
        // its entry rather than adding to a log. Push three: a setting added
        // and a setting removed.
        tokio::fs::write(&on_the_server, b"quality = medium\nlegacy = 1\n").await.unwrap();
        push_everything(&paths, &server.id).await;
        tokio::fs::write(&on_the_server, b"quality = high\nlegacy = 1\n").await.unwrap();
        push_everything(&paths, &server.id).await;
        tokio::fs::write(&on_the_server, b"quality = high\nshadows = on\n").await.unwrap();
        push_everything(&paths, &server.id).await;

        let share = crate::server::share::load(&paths, &server.id).await.unwrap().unwrap();
        assert_eq!(
            share.settings.overrides.len(),
            3,
            "the list grew into a log of pushes: {:?}",
            share.settings.overrides
        );

        let pack = pack_now(&paths, &server, "b").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"quality = high\nshadows = on\n",
            "the backlog did not land as the current values in one pass"
        );
        assert_eq!(player.settings_at, Some(share.settings.at));
    }

    /// A player whose instance predates all of this has had every config file
    /// replaced on every Play, so their copy already holds the admin's values.
    /// They get each file whole exactly once more, with nothing spliced — and
    /// merge from then on.
    #[tokio::test]
    async fn an_instance_that_has_caught_up_with_nothing_gets_the_file_whole_once() {
        let (scratch, server) = small_server(
            "push-none",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/a.properties", b"a=1\nb=2\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        keep_in_sync(&paths, &server.id).await;
        let mut player = join_now(&paths, &server, "push-none-player", &[]).await;
        let mine = paths.game_dir(&player.id).join("config/a.properties");
        // Joined before any of this existed, which reads as having caught up
        // with nothing.
        player.settings_at = None;
        tokio::fs::write(&mine, b"a=99\nb=99\n").await.unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/a.properties"), b"a=1\nb=3\n").await.unwrap();
        push_everything(&paths, &server.id).await;
        let pack = pack_now(&paths, &server, "b").await;
        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"a=1\nb=3\n",
            "the file was not sent whole to an instance that had caught up with nothing"
        );
        // Whole, so nothing was spliced and there is nothing to report: the
        // file arrived holding every setting the admin owns.
        assert!(report.settings.is_none(), "{:?}", report.settings);

        // From then on it is theirs, and a push reaches only its own setting.
        tokio::fs::write(&mine, b"a=99\nb=3\n").await.unwrap();
        tokio::fs::write(run.join("config/a.properties"), b"a=1\nb=4\n").await.unwrap();
        push_everything(&paths, &server.id).await;
        let pack = pack_now(&paths, &server, "c").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(std::fs::read(&mine).unwrap(), b"a=99\nb=4\n");
    }

    /// The player's own switch, with no exceptions: nothing of the server's
    /// settings is written while it is off, and nothing records that they
    /// caught up — so turning it back on applies the whole backlog at once.
    #[tokio::test]
    async fn with_the_servers_settings_turned_off_nothing_is_written_and_nothing_is_lost() {
        let (scratch, server) = small_server(
            "push-off",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/kept.properties", b"a=1\n".to_vec()),
                ("config/forced.properties", b"b=1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        keep_in_sync(&paths, &server.id).await;
        // One file the admin replaces every time, to prove the switch stops
        // that too rather than only the per-setting pushes.
        let mut share = crate::server::share::load(&paths, &server.id).await.unwrap().unwrap();
        share.settings.choose("config/forced.properties", Policy::Enforce);
        crate::server::share::save(&paths, &server.id, &share).await.unwrap();

        let mut player = join_now(&paths, &server, "push-off-player", &[]).await;
        let game = paths.game_dir(&player.id);
        let (kept, forced) = (
            game.join("config/kept.properties"),
            game.join("config/forced.properties"),
        );
        let caught_up = player.settings_at.expect("the join recorded where it got to");
        player.server_settings_off = true;
        tokio::fs::write(&kept, b"a=99\n").await.unwrap();
        tokio::fs::write(&forced, b"b=99\n").await.unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/kept.properties"), b"a=2\n").await.unwrap();
        tokio::fs::write(run.join("config/forced.properties"), b"b=2\n").await.unwrap();
        push_everything(&paths, &server.id).await;
        let pack = pack_now(&paths, &server, "b").await;
        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(std::fs::read(&kept).unwrap(), b"a=99\n");
        assert_eq!(
            std::fs::read(&forced).unwrap(),
            b"b=99\n",
            "a file the admin replaces every time was written while the switch was off"
        );
        assert!(report.settings.is_none(), "{:?}", report.settings);
        assert_eq!(
            player.settings_at,
            Some(caught_up),
            "the instance recorded catching up with a push it never applied"
        );

        // Turned back on, with no new pack and no new push: everything that
        // happened in between lands in one pass.
        player.server_settings_off = false;
        let pack = pack_now(&paths, &server, "c").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(std::fs::read(&kept).unwrap(), b"a=2\n");
        assert_eq!(std::fs::read(&forced).unwrap(), b"b=2\n");
        assert!(player.settings_at.unwrap() > caught_up);
    }

    /// A settings file the player has made something other than an ordinary
    /// file is not written through, and is named in their report. The whole
    /// file path never gets written either, because this is the copy they
    /// kept — so there is nothing owed and nothing is retried behind their
    /// back.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_setting_is_not_spliced_through_a_link_and_is_reported() {
        let (scratch, server) = small_server(
            "push-link",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/a.properties", b"a=1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        keep_in_sync(&paths, &server.id).await;
        let mut player = join_now(&paths, &server, "push-link-player", &[]).await;
        let game = paths.game_dir(&player.id);
        let mine = game.join("config/a.properties");

        // The player's own file is a link to somewhere else of theirs.
        let elsewhere = game.join("shared.properties");
        tokio::fs::write(&elsewhere, b"a=99\n").await.unwrap();
        tokio::fs::remove_file(&mine).await.unwrap();
        std::os::unix::fs::symlink(&elsewhere, &mine).unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/a.properties"), b"a=2\n").await.unwrap();
        push_everything(&paths, &server.id).await;
        let pack = pack_now(&paths, &server, "b").await;
        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(
            std::fs::read(&elsewhere).unwrap(),
            b"a=99\n",
            "the splice was written through a link"
        );
        let settings = report.settings.expect("a skipped file is worth saying");
        assert_eq!(settings.skipped.len(), 1, "{settings:?}");
        assert!(
            settings.skipped[0].contains("config/a.properties")
                && settings.skipped[0].contains("ordinary file"),
            "{settings:?}"
        );
        assert!(
            player.settings_owed.is_empty(),
            "a file the player keeps their own copy of was left owed: {:?}",
            player.settings_owed
        );
    }

    /// A pack whose settings half is past a ceiling, or names a path Geode
    /// will not write settings to, is not applied at all — every file is
    /// written whole, which is what every Geode did before any of this — and
    /// the reason names what it hit.
    #[tokio::test]
    async fn a_settings_half_past_a_ceiling_is_not_applied_and_says_what_it_hit() {
        let (scratch, server) = small_server(
            "push-hostile",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/a.properties", b"a=1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        keep_in_sync(&paths, &server.id).await;
        let mut player = join_now(&paths, &server, "push-hostile-player", &[]).await;
        let mine = paths.game_dir(&player.id).join("config/a.properties");
        tokio::fs::write(&mine, b"a=99\n").await.unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/a.properties"), b"a=2\n").await.unwrap();
        let pack = pack_now(&paths, &server, "b").await;
        // A half no Geode would write: a setting spliced into a jar.
        rewrite_pack(
            &pack,
            |m| {
                let half = m.settings.as_mut().expect("a server pack syncs its config");
                half.at += 1;
                half.overrides.push(Override {
                    path: "mods/a.jar".into(),
                    key: vec!["anything".into()],
                    value: Some("x".into()),
                    was: None,
                    at: half.at,
                });
            },
            &[],
        );

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert!(
            report.missing.iter().any(|line| line.contains("mods/a.jar")),
            "the refusal did not name what it hit: {:?}",
            report.missing
        );
        assert_eq!(
            std::fs::read(&mine).unwrap(),
            b"a=2\n",
            "a pack with no usable settings half should write each file whole"
        );
        assert_eq!(std::fs::read(paths.game_dir(&player.id).join("mods/a.jar")).unwrap(), b"a");
    }

    /// A push is what players are told about: the revision moves for a setting
    /// the admin sends, and the pack carries the same list the description was
    /// worked out from.
    #[tokio::test]
    async fn pushing_a_setting_moves_the_revision_and_rides_the_pack() {
        let (scratch, server) = small_server(
            "push-revision",
            &[
                ("mods/a.jar", b"a".to_vec()),
                ("config/a.properties", b"a=1\n".to_vec()),
                ("config/sent-once.properties", b"b=1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        keep_in_sync(&paths, &server.id).await;
        let mut share = crate::server::share::load(&paths, &server.id).await.unwrap().unwrap();
        share.settings.choose("config/sent-once.properties", Policy::ShipOnce);
        crate::server::share::save(&paths, &server.id, &share).await.unwrap();
        let before = revision_of(&paths, &server).await;

        // An edit nobody has pushed yet reaches no player, so it moves
        // nothing: that is what makes a mod rewriting its own config on boot
        // cost nobody a download.
        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/a.properties"), b"a=2\n").await.unwrap();
        assert_eq!(revision_of(&paths, &server).await, before);

        // Pushing it does move it.
        push_everything(&paths, &server.id).await;
        let pushed = revision_of(&paths, &server).await;
        assert_ne!(pushed, before, "a push reached nobody");

        // And the pack says the same thing the description does.
        let pack = pack_now(&paths, &server, "b").await;
        let manifest = read_manifest(&pack).unwrap().unwrap();
        let half = manifest.settings.expect("a server pack syncs its config");
        assert_eq!(half.overrides.len(), 1);
        assert_eq!(half.overrides[0].path, "config/a.properties");
        assert_eq!(half.overrides[0].value.as_deref(), Some("2"));
        assert_eq!(half.overrides[0].was.as_deref(), Some("1"));
        assert_eq!(half.at, half.overrides[0].at);
        assert_eq!(manifest.revision.as_deref(), Some(pushed.as_str()));
        assert_eq!(
            server_pack_info(&paths, &server).await.unwrap().revision,
            pushed
        );

        // A setting inside a file players are sent once reaches nobody, so it
        // is in no revision either — the list is where it is, and that is not
        // what the pack carries about it.
        let mut share = crate::server::share::load(&paths, &server.id).await.unwrap().unwrap();
        share.settings.overrides.push(Override {
            path: "config/sent-once.properties".into(),
            key: vec!["b".into()],
            value: Some("2".into()),
            was: Some("1".into()),
            at: share.settings.at,
        });
        crate::server::share::save(&paths, &server.id, &share).await.unwrap();
        assert_eq!(revision_of(&paths, &server).await, pushed);
    }

    /// A new version of a server's mod usually has a new file name. The player
    /// who turned the old one off keeps it off, because it is the same project.
    #[tokio::test]
    async fn a_renamed_server_mod_stays_off_for_a_player_who_turned_it_off() {
        let (scratch, mut server) =
            small_server("renamed", &[("mods/p-1.jar", b"p one".to_vec())], false).await;
        let paths = scratch.paths.clone();
        server.content.push(ContentRecord {
            project_id: "P".into(),
            version_id: "v1".into(),
            ..record("modrinth", "p-1.jar", ContentKind::Mod)
        });
        let mut player = join_now(&paths, &server, "update-renamed", &["p-1.jar"]).await;
        let mods = paths.game_dir(&player.id).join("mods");
        crate::instance::content::set_enabled_in(&mods, "p-1.jar", false)
            .await
            .unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::remove_file(run.join("mods/p-1.jar"))
            .await
            .unwrap();
        tokio::fs::write(run.join("mods/p-2.jar"), b"p two")
            .await
            .unwrap();
        server.content = vec![ContentRecord {
            project_id: "P".into(),
            version_id: "v2".into(),
            ..record("modrinth", "p-2.jar", ContentKind::Mod)
        }];
        let pack = pack_now(&paths, &server, "b").await;
        seed(&paths, &server, &player, &["p-2.jar"]).await;

        update_offline(&paths, &mut player, &pack).await.unwrap();

        for gone in ["p-1.jar", "p-1.jar.disabled", "p-2.jar"] {
            assert!(!mods.join(gone).exists(), "{gone} is still there");
        }
        assert!(mods.join("p-2.jar.disabled").is_file());
        assert!(
            record_of(&player, "p-1.jar").is_none(),
            "{:?}",
            player.content
        );
        assert_eq!(
            record_of(&player, "p-2.jar").map(|r| r.origin),
            Some(Origin::Server)
        );
    }

    /// A mod the player keeps turned off is already on their disk, under the
    /// other name. An update moves it to the name the pack writes rather than
    /// fetching it all over again — so keeping a server's mods off costs no
    /// download, and an update still finishes when the mod cannot be fetched
    /// at all.
    #[tokio::test]
    async fn a_mod_the_player_keeps_off_is_not_fetched_again() {
        let (scratch, mut server) = small_server(
            "kept-off",
            &[("mods/idx.jar", b"from modrinth".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        server.content.push(ContentRecord {
            project_id: "I".into(),
            ..record("modrinth", "idx.jar", ContentKind::Mod)
        });
        let mut player = join_now(&paths, &server, "update-kept-off", &["idx.jar"]).await;
        let mods = paths.game_dir(&player.id).join("mods");
        crate::instance::content::set_enabled_in(&mods, "idx.jar", false)
            .await
            .unwrap();

        // Something else changed; the mod the player keeps off did not, and
        // the only copy of it anywhere is theirs — anything fetched reaches
        // the dead proxy.
        tokio::fs::write(paths.server_run(&server.id).join("mods/new.jar"), b"new")
            .await
            .unwrap();
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert!(report.missing.is_empty(), "{report:?}");
        assert!(mods.join("idx.jar.disabled").is_file() && !mods.join("idx.jar").exists());
        assert_eq!(
            std::fs::read(mods.join("idx.jar.disabled")).unwrap(),
            b"from modrinth"
        );
        assert_eq!(
            record_of(&player, "idx.jar").map(|r| r.origin),
            Some(Origin::Server)
        );
        assert!(mods.join("new.jar").is_file());
    }

    /// Only the player's copy of the very file the pack names takes its name.
    /// Another build of the same mod stays where it is and is fetched, so an
    /// update that stops halfway never leaves an old jar sitting under the new
    /// version's name.
    #[tokio::test]
    async fn only_the_file_the_pack_names_takes_its_name() {
        let scratch = Scratch::new("packs-name");
        let dir = scratch.paths.root.clone();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("x.jar.disabled"), b"one")
            .await
            .unwrap();
        let sha1 = crate::net::sha1_file(&dir.join("x.jar.disabled"))
            .await
            .unwrap();
        let another = "0".repeat(40);

        take_the_packs_name(&dir, "x.jar", false, Some(&another))
            .await
            .unwrap();
        assert!(dir.join("x.jar.disabled").is_file() && !dir.join("x.jar").exists());

        // The file the index names, and one the pack carries itself.
        take_the_packs_name(&dir, "x.jar", false, Some(&sha1))
            .await
            .unwrap();
        assert!(dir.join("x.jar").is_file() && !dir.join("x.jar.disabled").exists());
        take_the_packs_name(&dir, "x.jar", true, None)
            .await
            .unwrap();
        assert!(dir.join("x.jar.disabled").is_file() && !dir.join("x.jar").exists());

        // A copy already under the name the pack writes is left alone, and so
        // is the other one beside it.
        tokio::fs::write(dir.join("x.jar"), b"two").await.unwrap();
        take_the_packs_name(&dir, "x.jar", false, None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(dir.join("x.jar")).unwrap(), b"two");
        assert!(dir.join("x.jar.disabled").is_file());
    }

    /// A player's setting applies to what the server sends later, and a
    /// server-side mod they turned off by itself stays off with the setting on.
    #[tokio::test]
    async fn new_server_side_mods_follow_the_players_setting() {
        // Kept off: a server-side mod the server adds arrives off.
        let (scratch, server) = small_server(
            "side-off",
            &[("mods/homemade.jar", fabric_jar("homemade", "*"))],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = fresh("update-side-off");
        let _ = tokio::fs::remove_dir_all(paths.instance(&player.id)).await;
        player.server_side_mods_off = true;
        let pack = pack_now(&paths, &server, "a").await;
        import(
            &offline(),
            &paths,
            &mut player,
            &pack,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(
            run.join("mods/new-side.jar"),
            fabric_jar("newside", "server"),
        )
        .await
        .unwrap();
        let pack = pack_now(&paths, &server, "b").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();

        let mods = paths.game_dir(&player.id).join("mods");
        assert!(
            mods.join("new-side.jar.disabled").is_file() && !mods.join("new-side.jar").exists()
        );
        assert_eq!(
            record_of(&player, "new-side.jar").and_then(|r| r.side),
            Some(Side::Server)
        );
        assert!(mods.join("homemade.jar").is_file());
        let _ = tokio::fs::remove_dir_all(&paths.root).await;

        // Kept on: the one the player turned off stays off.
        let (scratch, server) = small_server(
            "side-on",
            &[("mods/side.jar", fabric_jar("side", "server"))],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-side-on", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");
        assert!(mods.join("side.jar").is_file());
        crate::instance::content::set_enabled_in(&mods, "side.jar", false)
            .await
            .unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("mods/other.jar"), b"other")
            .await
            .unwrap();
        let pack = pack_now(&paths, &server, "b").await;
        update_offline(&paths, &mut player, &pack).await.unwrap();

        assert!(mods.join("side.jar.disabled").is_file() && !mods.join("side.jar").exists());
        assert!(mods.join("other.jar").is_file());
    }

    /// Two copies of one mod is a game that does not start. When the server
    /// starts sending a project the player installed themselves, the player's
    /// copy is turned off, not deleted, stays theirs, and they are told.
    #[tokio::test]
    async fn the_players_own_copy_of_a_mod_the_server_now_ships_is_turned_off() {
        let (scratch, mut server) = small_server(
            "own-copy",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-own-copy", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");
        tokio::fs::write(mods.join("mine-p.jar"), b"the player's P")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "P".into(),
            name: "My P".into(),
            ..record("modrinth", "mine-p.jar", ContentKind::Mod)
        });

        tokio::fs::write(
            paths.server_run(&server.id).join("mods/p.jar"),
            b"the server's P",
        )
        .await
        .unwrap();
        server.content.push(ContentRecord {
            project_id: "P".into(),
            ..record("modrinth", "p.jar", ContentKind::Mod)
        });
        let pack = pack_now(&paths, &server, "b").await;
        seed(&paths, &server, &player, &["p.jar"]).await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(report.turned_off, vec!["My P".to_string()]);
        assert!(mods.join("mine-p.jar.disabled").is_file() && !mods.join("mine-p.jar").exists());
        assert_eq!(
            record_of(&player, "mine-p.jar").map(|r| r.origin),
            Some(Origin::User)
        );
        assert!(mods.join("p.jar").is_file());
        assert_eq!(
            record_of(&player, "p.jar").map(|r| r.origin),
            Some(Origin::Server)
        );

        // Said again by an update that runs again — after one that stopped
        // partway, say. What is reported is what is off, not what this run
        // happened to switch.
        let again = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(again.turned_off, vec!["My P".to_string()]);
    }

    /// The copy with no record at all: a jar dropped into the folder by hand,
    /// under a name of its own, of a mod the server now ships. Nothing says the
    /// two are one mod except the id each jar declares — and without reading
    /// that, both copies stayed on and the game would not start, which reads to
    /// the player as the update having broken their instance.
    #[tokio::test]
    async fn a_hand_dropped_copy_of_a_mod_the_server_now_ships_is_turned_off() {
        let (scratch, server) = small_server(
            "own-jar",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-own-jar", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        let theirs = fabric_jar("minimap", "client");
        tokio::fs::write(mods.join("minimap-1.2.jar"), &theirs)
            .await
            .unwrap();

        // The server starts shipping the same mod, under its own file name and
        // with no provenance either — the pack carries it as bytes.
        tokio::fs::write(
            paths.server_run(&server.id).join("mods/minimap.jar"),
            fabric_jar("minimap", "client"),
        )
        .await
        .unwrap();
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        // Named by its file, since no record of theirs gives it another name.
        assert_eq!(report.turned_off, vec!["minimap-1.2.jar".to_string()]);
        assert!(
            !mods.join("minimap-1.2.jar").exists(),
            "two copies of one mod stayed on"
        );
        assert_eq!(
            std::fs::read(mods.join("minimap-1.2.jar.disabled")).unwrap(),
            theirs,
            "their own file was not kept, bytes and all"
        );
        // Untracked before and untracked after: nothing of the server's claims
        // it, so it is still theirs to delete.
        assert!(
            record_of(&player, "minimap-1.2.jar").is_none(),
            "{:?}",
            player.content
        );
        assert!(mods.join("minimap.jar").is_file());

        // Said again by the next update, because it is still off.
        let again = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(again.turned_off, vec!["minimap-1.2.jar".to_string()]);
    }

    /// And only the same mod. A jar of theirs that declares a different id is
    /// not a second copy of anything, so it is left on and nothing is said —
    /// matching on anything looser would turn off mods at random.
    #[tokio::test]
    async fn a_hand_dropped_jar_declaring_another_mod_is_left_alone() {
        let (scratch, server) = small_server(
            "own-jar-other",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-own-jar-other", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        tokio::fs::write(mods.join("minimap-1.2.jar"), fabric_jar("minimap", "client"))
            .await
            .unwrap();

        tokio::fs::write(
            paths.server_run(&server.id).join("mods/shaders.jar"),
            fabric_jar("shaders", "client"),
        )
        .await
        .unwrap();
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert!(report.turned_off.is_empty(), "{report:?}");
        assert!(
            mods.join("minimap-1.2.jar").is_file(),
            "a mod of their own that the server does not ship was turned off"
        );
        assert!(!mods.join("minimap-1.2.jar.disabled").exists());
    }

    /// Give a shared server a list of mods it does not allow.
    async fn forbid(paths: &Paths, server: &Server, rules: &[Forbidden]) {
        let share = crate::server::share::load(paths, &server.id)
            .await
            .unwrap()
            .unwrap();
        crate::server::share::save(
            paths,
            &server.id,
            &ServerShare {
                forbidden: rules.to_vec(),
                ..share
            },
        )
        .await
        .unwrap();
    }

    fn by_project(project_id: &str, reason: &str) -> Forbidden {
        Forbidden {
            provider: "modrinth".into(),
            project_id: project_id.into(),
            reason: reason.into(),
            ..Default::default()
        }
    }

    fn by_mod_id(mod_id: &str, reason: &str) -> Forbidden {
        Forbidden {
            mod_id: mod_id.into(),
            reason: reason.into(),
            ..Default::default()
        }
    }

    /// A joined instance holding one mod of the player's own, by record, that
    /// the server does not allow — and that server's pack, with the rule in it.
    async fn one_mod_the_server_forbids(tag: &str) -> (Scratch, Instance, PathBuf) {
        let (scratch, server) = small_server(
            tag,
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, &format!("{tag}-player"), &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        tokio::fs::write(mods.join("minimap.jar"), b"their minimap")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "XRAY".into(),
            name: "X-ray Minimap".into(),
            ..record("modrinth", "minimap.jar", ContentKind::Mod)
        });

        forbid(
            &paths,
            &server,
            &[by_project("XRAY", "no x-ray on this server")],
        )
        .await;
        let pack = pack_now(&paths, &server, &format!("{tag}-pack")).await;
        (scratch, player, pack)
    }

    /// The mod a player installed themselves that the server does not allow:
    /// turned off with the admin's reason, by their own record or by what their
    /// own jar declares, and *kept* — bytes, file and record. Disabling is the
    /// whole of what this does; a player's own files are theirs.
    #[tokio::test]
    async fn a_mod_the_server_does_not_allow_is_turned_off_with_its_reason_and_kept() {
        let (scratch, server) = small_server(
            "ban",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-ban", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        // Installed through Geode, so a record names the project it came from.
        tokio::fs::write(mods.join("minimap.jar"), b"their minimap")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "XRAY".into(),
            name: "X-ray Minimap".into(),
            ..record("modrinth", "minimap.jar", ContentKind::Mod)
        });
        // Dropped into the folder by hand, so the id its jar declares is the
        // only thing that says what it is.
        let dropped = fabric_jar("freecam", "client");
        tokio::fs::write(mods.join("freecam-2.0.jar"), &dropped)
            .await
            .unwrap();
        // And one the rule says nothing about.
        tokio::fs::write(mods.join("sodium.jar"), fabric_jar("sodium", "client"))
            .await
            .unwrap();

        forbid(
            &paths,
            &server,
            &[
                by_project("XRAY", "no x-ray on this server"),
                by_mod_id("freecam", "no flying about on survival"),
            ],
        )
        .await;
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(
            report.banned,
            vec![
                Banned {
                    name: "X-ray Minimap".into(),
                    reason: "no x-ray on this server".into(),
                },
                Banned {
                    // No record names it, so it is named by its file.
                    name: "freecam-2.0.jar".into(),
                    reason: "no flying about on survival".into(),
                },
            ]
        );

        assert!(!mods.join("minimap.jar").exists());
        assert_eq!(
            std::fs::read(mods.join("minimap.jar.disabled")).unwrap(),
            b"their minimap",
            "their file was not kept, bytes and all"
        );
        // Still theirs: nothing of the server's claims it, so deleting it is
        // still their decision.
        assert_eq!(
            record_of(&player, "minimap.jar").map(|r| r.origin),
            Some(Origin::User)
        );
        assert!(!mods.join("freecam-2.0.jar").exists());
        assert_eq!(
            std::fs::read(mods.join("freecam-2.0.jar.disabled")).unwrap(),
            dropped
        );
        assert!(record_of(&player, "freecam-2.0.jar").is_none());

        // And nothing the rule did not name.
        assert!(mods.join("sodium.jar").is_file());
        assert!(mods.join("homemade.jar").is_file());
    }

    /// A ban is a standing rule, not a change that happened once — the one
    /// asymmetry with everything else an update applies. A player who turns the
    /// mod back on has it turned off again by the server's next update, and the
    /// honest consequence is that playing with it means playing somewhere else.
    /// What the test calls `update` is what a Play runs once it has a pack in
    /// hand, which is why the claim is about updates and not about Plays. A
    /// moved revision is one reason a Play fetches one; an owed settings file
    /// is the other, and the test below is that one.
    #[tokio::test]
    async fn a_mod_turned_back_on_is_turned_off_again_on_the_next_update() {
        let (scratch, mut player, pack) = one_mod_the_server_forbids("ban-again").await;
        let paths = scratch.paths.clone();
        let mods = paths.game_dir(&player.id).join("mods");

        let first = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(first.banned.len(), 1, "{first:?}");
        assert!(mods.join("minimap.jar.disabled").is_file());

        crate::instance::content::set_enabled_in(&mods, "minimap.jar.disabled", true)
            .await
            .unwrap();
        assert!(mods.join("minimap.jar").is_file());

        let again = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(again.banned, first.banned, "the rule stopped applying");
        assert!(
            !mods.join("minimap.jar").exists() && mods.join("minimap.jar.disabled").is_file(),
            "a mod turned back on stayed on"
        );

        // And an update that changed nothing says it too: what is reported is
        // what is off, not what this run happened to switch — the same rule
        // the duplicates report follows, and the reason a player who reads it
        // once is not left wondering whether it still applies.
        let steady = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(steady.banned, first.banned);
    }

    /// The Play that re-applies the rule without the server having changed a
    /// thing, which is why the claim above is about a pack arriving rather than
    /// about a revision moving.
    ///
    /// A settings file this instance could not write is left owed, and an owed
    /// path on its own makes the next Play fetch the server's pack —
    /// `commands::fetch_due`, where `update_due` alone would not. The pack that
    /// comes back is the revision the instance already holds, and it still runs
    /// a whole update through here, so a mod the player turned back on goes off
    /// again on that Play.
    ///
    /// Pinned because the two halves were built apart and met in a merge: bans
    /// were written when a pack only ever arrived on a revision that had moved,
    /// and the settings retry gave a pack a second reason to arrive.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pack_fetched_only_to_retry_a_settings_file_applies_the_bans_again() {
        use std::os::unix::fs::PermissionsExt;

        let (scratch, server) = small_server(
            "ban-owed",
            &[
                ("mods/homemade.jar", b"homemade".to_vec()),
                ("config/c.toml", b"c = 1\n".to_vec()),
            ],
            true,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "ban-owed-player", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        tokio::fs::write(mods.join("minimap.jar"), b"their minimap")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "XRAY".into(),
            name: "X-ray Minimap".into(),
            ..record("modrinth", "minimap.jar", ContentKind::Mod)
        });
        forbid(
            &paths,
            &server,
            &[by_project("XRAY", "no x-ray on this server")],
        )
        .await;

        // Their own copy of a file this server replaces, which their disk will
        // not now take — so the path is owed, which is the whole premise.
        let mine = paths.game_dir(&player.id).join("config/c.toml");
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o444)).unwrap();

        let pack = pack_now(&paths, &server, "ban-owed-pack").await;
        let first = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(first.banned.len(), 1, "{first:?}");
        assert_eq!(
            player.settings_owed,
            vec!["config/c.toml".to_string()],
            "nothing is owed, so this is not the case under test: {first:?}"
        );

        crate::instance::content::set_enabled_in(&mods, "minimap.jar.disabled", true)
            .await
            .unwrap();
        assert!(mods.join("minimap.jar").is_file());

        // The very same pack, so the revision has not moved: this is the run a
        // Play makes for the owed path and for nothing else.
        let again = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert_eq!(
            again.revision, first.revision,
            "the pack moved under the test, so this proves nothing"
        );
        assert_eq!(again.banned, first.banned, "the rule stopped applying");
        assert!(
            !mods.join("minimap.jar").exists() && mods.join("minimap.jar.disabled").is_file(),
            "a mod turned back on stayed on through a settings retry"
        );
        assert_eq!(
            player.settings_owed,
            vec!["config/c.toml".to_string()],
            "the file stopped being owed while it still had not landed"
        );
    }

    /// Nothing about a ban may ever reach a delete. The file keeps its bytes,
    /// and because the record stays the player's, the one delete that *is*
    /// theirs still works — while the refusal that guards the server's own
    /// mods is exactly as it was.
    #[tokio::test]
    async fn a_ban_never_deletes_and_the_refusal_on_the_servers_mods_is_untouched() {
        let (scratch, mut player, pack) = one_mod_the_server_forbids("ban-keeps").await;
        let paths = scratch.paths.clone();
        let mods = paths.game_dir(&player.id).join("mods");

        let before = sorted(
            std::fs::read_dir(&mods)
                .unwrap()
                .map(|e| {
                    let name = e.unwrap().file_name().to_string_lossy().to_string();
                    crate::instance::content::strip_disabled(&name).0.to_string()
                })
                .collect(),
        );

        update_offline(&paths, &mut player, &pack).await.unwrap();

        let after = sorted(
            std::fs::read_dir(&mods)
                .unwrap()
                .map(|e| {
                    let name = e.unwrap().file_name().to_string_lossy().to_string();
                    crate::instance::content::strip_disabled(&name).0.to_string()
                })
                .collect(),
        );
        assert_eq!(before, after, "a ban took a file away");

        // Theirs to delete, because the ban left the record theirs.
        crate::instance::content::remove_in(
            &mods,
            &mut player.content,
            ContentKind::Mod,
            "minimap.jar.disabled",
        )
        .await
        .unwrap();
        assert!(!mods.join("minimap.jar.disabled").exists());

        // And the server's own is still not theirs to delete.
        let refused = crate::instance::content::remove_in(
            &mods,
            &mut player.content,
            ContentKind::Mod,
            "homemade.jar",
        )
        .await
        .expect_err("a server mod was deleted")
        .to_string();
        assert!(refused.contains("came from the server's pack"), "{refused}");
        assert!(mods.join("homemade.jar").is_file());
    }

    /// The rule that makes a ban list safe to accept from a stranger: it names
    /// mods, and resolution runs against the player's own records and the ids
    /// their own jars declare. An entry holding a path, a `..`, or a file name
    /// matches nothing at all — twice over, since `forbidden_for` will not even
    /// send one.
    #[tokio::test]
    async fn a_forbidden_entry_naming_a_path_resolves_to_nothing() {
        let (scratch, server) = small_server(
            "ban-path",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-ban-path", &[]).await;
        let game = paths.game_dir(&player.id);
        let mods = game.join("mods");

        tokio::fs::write(mods.join("minimap.jar"), fabric_jar("minimap", "client"))
            .await
            .unwrap();
        tokio::fs::write(game.join("options.txt"), b"fov:90\n")
            .await
            .unwrap();

        // What a hand-written pack could hold. Every one of these names a file
        // rather than a mod, which is the whole point of refusing it.
        let hostile: Vec<Forbidden> = [
            "minimap.jar",
            "minimap.jar.disabled",
            "mods/minimap.jar",
            "../../instances/update-ban-path/mods/minimap.jar",
            "..",
            "*",
            "options.txt",
        ]
        .iter()
        .map(|named| by_mod_id(named, "a path, not a mod"))
        .chain([Forbidden {
            provider: "modrinth".into(),
            project_id: "../../options.txt".into(),
            reason: "a path, not a mod".into(),
            ..Default::default()
        }])
        .collect();

        let off = enforce_the_servers_bans(&paths, &player, &HashMap::new(), &hostile)
            .await
            .unwrap();
        assert!(off.is_empty(), "{off:?}");
        assert!(mods.join("minimap.jar").is_file());
        assert!(!mods.join("minimap.jar.disabled").exists());
        assert!(game.join("options.txt").is_file());

        // And no server of this build sends one: the share file is checked on
        // the way out, so what a pack carries is only ever identities.
        forbid(&paths, &server, &hostile).await;
        assert!(forbidden_for(&paths, &server.id).await.unwrap().is_empty());
        let pack = pack_now(&paths, &server, "b").await;
        let manifest = read_manifest(&pack).unwrap().unwrap();
        assert!(manifest.forbidden.is_empty(), "{:?}", manifest.forbidden);

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert!(report.banned.is_empty(), "{report:?}");
        assert!(mods.join("minimap.jar").is_file());
    }

    /// A list that arrives is held to the rules the admin's own typing is held
    /// to. A `geodepack` is only size-capped, so a hand-written one can carry a
    /// reason of any length over any number of lines — and the reason is read
    /// straight into the notice the player reads, where a second line is a
    /// second sentence somebody will take for Geode's own. An entry that would
    /// not pass where it was typed is dropped, and the rest of the pack still
    /// arrives.
    #[tokio::test]
    async fn a_rule_that_arrives_off_the_network_is_checked_like_the_admins_own() {
        let (scratch, server) = small_server(
            "ban-wire",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-ban-wire", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        tokio::fs::write(mods.join("minimap.jar"), fabric_jar("minimap", "client"))
            .await
            .unwrap();
        tokio::fs::write(mods.join("sodium.jar"), fabric_jar("sodium", "client"))
            .await
            .unwrap();

        let pack = pack_now(&paths, &server, "b").await;
        rewrite_pack(
            &pack,
            |manifest| {
                manifest.forbidden = vec![
                    // Longer than the notice can hold.
                    Forbidden {
                        reason: "x".repeat(crate::share::manifest::MAX_FORBIDDEN_REASON + 1),
                        ..by_mod_id("minimap", "")
                    },
                    // And a second line written to read as Geode's own.
                    by_mod_id(
                        "sodium",
                        "no x-ray\n\nGeode: your Microsoft session expired, sign in again",
                    ),
                ];
            },
            &[],
        );

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert!(report.banned.is_empty(), "{report:?}");
        assert!(mods.join("minimap.jar").is_file());
        assert!(mods.join("sodium.jar").is_file());

        // What is dropped is the entry, not the list: the same pack with a rule
        // an admin could have typed turns the mod off as it always did.
        let sound = pack_now(&paths, &server, "c").await;
        rewrite_pack(
            &sound,
            |manifest| manifest.forbidden = vec![by_mod_id("minimap", "no x-ray on this server")],
            &[],
        );

        let report = update_offline(&paths, &mut player, &sound).await.unwrap();

        assert_eq!(
            report.banned,
            vec![Banned {
                name: "minimap.jar".into(),
                reason: "no x-ray on this server".into(),
            }]
        );
        assert!(mods.join("minimap.jar.disabled").is_file());
        assert!(mods.join("sodium.jar").is_file());
    }

    /// A mod on disk under both of its names is two builds and one mod, and the
    /// enabled copy is the one a rule is read against — never whichever name
    /// the folder happens to be listed in. Deciding from one build's bytes and
    /// turning off the other decides nothing about either, and would make the
    /// same instance match or not match between two runs.
    #[tokio::test]
    async fn a_mod_under_both_its_names_is_judged_by_the_copy_the_rule_acts_on() {
        let (scratch, server) = small_server(
            "ban-two-names",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-ban-two-names", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        // The build they play with declares `coolmod`; an older one sitting
        // beside it, switched off, declares `xray`.
        let playing = fabric_jar("coolmod", "client");
        let older = fabric_jar("xray", "client");
        tokio::fs::write(mods.join("mod.jar"), &playing).await.unwrap();
        tokio::fs::write(mods.join("mod.jar.disabled"), &older)
            .await
            .unwrap();
        // And a mod they really are running that the rule names.
        tokio::fs::write(mods.join("peek.jar"), fabric_jar("xray", "client"))
            .await
            .unwrap();

        forbid(
            &paths,
            &server,
            &[by_mod_id("xray", "no x-ray on this server")],
        )
        .await;
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        // Only the one whose enabled copy is the banned mod, and nothing is
        // claimed about a mod they are not running.
        assert_eq!(
            report.banned,
            vec![Banned {
                name: "peek.jar".into(),
                reason: "no x-ray on this server".into(),
            }]
        );
        assert!(mods.join("peek.jar.disabled").is_file());
        assert_eq!(std::fs::read(mods.join("mod.jar")).unwrap(), playing);
        assert_eq!(
            std::fs::read(mods.join("mod.jar.disabled")).unwrap(),
            older,
            "the copy that decided was not the copy the rule would have acted on"
        );
    }

    /// The id an admin can read is the slug: it is what the project's own page
    /// and URL show, and the API id (`AANobbMI`) is not shown at all. A record
    /// keeps the slug beside the id, so a rule written with either matches the
    /// same mod — and a rule that matched nothing would say so nowhere, since
    /// nothing comes back from a player to the admin who wrote it.
    #[tokio::test]
    async fn a_rule_naming_a_projects_slug_matches_the_record_that_holds_it() {
        let (scratch, server) = small_server(
            "ban-slug",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-ban-slug", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        tokio::fs::write(mods.join("minimap.jar"), b"their minimap")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "AANobbMI".into(),
            slug: Some("xaeros-minimap".into()),
            name: "Xaero's Minimap".into(),
            ..record("modrinth", "minimap.jar", ContentKind::Mod)
        });
        // Another project of theirs, to show the slug reaches the record that
        // holds it and no other.
        tokio::fs::write(mods.join("sodium.jar"), b"their sodium")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "AANobbMJ".into(),
            slug: Some("sodium".into()),
            name: "Sodium".into(),
            ..record("modrinth", "sodium.jar", ContentKind::Mod)
        });

        // Pasted out of the URL bar, where a slug is lower case but a paste is
        // whatever the admin had.
        forbid(
            &paths,
            &server,
            &[by_project("Xaeros-Minimap", "no x-ray on this server")],
        )
        .await;
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert_eq!(
            report.banned,
            vec![Banned {
                name: "Xaero's Minimap".into(),
                reason: "no x-ray on this server".into(),
            }]
        );
        assert!(mods.join("minimap.jar.disabled").is_file());
        assert!(mods.join("sodium.jar").is_file());
    }

    /// A player's own copy is only turned off for a copy of the server's that
    /// is actually on. A server-side mod they keep off leaves their own copy
    /// the one they play with, and nothing is reported.
    #[tokio::test]
    async fn a_players_own_copy_stays_on_when_the_servers_is_off() {
        let (scratch, mut server) = small_server(
            "own-copy-off",
            &[("mods/homemade.jar", b"homemade".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = fresh("update-own-copy-off");
        let _ = tokio::fs::remove_dir_all(paths.instance(&player.id)).await;
        player.server_side_mods_off = true;
        let pack = pack_now(&paths, &server, "a").await;
        import(
            &offline(),
            &paths,
            &mut player,
            &pack,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        let mods = paths.game_dir(&player.id).join("mods");
        tokio::fs::write(mods.join("mine-s.jar"), b"the player's S")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "S".into(),
            name: "My S".into(),
            ..record("modrinth", "mine-s.jar", ContentKind::Mod)
        });

        // The server starts sending that project as a server-side mod, which
        // this player keeps off.
        tokio::fs::write(
            paths.server_run(&server.id).join("mods/s.jar"),
            fabric_jar("s", "server"),
        )
        .await
        .unwrap();
        server.content.push(ContentRecord {
            project_id: "S".into(),
            ..record("modrinth", "s.jar", ContentKind::Mod)
        });
        let pack = pack_now(&paths, &server, "b").await;
        seed(&paths, &server, &player, &["s.jar"]).await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        assert!(report.turned_off.is_empty(), "{report:?}");
        assert!(mods.join("s.jar.disabled").is_file() && !mods.join("s.jar").exists());
        assert!(
            mods.join("mine-s.jar").is_file(),
            "the player's own copy is the only one they have"
        );
    }

    /// An update that fails partway is saved as far as it got, so what it did
    /// has to be right already: a mod the server dropped is gone, what landed
    /// is the server's, and the player's own files are theirs. The game the
    /// instance starts stays the one it had until an update finishes.
    #[tokio::test]
    async fn a_failed_update_leaves_ownership_right_and_versions_alone() {
        let (scratch, mut server) = small_server(
            "failed",
            &[
                ("mods/gone.jar", b"gone".to_vec()),
                ("mods/stays.jar", b"stays".to_vec()),
            ],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-failed", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");
        tokio::fs::write(mods.join("own.jar"), b"the player's own")
            .await
            .unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::remove_file(run.join("mods/gone.jar"))
            .await
            .unwrap();
        tokio::fs::write(run.join("mods/new.jar"), b"new")
            .await
            .unwrap();
        // Indexed, and on no disk of the player's: fetching it reaches the
        // dead proxy and fails.
        tokio::fs::write(run.join("mods/unfetched.jar"), b"from modrinth")
            .await
            .unwrap();
        server.content.push(ContentRecord {
            project_id: "U".into(),
            ..record("modrinth", "unfetched.jar", ContentKind::Mod)
        });
        server.mc_version = "1.21.4".into();
        server.build = "0.17.0".into();
        let pack = pack_now(&paths, &server, "b").await;

        update_offline(&paths, &mut player, &pack)
            .await
            .expect_err("a download through the dead proxy");

        assert!(!mods.join("gone.jar").exists());
        assert!(
            record_of(&player, "gone.jar").is_none(),
            "{:?}",
            player.content
        );
        for landed in ["new.jar", "stays.jar"] {
            assert!(mods.join(landed).is_file(), "{landed}");
            assert_eq!(
                record_of(&player, landed).map(|r| r.origin),
                Some(Origin::Server),
                "{landed}"
            );
        }
        assert_eq!(
            std::fs::read(mods.join("own.jar")).unwrap(),
            b"the player's own"
        );
        assert!(
            record_of(&player, "own.jar").is_none(),
            "{:?}",
            player.content
        );
        assert_eq!(
            (player.mc_version.as_str(), player.loader_version.as_str()),
            ("1.21.1", "0.16.9")
        );
        assert!(player.pack.is_none(), "{:?}", player.pack);
    }

    /// The same when the update finishes but the new version is one of the
    /// files a player has to fetch by hand — a CurseForge mod, with no key
    /// here. Nothing has landed to hold the choice, so the old file stays.
    #[tokio::test]
    async fn a_mod_kept_off_survives_an_update_that_could_not_fetch_it() {
        let (scratch, mut server) = small_server(
            "kept-off-missing",
            &[("mods/hand.jar", b"hand".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-kept-off-missing", &[]).await;
        let mods = paths.game_dir(&player.id).join("mods");

        // The player holds the server's CurseForge mod, turned off. It reached
        // them through their own key on an earlier join; there is none here.
        tokio::fs::write(mods.join("p-1.jar.disabled"), b"p one")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            project_id: "123".into(),
            version_id: "v1".into(),
            origin: Origin::Server,
            ..record("curseforge", "p-1.jar", ContentKind::Mod)
        });

        // The admin moves that project on to a new version, under a new name.
        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("mods/p-2.jar"), b"p two")
            .await
            .unwrap();
        server.content = vec![ContentRecord {
            project_id: "123".into(),
            version_id: "v2".into(),
            ..record("curseforge", "p-2.jar", ContentKind::Mod)
        }];
        let pack = pack_now(&paths, &server, "missing").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();
        assert!(
            report.missing.iter().any(|m| m.contains("p-2.jar")),
            "{:?}",
            report.missing
        );
        assert!(
            mods.join("p-1.jar.disabled").is_file(),
            "the only thing holding the player's choice was deleted"
        );
        assert!(
            record_of(&player, "p-1.jar").is_some(),
            "{:?}",
            player.content
        );
    }

    /// An update that fails before the new version of a mod lands does not
    /// forget that the player had turned that mod off. The old file holds the
    /// choice until the new one is there to take it over, so the next try does
    /// not bring the mod back on.
    #[tokio::test]
    async fn a_mod_kept_off_is_not_forgotten_by_an_update_that_failed() {
        let (scratch, mut server) = small_server(
            "kept-off-fail",
            &[("mods/p-1.jar", b"p one".to_vec())],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        server.content.push(ContentRecord {
            project_id: "P".into(),
            version_id: "v1".into(),
            ..record("modrinth", "p-1.jar", ContentKind::Mod)
        });
        let mut player = join_now(&paths, &server, "update-kept-off-fail", &["p-1.jar"]).await;
        let mods = paths.game_dir(&player.id).join("mods");
        crate::instance::content::set_enabled_in(&mods, "p-1.jar", false)
            .await
            .unwrap();

        // A new version under a new name, which the player has not got and the
        // dead proxy cannot fetch.
        let run = paths.server_run(&server.id);
        tokio::fs::remove_file(run.join("mods/p-1.jar"))
            .await
            .unwrap();
        tokio::fs::write(run.join("mods/p-2.jar"), b"p two")
            .await
            .unwrap();
        server.content = vec![ContentRecord {
            project_id: "P".into(),
            version_id: "v2".into(),
            ..record("modrinth", "p-2.jar", ContentKind::Mod)
        }];
        let pack = pack_now(&paths, &server, "b").await;

        update_offline(&paths, &mut player, &pack)
            .await
            .expect_err("a download through the dead proxy");
        assert!(
            mods.join("p-1.jar.disabled").is_file(),
            "nothing else holds the player's choice yet"
        );
        assert!(
            record_of(&player, "p-1.jar").is_some(),
            "{:?}",
            player.content
        );

        // The next try, with the new version to hand: it arrives off, and the
        // old file goes.
        seed(&paths, &server, &player, &["p-2.jar"]).await;
        update_offline(&paths, &mut player, &pack).await.unwrap();

        for gone in ["p-1.jar", "p-1.jar.disabled", "p-2.jar"] {
            assert!(!mods.join(gone).exists(), "{gone} is still there");
        }
        assert!(mods.join("p-2.jar.disabled").is_file());
        assert!(
            record_of(&player, "p-1.jar").is_none(),
            "{:?}",
            player.content
        );
    }

    /// A cancelled update still leaves every file the pack brought in one state,
    /// on or off as it should be, and the versions as they were.
    #[tokio::test]
    async fn a_cancelled_update_still_settles_what_landed() {
        let (scratch, mut server) = small_server(
            "cancelled",
            &[
                ("mods/stays.jar", b"stays".to_vec()),
                ("mods/idx.jar", b"from modrinth".to_vec()),
            ],
            false,
        )
        .await;
        let paths = scratch.paths.clone();
        server.content.push(ContentRecord {
            project_id: "I".into(),
            ..record("modrinth", "idx.jar", ContentKind::Mod)
        });
        let mut player = join_now(&paths, &server, "update-cancelled", &["idx.jar"]).await;
        let mods = paths.game_dir(&player.id).join("mods");
        crate::instance::content::set_enabled_in(&mods, "idx.jar", false)
            .await
            .unwrap();

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("mods/new.jar"), b"new")
            .await
            .unwrap();
        // On the player's disk too, standing for a run interrupted after it
        // landed: a cancel is heard at the first entry of the extraction now,
        // so a run cancelled from the start brings nothing in of its own.
        tokio::fs::write(mods.join("new.jar"), b"new")
            .await
            .unwrap();
        tokio::fs::rename(
            run.join("mods/stays.jar"),
            run.join("mods/stays.jar.disabled"),
        )
        .await
        .unwrap();
        server.mc_version = "1.21.4".into();
        server.build = "0.17.0".into();
        let pack = pack_now(&paths, &server, "b").await;

        let progress = Progress::silent("t");
        progress
            .cancel_flag()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let result = update(&offline(), &paths, &mut player, &pack, None, &progress).await;
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");

        for jar in ["idx.jar", "stays.jar", "new.jar"] {
            let states = (
                mods.join(jar).is_file(),
                mods.join(format!("{jar}.disabled")).is_file(),
            );
            let want = if jar == "new.jar" {
                (true, false)
            } else {
                (false, true)
            };
            assert_eq!(states, want, "{jar}");
            assert_eq!(
                record_of(&player, jar).map(|r| r.origin),
                Some(Origin::Server),
                "{jar}"
            );
        }
        assert_eq!(
            (
                player.mc_version.as_str(),
                player.loader,
                player.loader_version.as_str()
            ),
            ("1.21.1", crate::meta::Loader::Fabric, "0.16.9")
        );
    }

    /// A pack says which revision it is, worked out from the very files it
    /// holds. What a player keeps is that, not the revision of a description
    /// read before the pack was built — a server changed and changed back in
    /// between would otherwise leave them holding one revision's name over
    /// another's content, with nothing left to notice it.
    #[tokio::test]
    async fn an_update_keeps_the_revision_the_pack_carries() {
        let (scratch, server) =
            small_server("pack-revision", &[("mods/a.jar", b"a".to_vec())], false).await;
        let paths = scratch.paths.clone();
        let mut player = join_now(&paths, &server, "update-pack-revision", &[]).await;

        let described = server_pack_info(&paths, &server).await.unwrap();
        tokio::fs::write(paths.server_run(&server.id).join("mods/b.jar"), b"b")
            .await
            .unwrap();
        let pack = pack_now(&paths, &server, "b").await;

        let report = update_offline(&paths, &mut player, &pack).await.unwrap();

        let carried = pack_revision(&read_manifest(&pack).unwrap().unwrap());
        assert_eq!(report.revision, carried);
        assert_eq!(
            report.revision.as_deref(),
            Some(revision_of(&paths, &server).await.as_str())
        );
        assert_ne!(
            report.revision.as_deref(),
            Some(described.revision.as_str())
        );

        // An admin's Geode from before packs carried one says nothing, and the
        // caller keeps the description's instead.
        let mut older = read_manifest(&pack).unwrap().unwrap();
        older.revision = None;
        assert_eq!(pack_revision(&older), None);
        older.revision = Some("  ".into());
        assert_eq!(pack_revision(&older), None);
    }

    /// Only a server's pack updates a joined instance. Anything else is refused
    /// before a single file or record is touched.
    #[tokio::test]
    async fn an_update_refuses_a_personal_pack() {
        let (scratch, inst) = fixture("personal-update", &[("mods", "homemade.jar")]).await;
        let paths = scratch.paths.clone();
        let pack = paths.root.join("personal.geodepack");
        export(Ask::Nobody, &paths, &inst, pack.clone(), &Selection::default(), &Progress::silent("t"))
            .await
            .unwrap();

        let mut player = fresh("personal-update-player");
        let mods = paths.game_dir(&player.id).join("mods");
        let _ = tokio::fs::remove_dir_all(paths.instance(&player.id)).await;
        tokio::fs::create_dir_all(&mods).await.unwrap();
        tokio::fs::write(mods.join("srv.jar"), b"the server's")
            .await
            .unwrap();
        player.content.push(ContentRecord {
            origin: Origin::Server,
            ..record("", "srv.jar", ContentKind::Mod)
        });
        let before = serde_json::to_value(&player.content).unwrap();

        let err = update_offline(&paths, &mut player, &pack)
            .await
            .expect_err("a personal pack");
        assert!(err.to_string().contains("server's pack"), "{err}");
        assert_eq!(serde_json::to_value(&player.content).unwrap(), before);
        let names: Vec<String> = std::fs::read_dir(&mods)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["srv.jar".to_string()]);
    }

    /// Every kind has an answer, and the ones that cannot give an honest one
    /// say why instead of naming a loader version that does not exist.
    #[test]
    fn every_server_kind_has_a_client_loader_or_a_reason() {
        use crate::meta::Loader;

        /// A loader and version, or words the refusal must contain.
        type Expected = std::result::Result<(Loader, &'static str), &'static str>;
        let table: &[(ServerKind, Expected)] = &[
            (ServerKind::Fabric, Ok((Loader::Fabric, "BUILD"))),
            (ServerKind::NeoForge, Ok((Loader::NeoForge, "BUILD"))),
            (ServerKind::Forge, Ok((Loader::Forge, "BUILD"))),
            (ServerKind::Paper, Ok((Loader::Vanilla, ""))),
            (ServerKind::Folia, Ok((Loader::Vanilla, ""))),
            (ServerKind::Purpur, Ok((Loader::Vanilla, ""))),
            (ServerKind::Spigot, Ok((Loader::Vanilla, ""))),
            (ServerKind::Vanilla, Ok((Loader::Vanilla, ""))),
            (ServerKind::Velocity, Err("proxy")),
            (ServerKind::Waterfall, Err("proxy")),
            (ServerKind::Quilt, Err("does not record")),
            (ServerKind::Mohist, Err("does not record")),
            (ServerKind::Arclight, Err("does not record")),
            (ServerKind::Banner, Err("does not record")),
        ];
        for kind in ServerKind::ALL {
            assert!(table.iter().any(|(k, _)| k == kind), "{kind:?} is missing from the table");
        }

        for (kind, expected) in table {
            let mut server = Server::new("s".into(), *kind, "1.21.1".into());
            server.build = "BUILD".into();
            match (client_loader(&server), expected) {
                (Ok((loader, version)), Ok((want, want_version))) => {
                    assert_eq!((loader, version.as_str()), (*want, *want_version), "{kind:?}");
                }
                (Err(e), Err(needle)) => {
                    let text = e.to_string();
                    assert!(text.contains(needle), "{kind:?}: {text}");
                    assert!(text.contains(kind.display_name()), "{kind:?} is not named: {text}");
                }
                (got, want) => panic!("{kind:?}: got {got:?}, wanted {want:?}"),
            }
        }
    }

    /// A modded server whose install never finished has no loader version, and
    /// a pack that names an empty one is a pack nothing can install.
    #[test]
    fn a_modded_server_with_no_build_is_refused() {
        for kind in [ServerKind::Fabric, ServerKind::NeoForge, ServerKind::Forge] {
            let server = Server::new("s".into(), kind, "1.21.1".into());
            let err = client_loader(&server).expect_err("an empty loader version must be refused");
            assert!(err.to_string().contains("no loader installed"), "{err}");
        }
    }

    /// The pack server hands these to a spawned task, which needs them `Send`.
    #[test]
    fn the_server_pack_futures_can_cross_threads() {
        fn send<T: Send>(_: T) {}
        let paths = Paths { root: PathBuf::from("unused") };
        let server = Server::new("s".into(), ServerKind::Fabric, "1.21.1".into());
        let progress = Progress::silent("t");
        send(export_server(&paths, &server, PathBuf::from("unused"), &progress));
        send(server_pack_info(&paths, &server));
    }

    /// This decides both what a server's pack may name and where a redirect
    /// from it may lead, so every shape that dresses up as Modrinth is refused
    /// twice over — once in the index, once in a `Location` header the CDN
    /// would have to send to reach it.
    #[test]
    fn only_modrinth_over_https_is_a_place_a_server_pack_may_download_from() {
        assert!(from_modrinth(
            "https://cdn.modrinth.com/data/AANobbMI/versions/abc/sodium.jar"
        ));
        assert!(from_modrinth(
            "https://CDN.Modrinth.com:443/data/x/versions/y/z.jar"
        ));
        for elsewhere in [
            "http://cdn.modrinth.com/data/x/versions/y/z.jar",
            "https://cdn.modrinth.com:8443/data/x/versions/y/z.jar",
            "https://cdn.modrinth.com.example.net/z.jar",
            "https://user@cdn.modrinth.com/z.jar",
            "https://github.com/owner/repo/releases/download/v1/z.jar",
            "https://192.168.1.1/z.jar",
            "file:///etc/passwd",
            "cdn.modrinth.com/z.jar",
        ] {
            assert!(!from_modrinth(elsewhere), "{elsewhere}");
        }
    }

    /// The same rule applied to a hop, plus the one the general client got for
    /// free and a custom policy does not: a pair of Modrinth addresses that
    /// redirect to each other is a chain that never ends on its own.
    #[test]
    fn a_server_packs_downloads_are_not_followed_around_forever() {
        let modrinth = "https://cdn.modrinth.com/data/x/versions/y/z.jar";
        assert!(may_follow(0, modrinth));
        assert!(may_follow(MAX_REDIRECTS, modrinth));
        assert!(!may_follow(MAX_REDIRECTS + 1, modrinth));
        assert!(!may_follow(0, "https://cdn.modrinth.com.example.net/z.jar"));
    }

    /// A redirect is not the only way off Modrinth: the client the pack's own
    /// downloads go through refuses a plain HTTP address before it is asked
    /// for, rather than requesting it and reading what came back. The address
    /// is one on the player's own machine, which is the thing that promise is
    /// about — and a refusal that is not a connection is what says it was
    /// never dialled.
    #[tokio::test]
    async fn a_server_packs_client_will_not_speak_plain_http() {
        let err = server_pack_client()
            .get("http://127.0.0.1:1/z.jar")
            .send()
            .await
            .expect_err("a plain HTTP address must not be requested");
        assert!(err.is_builder(), "{err}");
    }

    /// A server's pack in CurseForge's format carries no index to check, and
    /// its files are ids resolved through the CurseForge API with the player's
    /// own key. Nothing [`export_server`] writes takes that shape, so it is
    /// refused by name rather than quietly taking the other route.
    #[tokio::test]
    async fn a_curseforge_format_server_pack_is_refused() {
        use std::io::Write;
        let scratch = Scratch::new("cfpack");
        let dir = scratch.paths.root.clone();
        std::fs::create_dir_all(&dir).unwrap();

        let archive = dir.join("theirs.geodepack");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(
            br#"{"manifestType":"minecraftModpack","manifestVersion":1,
                 "minecraft":{"version":"1.21.1","modLoaders":[]},"name":"S","files":[]}"#,
        )
        .unwrap();
        zip.finish().unwrap();

        let err = check_server_downloads(&archive)
            .await
            .expect_err("a pack with no Modrinth index");
        assert!(err.to_string().contains("CurseForge's format"), "{err}");
    }

    #[test]
    fn a_name_becomes_a_safe_file_name() {
        assert_eq!(file_name("All the Mods 10"), "All the Mods 10.geodepack");
        assert_eq!(file_name("bad/name:here"), "bad-name-here.geodepack");
        assert_eq!(file_name("   "), "pack.geodepack");
    }

    /// An index may list a second host for a file, and the importer now tries
    /// it when the first will not answer. A server's pack is the one place
    /// that must not widen: it comes from a machine the player was only given
    /// an address for, so every URL it names — the mirror as much as the first
    /// — has to be Modrinth's, and has to be refused before a byte is
    /// fetched.
    #[tokio::test]
    async fn a_server_pack_is_still_held_to_modrinth_only() {
        let scratch = Scratch::new("mirror-host");
        let dir = scratch.paths.root.clone();
        std::fs::create_dir_all(&dir).unwrap();

        let index = serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "1",
            "name": "S",
            "files": [{
                "path": "mods/indexed.jar",
                "hashes": {},
                "fileSize": 7,
                "downloads": [
                    "https://cdn.modrinth.com/data/p/versions/v/indexed.jar",
                    "https://mirror.example.net/indexed.jar",
                ],
            }],
            "dependencies": { "minecraft": "1.21.1" },
        });
        let manifest = Manifest {
            geode_format: FORMAT,
            kind: Kind::Server,
            profile: Profile { name: "Survival".into(), ..Default::default() },
            icon: None,
            banner: None,
            source: None,
            files: Vec::new(),
            revision: None,
            listing: None,
            settings: None,
            forbidden: Vec::new(),
        };
        let archive = dir.join("theirs.geodepack");
        write_pack(&archive, &index, &manifest, &[]);

        let err = check_server_downloads(&archive)
            .await
            .expect_err("a mirror off Modrinth must be refused");
        assert!(
            err.to_string().contains("somewhere other than Modrinth"),
            "{err}"
        );

        let mut player = fresh("mirror-host-player");
        let err = import(
            &offline(),
            &scratch.paths,
            &mut player,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .expect_err("and refused by the import that reads it");
        assert!(
            err.to_string().contains("somewhere other than Modrinth"),
            "{err}"
        );
        assert!(
            !scratch.paths.game_dir(&player.id).join("mods").exists(),
            "the pack was read before it was refused"
        );
    }

    /// A `.geodepack`'s notes name CurseForge files by id, which the recipient
    /// resolves with their own key — so a pack carrying one needs a key as
    /// surely as a CurseForge pack does. Only the CurseForge *format* used to
    /// say so, which meant this pack discovered the need inside
    /// `resolve_references`, after the rest of the import had already run, and
    /// reported those mods as missing instead of asking first.
    #[tokio::test]
    async fn a_geodepack_needing_a_key_says_so_before_importing() {
        let scratch = Scratch::new("preflight");
        let dir = scratch.paths.root.clone();
        std::fs::create_dir_all(&dir).unwrap();

        let index = serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "1",
            "name": "Kitchen Sink",
            "files": [],
            "dependencies": { "minecraft": "1.21.1", "fabric-loader": "0.16.9" },
        });
        let mut manifest = Manifest {
            geode_format: FORMAT,
            kind: Kind::Instance,
            profile: Profile { name: "Kitchen Sink".into(), ..Default::default() },
            icon: None,
            banner: None,
            source: None,
            files: Vec::new(),
            revision: None,
            listing: None,
            settings: None,
            forbidden: Vec::new(),
        };

        let plain = dir.join("plain.geodepack");
        write_pack(&plain, &index, &manifest, &[]);
        assert!(
            !preview(&plain).await.unwrap().needs_curseforge_key,
            "a pack that names nothing by id needs no key"
        );

        manifest.files = vec![FileNote {
            path: "mods/create.jar".into(),
            side: None,
            source: Some(SourceRef {
                provider: "curseforge".into(),
                project_id: "328085".into(),
                version_id: "4712345".into(),
            }),
            disabled: false,
        }];
        let referencing = dir.join("referencing.geodepack");
        write_pack(&referencing, &index, &manifest, &[]);
        assert!(
            preview(&referencing).await.unwrap().needs_curseforge_key,
            "a note naming a CurseForge file needs the recipient's key"
        );
    }
}

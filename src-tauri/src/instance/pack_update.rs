//! Bringing an instance built from a published modpack up to a newer version
//! of that pack, in place.
//!
//! Three things, in the order a person meets them: noticing that the platform
//! has a newer version ([`check`]), saying what updating to it would do
//! ([`plan`]), and doing it ([`apply`]).
//!
//! # What this reuses, and what it could not
//!
//! The judgement is `share::pack::update`'s, which a server already had to
//! work out for a pack it pushes to its players: read what the pack brings
//! before touching anything; note what the player turned off *first*, by name
//! and by project, because a mod comes back under a new file name when its
//! version moves; move the player's copy to the name the pack will write so it
//! is not fetched all over again; import; settle what landed and put the
//! player's choices back on *every* exit including a failure; only then delete
//! what the pack no longer carries; turn off — never delete — the player's own
//! copy of a mod the pack now ships; and take the new version numbers last of
//! all, so a run that did not finish leaves an instance that still launches
//! and runs again next time.
//!
//! `share::pack::take_the_packs_name` and
//! `share::pack::turn_off_the_players_duplicates` are called directly, and
//! so are `share::pack::Arrival`, `share::pack::PlayerOff`,
//! `share::pack::content_file` and `share::pack::read_manifest`.
//!
//! Three of its steps had to be written again here, because all three are
//! asked of a different source of truth:
//!
//! * **Whose file is it.** A server's files carry `Origin::Server` on their
//!   record. A pack's do not and must not: a pack's mod is the player's to
//!   delete and gets its own per-mod update check, which is exactly what that
//!   tag turns off. So what the pack brought is recorded as it is brought, in
//!   `Instance::pack_files`, and that list stands in for the tag.
//! * **Config files.** A server's admin chooses a policy per path and
//!   `share::pack::landings` turns it into an action. A published pack carries
//!   no such thing, so the rule here is the flat one: a file the player
//!   already has is theirs and is kept, and one they do not have arrives. See
//!   [`how_the_packs_files_land`].
//! * **The game underneath.** A server decides what Minecraft version its
//!   players run, and its update simply takes it. A pack does not decide that
//!   for an instance that already has worlds in it — see [`refusal`].
//!
//! Two of its steps have no counterpart at all: a pack carries no list of mods
//! it forbids, and nothing here is server-side, so nothing is enforced; and
//! there is no reason to hold a published pack's downloads to Modrinth, since
//! the file came from the platform's own API rather than from an address a
//! stranger handed over.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Context, Error, Result};
use crate::meta::Loader;
use crate::net::Progress;
use crate::paths::Paths;
use crate::providers::{self, Provider, Version};
use crate::share::pack::{self, Arrival, PlayerOff};

use super::content::{self, ContentKind};
use super::{modpack, Instance, PackFile};

// ---------------------------------------------------------------------------
// Noticing
// ---------------------------------------------------------------------------

/// A newer version of the pack an instance was built from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackUpdate {
    /// What the instance's own record says it is on, so the offer reads as a
    /// move from something to something.
    pub current_version: String,
    pub version_id: String,
    pub version_number: String,
    /// `release`, `beta` or `alpha`, as the platform labels it.
    pub release_type: String,
    pub published: Option<String>,
}

/// Whether the platform has a newer version of this instance's pack.
///
/// `Ok(None)` is "nothing to offer", which covers an instance that was not
/// built from a pack at all and one built from a file on disk, since neither
/// has a project to ask about.
///
/// The same question [`crate::install::content::check_updates`] asks per mod,
/// asked once for the whole pack: what does the platform publish, which of it
/// would an install choose, and is that what this already holds. Unfiltered by
/// game version or loader, because a pack chooses those rather than being
/// chosen for one.
pub async fn check(
    client: &reqwest::Client,
    instance: &Instance,
    curseforge_key: Option<&str>,
) -> Result<Option<PackUpdate>> {
    let Some(pack) = &instance.pack else {
        return Ok(None);
    };
    let (Some(provider), Some(project_id)) = (pack.provider.as_deref(), pack.project_id.as_deref())
    else {
        return Ok(None);
    };
    let Some(provider) = Provider::parse(provider) else {
        return Ok(None);
    };
    // Said rather than swallowed: a check that can never answer should not
    // look like a check that answered "nothing new".
    if provider == Provider::CurseForge && curseforge_key.is_none() {
        return Err(Error::msg(
            "looking for a newer version of a CurseForge pack needs your CurseForge API key — \
             add one in Settings",
        ));
    }

    let versions =
        providers::versions(client, provider, project_id, None, None, curseforge_key).await?;
    Ok(
        newer_than_held(&versions, pack.version_id.as_deref(), &pack.version).map(|version| {
            PackUpdate {
                current_version: pack.version.clone(),
                version_id: version.id.clone(),
                version_number: version.version_number.clone(),
                release_type: version.release_type.clone(),
                published: version.published.clone(),
            }
        }),
    )
}

/// The version worth offering out of what the platform publishes, or none.
///
/// [`crate::install::content::pick_best`] chooses what an install would choose
/// — the newest release with a file behind it, else the newest prerelease.
/// This then refuses to call that an update unless it is one. The version
/// already held is not, obviously; nor is one the instance has already gone
/// past, which is what a player on a brand-new beta is when the newest
/// *release* is three entries further down. Offering it would read as an
/// update and install a downgrade.
///
/// Both platforms answer newest-first, which is what `pick_best` already
/// depends on, so "gone past" is "further down the list than the one held".
/// A held version the platform no longer lists has been withdrawn, and then
/// the best on offer is the only thing that can be said.
pub fn newer_than_held<'a>(
    versions: &'a [Version],
    held_id: Option<&str>,
    held_number: &str,
) -> Option<&'a Version> {
    let best = crate::install::content::pick_best(versions)?;
    let offered = versions.iter().position(|v| v.id == best.id)?;
    // By the id the instance recorded; by the version number for one saved
    // before ids were kept, which is all such an instance has to go on.
    let held = versions.iter().position(|v| match held_id {
        Some(id) => v.id == id,
        None => v.version_number == held_number,
    });
    match held {
        Some(held) if held <= offered => None,
        _ => Some(best),
    }
}

// ---------------------------------------------------------------------------
// What a pack version brings
// ---------------------------------------------------------------------------

/// What a pack says it carries, as far as the pack itself says it.
///
/// The two halves are not interchangeable and a pack of either format is short
/// of one of them. A `.mrpack` names a path and a hash per file, so its
/// content is known before a byte is fetched. A CurseForge pack names nothing
/// but `(project, file)` ids, and which file that is — and so what it will be
/// called on disk — is only known once the API has been asked, which is the
/// import's own job and not something to have a second opinion about here. So
/// a project is the thing both formats can be compared by, and a path is the
/// extra the one format gives.
#[derive(Debug, Default)]
pub(crate) struct Brings {
    /// Content files the pack writes at a path of its own, by kind and enabled
    /// name. The same shape `pack::arrivals` reads off a server's pack.
    named: HashMap<(ContentKind, String), Arrival>,
    /// The sha1 the pack's index published for one of those paths, so only the
    /// very file it means is moved to the name it writes.
    hashes: HashMap<(ContentKind, String), String>,
    /// Every project the pack names, however it names it.
    projects: HashSet<(String, String)>,
}

/// Read what a pack version carries, without installing any of it.
///
/// `format` is `modpack::PackPreview::format`, so which half of a pack to
/// read is a fact the reader already established rather than a guess made
/// twice.
async fn brings(archive: &Path, format: &str) -> Result<Brings> {
    let probe = archive.to_path_buf();
    let mut out = tokio::task::spawn_blocking(move || -> Result<Brings> {
        let mut out = Brings::default();
        if let Some(index) = modpack::read_index(&probe)? {
            for file in index.files {
                // What an index says a client does not install never lands,
                // the same rule the import and `pack::arrivals` both apply.
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
                if let Some(project) = &project {
                    out.projects.insert(project.clone());
                }
                if let Some((kind, base, off)) = pack::content_file(&file.path) {
                    if let Some(sha1) = file.hashes.get("sha1") {
                        out.hashes.insert((kind, base.clone()), sha1.clone());
                    }
                    out.named.insert(
                        (kind, base),
                        Arrival {
                            off,
                            side: None,
                            project,
                        },
                    );
                }
            }
        }
        // The Geode half, when the pack has one. A `.geodepack` names most of
        // its mods in its own notes rather than in the index beside them —
        // references, not bytes, which is this format's whole position — and
        // `share::pack::import` installs every one of them. A reader working
        // out what the pack brought has to see the same files the import
        // writes, or it files them all as the player's own.
        if let Some(manifest) = pack::read_manifest(&probe)? {
            for note in &manifest.files {
                let Some(source) = &note.source else {
                    continue;
                };
                let project = (source.provider.clone(), source.project_id.clone());
                out.projects.insert(project.clone());
                if let Some((kind, base, off)) = pack::content_file(&note.path) {
                    out.named.entry((kind, base)).or_insert(Arrival {
                        off: off || note.disabled,
                        side: None,
                        project: Some(project),
                    });
                }
            }
        }

        // The jars and packs a pack carries itself rather than naming, out of
        // the trees the import extracts — which a CurseForge pack names for
        // itself, so they are read off the pack and never assumed.
        let trees = modpack::carried_trees(&probe)?;
        let zip = modpack::open_pack(&probe)?;
        for name in zip.file_names() {
            let Some(rel) = trees
                .iter()
                .find_map(|root| name.strip_prefix(&format!("{root}/")))
            else {
                continue;
            };
            if let Some((kind, base, off)) = pack::content_file(rel) {
                out.named.entry((kind, base)).or_insert(Arrival {
                    off,
                    side: None,
                    project: None,
                });
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| Error::msg(format!("reading the pack's files panicked: {e}")))??;

    if format == "curseforge" {
        if let modpack::PackMods::CurseForge(ids) =
            modpack::index_mods(archive.to_path_buf()).await?
        {
            out.projects.extend(
                ids.into_iter()
                    .map(|id| ("curseforge".to_string(), id.to_string())),
            );
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Telling the truth before it happens
// ---------------------------------------------------------------------------

/// What updating to a pack version would do, before any of it is done.
///
/// The same shape as the export dialog's plan and for the same reason: nothing
/// here is destructive, and the person decides from it. Every list is names a
/// person would recognise — what their mods list calls them — rather than
/// paths.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePlan {
    pub name: String,
    pub from_version: String,
    pub to_version: String,
    pub to_version_id: String,
    /// The game this pack version is for, beside the one the instance runs.
    pub mc_version: String,
    pub loader: Loader,
    pub loader_version: String,
    pub from_mc_version: String,
    pub from_loader: Loader,
    /// Why Geode will not do this one in place, when it will not. The whole
    /// sentence to show, and the reason the Update button is not offered.
    pub refused: Option<String>,
    /// Content this version brings that the instance does not hold.
    pub arriving: Vec<String>,
    /// The pack's own content this version drops, which the update deletes.
    pub departing: Vec<String>,
    /// Content the pack never brought, which is the player's and stays
    /// untouched.
    pub keeping: Vec<String>,
    /// The player's own content this version of the pack takes over: a file at
    /// a path the pack now writes, which is replaced, and their own copy of a
    /// mod the pack now ships, which is turned off. Neither is deleted, and
    /// neither is "kept" either, so saying so is the point of the list.
    pub taken_over: Vec<String>,
    /// The pack's content the player turned off, which stays off.
    pub staying_off: Vec<String>,
    /// True when nothing records what this instance's pack brought, so the
    /// update replaces and adds but deletes nothing. See
    /// `Instance::pack_files`.
    pub nothing_recorded: bool,
}

/// Work out what an update to this pack version would do.
///
/// Downloads the pack to read it, which is the only way to know, and leaves it
/// in the cache so [`apply`] does not fetch it twice.
pub async fn plan(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &Instance,
    version_id: &str,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<UpdatePlan> {
    let ready = ready(
        client,
        paths,
        instance,
        version_id,
        curseforge_key,
        progress,
    )
    .await?;
    Ok(plan_ready(client, paths, instance, &ready, curseforge_key).await)
}

/// [`plan`] with the pack already resolved, fetched and read.
///
/// The same split as [`apply_ready`] and in the same place: everything above
/// this asks the platform and fetches a file, everything below it reads this
/// instance's disk and that archive. Which is also what lets the tests check
/// what a person is shown without a network.
async fn plan_ready(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &Instance,
    ready: &Ready,
    curseforge_key: Option<&str>,
) -> UpdatePlan {
    let before = instance.pack_files.clone();
    let on_disk = content_on_disk(paths, &instance.id).await;
    let held: HashSet<(String, String)> = instance
        .content
        .iter()
        .filter(|r| !r.provider.is_empty() && !r.project_id.is_empty())
        .map(|r| (r.provider.clone(), r.project_id.clone()))
        .collect();

    // Arriving: a project the pack names that nothing here holds, and a jar it
    // carries under a name that is not in this instance. A project whose build
    // merely moved is neither — that is the pack doing its job, not a mod
    // appearing.
    let coming: Vec<&(String, String)> = ready
        .brings
        .projects
        .iter()
        .filter(|project| !held.contains(*project))
        .collect();
    let titles = titles_for(client, &coming, curseforge_key).await;
    let mut arriving: Vec<String> = Vec::new();
    for project in coming {
        // The file the pack names for it, which is the one thing a `.mrpack`
        // always has and reads better than any lookup.
        let named = ready
            .brings
            .named
            .iter()
            .find_map(|((_, name), a)| (a.project.as_ref() == Some(project)).then(|| name.clone()));
        arriving.push(
            named
                .or_else(|| titles.get(project).cloned())
                .unwrap_or_else(|| project.1.clone()),
        );
    }
    for ((kind, name), arrival) in &ready.brings.named {
        if arrival.project.is_some() || on_disk.contains(&(*kind, name.clone())) {
            continue;
        }
        arriving.push(name.clone());
    }

    // Departing: what the pack brought last time and does not bring now. An
    // entry that names a project is compared by project, because its file name
    // changes with every build; one that names none is compared by name, which
    // is all a carried jar ever had.
    let mut departing: Vec<String> = Vec::new();
    for file in before.iter().flatten() {
        if !has_departed(&ready.brings, file) {
            continue;
        }
        departing.push(name_of(instance, file));
    }

    let brought: HashSet<(ContentKind, String)> = before
        .iter()
        .flatten()
        .map(|f| (f.kind, f.file_name.clone()))
        .collect();

    // Taken over: the player's own content this version of the pack claims.
    // Two ways, and the update does a different thing about each —
    //
    // * a file of theirs at a path the pack now writes is written over, since
    //   `how_the_packs_files_land` replaces every content file;
    // * their own copy of a mod the pack now ships is turned *off*, never
    //   deleted, by `share::pack::turn_off_the_players_duplicates`.
    //
    // Neither is a file that "stays yours", which is the whole reason this
    // list exists: without it the preview promised a file would be left alone
    // and the update then replaced it.
    //
    // Only the project half of the second can be answered before anything is
    // fetched — the other half reads the mod ids out of the jars themselves,
    // and the pack's jars are not on disk until the update has run. So this
    // says less than the update will do and never more, which is the safe
    // direction for a list that decides what `keeping` promises.
    let mut taken_over: HashSet<(ContentKind, String)> = on_disk
        .iter()
        .filter(|key| !brought.contains(key) && ready.brings.named.contains_key(key))
        .cloned()
        .collect();
    for record in &instance.content {
        let key = (record.kind, record.file_name.clone());
        let theirs = record.origin == content::Origin::User
            && !brought.contains(&key)
            && on_disk.contains(&key);
        let project = (record.provider.clone(), record.project_id.clone());
        let shipped = !project.0.is_empty()
            && !project.1.is_empty()
            && ready.brings.projects.contains(&project);
        if theirs && shipped {
            taken_over.insert(key);
        }
    }

    // Keeping: everything in the content folders the pack neither brought nor
    // is taking over. With nothing recorded that is nearly all of it, which is
    // exactly right — nothing is deleted, so every file is kept.
    let name_for = |kind: ContentKind, name: &str| {
        instance
            .content
            .iter()
            .find(|r| r.kind == kind && r.file_name == name)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| name.to_string())
    };
    let mut keeping: Vec<String> = on_disk
        .iter()
        .filter(|key| !brought.contains(key) && !taken_over.contains(key))
        .map(|(kind, name)| name_for(*kind, name))
        .collect();
    let mut taken_over: Vec<String> = taken_over
        .iter()
        .map(|(kind, name)| name_for(*kind, name))
        .collect();

    let mut staying_off: Vec<String> =
        players_own_off_names(paths, instance, before.iter().flatten())
            .await
            .into_iter()
            .map(|(kind, name)| name_for(kind, &name))
            .collect();

    // Sorted, because what they are read out of are sets: a person who looks
    // twice should see the same list in the same order.
    for list in [
        &mut arriving,
        &mut departing,
        &mut keeping,
        &mut taken_over,
        &mut staying_off,
    ] {
        list.sort_unstable();
        list.dedup();
    }

    UpdatePlan {
        name: ready.described.name.clone(),
        from_version: instance
            .pack
            .as_ref()
            .map(|p| p.version.clone())
            .unwrap_or_default(),
        to_version: ready.version.version_number.clone(),
        to_version_id: ready.version.id.clone(),
        mc_version: ready.described.mc_version.clone(),
        loader: ready.described.loader,
        loader_version: ready.described.loader_version.clone(),
        from_mc_version: instance.mc_version.clone(),
        from_loader: instance.loader,
        refused: ready.refused.clone(),
        arriving,
        departing,
        keeping,
        taken_over,
        staying_off,
        nothing_recorded: before.is_none(),
    }
}

/// What to call the projects a pack named only by id.
///
/// A CurseForge pack names its mods by number and nothing else, and a number
/// in a confirmation dialog tells nobody anything. One batched lookup for the
/// names alone — never for where a file goes, which is the import's to decide
/// and not something to have a second opinion about. A project the platform
/// will not name is simply left out, and the caller shows its id.
async fn titles_for(
    client: &reqwest::Client,
    projects: &[&(String, String)],
    curseforge_key: Option<&str>,
) -> HashMap<(String, String), String> {
    let Some(key) = curseforge_key else {
        return HashMap::new();
    };
    let ids: Vec<u32> = projects
        .iter()
        .filter(|(provider, _)| provider == "curseforge")
        .filter_map(|(_, id)| id.parse().ok())
        .collect();
    if ids.is_empty() {
        return HashMap::new();
    }
    crate::providers::curseforge::projects_by_ids(client, &ids, key)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|project| (("curseforge".to_string(), project.id), project.name))
        .collect()
}

/// Whether the pack has stopped carrying a file it brought before.
fn has_departed(brings: &Brings, file: &PackFile) -> bool {
    if brings
        .named
        .contains_key(&(file.kind, file.file_name.clone()))
    {
        return false;
    }
    match file.project() {
        Some(project) => !brings.projects.contains(&project),
        None => true,
    }
}

/// What to call one of the pack's files to a person: their record's name for
/// it, else the file's own.
fn name_of(instance: &Instance, file: &PackFile) -> String {
    instance
        .content
        .iter()
        .find(|r| r.kind == file.kind && r.file_name == file.file_name)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| file.file_name.clone())
}

// ---------------------------------------------------------------------------
// Doing it
// ---------------------------------------------------------------------------

/// Bring an instance up to a newer version of the pack it was built from, in
/// place.
///
/// The order is `share::pack::update`'s, and the module header says which of
/// its steps are reused and which had to be asked of this instance's own
/// record of what its pack brought.
///
/// Everything the player owns is left alone: their worlds, their options,
/// their configs, the mods they added themselves, and whatever they turned
/// off. What the pack owns is replaced, and what the new version stopped
/// carrying is deleted.
///
/// Errors only before anything has been touched — no pack source, a version
/// the platform no longer has, a download that failed, or a version whose
/// Minecraft version or loader has moved. Past that point a failure is a line
/// in the returned list rather than an error, for the reason
/// `commands::import_modpack` gives: the instance is there and most of the new
/// pack is in it, and a failure invites a retry. The instance keeps the
/// version it already had until an update finishes, so the next check offers
/// this one again.
pub async fn apply(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    version_id: &str,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    let ready = ready(
        client,
        paths,
        instance,
        version_id,
        curseforge_key,
        progress,
    )
    .await?;
    apply_ready(client, paths, instance, ready, curseforge_key, progress).await
}

/// [`apply`] with the pack already resolved, fetched and read.
///
/// The split is where the network stops: everything above this asks the
/// platform what it publishes and fetches a file, and everything below it is
/// this instance, that archive and the disk. Which is also what lets the tests
/// run the whole sequence offline.
async fn apply_ready(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    ready: Ready,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    if let Some(refused) = ready.refused {
        return Err(Error::msg(refused));
    }

    let before = instance.pack_files.clone().unwrap_or_default();
    let player_off = players_own_off(paths, instance, &before).await;
    // Which files this instance already had a record for, so the build the
    // import adds can be told from the build it replaces.
    let had: HashSet<(ContentKind, String)> = instance
        .content
        .iter()
        .map(|record| (record.kind, record.file_name.clone()))
        .collect();
    // The import relabels the instance as whatever the pack's own index calls
    // itself, which is a name and a version and nothing to look an update up
    // by. The project this instance updates from is here, and is put back.
    let source = instance.pack.clone();

    let outcome = async {
        for ((kind, name), arrival) in &ready.brings.named {
            let dir = content::content_dir(paths, &instance.id, *kind);
            pack::take_the_packs_name(
                &dir,
                name,
                arrival.off,
                ready
                    .brings
                    .hashes
                    .get(&(*kind, name.clone()))
                    .map(String::as_str),
            )
            .await?;
        }

        let imported = modpack::import_into_at(
            client,
            crate::providers::curseforge::API,
            crate::providers::modrinth::API,
            paths,
            instance,
            &ready.archive,
            curseforge_key,
            &how_the_packs_files_land(),
            progress,
        )
        .await?;
        let mut problems = imported.left_out;
        problems.extend(imported.not_written.into_iter().map(|file| file.why));
        Ok::<_, Error>(problems)
    }
    .await;

    instance.pack = source;
    // The CurseForge half of the import appends its records rather than
    // replacing them, which a first import into an empty instance never
    // noticed and a second one into the same instance would.
    dedupe_records(instance);

    // Settled before the outcome is asked for, and not a line sooner, for the
    // reason `share::pack::update` gives: a half that failed or was cancelled
    // still left files on disk, and whose they are does not depend on how it
    // ended.
    let arrived = what_arrived(paths, instance, &ready.brings, &had, &before).await;
    settle(paths, instance, &arrived, &player_off).await?;
    remove_departed(paths, instance, &before, &arrived).await?;
    let turned_off = pack::turn_off_the_players_duplicates(paths, instance, &arrived).await?;
    let now = recorded(paths, instance, &before, &arrived).await;
    instance.pack_files = Some(now);

    let landed = outcome.is_ok();
    if landed {
        crate::install::content::name_what_arrived(client, paths, instance, progress).await;
    }

    let mut problems = match outcome {
        Ok(problems) => problems,
        // The player's answer, not a problem to report back to them.
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(e) => vec![format!(
            "{e} — this instance is still on {}, and the update can be run again",
            source_version(instance)
        )],
    };
    if !turned_off.is_empty() {
        problems.push(format!(
            "turned off your own copy of {}, because this version of the pack ships the same \
             mod — two copies of one mod stops the game starting",
            turned_off.join(", ")
        ));
    }

    // Last of all, and only on a run that finished: the version is what the
    // next check compares against, and recording it for a run that did not
    // land would say this instance holds a pack it does not. A file the pack
    // only *named* and nobody may download is not that — the import finished,
    // and the line about it is the same line a fresh install of this pack
    // would have ended with.
    if landed {
        if let Some(pack) = &mut instance.pack {
            pack.version = ready.version.version_number;
            pack.version_id = Some(ready.version.id);
        }
        // The pack chooses the build of the loader, and this one is for the
        // same loader and the same Minecraft version — `refusal` is what makes
        // that true.
        instance.loader_version = ready.described.loader_version;
    }
    Ok(problems)
}

/// Write down what a pack just imported brought, so the first update knows.
///
/// Without this an instance installed today would be in the same position as
/// one installed last year: nothing recording which of its mods are the
/// pack's, so its first update could delete none of them. `format` is the
/// preview's, which the caller already read.
///
/// Answers nothing and fails at nothing on purpose. A pack this could not read
/// leaves the list unrecorded, which is the honest "nobody knows" — and an
/// import that worked should not be reported as broken because the bookkeeping
/// beside it did not.
pub async fn record_what_arrived(
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    format: &str,
) {
    if instance.pack.is_none() {
        return;
    }
    let Ok(brings) = brings(archive, format).await else {
        return;
    };
    // A fresh import had nothing before it, so everything it wrote is new.
    let arrived = what_arrived(paths, instance, &brings, &HashSet::new(), &[]).await;
    let now = recorded(paths, instance, &[], &arrived).await;
    instance.pack_files = Some(now);
}

/// How a pack's override files land in an instance that already exists.
///
/// The flat rule a published pack leaves room for, and the one the task of
/// keeping somebody's instance theirs decides: the pack owns its content
/// folders, and the player owns everything else they already have. So a mod,
/// resource pack, shader or datapack the pack carries is written over, and a
/// config, an options file, a world or a keybind the player already has is
/// left exactly as it is. A file they do not have arrives, which is how a new
/// mod's default config and a world the pack bundles still reach them.
///
/// The cost is stated rather than hidden: a pack that fixes a broken config in
/// its new version does not fix it for somebody already playing. The other way
/// round costs them an evening of tuning on every update, and the pack's own
/// changelog can tell them to delete a file.
pub(crate) fn how_the_packs_files_land() -> modpack::Overrides {
    modpack::Overrides::decided(|rel, present| {
        let is_content = rel
            .to_str()
            .is_some_and(|rel| pack::content_file(rel).is_some());
        if is_content || !present {
            modpack::Landing::Replace
        } else {
            modpack::Landing::Keep
        }
    })
}

/// What the player themselves turned off among the pack's files, read before
/// the new version lands.
///
/// `share::pack::players_own_off`'s question of this instance's own record of
/// what its pack brought. A file the pack itself shipped turned off is not the
/// player's choice and is left to the new version to decide again. Projects as
/// well as names, because the same mod comes back under a new file name when
/// its version moves.
async fn players_own_off(paths: &Paths, instance: &Instance, before: &[PackFile]) -> PlayerOff {
    let mut player_off = PlayerOff::default();
    for key in players_own_off_names(paths, instance, before.iter()).await {
        if let Some(project) = before
            .iter()
            .find(|f| (f.kind, f.file_name.clone()) == key)
            .and_then(PackFile::project)
        {
            player_off.projects.insert(project);
        }
        player_off.names.insert(key);
    }
    player_off
}

/// The pack's files the player has turned off, by kind and enabled name.
async fn players_own_off_names<'a>(
    paths: &Paths,
    instance: &Instance,
    before: impl Iterator<Item = &'a PackFile>,
) -> Vec<(ContentKind, String)> {
    let mut out = Vec::new();
    for file in before {
        if file.off {
            continue;
        }
        let dir = content::content_dir(paths, &instance.id, file.kind);
        if content::is_regular_file(&dir.join(content::disabled_name(&file.file_name))).await {
            out.push((file.kind, file.file_name.clone()));
        }
    }
    out
}

/// Every content file the new version of the pack brought, by kind and enabled
/// name.
///
/// Read after the import rather than before it, which is the one thing
/// `share::pack::arrivals` does not have to do. A `.mrpack` names a path per
/// file and so could be read either way; a CurseForge pack names `(project,
/// file)` ids, and what those are called on disk is known only once the API
/// has answered — which the import has just done, and written down. So the
/// paths come off the pack, and only a project the pack named *no* path for is
/// looked up among the records the import left.
///
/// Only a project with no path, and that distinction is load-bearing: a
/// project the pack ships a new build of has two records at this moment, the
/// old build's and the new one's, and claiming both as arrivals would leave
/// the old file looking like something the pack still carries — never deleted,
/// and a second copy of a mod beside the one that replaced it. Which of the
/// two is the pack's is decided by `had`, the records that were there before
/// the import ran: what is new is what just arrived.
///
/// And when nothing new appeared for a project at all — which is what a
/// CurseForge file left out looks like, and one failed download leaves every
/// record of that import unwritten — the only file that may be claimed for it
/// is one the pack brought *before*. The player's own copy of a mod the pack
/// also ships is a record of that project too, and claiming it would turn
/// their own file on against their choice and write it down as the pack's, for
/// the next version of the pack to delete.
async fn what_arrived(
    paths: &Paths,
    instance: &Instance,
    brings: &Brings,
    had: &HashSet<(ContentKind, String)>,
    before: &[PackFile],
) -> HashMap<(ContentKind, String), Arrival> {
    let mut arrived = brings.named.clone();
    let by_path: HashSet<(String, String)> = brings
        .named
        .values()
        .filter_map(|arrival| arrival.project.clone())
        .collect();
    let brought_before: HashSet<(ContentKind, String)> = before
        .iter()
        .map(|file| (file.kind, file.file_name.clone()))
        .collect();

    for project in &brings.projects {
        if by_path.contains(project) {
            continue;
        }
        let mut candidates: Vec<(ContentKind, String, Option<crate::instance::jar::Side>)> =
            Vec::new();
        for record in &instance.content {
            if (record.provider.clone(), record.project_id.clone()) != *project {
                continue;
            }
            let dir = content::content_dir(paths, &instance.id, record.kind);
            if content::either_name_in(&dir, &record.file_name)
                .await
                .is_none()
            {
                continue;
            }
            candidates.push((record.kind, record.file_name.clone(), record.side));
        }
        // A build that appeared during this import is the one the pack just
        // installed. Nothing new appeared when the pack ships the file this
        // instance already had — or when the file never landed — and then the
        // only file that can be the pack's is the one it brought last time.
        let fresh: Vec<_> = candidates
            .iter()
            .filter(|(kind, name, _)| !had.contains(&(*kind, name.clone())))
            .cloned()
            .collect();
        let settled: Vec<_> = if fresh.is_empty() {
            candidates
                .into_iter()
                .filter(|(kind, name, _)| brought_before.contains(&(*kind, name.clone())))
                .collect()
        } else {
            fresh
        };
        for (kind, name, side) in settled {
            arrived.entry((kind, name)).or_insert(Arrival {
                // An index entry and a manifest entry are both installed on;
                // only a path that spells `.disabled` is off, and a path is
                // already in `named`.
                off: false,
                side,
                project: Some(project.clone()),
            });
        }
    }
    arrived
}

/// Put each of the pack's files on disk once, in the state it should be in.
///
/// `share::pack::settle_server_files` without the ownership: a pack's file
/// keeps the record the import wrote for it, provider and project and all,
/// because that record is what gives the player their own update check on it
/// and their own right to delete it. What is settled is the *state*: one copy
/// of each file, off when the pack ships it off or when the player turned it
/// off before.
///
/// Safe to run after a failed import, and should be: a file that did not land
/// is simply skipped.
async fn settle(
    paths: &Paths,
    instance: &Instance,
    arrived: &HashMap<(ContentKind, String), Arrival>,
    player_off: &PlayerOff,
) -> Result<()> {
    for ((kind, name), arrival) in arrived {
        let dir = content::content_dir(paths, &instance.id, *kind);
        let disabled = content::disabled_name(name);
        let (fresh, stale) = if arrival.off {
            (&disabled, name)
        } else {
            (name, &disabled)
        };
        let (has_fresh, has_stale) = (
            content::is_regular_file(&dir.join(fresh)).await,
            content::is_regular_file(&dir.join(stale)).await,
        );
        if !has_fresh && !has_stale {
            continue;
        }
        // On and off at once is one mod twice, and the copy the pack just
        // wrote is the one it named.
        if has_fresh && has_stale {
            tokio::fs::remove_file(dir.join(stale)).await?;
        }
        let current = if has_fresh { fresh } else { stale };

        let off = arrival.off
            || player_off.names.contains(&(*kind, name.clone()))
            || arrival
                .project
                .as_ref()
                .is_some_and(|project| player_off.projects.contains(project));
        if (current == &disabled) != off {
            content::set_enabled_in(&dir, current, !off).await?;
        }
    }
    Ok(())
}

/// Delete the pack's files the new version no longer brings.
///
/// `share::pack::remove_departed` over this instance's record of what its
/// pack brought, and after the import for the reason it gives: a mod the
/// player turned off whose new build comes under a new file name has nothing
/// but its old file holding that choice until the new one has landed. Deleted
/// first, an update that then failed would forget it and bring the mod back on
/// next time.
///
/// Nothing the pack did not bring is ever touched. With no record of what it
/// brought there is nothing this can say, and it deletes nothing at all.
async fn remove_departed(
    paths: &Paths,
    instance: &mut Instance,
    before: &[PackFile],
    arrived: &HashMap<(ContentKind, String), Arrival>,
) -> Result<()> {
    for file in before {
        if arrived.contains_key(&(file.kind, file.file_name.clone())) {
            continue;
        }
        if waiting_for_its_replacement(paths, instance, file, arrived).await {
            continue;
        }
        // A record names one file in its own folder. One naming anything else
        // was not written by Geode, and nothing is deleted on its word.
        let one_file = Path::new(&file.file_name);
        if crate::paths::is_safe_relative(one_file) && one_file.components().count() == 1 {
            let dir = content::content_dir(paths, &instance.id, file.kind);
            for name in [
                file.file_name.clone(),
                content::disabled_name(&file.file_name),
            ] {
                match tokio::fs::remove_file(dir.join(name)).await {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
        }
        instance
            .content
            .retain(|r| !(r.kind == file.kind && r.file_name == file.file_name));
    }
    Ok(())
}

/// Whether this file is still the instance's only copy of a mod the new
/// version of the pack carries under a different name, because the file that
/// should replace it has not landed.
///
/// Wider than the server's rule, deliberately. A server only waits on a file
/// the player had turned *off*, because that file is the only thing holding
/// their choice — and if a mod goes missing from a joined instance, the player
/// cannot join until the next update fixes it anyway. A pack's instance is
/// played whether or not its update finished, so deleting a mod whose
/// replacement never downloaded would leave somebody a working instance with a
/// hole in it. Nothing is lost by waiting: the version is not recorded, so the
/// next check offers the update again and finishes the job.
async fn waiting_for_its_replacement(
    paths: &Paths,
    instance: &Instance,
    file: &PackFile,
    arrived: &HashMap<(ContentKind, String), Arrival>,
) -> bool {
    let Some(project) = file.project() else {
        return false;
    };
    for ((kind, name), arrival) in arrived {
        if arrival.project.as_ref() != Some(&project) {
            continue;
        }
        let dir = content::content_dir(paths, &instance.id, *kind);
        if content::either_name_in(&dir, name).await.is_none() {
            return true;
        }
    }
    false
}

/// What the pack's files are now, to compare the version after this one
/// against.
///
/// What landed, plus whatever of the last version's is still on disk — a file
/// left waiting for its replacement, or one a failed run never got to. Left
/// out, those would stop being the pack's, and the next update would leave
/// them behind for ever.
async fn recorded(
    paths: &Paths,
    instance: &Instance,
    before: &[PackFile],
    arrived: &HashMap<(ContentKind, String), Arrival>,
) -> Vec<PackFile> {
    let mut out: Vec<PackFile> = Vec::new();
    for ((kind, name), arrival) in arrived {
        let dir = content::content_dir(paths, &instance.id, *kind);
        if content::either_name_in(&dir, name).await.is_none() {
            continue;
        }
        let (provider, project_id) = arrival.project.clone().unwrap_or_default();
        out.push(PackFile {
            kind: *kind,
            file_name: name.clone(),
            off: arrival.off,
            provider,
            project_id,
        });
    }
    for file in before {
        if out.iter().any(|f| f.kind == file.kind && f.file_name == file.file_name) {
            continue;
        }
        let dir = content::content_dir(paths, &instance.id, file.kind);
        if content::either_name_in(&dir, &file.file_name).await.is_some() {
            out.push(file.clone());
        }
    }
    out.sort_unstable_by(|a, b| {
        a.kind
            .folder()
            .cmp(b.kind.folder())
            .then_with(|| a.file_name.cmp(&b.file_name))
    });
    out
}

/// One record per file, keeping the last.
///
/// The CurseForge arm of the import extends the record list rather than
/// replacing matching entries the way the Modrinth arm does, which importing
/// into a fresh instance never noticed. Importing into one that already holds
/// the same pack does.
fn dedupe_records(instance: &mut Instance) {
    let mut seen: HashSet<(ContentKind, String)> = HashSet::new();
    let mut kept: Vec<content::ContentRecord> = Vec::new();
    for record in instance.content.drain(..).rev() {
        if seen.insert((record.kind, record.file_name.clone())) {
            kept.push(record);
        }
    }
    kept.reverse();
    instance.content = kept;
}

// ---------------------------------------------------------------------------
// Getting ready, and the one update Geode will not do
// ---------------------------------------------------------------------------

/// The pack version in hand and read, with the reason not to install it when
/// there is one.
struct Ready {
    archive: PathBuf,
    version: Version,
    described: modpack::PackPreview,
    brings: Brings,
    refused: Option<String>,
}

/// Resolve a pack version, fetch it, and read what it is.
///
/// Everything that can go wrong before an instance is touched goes wrong here,
/// which is what lets [`apply`] report the rest as a list instead of a failure.
async fn ready(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &Instance,
    version_id: &str,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Ready> {
    let pack = instance
        .pack
        .as_ref()
        .ok_or_else(|| Error::msg("this instance was not made from a modpack"))?;
    let (Some(provider), Some(project_id)) = (pack.provider.as_deref(), pack.project_id.as_deref())
    else {
        return Err(Error::msg(
            "this instance's pack came from a file rather than from Modrinth or CurseForge, so \
             there is no project to fetch a newer version from",
        ));
    };
    let provider = Provider::parse(provider)
        .ok_or_else(|| Error::msg("this instance's pack names a provider Geode does not know"))?;

    let versions =
        providers::versions(client, provider, project_id, None, None, curseforge_key).await?;
    let version = versions
        .into_iter()
        .find(|v| v.id == version_id)
        .ok_or_else(|| Error::msg("that pack version is no longer available"))?;
    if let Some(reason) = &version.blocked_reason {
        return Err(Error::msg(reason.clone()));
    }
    let file = version
        .primary_file()
        .ok_or_else(|| Error::msg("that pack version has no downloadable file"))?;

    let archive = paths.cache().join("packs").join(&file.filename);
    progress.stage("Downloading pack", 1).await;
    crate::net::fetch_one(
        client,
        &crate::net::Download::new(file.url.clone(), archive.clone())
            .sha1(file.sha1.clone())
            .size(file.size)
            .label(pack.name.clone()),
    )
    .await
    .ctx("downloading the pack's new version")?;
    progress.advance(1, &file.filename).await;

    let described = modpack::preview(archive.clone()).await?;
    if described.needs_curseforge_key && curseforge_key.is_none() {
        return Err(Error::msg(
            "this CurseForge pack needs your CurseForge API key to resolve its contents — add \
             one in Settings",
        ));
    }
    let brings = brings(&archive, &described.format).await?;
    let refused = refusal(instance, &described);

    Ok(Ready {
        archive,
        version,
        described,
        brings,
        refused,
    })
}

/// Why Geode will not update this instance to this pack version in place.
///
/// A pack version that changed its mods and a pack version that changed the
/// game underneath them are not the same thing. Moving an instance to another
/// Minecraft version or another loader means every world in it is opened by a
/// different game — which converts it, and not back — every config belongs to
/// mods that are gone, and every jar on disk is built for the wrong thing. So
/// this one is not done in place at all: the instance is left exactly as it
/// is, still playable, and the person is told to install the new version
/// beside it, which `install_modpack` already does and which leaves their
/// worlds where they are.
///
/// The loader's own build moving is not this: Fabric 0.16.9 to 0.16.14 at the
/// same Minecraft version is what a pack update routinely carries, and
/// [`apply`] takes it.
fn refusal(instance: &Instance, described: &modpack::PackPreview) -> Option<String> {
    let game_moved = described.mc_version != instance.mc_version;
    let loader_moved = described.loader != instance.loader;
    if !game_moved && !loader_moved {
        return None;
    }
    Some(format!(
        "this version of the pack is for Minecraft {} on {}, and this instance is Minecraft {} on \
         {}. Geode does not change the game under an instance that already has worlds, configs \
         and mods built for the one it has — install this version as a new instance instead, and \
         this one keeps playing.",
        described.mc_version,
        described.loader.display_name(),
        instance.mc_version,
        instance.loader.display_name(),
    ))
}

/// Which version of its pack the instance is on, for a sentence about a run
/// that did not finish.
fn source_version(instance: &Instance) -> String {
    instance
        .pack
        .as_ref()
        .map(|pack| pack.version.clone())
        .unwrap_or_else(|| "the version it had".to_string())
}

/// Every content file in the instance's four content folders, by kind and
/// enabled name.
///
/// Off the disk rather than off the records, because a jar the player dropped
/// in by hand has no record and is exactly the kind of file the person reading
/// a plan wants to be told is safe.
async fn content_on_disk(paths: &Paths, id: &str) -> HashSet<(ContentKind, String)> {
    let mut out = HashSet::new();
    for kind in crate::install::identity::INSTANCE_KINDS {
        let dir = content::content_dir(paths, id, kind);
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !content::is_content_file(&name, kind) {
                continue;
            }
            let (base, _) = content::strip_disabled(&name);
            out.insert((kind, base.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! The whole sequence, run offline against a scratch directory.
    //!
    //! Every one of these fixtures carries its mods rather than downloading
    //! them: the files the index names are already on disk with the sha1 the
    //! index publishes, which `net::already_valid` recognises and skips — the
    //! same trick `share::pack`'s own fixtures use. The client handed in
    //! cannot reach anything, so a test that *did* ask the network would fail
    //! rather than pass slowly.

    use super::*;
    use crate::instance::content::{ContentRecord, Origin};
    use crate::paths::Scratch;
    use std::io::Write;

    /// A client whose every request reaches a port nothing listens on.
    fn offline() -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap()
    }

    fn sha1_of(bytes: &[u8]) -> String {
        use sha1::{Digest, Sha1};
        let mut h = Sha1::new();
        h.update(bytes);
        hex::encode(h.finalize())
    }

    /// Where a Modrinth index says one of its files lives.
    fn cdn(project: &str, version: &str, file: &str) -> String {
        format!("https://cdn.modrinth.com/data/{project}/versions/{version}/{file}")
    }

    /// One entry in a pack's index: where it goes, which project it is, and
    /// the bytes it should turn out to be.
    struct Indexed<'a> {
        path: &'a str,
        project: &'a str,
        bytes: &'a [u8],
    }

    /// A `.mrpack` built from parts.
    fn write_mrpack(
        dest: &Path,
        version: &str,
        mc: &str,
        loader_version: &str,
        indexed: &[Indexed<'_>],
        overrides: &[(&str, &[u8])],
    ) {
        let files: Vec<serde_json::Value> = indexed
            .iter()
            .map(|file| {
                let name = Path::new(file.path).file_name().unwrap().to_str().unwrap();
                serde_json::json!({
                    "path": file.path,
                    "hashes": { "sha1": sha1_of(file.bytes) },
                    "downloads": [cdn(file.project, version, name)],
                    "fileSize": file.bytes.len(),
                })
            })
            .collect();
        let index = serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": version,
            "name": "Test Pack",
            "files": files,
            "dependencies": { "minecraft": mc, "fabric-loader": loader_version },
        });

        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("modrinth.index.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(&index).unwrap()).unwrap();
        for (name, bytes) in overrides {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A CurseForge pack that names its own overrides folder and carries one
    /// jar inside it, which is the shape that catches a reader looking in the
    /// folder the convention uses rather than the one the pack named.
    fn write_cf_pack(dest: &Path, overrides: &str, carried: &str) {
        let manifest = serde_json::json!({
            "manifestType": "minecraftModpack",
            "manifestVersion": 1,
            "minecraft": {
                "version": "1.21.1",
                "modLoaders": [{ "id": "fabric-0.16.14", "primary": true }],
            },
            "name": "Test Pack",
            "version": "2.0",
            "files": [],
            "overrides": overrides,
        });
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.start_file(format!("{overrides}/{carried}"), options)
            .unwrap();
        zip.write_all(b"carried").unwrap();
        zip.finish().unwrap();
    }

    /// The Geode half of a `.geodepack`: one mod named as a reference rather
    /// than carried or put in the index beside it.
    fn geode_half(path: &str, project: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "geodeFormat": 1,
            "kind": "instance",
            "profile": { "name": "Test Pack" },
            "files": [{
                "path": path,
                "source": {
                    "provider": "modrinth",
                    "projectId": project,
                    "versionId": "ev1",
                },
            }],
        }))
        .unwrap()
    }

    fn record(file_name: &str, project: &str) -> ContentRecord {
        ContentRecord {
            file_name: file_name.into(),
            kind: ContentKind::Mod,
            provider: "modrinth".into(),
            project_id: project.into(),
            version_id: "old".into(),
            name: file_name.into(),
            version_number: "1".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Origin::User,
            requires: None,
            side: None,
            chosen_side: None,
            keep_on_server: false,
            server_off: false,
        }
    }

    fn brought(file_name: &str, project: &str) -> PackFile {
        PackFile {
            kind: ContentKind::Mod,
            file_name: file_name.into(),
            off: false,
            provider: if project.is_empty() {
                String::new()
            } else {
                "modrinth".into()
            },
            project_id: project.into(),
        }
    }

    /// An instance as a v1 install of the fixture pack left it, with the
    /// player's own things beside the pack's.
    ///
    /// Four of the pack's files: two tracked mods whose next build comes under
    /// a new name, one tracked mod the next version drops, and one jar the
    /// pack carried itself. Two of the player's: a jar they dropped in by hand
    /// and a mod they installed through Geode. Plus a config they edited, a
    /// world, and their options.
    async fn instance_on_v1(scratch: &Scratch) -> Instance {
        let paths = &scratch.paths;
        let mut inst = crate::instance::create(
            paths,
            "Test Pack".into(),
            "1.21.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        )
        .await
        .unwrap();
        let game = paths.game_dir(&inst.id);
        let mods = game.join("mods");

        for (name, bytes) in [
            ("alpha-1.jar", b"alpha one".as_slice()),
            ("beta-1.jar.disabled", b"beta one"),
            ("delta-1.jar", b"delta one"),
            ("bundled.jar", b"bundled one"),
            ("mine.jar", b"my own jar"),
            ("theirs.jar", b"a mod i installed"),
        ] {
            tokio::fs::write(mods.join(name), bytes).await.unwrap();
        }
        tokio::fs::write(game.join("config").join("pack.cfg"), b"my own settings")
            .await
            .unwrap();
        tokio::fs::create_dir_all(game.join("saves").join("home"))
            .await
            .unwrap();
        tokio::fs::write(game.join("saves").join("home").join("level.dat"), b"my world")
            .await
            .unwrap();
        tokio::fs::write(game.join("options.txt"), b"fov:90").await.unwrap();

        inst.content = vec![
            record("alpha-1.jar", "AAAAAAAA"),
            record("beta-1.jar", "BBBBBBBB"),
            record("delta-1.jar", "DDDDDDDD"),
            record("theirs.jar", "TTTTTTTT"),
        ];
        inst.pack_files = Some(vec![
            brought("alpha-1.jar", "AAAAAAAA"),
            brought("beta-1.jar", "BBBBBBBB"),
            brought("delta-1.jar", "DDDDDDDD"),
            brought("bundled.jar", ""),
        ]);
        inst.pack = Some(crate::instance::PackSource {
            name: "Test Pack".into(),
            version: "1.0".into(),
            provider: Some("modrinth".into()),
            project_id: Some("PACKPACK".into()),
            slug: Some("test-pack".into()),
            icon_url: None,
            banner_url: None,
            summary: None,
            page_url: None,
            version_id: Some("pv1".into()),
        });
        inst.save(paths).await.unwrap();
        inst
    }

    /// Version 2 of the fixture pack: new builds of alpha and beta under new
    /// file names, delta and the carried jar dropped, a new carried jar, the
    /// pack's own config and options, and a newer Fabric.
    ///
    /// `land` decides whether the two indexed jars are already on disk with
    /// the right bytes. False is the only way a fixture can make a download
    /// fail without reaching the network.
    async fn v2(scratch: &Scratch, inst: &Instance, land: bool) -> PathBuf {
        let archive = scratch.paths.cache().join("packs").join("test-pack-2.mrpack");
        write_mrpack(
            &archive,
            "2.0",
            "1.21.1",
            "0.16.14",
            &[
                Indexed { path: "mods/alpha-2.jar", project: "AAAAAAAA", bytes: b"alpha two" },
                Indexed { path: "mods/beta-2.jar", project: "BBBBBBBB", bytes: b"beta two" },
            ],
            &[
                ("overrides/mods/gamma.jar", b"gamma one".as_slice()),
                ("overrides/config/pack.cfg", b"the pack's settings"),
                ("overrides/options.txt", b"fov:70"),
            ],
        );
        if land {
            let mods = scratch.paths.game_dir(&inst.id).join("mods");
            tokio::fs::write(mods.join("alpha-2.jar"), b"alpha two").await.unwrap();
            tokio::fs::write(mods.join("beta-2.jar"), b"beta two").await.unwrap();
        }
        archive
    }

    /// The pack in hand and read, as `ready` would have handed it over.
    async fn ready_for(archive: &Path, instance: &Instance, version: &str) -> Ready {
        let described = modpack::preview(archive.to_path_buf()).await.unwrap();
        let brings = brings(archive, &described.format).await.unwrap();
        let refused = refusal(instance, &described);
        Ready {
            archive: archive.to_path_buf(),
            version: Version {
                provider: Provider::Modrinth,
                id: format!("pv{version}"),
                project_id: "PACKPACK".into(),
                name: "Test Pack".into(),
                version_number: version.into(),
                game_versions: Vec::new(),
                loaders: Vec::new(),
                release_type: "release".into(),
                published: None,
                downloads: 0,
                files: Vec::new(),
                dependencies: Vec::new(),
                blocked_reason: None,
                blocked_sha1: None,
            },
            described,
            brings,
            refused,
        }
    }

    async fn update(scratch: &Scratch, inst: &mut Instance, archive: &Path, version: &str) -> Result<Vec<String>> {
        let ready = ready_for(archive, inst, version).await;
        apply_ready(
            &offline(),
            &scratch.paths,
            inst,
            ready,
            None,
            &Progress::silent("t"),
        )
        .await
    }

    async fn exists(paths: &Paths, inst: &Instance, rel: &str) -> bool {
        tokio::fs::metadata(paths.game_dir(&inst.id).join(rel))
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn a_plan_says_what_arrives_what_goes_and_what_stays_before_anything_is_done() {
        let scratch = Scratch::new("pu-plan");
        let inst = instance_on_v1(&scratch).await;
        // Nothing seeded: a plan reads the pack and the disk and writes to
        // neither, so the instance is still on v1 while it is read out.
        let archive = v2(&scratch, &inst, false).await;
        let ready = ready_for(&archive, &inst, "2.0").await;

        let plan = plan_ready(&offline(), &scratch.paths, &inst, &ready, None).await;

        assert!(plan.refused.is_none(), "{:?}", plan.refused);
        assert!(!plan.nothing_recorded);
        assert_eq!(
            plan.arriving,
            ["gamma.jar"],
            "the jar this version carries that the instance does not have — a mod \
             whose build merely moved is not an arrival"
        );
        assert_eq!(
            plan.departing,
            ["bundled.jar", "delta-1.jar"],
            "the pack's own mod it dropped, and the jar it stopped carrying"
        );
        assert_eq!(
            plan.keeping,
            ["mine.jar", "theirs.jar"],
            "the jar they dropped in and the mod they installed"
        );
        assert!(
            plan.taken_over.is_empty(),
            "this version ships nothing of theirs: {:?}",
            plan.taken_over
        );
        assert_eq!(plan.staying_off, ["beta-1.jar"]);
    }

    #[tokio::test]
    async fn a_plan_never_promises_a_file_the_pack_takes_over_stays_yours() {
        let scratch = Scratch::new("pu-plan-unknown");
        // An instance from before Geode wrote down what its pack brought, so
        // every file in it looks like the player's own — including the two the
        // update turns off because this version of the pack ships them.
        let mut inst = instance_on_v1(&scratch).await;
        inst.pack_files = None;
        let archive = v2(&scratch, &inst, false).await;
        let ready = ready_for(&archive, &inst, "2.0").await;

        let plan = plan_ready(&offline(), &scratch.paths, &inst, &ready, None).await;

        assert!(plan.nothing_recorded);
        assert!(plan.departing.is_empty(), "{:?}", plan.departing);
        assert_eq!(
            plan.taken_over,
            ["alpha-1.jar", "beta-1.jar"],
            "their copies of the two mods this version ships"
        );
        for taken in &plan.taken_over {
            assert!(
                !plan.keeping.contains(taken),
                "{taken} is promised as kept and taken over at once"
            );
        }
        assert!(
            plan.keeping.contains(&"mine.jar".to_string())
                && plan.keeping.contains(&"delta-1.jar".to_string()),
            "and everything this version does not ship really is theirs: {:?}",
            plan.keeping
        );
    }

    #[tokio::test]
    async fn a_curseforge_pack_is_read_from_the_overrides_folder_it_names() {
        let scratch = Scratch::new("pu-cf-tree");
        let archive = scratch.paths.cache().join("packs").join("theirs.zip");
        write_cf_pack(&archive, "theirfiles", "mods/carried.jar");

        let brings = brings(&archive, "curseforge").await.unwrap();

        assert!(
            brings
                .named
                .contains_key(&(ContentKind::Mod, "carried.jar".to_string())),
            "the import extracts whatever tree the manifest names, so a reader \
             that looks in overrides/ decides this jar departed and deletes the \
             copy the import just wrote: {:?}",
            brings.named.keys()
        );
    }

    #[tokio::test]
    async fn a_pack_that_names_its_mods_in_its_geode_half_is_read_from_there() {
        let scratch = Scratch::new("pu-geode-half");
        let archive = scratch
            .paths
            .cache()
            .join("packs")
            .join("shared.geodepack");
        write_mrpack(
            &archive,
            "2.0",
            "1.21.1",
            "0.16.14",
            &[],
            &[("geode/manifest.json", &geode_half("mods/eps-1.jar", "EEEEEEEE"))],
        );

        let brings = brings(&archive, "mrpack").await.unwrap();

        assert!(
            brings
                .projects
                .contains(&("modrinth".to_string(), "EEEEEEEE".to_string())),
            "the Geode half is where a shared pack keeps most of its mods, and \
             the import installs every one of them: {:?}",
            brings.projects
        );
        assert!(
            brings
                .named
                .contains_key(&(ContentKind::Mod, "eps-1.jar".to_string())),
            "{:?}",
            brings.named.keys()
        );
    }

    #[tokio::test]
    async fn a_mod_the_player_added_themselves_survives_a_pack_update() {
        let scratch = Scratch::new("pu-mine");
        let mut inst = instance_on_v1(&scratch).await;
        let archive = v2(&scratch, &inst, true).await;

        let problems = update(&scratch, &mut inst, &archive, "2.0").await.unwrap();
        assert!(problems.is_empty(), "{problems:?}");

        assert!(exists(&scratch.paths, &inst, "mods/mine.jar").await, "the jar they dropped in");
        assert!(exists(&scratch.paths, &inst, "mods/theirs.jar").await, "the mod they installed");
        assert!(
            inst.content.iter().any(|r| r.file_name == "theirs.jar"),
            "and its record"
        );
    }

    #[tokio::test]
    async fn a_players_world_config_and_options_survive_a_pack_update() {
        let scratch = Scratch::new("pu-theirs");
        let mut inst = instance_on_v1(&scratch).await;
        let archive = v2(&scratch, &inst, true).await;

        update(&scratch, &mut inst, &archive, "2.0").await.unwrap();

        let game = scratch.paths.game_dir(&inst.id);
        assert_eq!(
            tokio::fs::read(game.join("config").join("pack.cfg")).await.unwrap(),
            b"my own settings",
            "the config they edited is still theirs"
        );
        assert_eq!(
            tokio::fs::read(game.join("options.txt")).await.unwrap(),
            b"fov:90",
            "and so are their options"
        );
        assert_eq!(
            tokio::fs::read(game.join("saves").join("home").join("level.dat")).await.unwrap(),
            b"my world"
        );
        // A file of the pack's they did not have arrives, which is how a new
        // mod's default config reaches them at all.
        assert!(exists(&scratch.paths, &inst, "mods/gamma.jar").await);
    }

    #[tokio::test]
    async fn a_mod_the_new_pack_dropped_is_removed() {
        let scratch = Scratch::new("pu-dropped");
        let mut inst = instance_on_v1(&scratch).await;
        let archive = v2(&scratch, &inst, true).await;

        update(&scratch, &mut inst, &archive, "2.0").await.unwrap();

        assert!(
            !exists(&scratch.paths, &inst, "mods/delta-1.jar").await,
            "a tracked mod this version of the pack no longer carries"
        );
        assert!(
            !inst.content.iter().any(|r| r.file_name == "delta-1.jar"),
            "and its record goes with it"
        );
        assert!(
            !exists(&scratch.paths, &inst, "mods/bundled.jar").await,
            "and so does a jar the pack used to carry itself"
        );
        // The build that replaced one is here and the one it replaced is not.
        assert!(exists(&scratch.paths, &inst, "mods/alpha-2.jar").await);
        assert!(!exists(&scratch.paths, &inst, "mods/alpha-1.jar").await);
        assert_eq!(
            inst.pack_files.as_ref().unwrap().len(),
            3,
            "alpha, beta and gamma are the pack's now: {:?}",
            inst.pack_files
        );
    }

    #[tokio::test]
    async fn a_mod_the_player_had_turned_off_is_still_off_under_its_new_name() {
        let scratch = Scratch::new("pu-off");
        let mut inst = instance_on_v1(&scratch).await;
        let archive = v2(&scratch, &inst, true).await;

        update(&scratch, &mut inst, &archive, "2.0").await.unwrap();

        assert!(
            exists(&scratch.paths, &inst, "mods/beta-2.jar.disabled").await,
            "the new build of a mod they keep off arrives off"
        );
        assert!(
            !exists(&scratch.paths, &inst, "mods/beta-2.jar").await,
            "and not on as well"
        );
        assert!(
            !exists(&scratch.paths, &inst, "mods/beta-1.jar.disabled").await,
            "the old build goes, because the new one holds the choice now"
        );
    }

    #[tokio::test]
    async fn a_pack_version_that_moved_minecraft_is_refused_and_changes_nothing() {
        let scratch = Scratch::new("pu-moved");
        let mut inst = instance_on_v1(&scratch).await;
        let archive = scratch.paths.cache().join("packs").join("test-pack-3.mrpack");
        write_mrpack(
            &archive,
            "3.0",
            "1.22",
            "0.17.0",
            &[],
            &[("overrides/mods/gamma.jar", b"gamma one".as_slice())],
        );

        let err = update(&scratch, &mut inst, &archive, "3.0").await.unwrap_err();
        let said = err.to_string();
        assert!(said.contains("1.22") && said.contains("1.21.1"), "{said}");
        assert!(said.contains("new instance"), "it says what to do instead: {said}");

        assert_eq!(inst.mc_version, "1.21.1", "the game underneath is where it was");
        assert_eq!(inst.pack.as_ref().unwrap().version, "1.0");
        assert!(exists(&scratch.paths, &inst, "mods/alpha-1.jar").await);
        assert!(
            !exists(&scratch.paths, &inst, "mods/gamma.jar").await,
            "and nothing of the refused version landed"
        );
    }

    #[tokio::test]
    async fn an_update_that_could_not_fetch_a_mod_says_so_and_still_launches() {
        let scratch = Scratch::new("pu-failed");
        let mut inst = instance_on_v1(&scratch).await;
        // Nothing is seeded on disk, so the two jars the index names have to
        // be downloaded — and this client can reach nothing.
        let archive = v2(&scratch, &inst, false).await;

        let problems = update(&scratch, &mut inst, &archive, "2.0").await.unwrap();
        assert!(
            problems.iter().any(|line| line.contains("still on 1.0")),
            "it names the version the instance is still on: {problems:?}"
        );

        assert_eq!(inst.pack.as_ref().unwrap().version, "1.0", "so the next check offers it again");
        assert_eq!(inst.loader_version, "0.16.9", "and the loader is where it was");
        assert!(
            exists(&scratch.paths, &inst, "mods/alpha-1.jar").await,
            "the build that was there is kept, because the one replacing it never landed"
        );
        assert!(
            exists(&scratch.paths, &inst, "mods/beta-1.jar.disabled").await,
            "off, as they left it"
        );
        assert!(exists(&scratch.paths, &inst, "mods/mine.jar").await, "and their own mod");
    }

    /// A CurseForge pack names `(project, file)` ids and no paths, so what the
    /// pack's own copy of a mod is called is read off the records the import
    /// left — and a pack whose download failed leaves none of them. The
    /// player's own copy of that same mod is a record of that project too.
    ///
    /// Claimed for the pack, it is turned on against their choice, written
    /// down as the pack's, and deleted by the next version that drops the mod.
    /// One CurseForge file left out — a withdrawn file, a blocked one, a
    /// network blip — does that to every mod the player also holds their own
    /// copy of.
    #[tokio::test]
    async fn a_mod_of_the_players_own_is_not_claimed_when_the_packs_own_copy_never_landed() {
        let scratch = Scratch::new("pu-claimed");
        let mut inst = instance_on_v1(&scratch).await;
        // Their own copy of the mod the pack calls `alpha`, under their own
        // name for it, turned off.
        let mods = scratch.paths.game_dir(&inst.id).join("mods");
        tokio::fs::write(mods.join("mine-alpha.jar.disabled"), b"my own alpha")
            .await
            .unwrap();
        inst.content.push(record("mine-alpha.jar", "AAAAAAAA"));

        // A pack version that names that project and no path for it, and
        // installs nothing — the shape a CurseForge pack has before its files
        // resolve, and the shape one whose download failed keeps.
        let archive = scratch.paths.cache().join("packs").join("named-only.mrpack");
        write_mrpack(&archive, "2.0", "1.21.1", "0.16.14", &[], &[]);
        let mut ready = ready_for(&archive, &inst, "2.0").await;
        ready
            .brings
            .projects
            .insert(("modrinth".into(), "AAAAAAAA".into()));

        apply_ready(
            &offline(),
            &scratch.paths,
            &mut inst,
            ready,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        let pack_files = inst.pack_files.as_ref().unwrap();
        assert!(
            !pack_files.iter().any(|f| f.file_name == "mine-alpha.jar"),
            "their own mod was written down as the pack's, for the next version to \
             delete: {pack_files:?}"
        );
        assert!(
            exists(&scratch.paths, &inst, "mods/mine-alpha.jar.disabled").await
                && !exists(&scratch.paths, &inst, "mods/mine-alpha.jar").await,
            "and it is still off, as they left it"
        );
        // The file the pack really did bring for that project last time is
        // still the pack's, and still on: that is what the fallback is for.
        assert!(
            pack_files.iter().any(|f| f.file_name == "alpha-1.jar"),
            "{pack_files:?}"
        );
        assert!(exists(&scratch.paths, &inst, "mods/alpha-1.jar").await);
    }

    #[tokio::test]
    async fn a_finished_update_records_the_version_it_landed() {
        let scratch = Scratch::new("pu-version");
        let mut inst = instance_on_v1(&scratch).await;
        let archive = v2(&scratch, &inst, true).await;

        update(&scratch, &mut inst, &archive, "2.0").await.unwrap();

        let pack = inst.pack.as_ref().unwrap();
        assert_eq!(pack.version, "2.0");
        assert_eq!(pack.version_id.as_deref(), Some("pv2.0"));
        assert_eq!(
            pack.project_id.as_deref(),
            Some("PACKPACK"),
            "and it is still the pack it was, not whatever the index called itself"
        );
        assert_eq!(inst.loader_version, "0.16.14", "the loader build the pack asks for");
        assert_eq!(inst.mc_version, "1.21.1");
    }

    #[tokio::test]
    async fn an_instance_with_nothing_recorded_deletes_nothing() {
        let scratch = Scratch::new("pu-unknown");
        let mut inst = instance_on_v1(&scratch).await;
        // An instance from before Geode wrote down what its pack brought.
        inst.pack_files = None;
        let archive = v2(&scratch, &inst, true).await;

        let problems = update(&scratch, &mut inst, &archive, "2.0").await.unwrap();

        assert!(
            exists(&scratch.paths, &inst, "mods/delta-1.jar").await,
            "every file looks equally like the player's, so none is deleted"
        );
        assert!(exists(&scratch.paths, &inst, "mods/alpha-2.jar").await, "the new one still arrives");
        // And the old build of it, which now looks like a mod of the player's
        // own, is turned off rather than deleted: two copies of one mod stops
        // the game starting, and their file is still theirs.
        assert!(exists(&scratch.paths, &inst, "mods/alpha-1.jar.disabled").await);
        assert!(
            problems.iter().any(|line| line.contains("alpha-1.jar")),
            "and they are told: {problems:?}"
        );
        assert!(
            inst.pack_files.is_some(),
            "it is recorded now, so the next update knows"
        );
    }

    /// The mistake this repo has now fixed three times elsewhere: a lookup
    /// that cannot answer reading as "nothing new". `Ok(None)` is what the
    /// Pack tab shows as up to date and offers no update from, so the one
    /// check that can never answer — a CurseForge pack with no key in Settings
    /// — has to be an error with a sentence in it instead.
    ///
    /// Beside the two answers that really are nothing to offer, so the error
    /// is not simply what this returns for everything.
    #[tokio::test]
    async fn a_check_that_cannot_ask_says_so_rather_than_reading_as_up_to_date() {
        let scratch = Scratch::new("pu-no-key");
        let mut inst = instance_on_v1(&scratch).await;
        inst.pack.as_mut().unwrap().provider = Some("curseforge".into());

        let err = check(&offline(), &inst, None).await.unwrap_err();
        assert!(
            err.to_string().contains("CurseForge API key"),
            "a check with no key has to say what is missing: {err}"
        );

        let mut no_pack = inst.clone();
        no_pack.pack = None;
        assert!(
            check(&offline(), &no_pack, None).await.unwrap().is_none(),
            "an instance that was not built from a pack has nothing to offer"
        );
        let mut from_a_file = inst.clone();
        from_a_file.pack.as_mut().unwrap().project_id = None;
        assert!(
            check(&offline(), &from_a_file, None).await.unwrap().is_none(),
            "and neither has one built from a file on disk"
        );
    }

    #[test]
    fn a_release_the_instance_has_already_gone_past_is_not_an_update() {
        let version = |id: &str, number: &str, release_type: &str| Version {
            provider: Provider::Modrinth,
            id: id.into(),
            project_id: "PACKPACK".into(),
            name: "Test Pack".into(),
            version_number: number.into(),
            game_versions: Vec::new(),
            loaders: Vec::new(),
            release_type: release_type.into(),
            published: None,
            downloads: 0,
            files: vec![crate::providers::VersionFile {
                url: "https://example.invalid/p.mrpack".into(),
                filename: "p.mrpack".into(),
                sha1: None,
                sha512: None,
                size: None,
                primary: true,
            }],
            dependencies: Vec::new(),
            blocked_reason: None,
            blocked_sha1: None,
        };
        // Newest first, as both platforms answer.
        let versions = vec![
            version("c", "3.0", "beta"),
            version("b", "2.0", "release"),
            version("a", "1.0", "release"),
        ];

        assert_eq!(
            newer_than_held(&versions, Some("a"), "1.0").map(|v| v.id.as_str()),
            Some("b"),
            "a newer release is an update"
        );
        assert!(
            newer_than_held(&versions, Some("b"), "2.0").is_none(),
            "the version already held is not"
        );
        assert!(
            newer_than_held(&versions, Some("c"), "3.0").is_none(),
            "and neither is the release below the beta they are on"
        );
        assert_eq!(
            newer_than_held(&versions, None, "1.0").map(|v| v.id.as_str()),
            Some("b"),
            "an instance that recorded no id is placed by its version number"
        );
        assert_eq!(
            newer_than_held(&versions, Some("gone"), "9.9").map(|v| v.id.as_str()),
            Some("b"),
            "a withdrawn version leaves the best on offer the only thing to say"
        );
    }
}

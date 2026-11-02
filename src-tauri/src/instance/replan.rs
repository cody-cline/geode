//! Asking the plan resolver where an *installed* instance could run.
//!
//! An instance's mods are projects pinned to one Minecraft version, and
//! [`crate::plan::resolve`] already answers "given this set of projects, which
//! (loader, game version) targets can run them all, and what does each cost?"
//! Nothing had ever pointed it at an instance. [`crate::plan::pack`] makes the
//! identical move for a published modpack and states the reason there: a pack
//! is an archive of *files* pinned to one Minecraft version, a plan wants the
//! *projects* — stripped of the version the pack happened to be built for — so
//! that the planner is free to re-target the whole set. An instance is the same
//! shape.
//!
//! This lives under `instance/` rather than in `plan/` because the instance is
//! what it reads: the provenance records in its `instance.json`, and the jars in
//! its mods folder that have no record at all. Nothing here creates, loads or
//! saves a plan — the resolver takes a list of mods, and this is the one place
//! that knows how to get that list out of something installed.
//! `instance/export.rs` and `instance/pack_update.rs` sit here for the same
//! reason.
//!
//! **Read-only.** It answers "what would moving to X cost me?" and touches
//! nothing on disk: no download, no rename, and no edit to `instance.json`.

use crate::connector::SupportMatrix;
use crate::error::{Context, Result};
use crate::net::Progress;
use crate::paths::Paths;
use crate::plan::resolve::{Resolution, SupportCache, UnresolvedMod};
use crate::plan::PlanMod;

use super::content::{ContentItem, ContentKind, Origin};

/// An instance's mods as the resolver wants them, plus the ones it cannot be
/// asked about and why.
///
/// Versionless on purpose. A [`super::content::ContentRecord`] knows which
/// version of each mod is installed, and it is a strict superset of
/// [`PlanMod`]'s fields, so the conversion is a map that drops exactly the thing
/// the question is about — carrying the installed version here would pin the
/// answer to the target the instance is already on.
///
/// The listing is taken rather than the records alone because a record is not
/// the whole folder: a jar dropped in by hand has none. Two kinds of mod come
/// back named in the second list, never silently dropped — a target reading
/// "all 4 mods run here" while twenty jars beside them were never looked at has
/// told somebody a move is clean when nobody checked. `ContentRecord::requires`
/// documents the same trap in the repo's own words: an empty answer and an
/// unknown one are different promises.
///
/// * **No provider and project id.** A hand-dropped jar, or one whose record
///   holds nothing but a choice someone made about it. No platform knows the
///   file, so nothing can be asked about which versions it has.
/// * **A server's own mods.** `install::content::check_updates` filters
///   `Origin::Server` records out of its question for a reason that holds just
///   as firmly here: those mods move when the server's do and never from this
///   side. The server chooses what it runs, sharing runs one way, and the next
///   sync would put its own version back — so re-targeting one is not on offer,
///   and saying so is the whole of what this can usefully report about it.
pub fn versionless(items: &[ContentItem]) -> (Vec<PlanMod>, Vec<UnresolvedMod>) {
    let mut mods = Vec::new();
    let mut unchecked = Vec::new();

    for item in items {
        // Asked before the provider, because a server's mod has one.
        if item.origin == Origin::Server {
            unchecked.push(UnresolvedMod {
                provider: item.provider.clone().unwrap_or_default(),
                project_id: item.project_id.clone().unwrap_or_default(),
                name: item.name.clone(),
                reason: "came from the server's pack, so the server decides which version \
                         it runs"
                    .into(),
            });
            continue;
        }

        match (item.provider.as_deref(), item.project_id.as_deref()) {
            (Some(provider), Some(project_id)) if !provider.is_empty() && !project_id.is_empty() => {
                mods.push(PlanMod {
                    provider: provider.to_string(),
                    project_id: project_id.to_string(),
                    name: item.name.clone(),
                    slug: item.slug.clone(),
                    icon_url: item.icon_url.clone(),
                });
            }
            // No identity to look up, so no identity to report either: the
            // name is all there is, and it is the file's own.
            _ => unchecked.push(UnresolvedMod {
                provider: String::new(),
                project_id: String::new(),
                name: item.name.clone(),
                reason: "was not installed from Modrinth or CurseForge, so there is no \
                         version list to check it against"
                    .into(),
            }),
        }
    }

    (mods, unchecked)
}

/// Rank the loaders and Minecraft versions this instance's mods could move to.
///
/// Mods only. Resource packs, shaders and datapacks are not loader-specific —
/// the same distinction `install::content::check_updates` draws — so they have
/// no say in which target can run the instance.
#[allow(clippy::too_many_arguments)] // the resolver's own coordinates, travelling together
pub async fn resolve(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &super::Instance,
    jars: &super::jar::MetaCache,
    matrix: &SupportMatrix,
    cache: &SupportCache,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Resolution> {
    let items = super::content::list(paths, instance, ContentKind::Mod, jars)
        .await
        .ctx("reading this instance's mods")?;
    let (mods, mut unchecked) = versionless(&items);

    let mut resolution =
        crate::plan::resolve::resolve(client, &mods, matrix, cache, curseforge_key, progress)
            .await?;
    // Beside the ones the resolver itself could not read. Both answer the same
    // question — which of this instance's mods had no say in the ranking.
    resolution.unresolved.append(&mut unchecked);
    Ok(resolution)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(name: &str, provider: &str, project_id: &str) -> ContentItem {
        ContentItem {
            file_name: format!("{name}.jar"),
            kind: ContentKind::Mod,
            name: name.into(),
            provider: Some(provider.into()),
            project_id: Some(project_id.into()),
            ..Default::default()
        }
    }

    /// A jar nobody has provenance for, as `content::list` reports one.
    fn from_disk(name: &str) -> ContentItem {
        ContentItem {
            file_name: format!("{name}.jar"),
            kind: ContentKind::Mod,
            name: name.into(),
            from_disk: true,
            ..Default::default()
        }
    }

    #[test]
    fn an_installed_mod_becomes_a_versionless_plan_entry() {
        let mut item = tracked("Sodium", "modrinth", "AANobbMI");
        item.slug = Some("sodium".into());
        item.icon_url = Some("https://example.invalid/sodium.png".into());
        // What the instance is on today, and what must not survive the move.
        item.version = "0.6.0".into();
        item.version_id = Some("abc123".into());

        let (mods, unchecked) = versionless(&[item]);
        assert!(unchecked.is_empty(), "{unchecked:?}");
        assert_eq!(
            mods,
            vec![PlanMod {
                provider: "modrinth".into(),
                project_id: "AANobbMI".into(),
                name: "Sodium".into(),
                slug: Some("sodium".into()),
                icon_url: Some("https://example.invalid/sodium.png".into()),
            }]
        );
    }

    /// The trap this exists for: a hand-dropped jar has no project id, so the
    /// resolver is blind to it. Reporting it is the difference between "this
    /// move is clean" and "this move is clean for the mods anyone could check".
    #[test]
    fn an_untracked_jar_is_reported_rather_than_counted() {
        let (mods, unchecked) = versionless(&[
            tracked("Sodium", "modrinth", "AANobbMI"),
            from_disk("homemade"),
        ]);

        assert_eq!(mods.len(), 1, "{mods:?}");
        assert_eq!(mods[0].name, "Sodium");

        assert_eq!(unchecked.len(), 1, "{unchecked:?}");
        assert_eq!(unchecked[0].name, "homemade");
        assert!(unchecked[0].provider.is_empty());
        assert!(unchecked[0].project_id.is_empty());
        assert!(
            unchecked[0].reason.contains("Modrinth or CurseForge"),
            "the reason says why nothing could be asked: {}",
            unchecked[0].reason
        );
    }

    /// A record that holds a choice about a file and nothing about where it came
    /// from reads as untracked in `content::list`, and has to read that way
    /// here too — an empty provider is not a provider.
    #[test]
    fn a_record_with_no_provider_is_untracked_too() {
        let mut item = tracked("mystery", "", "");
        item.from_disk = true;

        let (mods, unchecked) = versionless(&[item]);
        assert!(mods.is_empty(), "{mods:?}");
        assert_eq!(unchecked.len(), 1);
        assert_eq!(unchecked[0].name, "mystery");
    }

    /// `install::content::check_updates` leaves a server's mods out of its
    /// question; this leaves them out of the ranking for the same reason. It
    /// still names them, because a joined instance's folder is mostly the
    /// server's and a silent drop would make the count a lie.
    #[test]
    fn a_servers_mod_is_named_but_never_ranked() {
        let mine = tracked("Xaeros", "modrinth", "MINE");
        let theirs = ContentItem {
            origin: Origin::Server,
            ..tracked("Anti-Cheat", "modrinth", "THEIRS")
        };

        let (mods, unchecked) = versionless(&[mine, theirs]);

        assert_eq!(mods.len(), 1, "{mods:?}");
        assert_eq!(mods[0].project_id, "MINE");

        assert_eq!(unchecked.len(), 1, "{unchecked:?}");
        assert_eq!(unchecked[0].name, "Anti-Cheat");
        // Its identity is known, unlike an untracked jar's, so it is reported.
        assert_eq!(unchecked[0].project_id, "THEIRS");
        assert!(
            unchecked[0].reason.contains("server's pack"),
            "{}",
            unchecked[0].reason
        );
    }

    #[test]
    fn an_instance_with_nothing_installed_asks_nothing() {
        let (mods, unchecked) = versionless(&[]);
        assert!(mods.is_empty());
        assert!(unchecked.is_empty());
    }
}

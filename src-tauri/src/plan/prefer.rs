//! Preferring Modrinth identities over CurseForge ones.
//!
//! A plan should name each mod by its Modrinth project wherever one exists,
//! whatever route it arrived by. Mods copied out of a CurseForge pack are the
//! case that makes this matter: the manifest identifies them by numeric id, so
//! without this they stay CurseForge entries, and everything downstream —
//! resolving which versions exist, and installing them — would need the user's
//! API key for mods that are sitting on Modrinth needing no key at all.
//!
//! Matching reuses [`crate::providers::identity_key`], the same normalised
//! slug-then-name rule search already uses to fold the two platforms' listings
//! together, rather than inventing a second notion of "the same project".

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::RwLock;

use crate::providers::{self, Project, ProjectKind, SearchQuery, SortBy};

use super::PlanMod;

/// Rewrite every CurseForge mod that turns out to also live on Modrinth.
///
/// Best-effort by design: a lookup that fails, or a mod that genuinely only
/// exists on CurseForge, keeps the identity it came with. Being offline should
/// cost you the upgrade, not the edit.
pub async fn prefer_modrinth(client: &reqwest::Client, mods: &mut [PlanMod], cache: &TwinCache) {
    let pending: Vec<usize> = mods
        .iter()
        .enumerate()
        .filter(|(_, m)| m.provider.eq_ignore_ascii_case("curseforge"))
        .map(|(i, _)| i)
        .collect();
    if pending.is_empty() {
        return;
    }

    const CONCURRENCY: usize = 8;
    let mut queue = pending.into_iter();
    let mut inflight = FuturesUnordered::new();

    for _ in 0..CONCURRENCY {
        match queue.next() {
            Some(i) => inflight.push(look_up(client, cache, i, mods[i].clone())),
            None => break,
        }
    }

    let mut found: Vec<(usize, PlanMod)> = Vec::new();
    while let Some((index, twin)) = inflight.next().await {
        if let Some(twin) = twin {
            found.push((index, twin));
        }
        if let Some(i) = queue.next() {
            inflight.push(look_up(client, cache, i, mods[i].clone()));
        }
    }

    for (index, twin) in found {
        mods[index] = twin;
    }
}

async fn look_up(
    client: &reqwest::Client,
    cache: &TwinCache,
    index: usize,
    mod_: PlanMod,
) -> (usize, Option<PlanMod>) {
    if let Some(hit) = cache.get(&mod_).await {
        return (index, hit);
    }
    let twin = search_modrinth(client, &mod_).await;
    cache.put(&mod_, twin.clone()).await;
    (index, twin)
}

async fn search_modrinth(client: &reqwest::Client, mod_: &PlanMod) -> Option<PlanMod> {
    // The slug is the strong signal, and costs one request. CurseForge and
    // Modrinth agree on it far more often than not.
    if let Some(slug) = mod_.slug.as_deref().filter(|s| !s.is_empty()) {
        if let Ok(project) = providers::modrinth::project(client, slug).await {
            if is_twin(mod_, &project) {
                return Some(PlanMod::from_project(&project));
            }
        }
    }

    // Where the slugs disagree — `ae2` against `appliedenergistics2` — the
    // display name usually still lines up.
    let query = SearchQuery {
        query: mod_.name.clone(),
        kind: ProjectKind::Mod,
        game_version: None,
        loader: None,
        categories: Vec::new(),
        sort: SortBy::Relevance,
        offset: 0,
        limit: 5,
        providers: None,
    };
    let hits = providers::modrinth::search(client, &query).await.ok()?;
    hits.iter()
        .find(|hit| is_twin(mod_, hit))
        .map(PlanMod::from_project)
}

/// Whether a Modrinth project is the same mod as this plan entry.
///
/// Kept strict on purpose. A wrong match here does not show a confusing row, it
/// installs the wrong mod — so an entry with nothing to match on stays where it
/// is rather than taking the closest-looking hit.
fn is_twin(mod_: &PlanMod, candidate: &Project) -> bool {
    if candidate.kind != ProjectKind::Mod {
        return false;
    }
    let want = providers::identity_key(mod_.slug.as_deref().unwrap_or(""), &mod_.name);
    !want.is_empty() && want == providers::identity_key(&candidate.slug, &candidate.name)
}

/// Remembers which CurseForge projects have a Modrinth twin, for the session.
///
/// The negative answers are the ones worth keeping: a mod that really is
/// CurseForge-only stays in the plan, and without this it would be looked up
/// again on every single save.
/// A mod's provider and project id, as [`PlanMod::key`] returns them.
type ModKey = (String, String);
/// When we last looked, and the twin if there is one.
type Cached = (Instant, Option<PlanMod>);

pub struct TwinCache {
    inner: RwLock<HashMap<ModKey, Cached>>,
}

const TTL: Duration = Duration::from_secs(900);

impl TwinCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    async fn get(&self, mod_: &PlanMod) -> Option<Option<PlanMod>> {
        let map = self.inner.read().await;
        let (at, twin) = map.get(&mod_.key())?;
        (at.elapsed() < TTL).then(|| twin.clone())
    }

    async fn put(&self, mod_: &PlanMod, twin: Option<PlanMod>) {
        self.inner
            .write()
            .await
            .insert(mod_.key(), (Instant::now(), twin));
    }
}

impl Default for TwinCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Provider;

    fn cf(slug: &str, name: &str) -> PlanMod {
        PlanMod {
            provider: "curseforge".into(),
            project_id: "12345".into(),
            name: name.into(),
            slug: Some(slug.into()),
            icon_url: None,
        }
    }

    fn modrinth(slug: &str, name: &str) -> Project {
        Project {
            provider: Provider::Modrinth,
            id: "AANobbMI".into(),
            slug: slug.into(),
            name: name.into(),
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
            also_on: vec![],
        }
    }

    #[test]
    fn matches_when_the_slugs_agree() {
        assert!(is_twin(&cf("sodium", "Sodium"), &modrinth("sodium", "Sodium")));
    }

    #[test]
    fn matches_across_slug_punctuation() {
        assert!(is_twin(
            &cf("justenoughitems", "Just Enough Items"),
            &modrinth("just-enough-items", "Just Enough Items"),
        ));
    }

    #[test]
    fn matches_on_the_name_when_a_slug_is_too_short_to_trust() {
        // `ae2` is under the slug threshold, so the display name decides.
        assert!(is_twin(
            &cf("ae2", "Applied Energistics 2"),
            &modrinth("ae2", "Applied Energistics 2"),
        ));
    }

    #[test]
    fn refuses_a_different_mod() {
        assert!(!is_twin(&cf("sodium", "Sodium"), &modrinth("rubidium", "Rubidium")));
    }

    #[test]
    fn refuses_a_project_that_is_not_a_mod() {
        let mut pack = modrinth("sodium", "Sodium");
        pack.kind = ProjectKind::Modpack;
        assert!(!is_twin(&cf("sodium", "Sodium"), &pack));
    }

    #[test]
    fn refuses_an_entry_with_nothing_to_match_on() {
        let mut nameless = cf("", "");
        nameless.slug = None;
        assert!(!is_twin(&nameless, &modrinth("sodium", "Sodium")));
    }

    #[tokio::test]
    async fn a_plan_with_no_curseforge_mods_needs_no_lookups() {
        // No network is available in a unit test, so this passing at all is the
        // assertion: nothing was fetched.
        let mut mods = vec![PlanMod {
            provider: "modrinth".into(),
            project_id: "AANobbMI".into(),
            name: "Sodium".into(),
            slug: Some("sodium".into()),
            icon_url: None,
        }];
        prefer_modrinth(&crate::net::client(), &mut mods, &TwinCache::new()).await;
        assert_eq!(mods[0].provider, "modrinth");
    }
}

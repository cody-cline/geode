//! Finding a loader and game version that can run a whole plan.
//!
//! The shape of the problem: every mod publishes files for some set of
//! *(loader, game version)* pairs, and we need the one pair that covers as much
//! of the plan as possible — preferring Fabric, then NeoForge, then Forge.
//!
//! Two things make it more than a set intersection:
//!
//! * **Sinytra Connector** lets a Fabric file run on (Neo)Forge, so a mod with
//!   no native build is not necessarily a dead end. It is a compromise rather
//!   than a solution, so it costs.
//! * **Neighbouring patch versions** usually load — a 1.21.3 Fabric jar on
//!   1.21.1 is normally fine, the same jar on NeoForge normally is not. Also a
//!   compromise, and a dearer one on the Forge side.
//!
//! Pairs are read from the files themselves, never from a project's `loaders`
//! and `game_versions` arrays: those are independent unions, and a mod listing
//! both Fabric and 1.21.1 has not promised a Fabric build *for* 1.21.1. The
//! same trap [`crate::connector`] documents, for the same reason.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::connector::SupportMatrix;
use crate::error::Result;
use crate::net::Progress;
use crate::providers::{self, Provider, Version};

use super::{Plan, PlanMod};

/// Loaders a plan can target, most preferred first.
///
/// Fabric leads by the user's preference, then NeoForge, then Forge. Quilt is
/// last: it runs Fabric mods, so it is only ever the answer when something
/// ships a Quilt file and nothing else.
pub const LOADER_ORDER: [&str; 4] = ["fabric", "neoforge", "forge", "quilt"];

fn loader_rank(loader: &str) -> usize {
    LOADER_ORDER
        .iter()
        .position(|l| *l == loader)
        .unwrap_or(LOADER_ORDER.len())
}

fn is_forge_family(loader: &str) -> bool {
    matches!(loader, "forge" | "neoforge")
}

/// A plain Minecraft release: `1.21` or `1.21.1`, never `24w14a` or `1.21-pre2`.
///
/// Snapshots would multiply the candidate list by an order of magnitude to
/// suggest targets nobody plays a modpack on.
fn is_release(v: &str) -> bool {
    let mut parts = 0;
    for segment in v.split('.') {
        if segment.is_empty() || !segment.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        parts += 1;
    }
    (2..=3).contains(&parts)
}

/// `1.21.1` and `1.21.3` share the family `1.21`; `1.20.1` does not.
///
/// This is the unit of "minor versions can generally be combined" — within a
/// family a jar usually still loads, across families it does not.
fn family(v: &str) -> &str {
    match v.match_indices('.').nth(1) {
        Some((at, _)) => &v[..at],
        None => v,
    }
}

/// Sortable form of a release version, so `1.21.10` ranks above `1.21.9`.
fn version_key(v: &str) -> (u32, u32, u32) {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Every (loader, game version) pair one project ships a usable file for.
#[derive(Debug, Clone, Default)]
pub struct Support {
    pairs: HashSet<(String, String)>,
}

impl Support {
    pub fn from_versions(versions: &[Version]) -> Self {
        Self::from_pairs(providers::pairs_from_versions(versions))
    }

    /// Keep the pairs a plan can actually target: a loader we can install, and
    /// a plain Minecraft release.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut kept = HashSet::new();
        for (loader, game) in pairs {
            let loader = loader.to_ascii_lowercase();
            if LOADER_ORDER.contains(&loader.as_str()) && is_release(&game) {
                kept.insert((loader, game));
            }
        }
        Self { pairs: kept }
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    fn has(&self, loader: &str, game: &str) -> bool {
        self.pairs
            .contains(&(loader.to_string(), game.to_string()))
    }

    /// The newest version this loader is covered for inside `game`'s family,
    /// ignoring `game` itself. Newest because a jar built for a later patch of
    /// the same family is the usual case and normally still loads.
    fn nearest(&self, loader: &str, game: &str) -> Option<String> {
        let want = family(game);
        self.pairs
            .iter()
            .filter(|(l, g)| l == loader && g != game && family(g) == want)
            .map(|(_, g)| g.clone())
            .max_by_key(|g| version_key(g))
    }

    /// Where this mod *can* run, one entry per loader it supports.
    ///
    /// One per loader rather than the best few overall, because the best few
    /// overall is a lie by omission: JEI publishes for three loaders across
    /// dozens of versions, and taking the top three by our own loader
    /// preference listed three Fabric versions while hiding all thirty-nine
    /// Forge ones. Every loader appears here, so the list can be read as
    /// complete.
    ///
    /// The version shown is the target's own wherever the mod has it, because
    /// "the same version, on another loader" is the most useful thing this can
    /// say — it means change one thing, not two.
    fn reachable_on(&self, game: &str) -> Vec<(String, String)> {
        let mut loaders: Vec<String> = self.pairs.iter().map(|(l, _)| l.clone()).collect();
        loaders.sort_by_key(|l| loader_rank(l));
        loaders.dedup();

        loaders
            .into_iter()
            .filter_map(|loader| {
                let version = if self.has(&loader, game) {
                    game.to_string()
                } else {
                    self.pairs
                        .iter()
                        .filter(|(l, _)| *l == loader)
                        .map(|(_, g)| g.clone())
                        .max_by_key(|g| version_key(g))?
                };
                Some((loader, version))
            })
            .collect()
    }
}

/// How well one mod fits a candidate target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Fit {
    /// A file for exactly this loader and version.
    Native,
    /// A file for this loader, but a neighbouring patch version.
    NearVersion,
    /// A Fabric file, run on (Neo)Forge through Sinytra Connector.
    Connector,
    /// A Fabric file from a neighbouring patch version, through Connector.
    ConnectorNear,
    /// Nothing here works.
    Missing,
}

impl Fit {
    /// What this compromise costs the plan.
    ///
    /// Zero for a native file. A near-version jar is cheap on Fabric, where
    /// cross-patch loading usually works, and dear on the Forge side, where it
    /// usually does not. Connector sits between them, and stacking both is the
    /// sum of the two.
    fn cost(self, loader: &str) -> u32 {
        match self {
            Fit::Native => 0,
            Fit::NearVersion => {
                if is_forge_family(loader) {
                    4
                } else {
                    2
                }
            }
            Fit::Connector => 3,
            Fit::ConnectorNear => 7,
            // Counted separately: a missing mod is not a compromise, it is a
            // hard conflict, and no amount of cost should outweigh one.
            Fit::Missing => 0,
        }
    }
}

/// One mod's verdict at one target.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModFit {
    pub provider: String,
    pub project_id: String,
    pub name: String,
    pub fit: Fit,
    /// The game version whose file would be used, when it is not the target's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_version: Option<String>,
    /// Where this mod can run instead, when it cannot run here: one
    /// human-readable pair like `Fabric 1.20.1` per loader it supports, in
    /// preference order. Complete, so it can be read as the whole answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elsewhere: Vec<String>,
}

/// A candidate answer: one loader, one game version, and what it costs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub loader: String,
    pub game_version: String,
    pub fits: Vec<ModFit>,
    /// Mods with no way to run here at all.
    pub missing: usize,
    /// Mods that would need Sinytra Connector.
    pub connector: usize,
    /// Mods that would run on a jar built for a neighbouring version.
    pub near: usize,
    pub native: usize,
    /// Summed compromise cost, for ordering. Lower is better.
    pub cost: u32,
}

/// A mod that has nowhere to go, and why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedMod {
    pub provider: String,
    pub project_id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    /// Candidate targets, best first.
    pub targets: Vec<Target>,
    /// Mods we could not read a file list for at all.
    pub unresolved: Vec<UnresolvedMod>,
    /// Non-fatal problems worth showing, e.g. a missing CurseForge key.
    pub warnings: Vec<String>,
}

fn label(loader: &str, game: &str) -> String {
    let name = match loader {
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        other => other,
    };
    format!("{name} {game}")
}

/// Decide how one mod fares at one target.
fn classify(
    support: &Support,
    loader: &str,
    game: &str,
    matrix: &SupportMatrix,
) -> (Fit, Option<String>) {
    if support.has(loader, game) {
        return (Fit::Native, None);
    }
    // Quilt loads Fabric mods directly — no compatibility layer involved.
    if loader == "quilt" && support.has("fabric", game) {
        return (Fit::Native, None);
    }

    let connector_here = is_forge_family(loader) && matrix.supports(game, loader);
    if connector_here && support.has("fabric", game) {
        return (Fit::Connector, None);
    }

    if let Some(near) = support.nearest(loader, game) {
        return (Fit::NearVersion, Some(near));
    }
    if loader == "quilt" {
        if let Some(near) = support.nearest("fabric", game) {
            return (Fit::NearVersion, Some(near));
        }
    }
    if connector_here {
        if let Some(near) = support.nearest("fabric", game) {
            return (Fit::ConnectorNear, Some(near));
        }
    }

    (Fit::Missing, None)
}

/// Score and order every target the plan's mods make possible.
///
/// Pure, so the whole ranking is testable without touching the network.
///
/// The ordering is lexicographic rather than a weighted sum, because each step
/// answers a different question and they are not interchangeable: a target that
/// drops a mod is never rescued by being on a nicer loader, and a shaky Fabric
/// answer should lose to a clean NeoForge one.
pub fn rank(mods: &[(PlanMod, Support)], matrix: &SupportMatrix, limit: usize) -> Vec<Target> {
    let mut candidates: HashSet<(String, String)> = HashSet::new();
    for (_, support) in mods {
        for pair in &support.pairs {
            candidates.insert(pair.clone());
        }
    }

    let mut targets: Vec<Target> = candidates
        .into_iter()
        .map(|(loader, game)| {
            let mut target = Target {
                fits: Vec::with_capacity(mods.len()),
                missing: 0,
                connector: 0,
                near: 0,
                native: 0,
                cost: 0,
                loader: loader.clone(),
                game_version: game.clone(),
            };

            for (plan_mod, support) in mods {
                let (fit, via) = classify(support, &loader, &game, matrix);
                match fit {
                    Fit::Native => target.native += 1,
                    Fit::Connector => target.connector += 1,
                    Fit::NearVersion => target.near += 1,
                    Fit::ConnectorNear => {
                        target.connector += 1;
                        target.near += 1;
                    }
                    Fit::Missing => target.missing += 1,
                }
                target.cost += fit.cost(&loader);

                target.fits.push(ModFit {
                    provider: plan_mod.provider.clone(),
                    project_id: plan_mod.project_id.clone(),
                    name: plan_mod.name.clone(),
                    fit,
                    via_version: via,
                    elsewhere: if fit == Fit::Missing {
                        support
                            .reachable_on(&game)
                            .iter()
                            .map(|(l, g)| label(l, g))
                            .collect()
                    } else {
                        Vec::new()
                    },
                });
            }
            target
        })
        .collect();

    targets.sort_by(|a, b| {
        a.missing
            .cmp(&b.missing)
            .then_with(|| a.cost.cmp(&b.cost))
            .then_with(|| loader_rank(&a.loader).cmp(&loader_rank(&b.loader)))
            .then_with(|| version_key(&b.game_version).cmp(&version_key(&a.game_version)))
    });
    targets.truncate(limit);
    targets
}

/// How many candidate targets to hand back. Enough to show the runner-up
/// loaders and a fallback version, few enough to read.
const TARGET_LIMIT: usize = 6;

/// Read every mod's file list, then rank the targets they allow.
pub async fn resolve(
    client: &reqwest::Client,
    plan: &Plan,
    matrix: &SupportMatrix,
    cache: &SupportCache,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Resolution> {
    let mut warnings = Vec::new();
    let mut unresolved = Vec::new();
    let mut resolved: Vec<(PlanMod, Support)> = Vec::new();

    if plan.mods.is_empty() {
        return Ok(Resolution {
            targets: Vec::new(),
            unresolved,
            warnings,
        });
    }

    progress
        .stage("Reading published versions", plan.mods.len() as u64)
        .await;

    // Bounded concurrency: Modrinth is generous but a 60-mod plan firing 60
    // simultaneous requests is rude and gets rate-limited for it.
    const CONCURRENCY: usize = 8;
    let mut pending = plan.mods.iter().cloned();
    let mut inflight = FuturesUnordered::new();

    for _ in 0..CONCURRENCY.min(plan.mods.len()) {
        if let Some(plan_mod) = pending.next() {
            inflight.push(support_for(client, cache, plan_mod, curseforge_key));
        }
    }

    while let Some((plan_mod, outcome)) = inflight.next().await {
        progress.advance(1, &plan_mod.name).await;
        progress.check_cancelled()?;

        match outcome {
            Ok(support) if support.is_empty() => unresolved.push(UnresolvedMod {
                provider: plan_mod.provider.clone(),
                project_id: plan_mod.project_id.clone(),
                name: plan_mod.name.clone(),
                reason: "publishes no downloadable file for any loader".into(),
            }),
            Ok(support) => resolved.push((plan_mod, support)),
            Err(e) => unresolved.push(UnresolvedMod {
                provider: plan_mod.provider.clone(),
                project_id: plan_mod.project_id.clone(),
                name: plan_mod.name.clone(),
                reason: e.to_string(),
            }),
        }

        if let Some(plan_mod) = pending.next() {
            inflight.push(support_for(client, cache, plan_mod, curseforge_key));
        }
    }

    if curseforge_key.is_none()
        && plan
            .mods
            .iter()
            .any(|m| m.provider.eq_ignore_ascii_case("curseforge"))
    {
        warnings.push(
            "CurseForge mods cannot be checked without your own API key — add one in Settings."
                .into(),
        );
    }
    if matrix.is_empty() {
        warnings.push(
            "Sinytra Connector's version list could not be read, so Fabric mods are not \
             being offered on Forge or NeoForge."
                .into(),
        );
    }

    // Order is otherwise whatever the requests finished in, which would make
    // the same plan rank differently between runs.
    resolved.sort_by_key(|(plan_mod, _)| plan_mod.key());

    Ok(Resolution {
        targets: rank(&resolved, matrix, TARGET_LIMIT),
        unresolved,
        warnings,
    })
}

async fn support_for(
    client: &reqwest::Client,
    cache: &SupportCache,
    plan_mod: PlanMod,
    curseforge_key: Option<&str>,
) -> (PlanMod, Result<Support>) {
    let result = async {
        if let Some(hit) = cache.get(&plan_mod).await {
            return Ok(hit);
        }
        let provider = Provider::parse(&plan_mod.provider)
            .ok_or_else(|| crate::error::Error::msg("unknown provider"))?;
        // Everything the project has ever shipped, not a recent page of it:
        // what a plan can target is defined by the whole history.
        let pairs =
            providers::support_pairs(client, provider, &plan_mod.project_id, curseforge_key)
                .await?;
        let support = Support::from_pairs(pairs);
        cache.put(&plan_mod, support.clone()).await;
        Ok(support)
    }
    .await;
    (plan_mod, result)
}

/// Caches each project's support table for the session.
///
/// Resolving re-reads every mod in the plan, and adding one mod re-resolves the
/// lot — without this, a 40-mod plan would spend 40 requests to answer a
/// question that changed for one of them.
pub struct SupportCache {
    inner: RwLock<HashMap<(String, String), (Instant, Support)>>,
}

/// Long enough that editing a plan is instant, short enough that a mod
/// publishing a new build shows up within one sitting.
const TTL: Duration = Duration::from_secs(900);

impl SupportCache {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    async fn get(&self, plan_mod: &PlanMod) -> Option<Support> {
        let map = self.inner.read().await;
        let (at, support) = map.get(&plan_mod.key())?;
        (at.elapsed() < TTL).then(|| support.clone())
    }

    async fn put(&self, plan_mod: &PlanMod, support: Support) {
        self.inner
            .write()
            .await
            .insert(plan_mod.key(), (Instant::now(), support));
    }
}

impl Default for SupportCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::SupportPair;

    fn support(pairs: &[(&str, &str)]) -> Support {
        Support {
            pairs: pairs
                .iter()
                .map(|(l, g)| (l.to_string(), g.to_string()))
                .collect(),
        }
    }

    fn plan_mod(name: &str) -> PlanMod {
        PlanMod {
            provider: "modrinth".into(),
            project_id: name.into(),
            name: name.into(),
            slug: None,
            icon_url: None,
        }
    }

    fn entry(name: &str, pairs: &[(&str, &str)]) -> (PlanMod, Support) {
        (plan_mod(name), support(pairs))
    }

    /// Connector covering NeoForge on 1.21.1 only, like the real matrix.
    fn matrix() -> SupportMatrix {
        SupportMatrix::from_pairs(vec![SupportPair {
            loader: "neoforge".into(),
            game_version: "1.21.1".into(),
        }])
    }

    fn empty_matrix() -> SupportMatrix {
        SupportMatrix::from_pairs(Vec::new())
    }

    #[test]
    fn reads_release_versions_and_ignores_snapshots() {
        assert!(is_release("1.21"));
        assert!(is_release("1.21.1"));
        assert!(!is_release("24w14a"));
        assert!(!is_release("1.21-pre2"));
        assert!(!is_release("1"));
        assert!(!is_release("1.21.1.1"));
    }

    #[test]
    fn groups_patch_versions_into_one_family() {
        assert_eq!(family("1.21.1"), "1.21");
        assert_eq!(family("1.21.10"), "1.21");
        assert_eq!(family("1.21"), "1.21");
        assert_ne!(family("1.20.1"), family("1.21.1"));
    }

    #[test]
    fn prefers_fabric_when_both_loaders_work() {
        let mods = vec![
            entry("a", &[("fabric", "1.21.1"), ("neoforge", "1.21.1")]),
            entry("b", &[("fabric", "1.21.1"), ("neoforge", "1.21.1")]),
        ];
        let best = &rank(&mods, &matrix(), 6)[0];
        assert_eq!(best.loader, "fabric");
        assert_eq!(best.game_version, "1.21.1");
        assert_eq!(best.missing, 0);
        assert_eq!(best.cost, 0);
    }

    #[test]
    fn covering_every_mod_beats_the_preferred_loader() {
        // Fabric would drop `b` entirely; NeoForge runs the lot.
        let mods = vec![
            entry("a", &[("fabric", "1.21.1"), ("neoforge", "1.21.1")]),
            entry("b", &[("neoforge", "1.21.1")]),
        ];
        let best = &rank(&mods, &empty_matrix(), 6)[0];
        assert_eq!(best.loader, "neoforge");
        assert_eq!(best.missing, 0);
    }

    #[test]
    fn a_clean_neoforge_beats_a_compromised_fabric() {
        // Both cover everything, but Fabric only by reaching across the family.
        let mods = vec![
            entry("a", &[("fabric", "1.21.3"), ("neoforge", "1.21.1")]),
            entry("b", &[("fabric", "1.21.1"), ("neoforge", "1.21.1")]),
        ];
        let best = &rank(&mods, &empty_matrix(), 6)[0];
        assert_eq!(best.loader, "neoforge");
        assert_eq!(best.cost, 0);
    }

    #[test]
    fn connector_carries_a_fabric_only_mod_onto_neoforge() {
        let mods = vec![
            entry("native", &[("neoforge", "1.21.1")]),
            entry("fabric-only", &[("fabric", "1.21.1")]),
        ];
        let targets = rank(&mods, &matrix(), 6);
        let neoforge = targets
            .iter()
            .find(|t| t.loader == "neoforge" && t.game_version == "1.21.1")
            .expect("a NeoForge target");
        assert_eq!(neoforge.missing, 0);
        assert_eq!(neoforge.connector, 1);
        assert_eq!(neoforge.cost, Fit::Connector.cost("neoforge"));
    }

    #[test]
    fn connector_is_not_offered_where_it_has_no_build() {
        // Same shape as above, but Connector covers nothing at 1.20.1.
        let mods = vec![
            entry("native", &[("neoforge", "1.20.1")]),
            entry("fabric-only", &[("fabric", "1.20.1")]),
        ];
        let targets = rank(&mods, &matrix(), 6);
        let neoforge = targets
            .iter()
            .find(|t| t.loader == "neoforge" && t.game_version == "1.20.1")
            .expect("a NeoForge target");
        assert_eq!(neoforge.missing, 1);
        assert_eq!(neoforge.connector, 0);
    }

    #[test]
    fn a_native_build_is_used_rather_than_connector() {
        // The mod ships both; Connector is a last resort, not a coin toss.
        let mods = vec![entry("both", &[("fabric", "1.21.1"), ("neoforge", "1.21.1")])];
        let targets = rank(&mods, &matrix(), 6);
        let neoforge = targets
            .iter()
            .find(|t| t.loader == "neoforge")
            .expect("a NeoForge target");
        assert_eq!(neoforge.fits[0].fit, Fit::Native);
        assert_eq!(neoforge.connector, 0);
    }

    #[test]
    fn reaching_across_versions_costs_more_on_the_forge_side() {
        assert!(Fit::NearVersion.cost("neoforge") > Fit::NearVersion.cost("fabric"));
        assert!(Fit::NearVersion.cost("forge") > Fit::NearVersion.cost("quilt"));
        // Stacking both compromises is dearer than either alone.
        assert!(Fit::ConnectorNear.cost("neoforge") > Fit::Connector.cost("neoforge"));
        assert!(Fit::ConnectorNear.cost("neoforge") > Fit::NearVersion.cost("neoforge"));
    }

    #[test]
    fn a_near_version_names_the_build_it_would_use() {
        let mods = vec![entry("a", &[("fabric", "1.21.3"), ("fabric", "1.21.4")])];
        let targets = rank(&mods, &empty_matrix(), 6);
        let at_3 = targets
            .iter()
            .find(|t| t.game_version == "1.21.3")
            .expect("a 1.21.3 target");
        // Its own build is used where it exists.
        assert_eq!(at_3.fits[0].fit, Fit::Native);

        let at_4 = targets
            .iter()
            .find(|t| t.game_version == "1.21.4")
            .expect("a 1.21.4 target");
        assert_eq!(at_4.fits[0].fit, Fit::Native);
    }

    #[test]
    fn a_neighbouring_patch_is_reached_for_but_a_different_family_is_not() {
        let mods = vec![
            entry("anchor", &[("fabric", "1.21.1")]),
            entry("late", &[("fabric", "1.21.4"), ("fabric", "1.20.1")]),
        ];
        let targets = rank(&mods, &empty_matrix(), 6);
        let at_1211 = targets
            .iter()
            .find(|t| t.loader == "fabric" && t.game_version == "1.21.1")
            .expect("a 1.21.1 target");
        let late = &at_1211.fits[1];
        assert_eq!(late.fit, Fit::NearVersion);
        // 1.21.4 is in the family; 1.20.1 is not, so it is never the answer.
        assert_eq!(late.via_version.as_deref(), Some("1.21.4"));
    }

    #[test]
    fn a_mod_that_fits_nowhere_here_says_where_it_does_fit() {
        let mods = vec![
            entry("modern", &[("fabric", "1.21.1")]),
            entry("ancient", &[("forge", "1.12.2")]),
        ];
        let best = &rank(&mods, &empty_matrix(), 6)[0];
        assert_eq!(best.missing, 1);
        let stranded = best
            .fits
            .iter()
            .find(|f| f.name == "ancient")
            .expect("the stranded mod");
        assert_eq!(stranded.fit, Fit::Missing);
        assert_eq!(stranded.elsewhere, vec!["Forge 1.12.2"]);
    }

    #[test]
    fn a_stranded_mod_names_every_loader_it_supports() {
        // Shaped like JEI: many Forge versions, fewer elsewhere. Reporting the
        // best three overall used to list three Fabric builds and hide Forge
        // entirely, which is how a mod that runs almost everywhere came to read
        // as Fabric-only.
        let mods = vec![
            entry("anchor", &[("fabric", "1.20.4")]),
            entry(
                "jei",
                &[
                    ("forge", "1.12.2"),
                    ("forge", "1.20.1"),
                    ("forge", "1.21.1"),
                    ("neoforge", "1.21.1"),
                    ("fabric", "26.1.2"),
                    ("fabric", "26.2"),
                ],
            ),
        ];
        let targets = rank(&mods, &empty_matrix(), 20);
        let at = targets
            .iter()
            .find(|t| t.loader == "fabric" && t.game_version == "1.20.4")
            .expect("a 1.20.4 Fabric target");
        let stranded = at
            .fits
            .iter()
            .find(|f| f.name == "jei")
            .expect("the stranded mod");

        assert_eq!(stranded.fit, Fit::Missing);
        // Every loader present, newest version of each, preference order.
        assert_eq!(
            stranded.elsewhere,
            vec!["Fabric 26.2", "NeoForge 1.21.1", "Forge 1.21.1"]
        );
    }

    #[test]
    fn a_stranded_mod_points_at_the_targets_own_version_where_it_has_one() {
        // Forge has both 1.21.1 and a newer 1.22. The useful answer is the one
        // that changes only the loader, not the loader *and* the version.
        let mods = vec![
            entry("anchor", &[("fabric", "1.21.1")]),
            entry("forge-only", &[("forge", "1.21.1"), ("forge", "1.22")]),
        ];
        let targets = rank(&mods, &empty_matrix(), 20);
        let at = targets
            .iter()
            .find(|t| t.loader == "fabric" && t.game_version == "1.21.1")
            .expect("a 1.21.1 Fabric target");
        let stranded = at
            .fits
            .iter()
            .find(|f| f.name == "forge-only")
            .expect("the stranded mod");

        assert_eq!(stranded.elsewhere, vec!["Forge 1.21.1"]);
    }

    #[test]
    fn quilt_runs_fabric_mods_without_a_compatibility_layer() {
        let mods = vec![
            entry("quilt-only", &[("quilt", "1.21.1")]),
            entry("fabric-only", &[("fabric", "1.21.1")]),
        ];
        let targets = rank(&mods, &empty_matrix(), 6);
        let quilt = targets
            .iter()
            .find(|t| t.loader == "quilt")
            .expect("a Quilt target");
        assert_eq!(quilt.missing, 0);
        assert_eq!(quilt.connector, 0);
        assert_eq!(quilt.cost, 0);
    }

    #[test]
    fn the_newest_version_wins_an_otherwise_exact_tie() {
        let mods = vec![entry(
            "a",
            &[("fabric", "1.21.1"), ("fabric", "1.20.1"), ("fabric", "1.21")],
        )];
        let best = &rank(&mods, &empty_matrix(), 6)[0];
        assert_eq!(best.game_version, "1.21.1");
    }

    #[test]
    fn ranking_is_stable_for_the_same_input() {
        let mods = vec![
            entry("a", &[("fabric", "1.21.1"), ("neoforge", "1.21.1")]),
            entry("b", &[("fabric", "1.21.1")]),
        ];
        let first = rank(&mods, &matrix(), 6);
        let second = rank(&mods, &matrix(), 6);
        let shape = |t: &[Target]| -> Vec<(String, String)> {
            t.iter()
                .map(|t| (t.loader.clone(), t.game_version.clone()))
                .collect()
        };
        assert_eq!(shape(&first), shape(&second));
    }

    #[test]
    fn a_blocked_file_supports_nothing() {
        let mut version = crate::providers::Version {
            provider: Provider::Modrinth,
            id: "v".into(),
            project_id: "p".into(),
            name: "v".into(),
            version_number: "1".into(),
            game_versions: vec!["1.21.1".into()],
            loaders: vec!["fabric".into()],
            release_type: "release".into(),
            published: None,
            downloads: 0,
            files: vec![crate::providers::VersionFile {
                url: "https://example.invalid/a.jar".into(),
                filename: "a.jar".into(),
                sha1: None,
                sha512: None,
                size: None,
                primary: true,
            }],
            dependencies: vec![],
            blocked_reason: None,
            blocked_sha1: None,
        };
        assert!(!Support::from_versions(&[version.clone()]).is_empty());

        version.blocked_reason = Some("author opted out".into());
        assert!(Support::from_versions(&[version]).is_empty());
    }

    #[test]
    fn an_empty_plan_has_no_targets() {
        assert!(rank(&[], &matrix(), 6).is_empty());
    }
}

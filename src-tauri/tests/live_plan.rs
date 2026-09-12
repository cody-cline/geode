//! The plan resolver and builder against the real Modrinth API.
//!
//! The resolver's unit tests pin the ranking rules against hand-built support
//! tables. What they cannot check is the step before that: whether a real
//! project's published files turn into the pairs we think they do. Modrinth can
//! change a field shape, a mod can drop a loader, and a frozen fixture would
//! never notice.
//!
//! These hit the network, so they are `#[ignore]` by default:
//!
//! ```sh
//! cargo test --test live_plan -- --ignored --nocapture
//! ```
//!
//! Assertions lean on invariants that must hold whatever third parties publish,
//! rather than on one mod's current build list — otherwise the suite breaks
//! every time an author ships something new.

use geode_lib::connector::SupportCache;
use geode_lib::instance::content::ContentKind;
use geode_lib::net::{self, Progress};
use geode_lib::paths::Paths;
use geode_lib::plan::resolve::{self, Fit, Resolution};
use geode_lib::plan::{Plan, PlanMod};
use geode_lib::providers::Provider;

/// A throwaway data directory per test. Named, because these run in parallel in
/// one process and the cleanup at the end of one would otherwise delete the
/// tree another is still writing into.
fn scratch_paths(name: &str) -> Paths {
    let dir = std::env::temp_dir().join(format!(
        "geode-plan-test-{}-{name}",
        std::process::id()
    ));
    Paths { root: dir }
}

/// Modrinth resolves slugs wherever it resolves ids, so a plan can be written
/// out by name.
fn one_mod(slug: &str) -> PlanMod {
    PlanMod {
        provider: "modrinth".into(),
        project_id: slug.into(),
        name: slug.into(),
        slug: Some(slug.into()),
        icon_url: None,
    }
}

fn plan_of(slugs: &[&str]) -> Plan {
    let mut plan = Plan::new("Live test".into());
    plan.mods = slugs.iter().map(|slug| one_mod(slug)).collect();
    plan
}

async fn resolve_plan(slugs: &[&str]) -> Resolution {
    let client = net::client();
    let matrix = SupportCache::new()
        .get(&client)
        .await
        .expect("Connector's matrix should be readable");
    assert!(
        !matrix.is_empty(),
        "Connector publishes files, so the matrix should never come back empty"
    );

    resolve::resolve(
        &client,
        &plan_of(slugs),
        &matrix,
        &resolve::SupportCache::new(),
        None,
        &Progress::silent("test"),
    )
    .await
    .expect("resolving should succeed")
}

#[tokio::test]
#[ignore = "hits the network"]
async fn a_fabric_plan_lands_on_fabric() {
    // All three publish Fabric builds, so the preferred loader should also be
    // the one that covers everything.
    let resolution = resolve_plan(&["sodium", "lithium", "modmenu"]).await;

    assert!(
        resolution.unresolved.is_empty(),
        "every mod should be readable: {:?}",
        resolution.unresolved
    );
    let best = resolution.targets.first().expect("a best target");
    println!(
        "best: {} {} — {} native, {} connector, {} near, {} missing",
        best.loader, best.game_version, best.native, best.connector, best.near, best.missing
    );

    assert_eq!(best.loader, "fabric", "Fabric should win a Fabric-only plan");
    assert_eq!(best.missing, 0, "nothing should be left out");
    assert_eq!(best.connector, 0, "Fabric never needs a compatibility layer");
    assert_eq!(best.cost, 0, "a clean answer should cost nothing");
}

#[tokio::test]
#[ignore = "hits the network"]
async fn targets_come_back_best_first() {
    let resolution = resolve_plan(&["sodium", "jei", "modmenu", "jade"]).await;
    let targets = &resolution.targets;
    assert!(targets.len() > 1, "a plan this broad should offer choices");

    for pair in targets.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        // Hard conflicts dominate, then the cost of the compromises. Anything
        // else would put a worse answer above a better one.
        assert!(
            (a.missing, a.cost) <= (b.missing, b.cost),
            "{} {} (missing {}, cost {}) should not rank above {} {} (missing {}, cost {})",
            b.loader,
            b.game_version,
            b.missing,
            b.cost,
            a.loader,
            a.game_version,
            a.missing,
            a.cost,
        );
    }
}

#[tokio::test]
#[ignore = "hits the network"]
async fn connector_is_only_ever_offered_on_the_forge_side() {
    // Mod Menu is Fabric-only, so on a Forge-family target it is exactly the
    // case Connector exists for.
    let resolution = resolve_plan(&["sodium", "jei", "modmenu"]).await;

    let mut seen_connector = false;
    for target in &resolution.targets {
        assert_eq!(
            target.fits.len(),
            resolution.targets[0].fits.len(),
            "every target should judge the same set of mods"
        );

        for fit in &target.fits {
            if matches!(fit.fit, Fit::Connector | Fit::ConnectorNear) {
                seen_connector = true;
                assert!(
                    matches!(target.loader.as_str(), "forge" | "neoforge"),
                    "{} was offered Connector on {}, which cannot run it",
                    fit.name,
                    target.loader
                );
            }
            if fit.fit == Fit::Missing {
                println!(
                    "missing on {}: {} -> {:?}",
                    target.loader, fit.name, fit.elsewhere
                );
            }
        }
    }
    println!("connector route offered somewhere: {seen_connector}");
}

#[tokio::test]
#[ignore = "hits the network"]
async fn a_native_verdict_is_backed_by_a_real_file() {
    let resolution = resolve_plan(&["sodium", "jade"]).await;

    for target in &resolution.targets {
        for fit in &target.fits {
            // Native means "a file for exactly this pair", so it must never
            // carry a redirect to another version.
            if fit.fit == Fit::Native {
                assert!(
                    fit.via_version.is_none(),
                    "{} is native on {} {} yet points at {:?}",
                    fit.name,
                    target.loader,
                    target.game_version,
                    fit.via_version
                );
            }
            // Every compromise that reaches across versions must name the one
            // it reached for, or the build step has nothing to install.
            if matches!(fit.fit, Fit::NearVersion | Fit::ConnectorNear) {
                assert!(
                    fit.via_version.is_some(),
                    "{} reaches across versions on {} {} without saying which",
                    fit.name,
                    target.loader,
                    target.game_version
                );
            }
        }
    }
}

#[tokio::test]
#[ignore = "hits the network"]
async fn reads_a_published_pack_into_a_versionless_mod_list() {
    let client = net::client();
    let paths = scratch_paths("pack");
    paths.ensure_all().await.expect("scratch dirs");

    let mods = geode_lib::plan::pack::contents(
        &client,
        &paths,
        geode_lib::providers::Provider::Modrinth,
        "fabulously-optimized",
        None,
        None,
        &Progress::silent("test"),
    )
    .await
    .expect("reading the pack should succeed");

    assert!(!mods.is_empty(), "the pack should contain mods");
    println!("{} mods, first few: {:?}", mods.len(), &mods[..mods.len().min(3)]);

    for entry in &mods {
        assert_eq!(entry.provider, "modrinth");
        assert!(!entry.project_id.is_empty(), "every mod needs an identity");
        // A pack lists files; what a plan needs back is named projects. An
        // entry still wearing its filename means the lookup silently failed.
        assert!(!entry.name.is_empty(), "every mod should be named");
        assert!(
            !entry.name.ends_with(".jar"),
            "{} came back as a filename rather than a project name",
            entry.name
        );
    }

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

/// A mod that fits nowhere must not be described by a hint that hides most of
/// where it does fit.
///
/// JEI publishes for Forge, NeoForge and Fabric across dozens of versions.
/// Reporting the best few pairs overall, ordered by our own loader preference,
/// listed three Fabric builds and dropped all thirty-nine Forge ones — so a mod
/// that runs almost everywhere read as Fabric-only.
#[tokio::test]
#[ignore = "hits the network"]
async fn a_stranded_mod_is_not_described_by_a_truncated_hint() {
    let client = net::client();
    let matrix = SupportCache::new().get(&client).await.expect("matrix");

    // Ranked over the whole target space rather than the handful `resolve`
    // hands back. The interesting targets are the ones JEI does *not* cover,
    // and those are never near the top — pairing it with Fabric API, which
    // covers far more Fabric versions than JEI ever shipped Fabric builds for,
    // guarantees some exist.
    let mut mods = Vec::new();
    for slug in ["jei", "fabric-api"] {
        let versions =
            geode_lib::providers::versions(&client, Provider::Modrinth, slug, None, None, None)
                .await
                .expect("versions should be readable");
        mods.push((
            one_mod(slug),
            resolve::Support::from_versions(&versions),
        ));
    }
    let targets = resolve::rank(&mods, &matrix, 500);
    println!("{} candidate targets", targets.len());

    let mut checked_jei = false;
    for target in &targets {
        for fit in target.fits.iter().filter(|f| f.fit == Fit::Missing) {
            // One entry per loader, never the same loader twice: that is what
            // makes the list readable as complete rather than as a sample.
            let loaders: Vec<&str> = fit
                .elsewhere
                .iter()
                .filter_map(|entry| entry.split(' ').next())
                .collect();
            let mut unique = loaders.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                loaders.len(),
                unique.len(),
                "{} names a loader more than once on {} {}: {:?}",
                fit.name,
                target.loader,
                target.game_version,
                fit.elsewhere,
            );

            if fit.name == "jei" {
                checked_jei = true;
                println!(
                    "jei stranded on {} {} -> {:?}",
                    target.loader, target.game_version, fit.elsewhere
                );
                // The regression itself: Forge is where most of JEI lives, and
                // it must not be the thing that gets truncated away.
                assert!(
                    fit.elsewhere.iter().any(|e| e.starts_with("Forge")),
                    "jei runs on Forge, yet the hint never says so: {:?}",
                    fit.elsewhere
                );
            }
        }
    }

    // A guard that passes by never running is worse than no guard, so the case
    // having occurred is itself an assertion.
    assert!(
        checked_jei,
        "expected JEI to be stranded on at least one Fabric target it has no build for"
    );
}

/// The whole point of the feature, end to end: a versionless list becomes a
/// real instance with real jars in it.
///
/// This downloads the mods themselves but *not* Minecraft — the game's own
/// files are fetched at launch, not at install — so it costs a few megabytes.
#[tokio::test]
#[ignore = "hits the network and writes an instance"]
async fn builds_a_versionless_plan_into_a_real_instance() {
    let client = net::client();
    let paths = scratch_paths("build");
    paths.ensure_all().await.expect("scratch dirs");

    // Two mods that need nothing else, to keep the download small.
    let plan = plan_of(&["sodium", "lithium"]);
    let matrix = SupportCache::new().get(&client).await.expect("matrix");
    let resolution = resolve::resolve(
        &client,
        &plan,
        &matrix,
        &resolve::SupportCache::new(),
        None,
        &Progress::silent("test"),
    )
    .await
    .expect("resolving should succeed");

    let target = resolution.targets.first().expect("a target");
    println!("building on {} {}", target.loader, target.game_version);

    let report = geode_lib::plan::build::build(
        &client,
        &paths,
        &plan,
        target,
        Some("Live Build Test".into()),
        None,
        &Progress::silent("test"),
    )
    .await
    .expect("building should succeed");

    println!(
        "installed {:?}, skipped {:?}",
        report.installed, report.skipped
    );
    assert!(report.skipped.is_empty(), "nothing should be skipped");
    assert!(!report.installed.is_empty(), "files should have landed");

    // The jars are really on disk, in the folder the game reads.
    let mods_dir = paths.game_dir(&report.instance_id).join("mods");
    let mut jars = Vec::new();
    let mut entries = tokio::fs::read_dir(&mods_dir)
        .await
        .expect("the mods folder should exist");
    while let Some(entry) = entries.next_entry().await.expect("readable") {
        jars.push(entry.file_name().to_string_lossy().to_string());
    }
    println!("jars: {jars:?}");
    assert_eq!(jars.len(), 2, "both mods should be installed: {jars:?}");
    assert!(jars.iter().all(|j| j.ends_with(".jar")));

    // And the instance agrees with the target it was built for.
    let instance = geode_lib::instance::load(&paths, &report.instance_id)
        .await
        .expect("the instance should be saved");
    assert_eq!(instance.mc_version, target.game_version);
    assert_eq!(instance.loader.as_str(), target.loader);
    assert!(
        !instance.loader_version.is_empty(),
        "a loader build should have been chosen"
    );
    assert_eq!(
        instance
            .content
            .iter()
            .filter(|r| r.kind == ContentKind::Mod)
            .count(),
        2,
        "both mods should be tracked with provenance"
    );

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

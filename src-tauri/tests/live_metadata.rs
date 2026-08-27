//! Integration tests against the real upstream APIs.
//!
//! These hit the network, so they are `#[ignore]` by default and excluded from
//! a normal `cargo test`. Run them with:
//!
//! ```sh
//! cargo test --test live_metadata -- --ignored --nocapture
//! ```
//!
//! They exist because the parts most likely to break are the ones we do not
//! control: Mojang, Fabric and Modrinth can all change a field shape without
//! warning, and a unit test with a frozen fixture would never notice.

use geode_lib::meta::{self, piston, Loader};
use geode_lib::net;
use geode_lib::paths::Paths;
use geode_lib::providers::{self, ProjectKind, Provider, SearchQuery, SortBy};

fn scratch_paths() -> Paths {
    let dir = std::env::temp_dir().join(format!("geode-test-{}", std::process::id()));
    Paths { root: dir }
}

#[tokio::test]
#[ignore = "hits the network"]
async fn resolves_a_vanilla_version() {
    let client = net::client();
    let paths = scratch_paths();
    paths.ensure_all().await.unwrap();

    let manifest = piston::fetch_manifest(&client).await.unwrap();
    assert!(!manifest.versions.is_empty());

    let latest = manifest.latest.release.clone();
    let version = meta::vanilla_version_json(&client, &paths, &manifest, &latest)
        .await
        .unwrap();

    assert_eq!(version.id, latest);
    assert!(!version.main_class.is_empty(), "no main class");
    assert!(version.asset_index.is_some(), "no asset index");
    assert!(
        version.downloads.contains_key("client"),
        "no client jar download"
    );
    assert!(!version.libraries.is_empty(), "no libraries");

    // Every allowed library must resolve to something downloadable, or the
    // install would silently produce a broken classpath.
    let features = std::collections::HashMap::new();
    let allowed = version
        .libraries
        .iter()
        .filter(|l| l.is_allowed(&features))
        .count();
    assert!(allowed > 10, "suspiciously few libraries: {allowed}");

    println!("resolved {latest}: {allowed} libraries, main {}", version.main_class);
    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

#[tokio::test]
#[ignore = "hits the network"]
async fn merges_a_fabric_profile_onto_vanilla() {
    let client = net::client();
    let paths = scratch_paths();
    paths.ensure_all().await.unwrap();

    let manifest = piston::fetch_manifest(&client).await.unwrap();

    // Fabric lags the very newest snapshot, so pick a release it supports.
    let mut chosen = None;
    for candidate in manifest.versions.iter().filter(|v| v.kind == "release") {
        if let Ok(loaders) = meta::loader_versions(&client, Loader::Fabric, &candidate.id).await {
            if let Some(loader) = loaders.first() {
                chosen = Some((candidate.id.clone(), loader.clone()));
                break;
            }
        }
    }
    let (game_version, loader_version) = chosen.expect("no Fabric-supported release found");

    let vanilla = meta::vanilla_version_json(&client, &paths, &manifest, &game_version)
        .await
        .unwrap();
    let vanilla_libs = vanilla.libraries.len();
    let vanilla_main = vanilla.main_class.clone();

    let profile = meta::fabric::profile(
        &client,
        meta::fabric::Flavor::Fabric,
        &game_version,
        &loader_version,
    )
    .await
    .unwrap();

    assert!(
        profile.inherits_from.is_some(),
        "fabric profile should inherit from vanilla"
    );

    let merged = piston::merge_inherited(profile, vanilla);

    assert_ne!(merged.main_class, vanilla_main, "main class should be Fabric's");
    assert!(
        merged.libraries.len() > vanilla_libs,
        "merge lost libraries: {} <= {vanilla_libs}",
        merged.libraries.len()
    );
    assert!(merged.asset_index.is_some(), "merge lost the asset index");
    assert!(merged.inherits_from.is_none(), "inheritance not collapsed");

    // The invariant that matters is on the *resolved* classpath, after rules
    // have filtered out other platforms. Two jars of the same artifact there
    // means whichever the JVM sees first wins, which is how mismatched ASM or
    // Guava versions produce a NoSuchMethodError at startup.
    //
    // Note this is per group:artifact:classifier, not group:artifact — a
    // manifest legitimately lists `lwjgl:3.3.3` alongside
    // `lwjgl:3.3.3:natives-linux`, and modern versions need both.
    let features = std::collections::HashMap::new();
    let mut keys: Vec<String> = merged
        .libraries
        .iter()
        .filter(|l| l.is_allowed(&features) && !l.is_native_only())
        .map(|l| {
            let p: Vec<&str> = l.name.split(':').collect();
            match p.len() {
                0 | 1 => l.name.clone(),
                2 | 3 => format!("{}:{}", p[0], p[1]),
                _ => format!("{}:{}:{}", p[0], p[1], p[3]),
            }
        })
        .collect();
    let before = keys.len();
    keys.sort();
    let mut unique = keys.clone();
    unique.dedup();
    assert_eq!(
        before,
        unique.len(),
        "duplicate artifacts on the resolved classpath: {:?}",
        keys.windows(2)
            .filter(|w| w[0] == w[1])
            .map(|w| w[0].clone())
            .collect::<Vec<_>>()
    );

    // And the classpath must actually contain the game plus the loader.
    let client_jar = paths.version_jar(&game_version);
    let paths_on_cp = geode_lib::install::classpath(&paths, &merged, &client_jar, &features);
    assert!(
        paths_on_cp
            .iter()
            .any(|p| p.to_string_lossy().contains("fabric-loader")),
        "fabric-loader missing from the classpath"
    );
    assert!(
        paths_on_cp.last().is_some_and(|p| p
            .to_string_lossy()
            .ends_with(&format!("{game_version}.jar"))),
        "the client jar should be last on the classpath"
    );

    println!(
        "merged fabric {loader_version} onto {game_version}: {} libraries, main {}",
        merged.libraries.len(),
        merged.main_class
    );
    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

#[tokio::test]
#[ignore = "hits the network"]
async fn lists_loader_versions_for_every_family() {
    let client = net::client();
    let manifest = piston::fetch_manifest(&client).await.unwrap();

    // NeoForge only exists for 1.20.2+, so anchor on a version all four support.
    let game_version = manifest
        .versions
        .iter()
        .find(|v| v.kind == "release" && v.id.starts_with("1.21"))
        .map(|v| v.id.clone())
        .unwrap_or_else(|| manifest.latest.release.clone());

    for loader in [Loader::Fabric, Loader::Quilt, Loader::NeoForge, Loader::Forge] {
        match meta::loader_versions(&client, loader, &game_version).await {
            Ok(versions) => println!(
                "{}: {} builds for {game_version}{}",
                loader.display_name(),
                versions.len(),
                versions
                    .first()
                    .map(|v| format!(" (latest {v})"))
                    .unwrap_or_default()
            ),
            Err(e) => println!("{}: unavailable ({e})", loader.display_name()),
        }
    }
}

#[tokio::test]
#[ignore = "hits the network"]
async fn modrinth_search_returns_installable_versions() {
    let client = net::client();

    let query = SearchQuery {
        query: "sodium".into(),
        kind: ProjectKind::Mod,
        game_version: None,
        loader: Some("fabric".into()),
        categories: vec![],
        sort: SortBy::Downloads,
        offset: 0,
        limit: 5,
        providers: Some(vec![Provider::Modrinth]),
    };

    let results = providers::search(&client, &query, None).await.unwrap();
    assert!(!results.hits.is_empty(), "no search hits");

    let sodium = results
        .hits
        .iter()
        .find(|p| p.slug == "sodium")
        .expect("sodium not in results");
    assert!(sodium.downloads > 1_000_000);
    assert!(sodium.distribution_allowed);

    let versions = providers::versions(
        &client,
        Provider::Modrinth,
        &sodium.id,
        None,
        Some("fabric"),
        None,
    )
    .await
    .unwrap();

    assert!(!versions.is_empty(), "sodium has no fabric versions");
    let newest = &versions[0];
    let file = newest.primary_file().expect("no primary file");
    assert!(file.url.starts_with("https://"), "bad file url: {}", file.url);
    assert!(file.sha1.is_some(), "no sha1 to verify against");
    assert!(file.filename.ends_with(".jar"));

    println!(
        "sodium {} -> {} ({} bytes)",
        newest.version_number,
        file.filename,
        file.size.unwrap_or(0)
    );
}

#[tokio::test]
#[ignore = "hits the network"]
async fn curseforge_is_reported_off_without_a_key() {
    let client = net::client();

    let query = SearchQuery {
        query: "jei".into(),
        kind: ProjectKind::Mod,
        game_version: None,
        loader: None,
        categories: vec![],
        sort: SortBy::Relevance,
        offset: 0,
        limit: 5,
        providers: None,
    };

    let results = providers::search(&client, &query, None).await.unwrap();

    // Modrinth still answers, and the missing half is surfaced rather than
    // silently dropped.
    assert!(!results.hits.is_empty());
    assert!(
        results.warnings.iter().any(|w| w.contains("CurseForge")),
        "expected a CurseForge warning, got {:?}",
        results.warnings
    );
}

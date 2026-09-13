//! Which side a mod runs on, from the real Modrinth API.
//!
//! This decides what may be installed onto a server, and it fails quietly if it
//! fails at all: a renamed field or a wrong serde attribute leaves every answer
//! `None`, which is read as "probably fine", and Sodium sails onto a server
//! where it is at best dead weight.
//!
//! ```sh
//! cargo test --test live_sides -- --ignored --nocapture
//! ```

use geode_lib::net;
use geode_lib::providers::{self, ProjectKind, Provider, SearchQuery, SortBy};

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        query: text.into(),
        kind: ProjectKind::Mod,
        game_version: None,
        loader: None,
        categories: Vec::new(),
        sort: SortBy::Downloads,
        offset: 0,
        limit: 20,
        providers: Some(vec![Provider::Modrinth]),
    }
}

#[tokio::test]
#[ignore = "hits the network"]
async fn modrinth_says_which_side_a_mod_runs_on() {
    let client = net::client();

    // Rendering and UI on one side; a server-required mod on the other.
    for (slug, belongs_on_a_server) in [
        ("sodium", false),
        ("iris", false),
        ("create", true),
        ("lithium", true),
    ] {
        let project = providers::project(&client, Provider::Modrinth, slug, None)
            .await
            .unwrap_or_else(|e| panic!("{slug}: {e}"));

        println!(
            "{slug:<10} client={:<12?} server={:?}",
            project.client_side, project.server_side
        );
        assert!(
            project.server_side.is_some(),
            "{slug} came back with no side information at all — the field is \
             not being parsed"
        );
        assert_eq!(
            project.runs_on_a_server(),
            belongs_on_a_server,
            "{slug}: server_side={:?}",
            project.server_side
        );
    }
}

#[tokio::test]
#[ignore = "hits the network"]
async fn the_search_index_carries_it_too() {
    // Filtering a whole plan would otherwise cost one project lookup per mod.
    let client = net::client();
    let results = providers::search(&client, &query("sodium"), None)
        .await
        .expect("search should succeed");

    let sodium = results
        .hits
        .iter()
        .find(|h| h.slug == "sodium")
        .expect("Sodium should be the obvious hit for its own name");

    assert_eq!(
        sodium.server_side.as_deref(),
        Some("unsupported"),
        "the search index should carry side support, not just the project \
         endpoint"
    );
    assert!(!sodium.runs_on_a_server());
    println!("search: sodium server={:?}", sodium.server_side);
}

#[tokio::test]
#[ignore = "hits the network"]
async fn a_plugin_is_server_software_by_definition() {
    // SpigotMC publishes no such field, so this is ours to assert rather than
    // read: there is no client half of a Bukkit plugin to install.
    let client = net::client();
    let luckperms = providers::project(&client, Provider::Spiget, "28140", None)
        .await
        .expect("luckperms");

    assert!(luckperms.runs_on_a_server());
    assert_eq!(luckperms.server_side.as_deref(), Some("required"));
    assert_eq!(luckperms.client_side.as_deref(), Some("unsupported"));
}

/// What the *jar* says, which is the only answer worth acting on.
///
/// Modrinth's `client_side`/`server_side` are an author's tick-boxes on a web
/// form; the manifest inside the jar is what the loader actually reads. These
/// download real mods and check the parser against them, because the failure
/// mode is silent: a renamed field leaves every answer `None`, which reads as
/// "not declared" and quietly stops meaning anything.
#[tokio::test]
#[ignore = "hits the network, downloads real mods"]
async fn a_real_jar_declares_its_own_side() {
    use geode_lib::instance::jar::Side;

    let client = net::client();
    let dir = std::env::temp_dir().join(format!("geode-sides-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir).await.expect("scratch dir");

    // (slug, loader, what the jar should say about itself)
    let cases: [(&str, &str, Option<Side>); 3] = [
        // Sodium is a renderer. It declares environment "client".
        ("sodium", "fabric", Some(Side::Client)),
        // Fabric API ships for both and declares "*", even though it contains
        // client-only modules — the loader filters those itself, which is why
        // nested jars are deliberately not aggregated upward.
        ("fabric-api", "fabric", Some(Side::Both)),
        // NeoForge has no field for this at all, on the mod or the file. The
        // honest answer is that the jar did not say.
        ("jei", "neoforge", None),
    ];

    for (slug, loader, expected) in cases {
        let versions = geode_lib::providers::versions(
            &client,
            Provider::Modrinth,
            slug,
            None,
            Some(loader),
            None,
        )
        .await
        .unwrap_or_default();
        let Some(file) = versions.first().and_then(|v| v.files.first()) else {
            panic!("no {loader} build published for {slug}");
        };

        let jar = dir.join(&file.filename);
        geode_lib::net::fetch_one(
            &client,
            &geode_lib::net::Download::new(file.url.clone(), jar.clone()),
        )
        .await
        .unwrap_or_else(|e| panic!("downloading {slug}: {e}"));

        let meta = tokio::task::spawn_blocking(move || geode_lib::instance::jar::read(&jar))
            .await
            .expect("join")
            .unwrap_or_else(|| panic!("{slug} declared nothing readable at all"));

        assert_eq!(
            meta.side, expected,
            "{slug} ({loader}) declared {:?}, expected {expected:?}",
            meta.side
        );
        println!("{slug} ({loader}) -> {:?}", meta.side);
    }

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

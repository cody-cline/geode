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

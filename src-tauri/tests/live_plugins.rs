//! Plugin search, federated across Modrinth and SpigotMC.
//!
//! The interesting behaviour is not that each source answers — the per-source
//! tests cover that — but that the two are folded into one list, and that
//! SpigotMC stays out of the way when the search is for mods.
//!
//! ```sh
//! cargo test --test live_plugins -- --ignored --nocapture
//! ```
//!
//! CurseForge is absent from these runs unless a key is configured, which is
//! why nothing here asserts three sources.

use geode_lib::net;
use geode_lib::providers::{self, ProjectKind, Provider, SearchQuery, SortBy};

fn query(text: &str, kind: ProjectKind) -> SearchQuery {
    SearchQuery {
        query: text.into(),
        kind,
        game_version: None,
        loader: None,
        categories: Vec::new(),
        sort: SortBy::Downloads,
        offset: 0,
        limit: 20,
        providers: None,
    }
}

/// The same rule the merge uses to decide two listings are one project.
fn normalise(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[tokio::test]
#[ignore = "hits the network"]
async fn a_plugin_search_reaches_spigotmc() {
    let client = net::client();
    let results = providers::search(&client, &query("", ProjectKind::Plugin), None)
        .await
        .expect("search should succeed");

    assert!(!results.hits.is_empty(), "no plugins came back at all");

    let from_spigot = results
        .hits
        .iter()
        .filter(|h| {
            h.provider == Provider::Spiget
                || h.also_on.iter().any(|r| r.provider == Provider::Spiget)
        })
        .count();
    assert!(
        from_spigot > 0,
        "a plugin search should reach SpigotMC; got {} hits from {:?}",
        results.hits.len(),
        results
            .hits
            .iter()
            // Keyed by name rather than by the enum: sorting providers is a
            // diagnostic convenience and not worth an Ord on the real type.
            .map(|h| h.provider.as_str())
            .collect::<std::collections::BTreeSet<_>>()
    );
    println!(
        "{} plugins, {from_spigot} touching SpigotMC. warnings: {:?}",
        results.hits.len(),
        results.warnings
    );
}

#[tokio::test]
#[ignore = "hits the network"]
async fn the_same_plugin_is_one_row_not_two() {
    // LuckPerms is published on both, which is the case the merge exists for.
    // Spigot resources carry no slug precisely so the name can match here.
    let client = net::client();
    let results = providers::search(&client, &query("luckperms", ProjectKind::Plugin), None)
        .await
        .expect("search should succeed");

    let same: Vec<_> = results
        .hits
        .iter()
        .filter(|h| normalise(&h.name) == "luckperms")
        .collect();

    assert!(!same.is_empty(), "LuckPerms should be findable");
    assert_eq!(
        same.len(),
        1,
        "LuckPerms came back {} times instead of merging into one row: {:?}",
        same.len(),
        same.iter().map(|h| (h.provider, &h.id)).collect::<Vec<_>>()
    );

    let hit = same[0];
    println!(
        "luckperms: {:?} id {} also_on {:?}",
        hit.provider,
        hit.id,
        hit.also_on.iter().map(|r| r.provider).collect::<Vec<_>>()
    );
}

#[tokio::test]
#[ignore = "hits the network"]
async fn a_mod_search_never_reaches_spigotmc() {
    // SpigotMC carries nothing a Minecraft client can load, so it has no place
    // in a mod search however well the names match.
    let client = net::client();
    let results = providers::search(&client, &query("luckperms", ProjectKind::Mod), None)
        .await
        .expect("search should succeed");

    for hit in &results.hits {
        assert_ne!(
            hit.provider,
            Provider::Spiget,
            "{} came from SpigotMC in a mod search",
            hit.name
        );
        assert!(
            !hit.also_on.iter().any(|r| r.provider == Provider::Spiget),
            "{} was merged with a SpigotMC listing in a mod search",
            hit.name
        );
    }
    println!("{} mod hits, none from SpigotMC", results.hits.len());
}

#[tokio::test]
#[ignore = "hits the network"]
async fn asking_for_one_source_gets_only_that_source() {
    let client = net::client();
    let mut only_spigot = query("essentials", ProjectKind::Plugin);
    only_spigot.providers = Some(vec![Provider::Spiget]);

    let results = providers::search(&client, &only_spigot, None)
        .await
        .expect("search should succeed");

    assert!(!results.hits.is_empty());
    for hit in &results.hits {
        assert_eq!(hit.provider, Provider::Spiget, "{} is not from SpigotMC", hit.name);
    }
    println!("{} SpigotMC-only hits", results.hits.len());
}

/// A real plugin's declared commands, read from the real jar.
///
/// The hand-written cases in `jar.rs` prove the parser against the shape I
/// believe `plugin.yml` has. This proves the shape — EssentialsX declares about
/// 130 commands with packed alias lists and quoted descriptions, and is the
/// plugin most likely to be on a server whose console this feeds.
#[tokio::test]
#[ignore = "hits the network"]
async fn a_real_plugin_declares_its_commands() {
    let client = geode_lib::net::client();
    let versions = geode_lib::providers::versions(
        &client,
        Provider::Modrinth,
        "essentialsx",
        None,
        Some("paper"),
        None,
    )
    .await
    .expect("versions");
    let file = versions
        .first()
        .and_then(|v| v.files.first())
        .expect("a published file");

    let dir = std::env::temp_dir().join(format!("geode-plugin-cmds-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.expect("scratch dir");
    let jar = dir.join(&file.filename);
    geode_lib::net::fetch_one(
        &client,
        &geode_lib::net::Download::new(file.url.clone(), jar.clone()),
    )
    .await
    .expect("download");

    let meta = tokio::task::spawn_blocking(move || geode_lib::instance::jar::read(&jar))
        .await
        .expect("join")
        .expect("EssentialsX declares itself in plugin.yml");

    // Commands, and the aliases a server accepts exactly as it accepts them.
    for expected in ["afk", "eafk", "balance", "bal", "heal", "tpa"] {
        assert!(
            meta.commands.iter().any(|c| c == expected),
            "{expected} missing from {} read commands",
            meta.commands.len()
        );
    }
    // Never a key from elsewhere in the file.
    for absent in ["main", "softdepend", "api-version", "website", "authors"] {
        assert!(
            !meta.commands.iter().any(|c| c == absent),
            "{absent} is not a command"
        );
    }
    println!("{} commands read from EssentialsX", meta.commands.len());

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

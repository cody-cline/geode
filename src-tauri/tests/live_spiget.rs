//! SpigotMC, through the Spiget index, against the real API.
//!
//! Spiget is a third-party index of pages Spigot itself publishes no API for,
//! so its shape is entirely outside our control and entirely undocumented by
//! the people who own the data.
//!
//! ```sh
//! cargo test --test live_spiget -- --ignored --nocapture
//! ```

use geode_lib::net;
use geode_lib::providers::{self, Provider, SearchQuery, SortBy};

/// LuckPerms: hosted on SpigotMC itself, so it is downloadable.
const LUCKPERMS: &str = "28140";
/// ViaVersion: listed on SpigotMC, hosted elsewhere.
const VIAVERSION: &str = "19254";

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        query: text.into(),
        kind: providers::ProjectKind::Mod,
        game_version: None,
        loader: None,
        categories: Vec::new(),
        sort: SortBy::Downloads,
        offset: 0,
        limit: 5,
        providers: None,
    }
}

#[tokio::test]
#[ignore = "hits the network"]
async fn searching_by_name_finds_the_plugin() {
    let client = net::client();
    let hits = providers::spiget::search(&client, &query("LuckPerms"))
        .await
        .expect("search should succeed");

    assert!(!hits.is_empty(), "LuckPerms should be findable by name");
    for hit in &hits {
        assert_eq!(hit.provider, Provider::Spiget);
        assert!(!hit.name.is_empty());
        assert!(hit.page_url.starts_with("https://www.spigotmc.org/resources/"));
        // Everything here is published for the Bukkit family; a server decides
        // for itself whether it accepts Spigot plugins.
        assert_eq!(hit.loaders, vec!["spigot"]);
    }
    println!(
        "found: {}",
        hits.iter()
            .map(|h| h.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

#[tokio::test]
#[ignore = "hits the network"]
async fn an_empty_query_lists_popular_resources() {
    let client = net::client();
    let hits = providers::spiget::search(&client, &query(""))
        .await
        .expect("listing should succeed");
    assert!(!hits.is_empty());
    println!("{} popular resources, first: {}", hits.len(), hits[0].name);
}

#[tokio::test]
#[ignore = "hits the network"]
async fn a_hosted_resource_resolves_to_a_downloadable_file() {
    let client = net::client();
    let project = providers::spiget::project(&client, LUCKPERMS)
        .await
        .expect("project should resolve");
    assert_eq!(project.name, "LuckPerms");
    assert!(
        project.distribution_allowed,
        "LuckPerms is hosted on SpigotMC"
    );

    let versions = providers::spiget::versions(&client, LUCKPERMS)
        .await
        .expect("versions should resolve");
    // Spiget serves the current file only; offering a history would be
    // offering links that fail.
    assert_eq!(versions.len(), 1);

    let version = &versions[0];
    assert!(version.blocked_reason.is_none());
    let file = version.primary_file().expect("a downloadable file");
    assert!(file.filename.ends_with(".jar"), "{}", file.filename);
    println!("{} {} -> {}", project.name, version.version_number, file.url);

    // A single byte, to prove the redirect actually serves something.
    let resp = client
        .get(&file.url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .expect("download should respond");
    assert!(resp.status().is_success(), "got {}", resp.status());
}

#[tokio::test]
#[ignore = "hits the network"]
async fn an_externally_hosted_resource_refuses_with_a_reason() {
    let client = net::client();
    let project = providers::spiget::project(&client, VIAVERSION)
        .await
        .expect("project should resolve");
    assert!(
        !project.distribution_allowed,
        "ViaVersion is hosted off-site, so it is not ours to fetch"
    );

    let versions = providers::spiget::versions(&client, VIAVERSION)
        .await
        .expect("versions should resolve");
    let version = &versions[0];

    // The refusal has to explain itself, or the row just looks broken.
    let reason = version
        .blocked_reason
        .as_deref()
        .expect("an external resource should say why it cannot be downloaded");
    assert!(!reason.is_empty());
    assert!(
        version.files.is_empty(),
        "a blocked version should offer no file to try"
    );
    println!("viaversion: {reason}");
}

#[tokio::test]
#[ignore = "hits the network"]
async fn a_spigot_resource_is_not_a_mod_a_plan_can_target() {
    // The planner deals in client loaders. Being told plainly beats appearing
    // as a mod that publishes nothing.
    let client = net::client();
    let error = providers::support_pairs(&client, Provider::Spiget, LUCKPERMS, None)
        .await
        .expect_err("a plugin cannot be planned as a mod");
    assert!(error.to_string().contains("server plugins"), "{error}");
}

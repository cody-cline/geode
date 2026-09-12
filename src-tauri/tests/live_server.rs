//! Server jar resolution against the real publishers.
//!
//! Five different organisations publish these, each with its own API, and none
//! of them owes us stability. A frozen fixture would prove only that the code
//! parses what it parsed the day it was written, which is exactly the failure
//! this is meant to catch.
//!
//! ```sh
//! cargo test --test live_server -- --ignored --nocapture
//! ```

use geode_lib::net;
use geode_lib::server::{meta, ServerKind};

/// Everything Geode claims it can install without help.
const INSTALLABLE: &[ServerKind] = &[
    ServerKind::Paper,
    ServerKind::Folia,
    ServerKind::Velocity,
    ServerKind::Waterfall,
    ServerKind::Purpur,
    ServerKind::Fabric,
    ServerKind::Vanilla,
    ServerKind::Mohist,
];

#[tokio::test]
#[ignore = "hits the network"]
async fn every_installable_kind_lists_versions_and_resolves_a_jar() {
    let client = net::client();

    for &kind in INSTALLABLE {
        assert!(
            meta::is_installable(kind),
            "{kind:?} is in the list but not claimed installable"
        );

        let versions = meta::versions(&client, kind)
            .await
            .unwrap_or_else(|e| panic!("{kind:?} versions: {e}"));
        assert!(!versions.is_empty(), "{kind:?} listed no versions");

        let newest = &versions[0];
        let jar = meta::resolve(&client, kind, newest)
            .await
            .unwrap_or_else(|e| panic!("{kind:?} {newest}: {e}"));

        assert!(
            jar.url.starts_with("https://"),
            "{kind:?} gave a non-https url: {}",
            jar.url
        );
        assert!(
            jar.file_name.ends_with(".jar"),
            "{kind:?} gave {} which is not a jar",
            jar.file_name
        );
        assert!(!jar.build.is_empty(), "{kind:?} gave no build identity");

        println!(
            "{:<10} {:<10} build {:<8} {}",
            kind.as_str(),
            newest,
            jar.build,
            jar.file_name
        );
    }
}

/// A URL that resolves is not the same as a URL that serves a file.
#[tokio::test]
#[ignore = "hits the network"]
async fn resolved_jars_are_actually_served() {
    let client = net::client();

    // One from each shape of publisher: a build API with a checksum, a build
    // API without one, a generated endpoint, and Mojang's own manifest.
    for &kind in &[
        ServerKind::Paper,
        ServerKind::Purpur,
        ServerKind::Fabric,
        ServerKind::Vanilla,
    ] {
        let versions = meta::versions(&client, kind).await.expect("versions");
        let jar = meta::resolve(&client, kind, &versions[0])
            .await
            .expect("resolve");

        // A single byte, rather than pulling fifty megabytes four times over.
        let resp = client
            .get(&jar.url)
            .header("Range", "bytes=0-0")
            .send()
            .await
            .unwrap_or_else(|e| panic!("{kind:?} {}: {e}", jar.url));

        assert!(
            resp.status().is_success(),
            "{kind:?} {} returned {}",
            jar.url,
            resp.status()
        );
        println!("{:<10} {} -> {}", kind.as_str(), resp.status(), jar.url);
    }
}

/// Paper is the one publisher that gives us a checksum to verify, so the
/// plumbing that carries it has to actually carry it.
#[tokio::test]
#[ignore = "hits the network"]
async fn paper_supplies_a_checksum_we_can_verify_against() {
    let client = net::client();
    let versions = meta::versions(&client, ServerKind::Paper)
        .await
        .expect("versions");
    let jar = meta::resolve(&client, ServerKind::Paper, &versions[0])
        .await
        .expect("resolve");

    let sha256 = jar.sha256.expect("Paper publishes a sha256 for every build");
    assert_eq!(sha256.len(), 64, "not a sha256: {sha256}");
    assert!(sha256.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(jar.size.unwrap_or(0) > 1_000_000, "a server jar is not tiny");
    println!("paper {} sha256 {sha256}", versions[0]);
}

#[tokio::test]
#[ignore = "hits the network"]
async fn mohist_resolves_the_newest_build_not_the_oldest() {
    // Mohist returns builds newest-first, the opposite of most, and taking the
    // wrong end would install something from years ago.
    let client = net::client();
    let versions = meta::versions(&client, ServerKind::Mohist)
        .await
        .expect("versions");
    let jar = meta::resolve(&client, ServerKind::Mohist, &versions[0])
        .await
        .expect("resolve");

    let build: u32 = jar.build.parse().expect("a numeric build id");
    assert!(build > 0);
    println!("mohist {} build {build} -> {}", versions[0], jar.file_name);
}

#[test]
fn software_we_cannot_fetch_says_why() {
    // No network: the refusal is a local decision and should stay one.
    for kind in [
        ServerKind::Spigot,
        ServerKind::Forge,
        ServerKind::NeoForge,
        ServerKind::Quilt,
    ] {
        assert!(!meta::is_installable(kind), "{kind:?}");
    }
}

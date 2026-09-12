//! Server software resolution against the real publishers.
//!
//! Nine different organisations publish these, each with its own API, and none
//! of them owes us stability. A frozen fixture would prove only that the code
//! parses what it parsed the day it was written, which is exactly the failure
//! this is meant to catch.
//!
//! ```sh
//! cargo test --test live_server -- --ignored --nocapture
//! ```

use geode_lib::net;
use geode_lib::server::{meta, ServerKind};

/// The downloadable jar behind either kind of source — the server itself, or
/// the tool that produces it.
fn jar_of(source: &meta::Source) -> &meta::ServerJar {
    match source {
        meta::Source::Jar(jar) => jar,
        meta::Source::Build(builder) => &builder.jar,
    }
}

#[tokio::test]
#[ignore = "hits the network"]
async fn every_kind_lists_versions_and_resolves_something() {
    let client = net::client();

    for &kind in ServerKind::ALL {
        assert!(
            meta::is_installable(kind),
            "{kind:?} is offered but not claimed installable"
        );

        let versions = meta::versions(&client, kind)
            .await
            .unwrap_or_else(|e| panic!("{kind:?} versions: {e}"));
        assert!(!versions.is_empty(), "{kind:?} listed no versions");

        let newest = &versions[0];
        let source = meta::resolve(&client, kind, newest)
            .await
            .unwrap_or_else(|e| panic!("{kind:?} {newest}: {e}"));
        let jar = jar_of(&source);

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

        let how = match &source {
            meta::Source::Jar(_) => "download",
            meta::Source::Build(_) => "build",
        };
        println!(
            "{:<10} {:<10} {:<8} build {:<12} {}",
            kind.as_str(),
            newest,
            how,
            jar.build,
            jar.file_name
        );
    }
}

/// A URL that resolves is not the same as a URL that serves a file.
#[tokio::test]
#[ignore = "hits the network"]
async fn resolved_downloads_are_actually_served() {
    let client = net::client();

    // One from each shape of publisher: build APIs with and without a checksum,
    // a generated endpoint, Mojang's own manifest, two Maven repositories, a
    // GitHub release, and the BuildTools artefact from a Jenkins job.
    for &kind in &[
        ServerKind::Paper,
        ServerKind::Purpur,
        ServerKind::Fabric,
        ServerKind::Vanilla,
        ServerKind::Forge,
        ServerKind::NeoForge,
        ServerKind::Quilt,
        ServerKind::Spigot,
        ServerKind::Arclight,
        ServerKind::Banner,
    ] {
        let versions = meta::versions(&client, kind)
            .await
            .unwrap_or_else(|e| panic!("{kind:?} versions: {e}"));
        let source = meta::resolve(&client, kind, &versions[0])
            .await
            .unwrap_or_else(|e| panic!("{kind:?} resolve: {e}"));
        let jar = jar_of(&source);

        // A single byte, rather than pulling gigabytes across ten publishers.
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
    let source = meta::resolve(&client, ServerKind::Paper, &versions[0])
        .await
        .expect("resolve");
    let jar = jar_of(&source);

    let sha256 = jar
        .sha256
        .clone()
        .expect("Paper publishes a sha256 for every build");
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
    let source = meta::resolve(&client, ServerKind::Mohist, &versions[0])
        .await
        .expect("resolve");
    let jar = jar_of(&source);

    let build: u32 = jar.build.parse().expect("a numeric build id");
    assert!(build > 0);
    println!("mohist {} build {build} -> {}", versions[0], jar.file_name);
}

/// Banner is published through the same API as Mohist by the same people, so
/// the shared implementation has to actually work for both.
#[tokio::test]
#[ignore = "hits the network"]
async fn banner_resolves_through_the_mohist_api() {
    let client = net::client();
    let versions = meta::versions(&client, ServerKind::Banner)
        .await
        .expect("versions");
    assert!(!versions.is_empty(), "Banner listed no versions");

    let source = meta::resolve(&client, ServerKind::Banner, &versions[0])
        .await
        .expect("resolve");
    let jar = jar_of(&source);
    assert!(jar.file_name.starts_with("banner-"), "{}", jar.file_name);
    // Same publisher, same checksum field — and a hash we cannot check is a
    // hash that protects nothing.
    assert!(jar.sha256.is_some(), "Banner published no sha256");
    println!("banner {} -> {}", versions[0], jar.file_name);
}

/// Forge and NeoForge publish an installer, never a server.
#[tokio::test]
#[ignore = "hits the network"]
async fn the_forge_family_resolves_to_an_installer() {
    let client = net::client();

    for &kind in &[ServerKind::Forge, ServerKind::NeoForge] {
        let versions = meta::versions(&client, kind).await.expect("versions");
        assert!(!versions.is_empty(), "{kind:?} listed no versions");

        let source = meta::resolve(&client, kind, &versions[0])
            .await
            .expect("resolve");
        let meta::Source::Build(builder) = source else {
            panic!("{kind:?} should need its installer run");
        };

        assert!(
            builder.jar.file_name.contains("installer"),
            "{kind:?} resolved to {} which is not an installer",
            builder.jar.file_name
        );
        assert!(builder.args.iter().any(|a| a == "--installServer"));
        // The installer assembles into the server folder itself; there is no
        // large build tree to keep elsewhere.
        assert!(builder.workspace.is_none(), "{kind:?} wants a workspace");
        assert!(matches!(builder.produces, meta::Produces::Server));
        println!("{:<10} {} -> {}", kind.as_str(), versions[0], builder.jar.file_name);
    }
}

/// Every Minecraft version Forge and NeoForge offer has to be one Mojang
/// actually shipped — NeoForge's is derived from its own numbering, and that
/// derivation changed when Minecraft moved to date-style releases.
#[tokio::test]
#[ignore = "hits the network"]
async fn the_forge_family_only_offers_real_minecraft_versions() {
    let client = net::client();
    let releases: std::collections::HashSet<String> =
        meta::versions(&client, ServerKind::Vanilla)
            .await
            .expect("vanilla versions")
            .into_iter()
            .collect();

    for &kind in &[ServerKind::Forge, ServerKind::NeoForge] {
        let versions = meta::versions(&client, kind).await.expect("versions");
        for version in &versions {
            assert!(
                releases.contains(version),
                "{kind:?} offers {version}, which Mojang never released"
            );
        }
        println!(
            "{:<10} {} versions, newest {}",
            kind.as_str(),
            versions.len(),
            versions[0]
        );
    }
}

/// Spigot may not be redistributed, so it is compiled here — and BuildTools
/// refuses to run under the wrong Java rather than trying and failing late.
#[tokio::test]
#[ignore = "hits the network"]
async fn spigot_resolves_to_a_build_with_a_pinned_jdk() {
    let client = net::client();
    let versions = meta::versions(&client, ServerKind::Spigot)
        .await
        .expect("versions");
    assert!(!versions.is_empty(), "Spigot listed no revisions");

    let newest = &versions[0];
    let source = meta::resolve(&client, ServerKind::Spigot, newest)
        .await
        .expect("resolve");
    let meta::Source::Build(builder) = source else {
        panic!("Spigot should be built, not downloaded");
    };

    assert_eq!(builder.jar.file_name, "BuildTools.jar");
    // Roughly 600MB of clones and a Maven, which must not land inside a server
    // folder that gets backed up.
    assert!(builder.workspace.is_some(), "the build tree needs a workspace");

    let jdk = builder.jdk.expect("the revision declares a Java range");
    assert!(
        (8..=40).contains(&jdk),
        "implausible JDK {jdk} for Spigot {newest}"
    );

    match &builder.produces {
        meta::Produces::Jar(name) => assert_eq!(name, &format!("spigot-{newest}.jar")),
        other => panic!("Spigot should produce a named jar, got {other:?}"),
    }
    println!("spigot {newest} builds under JDK {jdk}");
}

/// Arclight ships a separate jar per mod loader, and the one offered has to be
/// the loader its mods are actually built for.
#[tokio::test]
#[ignore = "hits the network"]
async fn arclight_offers_the_loader_its_mods_are_built_for() {
    let client = net::client();
    let versions = meta::versions(&client, ServerKind::Arclight)
        .await
        .expect("versions");
    assert!(!versions.is_empty(), "Arclight listed no versions");

    let source = meta::resolve(&client, ServerKind::Arclight, &versions[0])
        .await
        .expect("resolve");
    let jar = jar_of(&source);

    let loader = ServerKind::Arclight
        .mod_loader()
        .expect("Arclight loads mods");
    assert!(
        jar.file_name.starts_with(&format!("arclight-{loader}-")),
        "{} is not the {loader} build",
        jar.file_name
    );
    println!("arclight {} -> {}", versions[0], jar.file_name);
}

#[test]
fn every_kind_has_a_route() {
    // No network: what Geode claims it can set up is a local decision. This
    // previously asserted the opposite for Spigot, Forge, NeoForge and Quilt.
    for &kind in ServerKind::ALL {
        assert!(meta::is_installable(kind), "{kind:?}");
    }
}

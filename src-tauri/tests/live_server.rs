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

use geode_lib::net::{self, Progress};
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


/// The question Browse asks before offering to host a pack: what can run this?
///
/// Two packs on two different loaders, because the answer is derived from the
/// pack's published metadata and a single fixture would not show the derivation
/// working — only that one pack happens to parse.
#[tokio::test]
#[ignore = "hits the network"]
async fn a_published_pack_names_the_servers_that_can_host_it() {
    let client = net::client();
    let paths = geode_lib::paths::Paths {
        root: std::env::temp_dir().join(format!("geode-pack-kinds-{}", std::process::id())),
    };
    paths.ensure_all().await.expect("scratch dirs");

    for slug in ["fabulously-optimized", "the-pixelmon-modpack"] {
        let pack = geode_lib::plan::pack::detailed(
            &client,
            &paths,
            geode_lib::providers::Provider::Modrinth,
            slug,
            None,
            None,
            &geode_lib::net::Progress::silent("test"),
        )
        .await
        .unwrap_or_else(|e| panic!("{slug}: {e}"));

        let loader = pack.loader.as_str();
        let kinds = geode_lib::server::from_pack::kinds_for(loader);
        assert!(
            !kinds.is_empty(),
            "{slug} is a {loader} pack and nothing was offered to host it"
        );
        // The picker's promise: every option runs the pack's own loader, and
        // the plain loader is offered first.
        for &kind in &kinds {
            assert_eq!(
                kind.mod_loader(),
                Some(loader),
                "{} does not run {loader}",
                kind.display_name()
            );
        }
        println!(
            "{slug} ({loader}, mc {}) -> {}",
            pack.mc_version,
            kinds
                .iter()
                .map(|k| k.display_name())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

/// The Windows answer to a missing Git, exercised from Linux.
///
/// Everything up to running the binary is platform-independent — find the
/// newest release, pick the right asset out of the dozen on it, download,
/// unpack — and that is where this can go wrong. Running the result cannot be
/// checked here; what can be checked is that the two things BuildTools needs
/// are in the archive, which is the part that would otherwise only be
/// discovered by a Windows user halfway through a Spigot build.
#[tokio::test]
#[ignore = "hits the network, downloads ~50 MB"]
async fn the_portable_git_geode_would_fetch_contains_what_buildtools_needs() {
    let client = net::client();
    let paths = geode_lib::paths::Paths {
        root: std::env::temp_dir().join(format!("geode-mingit-{}", std::process::id())),
    };
    let _ = tokio::fs::remove_dir_all(&paths.root).await;
    paths.ensure_all().await.expect("scratch dirs");

    geode_lib::server::git::fetch_mingit(&client, &paths, &Progress::silent("test"))
        .await
        .expect("fetching the portable Git should succeed");

    // What the build's PATH is built from, so an empty answer here is the bug
    // a Windows user would hit as "git: not found" mid-compile.
    let entries = geode_lib::server::git::path_entries(&paths);
    assert_eq!(entries.len(), 2, "a usable portable Git contributes two directories");
    for (dir, needed) in entries.iter().zip(["git.exe", "sh.exe"]) {
        let path = dir.join(needed);
        assert!(path.is_file(), "{} is missing, and BuildTools asks for it", path.display());
    }
    println!("portable git unpacked to {}", paths.root.display());

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

/// The probe, against a real server, end to end.
///
/// This is the regression guard for the whole feature. It downloads Mojang's
/// current server, starts it under the agent Geode ships, and checks three
/// things that matter separately: that the server is entirely unbothered, that
/// a tree was written, and that the tree is the real one rather than something
/// shaped like it.
///
/// The agent was developed against four server kinds — vanilla, Fabric,
/// NeoForge and Paper, the last two carrying a mod and a plugin that register
/// commands — but only vanilla can be fetched and run from a single test
/// without an installer step, so vanilla is what is guarded here.
#[tokio::test]
#[ignore = "hits the network, downloads ~60 MB and runs a server"]
async fn the_probe_reads_a_real_servers_commands_without_disturbing_it() {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let client = net::client();
    let paths = geode_lib::paths::Paths {
        root: std::env::temp_dir().join(format!("geode-probe-live-{}", std::process::id())),
    };
    let _ = tokio::fs::remove_dir_all(&paths.root).await;
    paths.ensure_all().await.expect("scratch dirs");

    let versions = meta::versions(&client, ServerKind::Vanilla)
        .await
        .expect("versions");
    let source = meta::resolve(&client, ServerKind::Vanilla, &versions[0])
        .await
        .expect("resolve");
    let jar_meta = jar_of(&source);

    let run = paths.server_run("probe-live");
    tokio::fs::create_dir_all(&run).await.expect("run dir");
    let jar = run.join(&jar_meta.file_name);
    geode_lib::net::fetch_one(
        &client,
        &geode_lib::net::Download::new(jar_meta.url.clone(), jar.clone()),
    )
    .await
    .expect("download the server");

    // A flat world on a free port, so this is quick and cannot collide with
    // anything the developer happens to be running.
    tokio::fs::write(run.join("eula.txt"), "eula=true\n")
        .await
        .expect("eula");
    tokio::fs::write(
        run.join("server.properties"),
        "server-port=25666\nserver-ip=127.0.0.1\nonline-mode=false\n\
         level-type=minecraft\\:flat\nview-distance=4\nsimulation-distance=4\n",
    )
    .await
    .expect("properties");

    let agent = geode_lib::server::probe::agent_arg(&paths, "probe-live")
        .await
        .expect("the shipped agent should be usable");

    let mut child = tokio::process::Command::new("java")
        .arg("-Xmx1G")
        .arg(&agent)
        .arg("-jar")
        .arg(&jar_meta.file_name)
        .arg("nogui")
        .current_dir(&run)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the server");

    // Long enough to boot a flat world and for the probe's first pass.
    tokio::time::sleep(std::time::Duration::from_secs(25)).await;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"stop\n").await;
    }
    let finished = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        child.wait_with_output(),
    )
    .await
    .expect("the server should stop on its own")
    .expect("wait");

    let log = String::from_utf8_lossy(&finished.stdout);
    assert!(
        finished.status.success(),
        "the probe must not cost the server its exit status: {}\n{}",
        finished.status,
        log.lines().rev().take(15).collect::<Vec<_>>().join("\n")
    );
    assert!(
        log.contains("Done ("),
        "the server should have finished starting"
    );
    assert!(
        !run.join("crash-reports").is_dir(),
        "the server crashed with the probe attached"
    );

    let tree = geode_lib::server::probe::read(&paths, "probe-live").await;
    assert!(
        tree.len() > 50,
        "a vanilla server has ninety-odd commands, not {}",
        tree.len()
    );
    // Not merely a list of names: the shape is what makes this better than
    // scraping `help`.
    let gamemode = tree
        .iter()
        .find(|c| c.name == "gamemode")
        .expect("every server has /gamemode");
    assert_eq!(
        gamemode.args,
        vec!["gamemode"],
        "the argument the command takes should be named"
    );
    let execute = tree.iter().find(|c| c.name == "execute").expect("/execute");
    assert!(
        execute.children.iter().any(|c| c.name == "as"),
        "subcommands should come through: /execute as ..."
    );
    println!("{} commands read from a live vanilla server", tree.len());

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

/// A name becomes a UUID only by asking Mojang, and the files these go into are
/// keyed by UUID.
///
/// Offline there is nothing honest to test: a fixture would prove that the
/// parser parses the fixture. What can break is the endpoint — Mojang has moved
/// these lookups before — and what must never happen is a name being written
/// into `whitelist.json` or `ops.json` under a UUID that was guessed.
#[tokio::test]
#[ignore = "hits the network"]
async fn mojang_resolves_a_name_to_the_uuid_the_files_are_keyed_by() {
    use geode_lib::server::players;

    let client = net::client();

    // Notch's UUID is the one fixed point in Minecraft: account one, and the
    // value every offline implementation is checked against.
    let notch = players::resolve_online(&client, "Notch")
        .await
        .expect("Mojang should know Notch");
    assert_eq!(notch.uuid, "069a79f4-44e9-4726-a5be-fca90e38aaf5");
    assert_eq!(
        notch.name, "Notch",
        "the canonical spelling is what goes in the file, not what was typed"
    );

    // Case is Mojang's to decide: whatever an admin types, the file records the
    // name as that account actually spells it.
    let lower = players::resolve_online(&client, "notch")
        .await
        .expect("a lookup is not case-sensitive");
    assert_eq!(lower.name, "Notch");
    assert_eq!(lower.uuid, notch.uuid);

    // A name nobody owns has to be reported, not worked around: writing a
    // derived UUID for it would whitelist somebody who does not exist and
    // never whitelist the person meant.
    let missing = players::resolve_online(&client, "GeodeNoSuchName9")
        .await
        .expect_err("that account does not exist");
    let text = missing.to_string();
    assert!(
        text.contains("GeodeNoSuchName9") && text.contains("no Minecraft account"),
        "the refusal should name who could not be found: {text}"
    );

    println!("Notch -> {}", notch.uuid);
}

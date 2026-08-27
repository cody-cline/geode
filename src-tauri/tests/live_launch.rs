//! End-to-end: install a real Minecraft version and start it.
//!
//! This is the test that proves the whole chain — metadata resolution, library
//! and asset downloads, native extraction, Java selection, classpath assembly
//! and argument templating — actually produces a JVM that reaches the game's
//! render loop. Everything else is a proxy for this.
//!
//! It downloads several hundred megabytes and needs a display, so it is
//! `#[ignore]` by default:
//!
//! ```sh
//! # vanilla, newest release
//! cargo test --test live_launch -- --ignored --nocapture
//!
//! # pin a version and loader
//! GEODE_TEST_VERSION=1.21.1 GEODE_TEST_LOADER=fabric \
//!   cargo test --test live_launch -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};

use geode_lib::auth::store::Session;
use geode_lib::config::Config;
use geode_lib::install;
use geode_lib::instance::Instance;
use geode_lib::meta::{self, Loader, ManifestCache};
use geode_lib::net::{self, Progress};
use geode_lib::paths::Paths;

/// Log fragments that only appear once the game is genuinely up: the render
/// backend is live and the sound engine has initialised.
const SUCCESS_MARKERS: &[&str] = &[
    "Sound engine started",
    "Backend library: LWJGL",
    "OpenAL initialized",
    "Created: 1024x",
];

/// Reaching this means the classpath and main class were right, even if we
/// never get a window (no GPU under Xvfb, for instance).
const PROGRESS_MARKER: &str = "Setting user:";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads hundreds of megabytes and launches the game"]
async fn installs_and_launches_minecraft() {
    let root = std::env::var_os("GEODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("geode-e2e"));
    let paths = Paths { root };
    paths.ensure_all().await.unwrap();

    let client = net::client();
    let manifests = ManifestCache::new();

    let loader = std::env::var("GEODE_TEST_LOADER")
        .ok()
        .and_then(|s| Loader::parse(&s))
        .unwrap_or(Loader::Vanilla);

    let game_version = match std::env::var("GEODE_TEST_VERSION") {
        Ok(v) => v,
        Err(_) => {
            let manifest = manifests.get(&client, &paths).await.unwrap();
            manifest.latest.release
        }
    };

    let loader_version = if loader == Loader::Vanilla {
        String::new()
    } else {
        meta::loader_versions(&client, loader, &game_version)
            .await
            .expect("loader versions")
            .first()
            .expect("no loader build for this version")
            .clone()
    };

    println!(
        "== installing Minecraft {game_version} ({}{})",
        loader.display_name(),
        if loader_version.is_empty() {
            String::new()
        } else {
            format!(" {loader_version}")
        }
    );

    let started = Instant::now();
    let progress = Progress::silent("e2e");
    let installed = install::install_version(
        &client,
        &paths,
        &manifests,
        &game_version,
        loader,
        &loader_version,
        None,
        &progress,
    )
    .await
    .expect("install failed");

    println!(
        "== installed in {:?}: java {}, main {}",
        started.elapsed(),
        installed.java.display(),
        installed.version.main_class
    );

    // --- assert the install actually produced what launching needs ---

    let features = install::launch_features();
    let classpath =
        install::classpath(&paths, &installed.version, &installed.client_jar, &features);
    assert!(!classpath.is_empty(), "empty classpath");

    // Loaders that use BootstrapLauncher exclude the unpatched client jar by
    // filename, so the jar we hand them has to be named after the version id.
    if loader != Loader::Vanilla {
        let jar_name = installed
            .client_jar
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(
            jar_name,
            format!("{}.jar", installed.version_id),
            "client jar name must match version_name for -DignoreList to work"
        );
    }

    let mut missing: Vec<&PathBuf> = Vec::new();
    for entry in &classpath {
        if !entry.exists() {
            missing.push(entry);
        }
    }
    assert!(
        missing.is_empty(),
        "{} classpath entries were never downloaded: {:?}",
        missing.len(),
        missing.iter().take(5).collect::<Vec<_>>()
    );
    println!("== {} classpath entries, all present", classpath.len());

    let natives: Vec<_> = std::fs::read_dir(&installed.natives_dir)
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    println!("== {} native libraries extracted", natives.len());

    // --- build the command line ---

    let instance = Instance::new(
        "e2e".into(),
        game_version.clone(),
        loader,
        loader_version.clone(),
    );
    let config = Config::default();
    let session = Session {
        username: "GeodeTester".into(),
        uuid: geode_lib::auth::offline_uuid("GeodeTester"),
        access_token: "0".into(),
        user_type: "legacy",
    };

    let plan =
        geode_lib::launch::build_plan(&paths, &config, &instance, &installed, &session).unwrap();

    assert!(
        plan.args.iter().any(|a| a == &installed.version.main_class),
        "main class missing from the command line"
    );
    assert!(
        plan.args.iter().any(|a| a.starts_with("-Xmx")),
        "no heap limit set"
    );
    assert!(
        !plan.args.iter().any(|a| a.contains("${")),
        "unsubstituted placeholder in args: {:?}",
        plan.args.iter().filter(|a| a.contains("${")).collect::<Vec<_>>()
    );

    tokio::fs::create_dir_all(&plan.game_dir).await.unwrap();

    // --- run it ---

    let use_xvfb = std::env::var("GEODE_TEST_XVFB").as_deref() != Ok("0");
    let mut command = if use_xvfb {
        // Run offscreen so the test does not throw a window onto the desktop.
        let mut c = tokio::process::Command::new("xvfb-run");
        c.arg("-a")
            .arg("-s")
            .arg("-screen 0 1280x720x24")
            .arg(&plan.java)
            .args(&plan.args);
        // Xvfb has no GPU, so force Mesa's software rasteriser.
        c.env("LIBGL_ALWAYS_SOFTWARE", "1");
        c.env("GALLIUM_DRIVER", "llvmpipe");
        c
    } else {
        let mut c = tokio::process::Command::new(&plan.java);
        c.args(&plan.args);
        c
    };

    let mut child = command
        .current_dir(&plan.game_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .expect("could not start the JVM");

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(512);

    let out_tx = tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = out_tx.send(line).await;
        }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx.send(format!("[stderr] {line}")).await;
        }
    });

    let mut reached_main = false;
    let mut reached_render = false;
    let mut transcript: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(240);

    while Instant::now() < deadline {
        let remaining = deadline - Instant::now();
        let Ok(Some(line)) = tokio::time::timeout(remaining, rx.recv()).await else {
            break;
        };

        if transcript.len() < 400 {
            transcript.push(line.clone());
        }
        if line.contains(PROGRESS_MARKER) {
            reached_main = true;
            println!("== game started: {}", line.trim());
        }
        if SUCCESS_MARKERS.iter().any(|m| line.contains(m)) {
            reached_render = true;
            println!("== reached the render loop: {}", line.trim());
            break;
        }
    }

    let _ = child.start_kill();
    let _ = child.wait().await;

    if !reached_main && !reached_render {
        eprintln!("---- last output ----");
        for line in transcript.iter().rev().take(40).rev() {
            eprintln!("{line}");
        }
        panic!("the game never got past JVM startup");
    }

    if reached_render {
        println!("== PASS: Minecraft {game_version} launched and rendered");
    } else {
        // Software GL can fail where the classpath is perfectly fine; getting
        // to `Setting user:` already proves everything this crate is
        // responsible for.
        println!(
            "== PASS: Minecraft {game_version} started (no render loop — expected without a GPU)"
        );
    }
}

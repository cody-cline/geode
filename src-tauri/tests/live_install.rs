//! Installing a server with the code that actually installs servers.
//!
//! `live_server` proves resolution: that the right URL and the right arguments
//! come back. Nothing proved the rest of the path — fetch the tool, run it,
//! look at what it left, and record how the server starts. That glue is exactly
//! where a wrong assumption about an installer's output becomes a server that
//! installs cleanly and then will not start, which no amount of checking URLs
//! would catch.
//!
//! ```sh
//! cargo test --test live_install -- --ignored --nocapture
//! ```
//!
//! Spigot is deliberately absent. A BuildTools run is ten minutes and 600MB of
//! clones, which is more than a test suite should ask for; it was verified by
//! hand instead, and the pieces it depends on — the declared JDK range, the
//! forced output name — are unit-tested.

use geode_lib::net::{self, Progress};
use geode_lib::paths::Paths;
use geode_lib::server::installer::LaunchKind;
use geode_lib::server::{self, install, run, ServerKind};

/// A throwaway data directory per test; they run in parallel in one process.
fn scratch(name: &str) -> Paths {
    let dir = std::env::temp_dir().join(format!(
        "geode-install-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    Paths { root: dir }
}

/// Create a server and install its software for real.
async fn install_server(paths: &Paths, kind: ServerKind, version: &str) -> server::Server {
    let client = net::client();
    let mut server = server::create(paths, format!("{kind:?} test"), kind, version.to_string())
        .await
        .expect("create the server folder");

    install::install_jar(&client, paths, &mut server, &Progress::silent("test"))
        .await
        .unwrap_or_else(|e| panic!("{kind:?} {version}: {e}"));
    server
}

/// The simple case, and the one every other case is measured against: the
/// download is the server.
#[tokio::test]
#[ignore = "downloads a server"]
async fn a_fabric_server_is_just_the_jar_it_downloaded() {
    let paths = scratch("fabric");
    let server = install_server(&paths, ServerKind::Fabric, "1.21.1").await;

    assert_eq!(server.launch, LaunchKind::Jar);
    assert!(server.jar.ends_with(".jar"), "{}", server.jar);
    assert!(
        tokio::fs::metadata(paths.server_run(&server.id).join(&server.jar))
            .await
            .is_ok(),
        "recorded {} but it is not on disk",
        server.jar
    );

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

/// Quilt's installer leaves two jars, and only one of them loads mods.
#[tokio::test]
#[ignore = "runs the Quilt installer"]
async fn quilt_starts_from_its_launcher_not_the_vanilla_jar_beside_it() {
    let paths = scratch("quilt");
    let server = install_server(&paths, ServerKind::Quilt, "1.21.1").await;
    let run_dir = paths.server_run(&server.id);

    assert_eq!(server.launch, LaunchKind::Jar);
    assert_eq!(
        server.jar, "quilt-server-launch.jar",
        "the installer drops a 50MB vanilla server.jar beside a 500-byte \
         launcher; choosing the big one starts Minecraft with no mod loader"
    );
    assert!(tokio::fs::metadata(run_dir.join(&server.jar)).await.is_ok());
    // And it really did fetch Minecraft alongside, as it was asked to.
    assert!(tokio::fs::metadata(run_dir.join("server.jar")).await.is_ok());

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

/// The case that motivated all of this: from 1.17 on, Forge's installer
/// produces no runnable jar whatsoever.
#[tokio::test]
#[ignore = "runs the Forge installer, which takes minutes"]
async fn a_modern_forge_server_launches_from_an_argument_file() {
    let paths = scratch("forge");
    let server = install_server(&paths, ServerKind::Forge, "1.20.1").await;
    let run_dir = paths.server_run(&server.id);

    let LaunchKind::ArgFile(file) = &server.launch else {
        panic!(
            "Forge 1.20.1 leaves no jar to run, so it has to launch from an \
             argument file; recorded {:?} instead",
            server.launch
        );
    };
    assert!(
        file.contains("minecraftforge"),
        "found {file}, which is not Forge's own argument file"
    );
    assert!(
        tokio::fs::metadata(run_dir.join(file)).await.is_ok(),
        "recorded {file} but it is not on disk"
    );

    // The command line built from it has to name that file, or the server is
    // launched with nothing to load.
    let args = run::build_args(&server, None);
    assert!(
        args.iter().any(|a| a == &format!("@{file}")),
        "the launch arguments never reference the argument file: {args:?}"
    );
    assert!(
        !args.iter().any(|a| a == "-jar"),
        "there is no jar to pass: {args:?}"
    );

    let _ = tokio::fs::remove_dir_all(&paths.root).await;
}

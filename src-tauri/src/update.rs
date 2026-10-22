//! Whether Geode offers to replace itself with a newer build, and what it says
//! about it.
//!
//! The downloading, the signature check and the file swap belong to
//! `tauri-plugin-updater`, and reaching that plugin needs the app shell — so
//! those two commands sit in `commands.rs` beside the others that take an
//! `AppHandle`. Everything that *decides* lives here instead, with no Tauri in
//! sight, because the decisions are the part worth testing and a test has no
//! window to hand it.
//!
//! The rule the whole feature rests on: an update is installed only if it was
//! signed by the key this binary was compiled with. A build compiled without a
//! key has no updater at all — `lib.rs` never registers the plugin — so there
//! is no path through this code that downloads something and hopes.

use std::time::Duration;

use semver::Version;

use crate::state::AppState;

/// How long the manifest fetch may take before the person is told instead of
/// left watching a spinner.
///
/// `reqwest` applies no request timeout of its own, so without this a host
/// that accepts the connection and never answers — a captive portal, a
/// stalled proxy — leaves `check_for_update` pending for the life of the
/// process, with Settings still reading "Looking for a newer Geode" and Check
/// again disabled. Generous for a few hundred bytes of JSON, and finite.
pub const MANIFEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the artefact download may take, which is a different question: it
/// is a hundred megabytes or more, over whatever connection somebody has.
///
/// This is a whole-request ceiling, body included, so it has to allow for a
/// slow line rather than a stalled one — half an hour is roughly 60 KB/s on a
/// Geode-sized AppImage. It is also the only ceiling there is on how much a
/// manifest can make Geode hold in memory: the plugin buffers the download
/// into a `Vec<u8>` and its progress callback cannot refuse a chunk, so what
/// bounds an absurdly large artefact is the clock, not a byte count.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The minisign public key every update is verified against, compiled in from
/// `GEODE_UPDATER_PUBKEY`.
///
/// Read from the environment rather than committed, for the same reason the
/// Microsoft client id is ([`crate::auth::default_client_id`]): the private
/// half belongs to whoever cuts the releases and lives only as a CI secret, and
/// a build nobody signs for should not claim to verify anything. Committing the
/// public half would also mean rotating a key in two places instead of one.
///
/// Absent, the feature is off end to end: no plugin, no check, no download, and
/// Settings says so rather than offering a button that cannot work.
pub fn public_key() -> Option<String> {
    usable_key(option_env!("GEODE_UPDATER_PUBKEY"))
}

/// A key is a key only if there is something in it.
///
/// Split out to be tested, and because the empty case is a real one rather than
/// a defensive flourish: GitHub hands a workflow an unset secret as the empty
/// string, which is exactly what a half-configured release build would compile
/// with. That has to mean "no updater", not "an updater that offers a download
/// it will then refuse to install".
fn usable_key(from_env: Option<&str>) -> Option<String> {
    from_env
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

/// Why Geode is saying nothing about a version it just heard about.
///
/// Every one of these is a quiet no. None of them is an error worth a toast:
/// the manifest is a file on the internet, and the only safe reading of one
/// that does not make sense is to carry on with the build already installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignored {
    /// The manifest, or this build itself, named something that is not a
    /// version. Nothing can be compared, so nothing is offered — a string that
    /// will not parse must never be read as "newer".
    Unreadable,
    /// The same release, or an older one. Offering it would be a downgrade
    /// dressed up as an update, and the usual cause is a manifest that has
    /// gone stale or been rolled back under us.
    NotNewer,
    /// Exactly the version the person chose to skip.
    Skipped,
}

/// Whether the version a manifest announces is one to put in front of somebody.
///
/// `skipped` is the version the person pressed "Skip this one" for, if any. A
/// skip is for that single release and not for updating in general, so the next
/// one through here is offered normally — which is also why a `skipped` string
/// that no longer parses suppresses nothing instead of suppressing everything.
pub fn offer(current: &str, announced: &str, skipped: Option<&str>) -> Result<(), Ignored> {
    let current = Version::parse(current).map_err(|_| Ignored::Unreadable)?;
    let announced = Version::parse(announced).map_err(|_| Ignored::Unreadable)?;

    if announced <= current {
        return Err(Ignored::NotNewer);
    }
    if skipped.and_then(|skipped| Version::parse(skipped).ok()) == Some(announced) {
        return Err(Ignored::Skipped);
    }
    Ok(())
}

/// What installing an update would interrupt right now, said to the person, or
/// `None` when nothing is running and it can go ahead.
///
/// Geode launches Minecraft and servers as child processes of itself and keeps
/// hold of their pipes: the console, the crash log, the Stop button and "the
/// game exited" are all the parent reading its own child. Installing an update
/// ends in a restart — on Windows the installer closes Geode and relaunches it,
/// on Linux and macOS Geode re-executes itself once the new files are in place —
/// and a restart takes every one of those pipes with it.
///
/// Which way that lands is not something to gamble a world on. Geode already
/// kills running games when its window goes away, so the restart may well
/// SIGKILL a game mid-save; and if the process exits before that teardown runs,
/// the JVM is instead reparented and plays on with no launcher attached, so
/// nothing shows its log, nothing can stop it, and its exit is heard by nobody.
/// Both are worse than waiting, so an install is refused while anything runs —
/// not queued, not forced.
///
/// A process is not the only thing worth protecting. A launch spends minutes
/// before there is any JVM to find — updating the instance from its server,
/// downloading libraries, rewriting `.minecraft` — and an install is writing
/// the same files for the same reasons. Neither appears in a process registry,
/// and a restart through either leaves an instance half written, so both count
/// here too.
pub fn restart_would_interrupt(state: &AppState) -> Option<String> {
    let mut what = Vec::new();
    match state.games.running_ids().len() {
        0 => {}
        1 => what.push("Minecraft".to_string()),
        n => what.push(format!("{n} copies of Minecraft")),
    }
    match state.servers.running_ids().len() {
        0 => {}
        1 => what.push("a server".to_string()),
        n => what.push(format!("{n} servers")),
    }
    match state.launching_count() {
        0 => {}
        1 => what.push("a launch that is still preparing".to_string()),
        n => what.push(format!("{n} launches that are still preparing")),
    }
    match state.installs_in_flight() {
        0 => {}
        1 => what.push("an install that is still running".to_string()),
        n => what.push(format!("{n} installs that are still running")),
    }
    if what.is_empty() {
        return None;
    }
    Some(format!(
        "Geode has to restart to finish installing an update, which would interrupt {}. \
         Close it, or wait for it to finish, and press Install again — a restart either kills \
         a running game outright or leaves it playing with no launcher attached to it, and a \
         launch or install it cuts in half leaves the instance half written.",
        what.join(" and ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_key_in_the_environment_is_not_a_key() {
        assert_eq!(usable_key(None), None);
        // What a release build compiled against a secret the owner never set
        // would see. It must switch the updater off, not half on.
        assert_eq!(usable_key(Some("")), None);
        assert_eq!(usable_key(Some("   \n")), None);
        assert_eq!(
            usable_key(Some("  dW50cnVzdGVkIGNvbW1lbnQ=  ")).as_deref(),
            Some("dW50cnVzdGVkIGNvbW1lbnQ=")
        );
    }

    #[test]
    fn only_a_newer_version_is_offered() {
        assert_eq!(offer("0.1.0", "0.2.0", None), Ok(()));
        // The same release, which is what every run after an update sees.
        assert_eq!(offer("0.2.0", "0.2.0", None), Err(Ignored::NotNewer));
        // A downgrade: a rolled-back or stale manifest must not walk somebody
        // backwards onto a build they already left.
        assert_eq!(offer("0.2.0", "0.1.0", None), Err(Ignored::NotNewer));
        assert_eq!(offer("0.10.0", "0.9.0", None), Err(Ignored::NotNewer));
    }

    #[test]
    fn a_version_that_is_not_a_version_is_ignored_rather_than_assumed_newer() {
        for announced in ["", "latest", "v0.2.0", "0.2", "nightly-2026-10-21"] {
            assert_eq!(
                offer("0.1.0", announced, None),
                Err(Ignored::Unreadable),
                "{announced:?} was read as a version"
            );
        }
        // And in the other direction, so a build whose own version is junk
        // never downloads anything either.
        assert_eq!(offer("unknown", "0.2.0", None), Err(Ignored::Unreadable));
    }

    #[test]
    fn a_skipped_version_stays_skipped_until_a_newer_one_arrives() {
        assert_eq!(offer("0.1.0", "0.2.0", Some("0.2.0")), Err(Ignored::Skipped));
        // Skipping one release is not switching updates off.
        assert_eq!(offer("0.1.0", "0.3.0", Some("0.2.0")), Ok(()));
        // A skip nobody can parse suppresses nothing, rather than everything.
        assert_eq!(offer("0.1.0", "0.2.0", Some("whatever")), Ok(()));
    }

    #[test]
    fn an_install_is_refused_while_a_game_is_running_and_says_what_is_running() {
        let state = AppState::new(
            crate::paths::Paths {
                root: std::env::temp_dir().join("geode-update-interrupt"),
            },
            crate::config::Config::default(),
        );
        assert_eq!(restart_would_interrupt(&state), None);

        state
            .games
            .insert("survival".into(), crate::launch::RunningGame::none_really());
        let refused = restart_would_interrupt(&state).expect("a game is running");
        assert!(refused.contains("Minecraft"), "{refused}");
        assert!(refused.contains("Close it"), "{refused}");

        state
            .games
            .insert("creative".into(), crate::launch::RunningGame::none_really());
        assert!(
            restart_would_interrupt(&state)
                .expect("two games")
                .contains("2 copies of Minecraft")
        );

        state.games.remove("survival");
        state.games.remove("creative");
        assert_eq!(restart_would_interrupt(&state), None);
    }

    /// A launch spends minutes before there is any process to find: updating
    /// the instance from its server, downloading libraries, rewriting
    /// `.minecraft`. `state.games` is empty for all of it, so Settings showed
    /// `blocked: null` and an enabled Install button over a launch that a
    /// restart would leave half-written. An install in flight is the same
    /// hole, reached from the same screen.
    #[test]
    fn work_with_nothing_to_kill_yet_still_blocks_an_install() {
        let state = AppState::new(
            crate::paths::Paths {
                root: std::env::temp_dir().join("geode-update-in-flight"),
            },
            crate::config::Config::default(),
        );
        assert_eq!(restart_would_interrupt(&state), None);

        let claim = state.claim_launch("survival").expect("nothing holds it");
        assert!(
            state.games.running_ids().is_empty(),
            "the point of this test is that no process exists yet"
        );
        let refused = restart_would_interrupt(&state).expect("a launch is preparing");
        assert!(refused.contains("a launch"), "{refused}");
        drop(claim);
        assert_eq!(restart_would_interrupt(&state), None);

        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        state.register_cancel("install-1", flag);
        let refused = restart_would_interrupt(&state).expect("an install is running");
        assert!(refused.contains("an install"), "{refused}");
        state.clear_cancel("install-1");
        assert_eq!(restart_would_interrupt(&state), None);
    }

    /// The endpoint and the committed key are configuration, not code, and
    /// both are load-bearing: the plugin reads the manifest from the endpoint
    /// in `tauri.conf.json`, and an `http://` one would hand an update over to
    /// anyone on the path. The plugin refuses plain http itself in a release
    /// build and only warns in a debug one, so this is the check that holds at
    /// a desk as well.
    ///
    /// The committed `pubkey` is empty on purpose. The real one is compiled in
    /// from `GEODE_UPDATER_PUBKEY` and the release job writes it here for the
    /// bundler alone; a key in the repository would be a key nobody can rotate
    /// and a placeholder would be worse, because it would look real.
    #[test]
    fn the_committed_config_names_an_https_endpoint_and_no_key() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let updater = &config["plugins"]["updater"];

        assert_eq!(
            updater["pubkey"],
            serde_json::json!(""),
            "a public key was committed to the repository"
        );

        let endpoints = updater["endpoints"]
            .as_array()
            .expect("plugins.updater.endpoints");
        assert!(!endpoints.is_empty(), "the updater has nowhere to look");
        for endpoint in endpoints {
            let url = endpoint.as_str().unwrap_or_default();
            assert!(
                url.starts_with("https://"),
                "the updater would fetch its manifest over plain http: {url}"
            );
        }
    }

    /// `requireSignedVersion` and the signer that records one have to move
    /// together, and this is the only place that can hold them to each other.
    ///
    /// `latest.json` is not signed; the signature covers the artefact alone.
    /// Without this flag the plugin's `verify_signed_version` returns early
    /// and never compares the version a manifest announces against the version
    /// the signature was made for — so anyone who can serve that file can
    /// announce 9.9.9 over the url and signature of a genuine old release and
    /// walk somebody backwards onto a build they already left, with no key at
    /// all. `Ignored::NotNewer` does not stop that: it only ever sees the
    /// string the manifest chose.
    ///
    /// The flag alone is not enough either, and is actively worse: against a
    /// signer that records no version every update fails instead. The Tauri
    /// CLI began writing `version:` into the signature's trusted comment in
    /// 2.12.0, and the release job runs `npm ci`, so the lockfile is what
    /// decides which signer cuts a release.
    #[test]
    fn the_signed_version_check_is_on_and_the_pinned_signer_records_one() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(
            config["plugins"]["updater"]["requireSignedVersion"],
            serde_json::json!(true),
            "without this the manifest's `version` is the only thing deciding which build \
             gets installed, and nobody signs a manifest"
        );

        let lock: serde_json::Value = serde_json::from_str(include_str!(
            "../../package-lock.json"
        ))
        .expect("package-lock.json");
        let pinned = lock["packages"]["node_modules/@tauri-apps/cli"]["version"]
            .as_str()
            .expect("the lockfile pins @tauri-apps/cli");
        let pinned = Version::parse(pinned).expect("the pinned CLI is a semver version");
        let records_signed_version = Version::parse("2.12.0").unwrap();
        assert!(
            pinned >= records_signed_version,
            "the pinned Tauri CLI is {pinned}, which signs with a trusted comment of \
             `timestamp:…\tfile:…` and no `version:` — every update would fail the check \
             above. {records_signed_version} is the first that records one."
        );
    }
}

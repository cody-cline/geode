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
//!
//! That rule leaves a build with no key unable to *install* anything, which is
//! not the same as having nothing to say. Reading the public releases API needs
//! no key, no token and no signature, so such a build still looks, still says
//! which version is out, and sends the person to fetch it themselves — see
//! [`latest_release`]. Both halves name their version through one
//! [`Announced`] and one [`offer`], so a skip, a downgrade, a version that will
//! not parse and the version already running are refused identically whichever
//! source named it.

use std::time::Duration;

use semver::Version;
use serde::Deserialize;

use crate::error::{Context, Result};
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

/// Where a build with no key asks what the current release is.
///
/// The public releases API: an ordinary GET with nothing in it but the user
/// agent every other request here carries. No token, no credentials, nothing
/// about the person and no query string for any of that to ride along in — a
/// build that cannot install an update has no business identifying itself to
/// find out there is one.
///
/// `releases/latest` resolves the way the signed endpoint's
/// `releases/latest/download` does: the most recent published release that is
/// neither a draft nor a prerelease. The two modes are therefore talking about
/// the same release rather than two ideas of "latest", and
/// `both_modes_look_at_the_same_repository` holds them to the same repository
/// as well.
pub const RELEASES_API: &str = "https://api.github.com/repos/cody-cline/geode/releases/latest";

/// The one release a keyless build asks about, as the API gives it. Every other
/// field in that response — the assets, the draft and prerelease flags, who cut
/// it — is something this mode cannot act on, so none of it is read.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    /// The tag, which is `vX.Y.Z` rather than a bare version. See
    /// [`Release::version`].
    pub tag_name: String,
    /// The release notes, which GitHub gives as the empty string when a
    /// release has none.
    pub body: Option<String>,
}

impl Release {
    /// The version the tag names.
    ///
    /// Releases are tagged `vX.Y.Z` (see docs/releasing.md) and
    /// `Version::parse` reads no leading `v`, so a tag handed straight to
    /// [`offer`] would be [`Ignored::Unreadable`] every time and this mode
    /// would never offer anything at all. Exactly one `v` comes off and
    /// nothing else is repaired: the signed manifest's own `version` field
    /// carries no `v`, and anything else a tag might say has to reach `offer`
    /// unchanged so both modes refuse the same strings.
    pub fn version(&self) -> &str {
        let tag = self.tag_name.trim();
        tag.strip_prefix('v').unwrap_or(tag)
    }

    /// This release as something a source has named.
    pub fn announced(self) -> Announced {
        Announced {
            version: self.version().to_string(),
            // A release with no notes comes back as `""`, and an empty "What
            // changed" to open is worse than no disclosure at all.
            notes: self.body.filter(|notes| !notes.trim().is_empty()),
        }
    }
}

/// Ask GitHub what the current release is.
///
/// Errors are the caller's to report rather than to read as "nothing newer":
/// the API is rate-limited by IP, answers 404 until the first release exists,
/// and is a host on the internet like any other. A build that cannot say
/// whether there is an update must say that, because "up to date" is a claim
/// and a failed lookup does not support it.
pub async fn latest_release(client: &reqwest::Client) -> Result<Release> {
    crate::net::get_json(client, RELEASES_API)
        .await
        .ctx("asking GitHub which Geode is the latest")
}

/// A version some source has named, before anything has decided whether to say
/// so.
///
/// Two sources name one: the signed manifest a build with a key reads through
/// `tauri-plugin-updater`, and the releases API a build without one reads
/// itself. What may then be done about it differs — only the first can be
/// installed — and nothing else does, which is why there is one type here and
/// one [`Announced::worth_mentioning`] between them instead of a second set of
/// rules that could drift from the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announced {
    pub version: String,
    pub notes: Option<String>,
}

impl Announced {
    /// The same offer back when it is one worth putting in front of somebody,
    /// and `None` for every quiet no [`Ignored`] lists.
    pub fn worth_mentioning(self, current: &str, skipped: Option<&str>) -> Option<Self> {
        offer(current, &self.version, skipped).is_ok().then_some(self)
    }
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

    /// The tag is the only thing the releases API offers that names a version,
    /// and it is not spelled the way a version is. Left alone, every release
    /// this mode ever sees is `Ignored::Unreadable` and a keyless build is
    /// back to saying nothing — the exact failure this half exists to fix,
    /// only quieter, because an unreadable version is deliberately
    /// indistinguishable from "nothing newer".
    #[test]
    fn a_release_tag_names_a_version_once_the_v_releases_are_tagged_with_is_off() {
        let tagged = |tag: &str| Release {
            tag_name: tag.to_string(),
            body: None,
        };
        assert_eq!(tagged("v0.2.0").version(), "0.2.0");
        assert_eq!(offer("0.1.0", tagged("v0.2.0").version(), None), Ok(()));
        // A tag without one, since nothing makes a release carry it.
        assert_eq!(tagged("0.2.0").version(), "0.2.0");
        assert_eq!(tagged(" v0.2.0\n").version(), "0.2.0");
        // One `v`, and no other repair: a tag that is not a version has to
        // reach `offer` as unreadable rather than be guessed at here, so both
        // modes refuse the same strings.
        assert_eq!(tagged("vv0.2.0").version(), "v0.2.0");
        assert_eq!(tagged("latest").version(), "latest");
        assert_eq!(
            offer("0.1.0", tagged("release-7").version(), None),
            Err(Ignored::Unreadable)
        );
    }

    /// Pinned against the shape GitHub actually returns, trimmed of the fields
    /// this mode cannot act on. A rename on either side — `tag_name` to
    /// something else, or serde asked for camelCase — is a lookup that fails
    /// to parse for everybody at once, and the only thing that would notice is
    /// a release nobody is offered.
    #[test]
    fn the_releases_api_response_is_read_for_the_tag_and_the_notes() {
        let release: Release = serde_json::from_str(
            r#"{
                "url": "https://api.github.com/repos/cody-cline/geode/releases/1",
                "html_url": "https://github.com/cody-cline/geode/releases/tag/v0.2.0",
                "id": 1,
                "tag_name": "v0.2.0",
                "name": "Geode 0.2.0",
                "draft": false,
                "prerelease": false,
                "body": "Servers can be run headless.",
                "assets": [{ "name": "geode_0.2.0_amd64.AppImage", "size": 94371840 }]
            }"#,
        )
        .expect("the releases API response");
        assert_eq!(release.version(), "0.2.0");
        let announced = release.announced();
        assert_eq!(announced.version, "0.2.0");
        assert_eq!(announced.notes.as_deref(), Some("Servers can be run headless."));

        // A release with no notes, which is what `""` means here.
        let bare: Release =
            serde_json::from_str(r#"{"tag_name": "v0.3.0", "body": ""}"#).expect("no notes");
        assert_eq!(bare.announced().notes, None);
    }

    /// The decision is one function for both halves, so this is the whole of
    /// what either of them may offer. A version skipped while reading the
    /// signed manifest is skipped while reading the releases API and the other
    /// way round, because there is no second place for either to disagree in.
    #[test]
    fn a_named_version_is_judged_the_same_whichever_source_named_it() {
        let named = |version: &str| Announced {
            version: version.to_string(),
            notes: Some("notes".to_string()),
        };
        assert_eq!(
            named("0.2.0").worth_mentioning("0.1.0", None),
            Some(named("0.2.0"))
        );
        // Every quiet no `offer` knows, reached through this one call: the
        // version already running, a downgrade, one that will not parse, and
        // the one the person pressed Skip this one for.
        assert_eq!(named("0.1.0").worth_mentioning("0.1.0", None), None);
        assert_eq!(named("0.0.9").worth_mentioning("0.1.0", None), None);
        assert_eq!(named("v0.2.0").worth_mentioning("0.1.0", None), None);
        assert_eq!(named("0.2.0").worth_mentioning("0.1.0", Some("0.2.0")), None);
        // And a skip is for one release, not for updating.
        assert_eq!(
            named("0.3.0").worth_mentioning("0.1.0", Some("0.2.0")),
            Some(named("0.3.0"))
        );
    }

    /// Two endpoints name the repository Geode updates from — the signed
    /// manifest in `tauri.conf.json` and the releases API above — and a build
    /// reads exactly one of them depending on whether it has a key. Pointed at
    /// different repositories they would describe different software to the
    /// same person, so they are held together here.
    ///
    /// The API url is also checked for having nothing in it but a path: a
    /// query string is where a token or anything else about the person would
    /// have to go, and this request carries neither.
    #[test]
    fn both_modes_look_at_the_same_repository() {
        assert!(
            RELEASES_API.starts_with("https://api.github.com/repos/"),
            "the releases lookup is not the public GitHub API over TLS: {RELEASES_API}"
        );
        assert!(
            !RELEASES_API.contains('?'),
            "the releases lookup carries query parameters, and it has nothing to say \
             about who is asking: {RELEASES_API}"
        );

        let repo_of = |url: &str, after: &str| {
            let rest = url.split(after).nth(1).expect("a github url");
            rest.split('/').take(2).collect::<Vec<_>>().join("/")
        };
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let manifest = config["plugins"]["updater"]["endpoints"][0]
            .as_str()
            .expect("plugins.updater.endpoints[0]");

        assert_eq!(
            repo_of(RELEASES_API, "https://api.github.com/repos/"),
            repo_of(manifest, "https://github.com/"),
            "a build with a key reads {manifest} and a build without one reads \
             {RELEASES_API}, which are different repositories"
        );
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

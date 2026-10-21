//! Opening Geode from a `geode://join` link.
//!
//! A link reaches the app one of three ways, and all of them end in [`open`]:
//!
//! - **Starting Geode** (Linux, Windows). The OS runs a new Geode with the
//!   link as its only argument. The deep-link plugin reads it while the app is
//!   still being built and announces it then, before anything is listening, so
//!   [`start`] reads it back with `get_current` instead.
//! - **Geode already running** (Linux, Windows). The OS still runs a second
//!   Geode. Single-instance hands its arguments to the running one over D-Bus
//!   or a window message and exits, and the running one announces the link.
//! - **macOS**. No second process: the link arrives as an Apple Event, which
//!   the plugin announces the same way.
//!
//! On Linux the scheme is claimed twice. The packaged desktop entry names
//! Geode as the `geode://` handler, written from `geode.desktop.hbs` because
//! the bundler's own template drops the `%u` that passes the link along
//! (tauri#15928) — without it GIO starts Geode with no link and KDE never
//! starts it at all. And [`start`] registers a handler entry on every launch,
//! because an AppImage or a dev build has no installer to do it and the file
//! it points at can move.
//!
//! Windows and macOS claim it once each, and only from a bundle — which is
//! what `tauri.windows.conf.json` and `tauri.macos.conf.json` are for. The
//! NSIS installer writes `Software\Classes\geode` into the hive it installed
//! into; the `.app`'s `Info.plist` carries a `CFBundleURLTypes` entry the
//! bundler builds from the `plugins.deep-link.desktop` block in
//! tauri.conf.json. Neither comes free: the MSI writes no such key, and macOS
//! cannot register a scheme at runtime at all, so a release build that was
//! never bundled is one no link on either platform can reach.
//!
//! A link is hostile until [`Opened::parse`] accepts it: any web page can
//! navigate to `geode://`, and any local process can start Geode with an
//! argument or call the running copy. Only the normalised form ever reaches
//! the webview, and nothing here acts on it — the window shows the player what
//! the pack is and waits for them to say yes.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_deep_link::DeepLinkExt;

use crate::events::Events;
use crate::share::link::Opened;
use crate::state::AppState;

/// The event a link is announced on. Payload: the normalised link string.
pub const JOIN_LINK: &str = "geode://join-link";

/// The label of the only window Geode opens. tauri.conf.json gives it none,
/// and a window without one is `main`.
const MAIN_WINDOW: &str = "main";

/// Start taking links. Call from `setup`, once [`AppState`] is managed: every
/// link is parked there.
pub fn start(app: &tauri::App) {
    // A failure is a warning, never a reason not to start. It fails wherever
    // `update-desktop-database` is missing, and a packaged Geode is still the
    // handler through its desktop entry. Windows installers register the
    // scheme themselves; only a debug build there has nothing that did. macOS
    // is absent because it cannot register a scheme at runtime at all.
    #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
    if let Err(e) = app.deep_link().register_all() {
        eprintln!("geode: could not register geode:// links: {e}");
    }

    // Subscribed before the launch link is read, so a link forwarded in
    // between is handled twice rather than not at all. Twice is harmless: the
    // second only replaces the first.
    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            open(&handle, url.as_str());
        }
    });
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for url in urls {
            open(app.handle(), url.as_str());
        }
    }
}

/// Take one link from outside: validate it, park it, announce it, and bring
/// the window forward so the player sees what they opened.
pub fn open(app: &AppHandle, raw: &str) {
    let link = match Opened::parse(raw) {
        Ok(link) => link.to_url(),
        Err(e) => {
            // Not the link itself: it carries a share code, which is a secret.
            eprintln!("geode: ignored a geode:// link that is not valid: {e}");
            return;
        }
    };
    let sink = crate::desktop::sink(app.clone());
    deliver(&app.state::<AppState>().pending_join, sink.as_ref(), link);
    bring_forward(app);
}

/// Unminimise, show and focus the main window.
///
/// Also what a second plain launch of Geode does to the first, so it is shared
/// with the single-instance callback.
pub fn bring_forward(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The link waiting for the window, if any, cleared as it is taken.
///
/// Taking clears it so it opens once: a webview reload, or React running an
/// effect twice, must not put the same prompt up again.
#[tauri::command]
pub fn take_pending_join(state: State<'_, AppState>) -> Option<String> {
    take(&state.pending_join)
}

/// Park `link`, replacing any link still waiting, then announce it.
///
/// Parked first, and the lock let go before announcing, so a window that
/// answers the event by taking the pending link finds this one there.
fn deliver(slot: &Mutex<Option<String>>, events: &dyn Events, link: String) {
    if let Ok(mut pending) = slot.lock() {
        *pending = Some(link.clone());
    }
    events.emit(JOIN_LINK, serde_json::Value::String(link));
}

fn take(slot: &Mutex<Option<String>>) -> Option<String> {
    slot.lock().ok().and_then(|mut pending| pending.take())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::events::Recorder;

    const A: &str = "geode://join?addr=203.0.113.7:25590&code=7K2QF9XWMP\
        &pin=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    const B: &str = "geode://join?addr=https://pack.example.com&code=3M8RT2VQ0Z";

    /// A window that answers the event by taking the pending link at once,
    /// and records what it found.
    struct EagerWindow {
        slot: Arc<Mutex<Option<String>>>,
        found: Mutex<Vec<(String, serde_json::Value, Option<String>)>>,
    }

    impl Events for EagerWindow {
        fn emit(&self, topic: &str, payload: serde_json::Value) {
            // `try_lock`, so a slot still locked by the announcer shows up as
            // nothing found instead of hanging the test.
            let taken = self.slot.try_lock().ok().and_then(|mut pending| pending.take());
            self.found.lock().unwrap().push((topic.to_string(), payload, taken));
        }
    }

    #[test]
    fn a_link_is_waiting_by_the_time_it_is_announced() {
        let slot = Arc::new(Mutex::new(None));
        let window = EagerWindow {
            slot: slot.clone(),
            found: Mutex::new(Vec::new()),
        };

        deliver(&slot, &window, A.to_string());

        let found = window.found.lock().unwrap();
        assert_eq!(
            *found,
            vec![(JOIN_LINK.to_string(), serde_json::json!(A), Some(A.to_string()))]
        );
    }

    #[test]
    fn a_newer_link_replaces_one_still_waiting_and_is_taken_once() {
        let slot = Mutex::new(None);
        let recorder = Recorder::default();

        deliver(&slot, &recorder, A.to_string());
        deliver(&slot, &recorder, B.to_string());

        assert_eq!(take(&slot).as_deref(), Some(B));
        assert_eq!(take(&slot), None);
        // Both were announced, in order, so a window already open ends on the
        // newest.
        let payloads: Vec<_> = recorder.events().into_iter().map(|(_, p)| p).collect();
        assert_eq!(payloads, vec![serde_json::json!(A), serde_json::json!(B)]);
    }

    /// The configuration the plugins and the bundler read, checked here
    /// because a mistake in it is silent until a player clicks a link: the
    /// deep-link plugin reads its section at startup, and the bundler only at
    /// packaging.
    #[test]
    fn the_app_claims_the_scheme_and_its_desktop_entry_passes_the_link() {
        let config: tauri::Config = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json is a valid Tauri config");

        let desktop = config
            .plugins
            .0
            .get("deep-link")
            .and_then(|deep_link| deep_link.get("desktop"))
            .cloned()
            .expect("plugins.deep-link.desktop is set");
        let protocol: tauri::utils::config::DeepLinkProtocol =
            serde_json::from_value(desktop).expect("a deep-link protocol");
        assert_eq!(protocol.schemes, vec![crate::share::link::SCHEME.to_string()]);

        let labels: Vec<_> = config.app.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, vec![MAIN_WINDOW]);

        let linux = &config.bundle.linux;
        let deb = linux.deb.desktop_template.as_ref().expect("deb has a desktop template");
        let rpm = linux.rpm.desktop_template.as_ref().expect("rpm has a desktop template");
        assert_eq!(deb, rpm);

        // Resolved against src-tauri, as the bundler resolves it.
        let template = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(deb))
            .expect("the desktop template exists");
        let exec: Vec<_> = template.lines().filter(|l| l.starts_with("Exec=")).collect();
        assert_eq!(exec, vec!["Exec={{exec}} %u"]);
    }

    /// Every platform builds the one bundle that claims the scheme for it.
    ///
    /// Which bundle that is differs, and getting it wrong ships a Geode no
    /// link reaches rather than one that fails to build:
    ///
    /// - **Linux** — the `deb`, `rpm` and AppImage desktop entries, above.
    /// - **Windows** — `nsis` only. The NSIS installer writes
    ///   `Software\Classes\geode`; the MSI has no equivalent step, so adding
    ///   `msi` here would ship an installer that registers nothing.
    /// - **macOS** — `app`, because `CFBundleURLTypes` lives in its
    ///   `Info.plist` and runtime registration is impossible there, and `dmg`
    ///   to have something to hand over.
    #[test]
    fn every_platform_builds_the_bundle_that_claims_the_scheme() {
        use tauri::utils::config::{BundleTarget, BundleType};

        /// `bundle.targets` from one config file, read through the bundler's
        /// own types so a target name it would not recognise fails here.
        fn targets(file: &str, text: &str) -> BundleTarget {
            let config: serde_json::Value =
                serde_json::from_str(text).unwrap_or_else(|e| panic!("{file} is not JSON: {e}"));
            serde_json::from_value(config["bundle"]["targets"].clone())
                .unwrap_or_else(|e| panic!("{file} bundle.targets: {e}"))
        }

        assert_eq!(
            targets("tauri.conf.json", include_str!("../tauri.conf.json")),
            BundleTarget::List(vec![BundleType::AppImage, BundleType::Deb, BundleType::Rpm])
        );
        assert_eq!(
            targets(
                "tauri.windows.conf.json",
                include_str!("../tauri.windows.conf.json")
            ),
            BundleTarget::List(vec![BundleType::Nsis])
        );
        assert_eq!(
            targets(
                "tauri.macos.conf.json",
                include_str!("../tauri.macos.conf.json")
            ),
            BundleTarget::List(vec![BundleType::App, BundleType::Dmg])
        );
    }
}

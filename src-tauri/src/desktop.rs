//! The desktop app's end of [`crate::events`].
//!
//! One adapter, in the only part of the crate that is allowed to know a window
//! exists. Everything else takes an `EventSink` and does not care whether the
//! other end is a webview, a WebSocket, or nothing at all.

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::events::{EventSink, Events};

pub struct Desktop(AppHandle);

impl Events for Desktop {
    fn emit(&self, topic: &str, payload: serde_json::Value) {
        // A failed emit means the window has gone. Nothing upstream can act on
        // that, and a job in flight should finish rather than unwind.
        let _ = self.0.emit(topic, payload);
    }
}

/// Wrap an app handle so the library can report to it.
pub fn sink(app: AppHandle) -> EventSink {
    Arc::new(Desktop(app))
}

#[cfg(test)]
mod tests {
    /// The app's identifier is the installed app's identity, not a URL, and
    /// moving it is a decision rather than a rename.
    ///
    /// It keys the single-instance lock on all three platforms, the webview's
    /// profile directory, the description Windows stores for the `geode://`
    /// registration, the NSIS manufacturer key that remembers where the last
    /// install went, and on macOS the `CFBundleIdentifier` — which is the
    /// LaunchServices identity carrying `CFBundleURLTypes`, and what signing
    /// and notarization are keyed to. Its second segment is also the publisher:
    /// `bundle.publisher` is unset, so tauri takes `cody-cline` from here for
    /// the Win32 `CompanyName` on both exes, the Apps & Features publisher and
    /// the `.deb` maintainer.
    ///
    /// What it does *not* key is Geode's own data. `paths.rs` roots instances,
    /// servers and `accounts.json` at `GEODE_HOME`, else `dirs::data_dir()`
    /// joined with a literal `geode`, and nothing in this crate reads the
    /// identifier or a Tauri app-directory helper — which is the whole reason
    /// this was cheap to move when the owner's GitHub account was renamed from
    /// `littlesquirt1` to `cody-cline`. The other half of why it was cheap is
    /// timing: it moved while the only install in existence was the owner's
    /// own. After a release the same move costs every install an abandoned
    /// webview profile, a forgotten install directory, and a fresh macOS app
    /// identity that re-registers the `geode://` handler as a different
    /// application. So this test is not here to forbid the next move, only to
    /// make it a decision and not a grep.
    #[test]
    fn the_app_identifier_moves_only_on_purpose() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json"))
                .expect("tauri.conf.json is not valid JSON");
        assert_eq!(
            config["identifier"], "dev.cody-cline.geode",
            "the identifier is the installed app's identity: moving it \
             abandons each existing install's webview profile and, on macOS, \
             its registered app identity"
        );
    }

    /// `opener:allow-open-url` grants the command and nothing more. The
    /// plugin decides each URL with `self.allowed.iter().any(..)`, which is
    /// `false` for an empty list, so the bare permission refuses every URL
    /// with `ForbiddenUrl` — and the frontend calls `openUrl` without
    /// awaiting it, so nothing is logged and nothing is shown. Every external
    /// link in the app did nothing at all, and the only way to find out was
    /// to click one.
    #[test]
    fn opening_a_url_is_granted_a_scope_and_not_only_the_command() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json"))
                .expect("capabilities/default.json is not valid JSON");

        let granted = capability["permissions"]
            .as_array()
            .expect("the capability grants no permissions at all");

        let open_url = granted
            .iter()
            .find(|p| p["identifier"] == "opener:allow-open-url")
            .expect(
                "opener:allow-open-url is granted as a bare string, which carries \
                 no scope — every openUrl call is refused and no link works",
            );

        let urls: Vec<&str> = open_url["allow"]
            .as_array()
            .map(|entries| entries.iter().filter_map(|e| e["url"].as_str()).collect())
            .unwrap_or_default();

        assert!(
            !urls.is_empty(),
            "opener:allow-open-url has no url in its allow list, so the scope \
             refuses everything just as a missing scope does"
        );
        // Naming the scheme is the point: it is what keeps `file://`,
        // `javascript:` and cleartext `http://` out of a browser launch.
        assert!(
            urls.iter().all(|u| u.starts_with("https://")),
            "a scope entry allows more than https, so a crafted url could \
             reach the browser as a local file or a script: {urls:?}"
        );
    }
}

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
    /// The app's identifier is its identity on disk, not a URL, and it does
    /// not follow a GitHub rename.
    ///
    /// It keys the single-instance lock, the `geode://` scheme registration,
    /// and the data directory on all three platforms. Changing it does not
    /// migrate anything: every existing install becomes a different
    /// application that cannot see its own instances, servers or accounts,
    /// and the old ones are left behind with nothing pointing at them. The
    /// owner's GitHub account was renamed from `littlesquirt1` to
    /// `cody-cline` and every URL moved with it; this deliberately did not,
    /// and this test is here so the next person to grep for the old name does
    /// not finish the job.
    #[test]
    fn the_app_identifier_does_not_follow_a_rename() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json"))
                .expect("tauri.conf.json is not valid JSON");
        assert_eq!(
            config["identifier"], "dev.littlesquirt1.geode",
            "the identifier is the installed app's identity: changing it \
             orphans every existing data directory"
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

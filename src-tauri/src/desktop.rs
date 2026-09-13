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

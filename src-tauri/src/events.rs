//! Where the library sends things that happen.
//!
//! Geode's logic — installing, launching, running servers — has nothing to do
//! with windows, but until now it emitted directly to a Tauri `AppHandle`,
//! which meant every part of it needed a desktop app to exist. That is the one
//! thing standing between this code and running headless in a container, where
//! the same events go to a WebSocket instead.
//!
//! So the library talks to this trait. The desktop app supplies an
//! implementation that forwards to the webview; a daemon supplies one that
//! fans out to whoever is connected; a test supplies none at all and the
//! events go nowhere, which is what [`crate::net::Progress::silent`] has always
//! done.

use std::sync::Arc;

/// Somewhere for an event to go.
///
/// Deliberately one method taking already-serialised JSON. The alternative — a
/// method per event kind — would put every event type in this file and mean
/// touching it to add one, which is how an abstraction becomes a bottleneck.
pub trait Events: Send + Sync + 'static {
    /// `topic` is the event name, e.g. `geode://progress`.
    fn emit(&self, topic: &str, payload: serde_json::Value);
}

pub type EventSink = Arc<dyn Events>;

/// Wrap anything serialisable and send it.
///
/// A failure to serialise is dropped rather than propagated: an event is a
/// notification, and nothing upstream can do anything useful about one that
/// could not be encoded.
pub fn emit<T: serde::Serialize>(sink: &Option<EventSink>, topic: &str, payload: &T) {
    let Some(sink) = sink else {
        return;
    };
    if let Ok(value) = serde_json::to_value(payload) {
        sink.emit(topic, value);
    }
}

/// Collects events in memory. For tests, and for seeing what a run produced.
#[derive(Default)]
pub struct Recorder {
    seen: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
}

impl Recorder {
    pub fn events(&self) -> Vec<(String, serde_json::Value)> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn topics(&self) -> Vec<String> {
        self.events().into_iter().map(|(t, _)| t).collect()
    }
}

impl Events for Recorder {
    fn emit(&self, topic: &str, payload: serde_json::Value) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push((topic.to_string(), payload));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_reach_a_sink_and_a_missing_sink_is_not_an_error() {
        let recorder = Arc::new(Recorder::default());
        let sink: Option<EventSink> = Some(recorder.clone());
        emit(&sink, "geode://progress", &serde_json::json!({ "id": "x" }));
        assert_eq!(recorder.topics(), vec!["geode://progress"]);

        // The headless case: no sink, no panic, nothing recorded.
        emit(&None, "geode://progress", &serde_json::json!({ "id": "y" }));
        assert_eq!(recorder.events().len(), 1);
    }
}

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::launch::RunningGame;
use crate::meta::ManifestCache;
use crate::paths::Paths;

/// Tracks which instances currently have a game process attached.
///
/// A synchronous mutex is deliberate: every operation is a map lookup, and the
/// process-exit task must be able to deregister without an async context.
#[derive(Clone, Default)]
pub struct GameRegistry {
    inner: Arc<Mutex<HashMap<String, RunningGame>>>,
}

impl GameRegistry {
    pub fn insert(&self, instance_id: String, game: RunningGame) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(instance_id, game);
        }
    }

    pub fn remove(&self, instance_id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(instance_id);
        }
    }

    pub fn is_running(&self, instance_id: &str) -> bool {
        self.inner
            .lock()
            .map(|m| m.contains_key(instance_id))
            .unwrap_or(false)
    }

    pub fn running_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn stop(&self, instance_id: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|m| m.get(instance_id).map(|g| g.request_stop()))
            .is_some()
    }

    pub fn stop_all(&self) {
        if let Ok(map) = self.inner.lock() {
            for game in map.values() {
                game.request_stop();
            }
        }
    }
}

pub struct AppState {
    pub paths: Paths,
    pub http: reqwest::Client,
    pub manifests: ManifestCache,
    pub connector: crate::connector::SupportCache,
    pub jars: crate::instance::jar::MetaCache,
    /// Per-project file coverage, so re-resolving a plan after one edit does
    /// not re-read every mod in it.
    pub plan_support: crate::plan::resolve::SupportCache,
    pub games: GameRegistry,
    /// Cached so every command does not re-read config.json from disk.
    config: tokio::sync::RwLock<Config>,
    /// Cancellation flags for in-flight installs, keyed by progress id.
    cancels: Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
}

impl AppState {
    pub fn new(paths: Paths, config: Config) -> Self {
        Self {
            paths,
            http: crate::net::client(),
            manifests: ManifestCache::new(),
            connector: crate::connector::SupportCache::new(),
            jars: crate::instance::jar::MetaCache::new(),
            plan_support: crate::plan::resolve::SupportCache::new(),
            games: GameRegistry::default(),
            config: tokio::sync::RwLock::new(config),
            cancels: Mutex::new(HashMap::new()),
        }
    }

    pub async fn config(&self) -> Config {
        self.config.read().await.clone()
    }

    pub async fn set_config(&self, config: Config) {
        *self.config.write().await = config;
    }

    pub fn register_cancel(&self, id: &str, flag: Arc<std::sync::atomic::AtomicBool>) {
        if let Ok(mut map) = self.cancels.lock() {
            map.insert(id.to_string(), flag);
        }
    }

    pub fn cancel(&self, id: &str) -> bool {
        if let Ok(map) = self.cancels.lock() {
            if let Some(flag) = map.get(id) {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    pub fn clear_cancel(&self, id: &str) {
        if let Ok(mut map) = self.cancels.lock() {
            map.remove(id);
        }
    }
}

use std::collections::{HashMap, HashSet};
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
    /// Which CurseForge projects have a Modrinth twin, so a plan that keeps a
    /// genuinely CurseForge-only mod does not re-ask on every save.
    pub plan_twins: crate::plan::prefer::TwinCache,
    pub games: GameRegistry,
    /// Which servers have a process attached, and the pipe to each console.
    pub servers: crate::server::run::ServerRegistry,
    /// Cached so every command does not re-read config.json from disk.
    config: tokio::sync::RwLock<Config>,
    /// Cancellation flags for in-flight installs, keyed by progress id.
    cancels: Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
    /// Instances whose launch has begun and has not yet spawned a game.
    ///
    /// The game registry only knows an instance once there is a process, and a
    /// launch spends the time before that updating from the server and
    /// downloading — rewriting the very files a second launch would read.
    launching: Mutex<HashSet<String>>,
    /// The latest `geode://join` link, validated and normalised, until the
    /// window takes it.
    ///
    /// Every link is parked here, and the `geode://join-link` event is only a
    /// nudge to come and take it. A link that starts the app arrives before
    /// any webview exists to hear an event, and treating the slot as the one
    /// source of truth means the window handles both cases the same way —
    /// and a reload never replays a link that was already taken. A newer link
    /// replaces an older one that was never taken.
    pub pending_join: Mutex<Option<String>>,
    /// The pack server and router mapping, running while anything is shared.
    pub share: crate::share::service::ShareService,
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
            plan_twins: crate::plan::prefer::TwinCache::new(),
            games: GameRegistry::default(),
            servers: crate::server::run::ServerRegistry::default(),
            config: tokio::sync::RwLock::new(config),
            cancels: Mutex::new(HashMap::new()),
            launching: Mutex::new(HashSet::new()),
            pending_join: Mutex::new(None),
            share: Default::default(),
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

    /// Take the launch of `id`, or `None` when one is already under way.
    ///
    /// The claim lasts as long as the returned guard, which the launch holds
    /// until the game is spawned or it gives up.
    pub fn claim_launch(&self, id: &str) -> Option<LaunchClaim<'_>> {
        let mut launching = self.launching.lock().ok()?;
        if !launching.insert(id.to_string()) {
            return None;
        }
        Some(LaunchClaim {
            state: self,
            id: id.to_string(),
        })
    }

    /// Whether a launch of `id` is preparing right now. What it is doing —
    /// updating from the server, renaming mods — is not to be changed under it.
    pub fn is_launching(&self, id: &str) -> bool {
        self.launching
            .lock()
            .map(|launching| launching.contains(id))
            .unwrap_or(false)
    }
}

/// One instance's launch, held from [`AppState::claim_launch`] until dropped.
///
/// A guard rather than a pair of calls, so a launch that returns early — an
/// account problem, a failed update, a panic — still frees the instance.
pub struct LaunchClaim<'a> {
    state: &'a AppState,
    id: String,
}

impl Drop for LaunchClaim<'_> {
    fn drop(&mut self) {
        if let Ok(mut launching) = self.state.launching.lock() {
            launching.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two launches of one instance would update it from its server twice at
    /// once, each over the other's files. The second is turned away until the
    /// first has the game running, or has given up.
    #[test]
    fn a_second_launch_of_the_same_instance_waits_for_the_first() {
        let state = AppState::new(
            crate::paths::Paths {
                root: std::env::temp_dir().join("geode-claim-launch"),
            },
            crate::config::Config::default(),
        );

        let first = state
            .claim_launch("survival")
            .expect("nothing holds it yet");
        assert!(state.is_launching("survival"));
        assert!(state.claim_launch("survival").is_none());

        // Another instance is nobody else's business.
        let other = state
            .claim_launch("creative")
            .expect("a different instance");
        assert!(state.is_launching("creative"));
        drop(other);
        assert!(!state.is_launching("creative"));

        drop(first);
        assert!(!state.is_launching("survival"));
        assert!(state.claim_launch("survival").is_some());
    }
}

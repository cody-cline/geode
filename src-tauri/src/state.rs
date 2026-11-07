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
    /// The offline-play offer a launch's own sign-in failure left behind,
    /// until the window takes it.
    ///
    /// Parked here for the reason `pending_join` below is: the one place that
    /// knows this is worth offering is the step that failed, and the window
    /// comes and takes it afterwards. It used to be asked of the account's
    /// state instead, which answers "this account's token is stale" and not
    /// "this is what went wrong just now" — so pressing Play on an instance
    /// that was already running, which fails before a sign-in is attempted at
    /// all, put a dialog about renewing a sign-in on screen beside an error
    /// about something else entirely.
    offline_offer: Mutex<Option<OfflineOffer>>,
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
    /// Shared so a listener that could not start can keep trying on a task of
    /// its own.
    pub share: std::sync::Arc<crate::share::service::ShareService>,
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
            offline_offer: Mutex::new(None),
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

    /// The CurseForge key to search and download with, if there is one. Owned,
    /// because every caller then lends it out as `as_deref()` for the length of
    /// a command.
    pub async fn cf_key(&self) -> Option<String> {
        self.config().await.effective_curseforge_key()
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

    /// How many launches are preparing, with no process to show for it yet.
    ///
    /// For anything that has to know whether work is in flight without caring
    /// whose: a restart during one of these leaves an instance half-rewritten,
    /// and [`crate::update::restart_would_interrupt`] is the caller.
    pub fn launching_count(&self) -> usize {
        self.launching
            .lock()
            .map(|launching| launching.len())
            .unwrap_or(0)
    }

    /// How many installs are in flight, counted by the cancellation flags they
    /// registered. The same question as [`Self::launching_count`], for the
    /// other kind of half-written instance a restart can leave behind.
    pub fn installs_in_flight(&self) -> usize {
        self.cancels.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// Everything that would be interrupted if this process went away now,
    /// named the way a person reads it: `["2 servers", "Minecraft"]`.
    ///
    /// Here rather than in either caller because there are two —
    /// [`crate::update::restart_would_interrupt`] refusing an update, and the
    /// warning before Geode exits — and two counters would drift within a
    /// month about what "running" means. State owns both registries and both
    /// counters, and is in the headless build as well.
    ///
    /// Servers lead, because a server may have other people on it and a region
    /// file killed mid-save is somebody else's loss, where a game is the
    /// owner's own session on their own machine. Both are named: this is read
    /// out to somebody deciding whether to go ahead, and "something is
    /// running" is not a thing anybody can decide on.
    pub fn what_is_running(&self) -> Vec<String> {
        let mut what = Vec::new();
        match self.servers.running_ids().len() {
            0 => {}
            1 => what.push("a server".to_string()),
            n => what.push(format!("{n} servers")),
        }
        match self.games.running_ids().len() {
            0 => {}
            1 => what.push("Minecraft".to_string()),
            n => what.push(format!("{n} copies of Minecraft")),
        }
        // A start that has not spawned yet is in neither registry, and it is
        // the worst thing to walk into: a Spigot server is five to fifteen
        // minutes of BuildTools, a Paper one a 50 MB download, and closing the
        // window used to warn about nothing and then kill it. See
        // [`crate::server::run::ServerRegistry::starting_count`].
        match self.servers.starting_count() {
            0 => {}
            1 => what.push("a server that is still starting".to_string()),
            n => what.push(format!("{n} servers that are still starting")),
        }
        match self.launching_count() {
            0 => {}
            1 => what.push("a launch that is still preparing".to_string()),
            n => what.push(format!("{n} launches that are still preparing")),
        }
        match self.installs_in_flight() {
            0 => {}
            1 => what.push("an install that is still running".to_string()),
            n => what.push(format!("{n} installs that are still running")),
        }
        what
    }
    /// Leave an offline-play offer for the window: this instance's launch could
    /// not renew `username`'s sign-in, and playing offline is the way past it.
    pub fn offer_offline_play(&self, instance_id: String, username: String) {
        if let Ok(mut slot) = self.offline_offer.lock() {
            *slot = Some(OfflineOffer {
                instance_id,
                username,
            });
        }
    }

    /// Forget any offer left by an earlier launch.
    ///
    /// Called by a launch before it can refuse for any reason, so an offer is
    /// only ever answered to the failure that left it.
    pub fn clear_offline_offer(&self) {
        if let Ok(mut slot) = self.offline_offer.lock() {
            *slot = None;
        }
    }

    /// Take the offer left for `instance_id`, if the offer is for that
    /// instance.
    ///
    /// Taken rather than read, because it answers one failure once: a dialog
    /// that could be raised twice from one sign-in problem is a dialog that
    /// outlives the thing it is about.
    ///
    /// Compared before it is taken, because there is one slot for every
    /// instance. Taking first meant a question about any other instance threw
    /// the offer away: the dialog the failed launch left it for never came up,
    /// because something else on screen had asked first.
    pub fn take_offline_play_offer(&self, instance_id: &str) -> Option<String> {
        let mut slot = self.offline_offer.lock().ok()?;
        if !slot
            .as_ref()
            .is_some_and(|offer| offer.instance_id == instance_id)
        {
            return None;
        }
        slot.take().map(|offer| offer.username)
    }
}

/// A launch that could not renew its account's sign-in, and the name that
/// account would play under offline.
struct OfflineOffer {
    /// The instance whose launch failed. The window asks about the instance it
    /// just failed to start, so an offer from another one is nothing to do
    /// with the error on screen.
    instance_id: String,
    username: String,
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

    /// A start that has not spawned yet was invisible here, and that is the
    /// one nobody can get back: a Spigot build is fifteen minutes of
    /// BuildTools, and closing the window said there was nothing to interrupt
    /// and then killed it. Both callers read this one list, so an update is
    /// refused during a build for the same reason.
    #[test]
    fn a_server_part_way_through_starting_is_something_to_interrupt() {
        let scratch = crate::paths::Scratch::new("state-what-is-running");
        let state = AppState::new(scratch.paths.clone(), crate::config::Config::default());
        assert!(state.what_is_running().is_empty());

        let claim = state.servers.claim_start("survival").expect("nothing holds it");
        assert_eq!(
            state.what_is_running(),
            vec!["a server that is still starting".to_string()]
        );

        // And once there is a process it is counted once, not twice: the
        // claim outlives the registry insert by a few statements.
        let (_typed, _console) = state
            .servers
            .fake_start("survival", crate::server::ServerKind::Paper);
        assert_eq!(state.what_is_running(), vec!["a server".to_string()]);

        drop(claim);
        assert_eq!(state.what_is_running(), vec!["a server".to_string()]);
    }

    /// The offer belongs to the failure that left it: the launch of one
    /// instance, once, and only until something else is tried.
    ///
    /// A dialog headed "Geode could not renew Notch's Microsoft sign-in" shown
    /// next to "that instance is already running" was two contradictory
    /// reasons on screen at once, which is what asking the account's state
    /// instead of the failure used to do.
    #[test]
    fn an_offline_play_offer_answers_one_launch_and_no_other() {
        let state = AppState::new(
            crate::paths::Paths {
                root: std::env::temp_dir().join("geode-offline-offer"),
            },
            crate::config::Config::default(),
        );

        // Nothing has failed, so there is nothing to offer.
        assert_eq!(state.take_offline_play_offer("survival"), None);

        state.offer_offline_play("survival".into(), "Notch".into());
        // Not the instance the dialog would be about.
        assert_eq!(state.take_offline_play_offer("creative"), None);
        // And asking about one is a question, not an answer: there is one slot
        // for every instance, and taking from it before reading whose it was
        // meant a row nobody failed to launch could destroy a live offer.
        assert_eq!(
            state.take_offline_play_offer("survival"),
            Some("Notch".to_string()),
            "an offer was destroyed by a question about another instance",
        );

        // Answered once, and not again: the error it belongs to is on screen
        // once.
        state.offer_offline_play("survival".into(), "Notch".into());
        assert_eq!(
            state.take_offline_play_offer("survival"),
            Some("Notch".to_string())
        );
        assert_eq!(state.take_offline_play_offer("survival"), None);

        // And a launch that gets as far as refusing for another reason clears
        // it first, so the refusal is answered on its own terms.
        state.offer_offline_play("survival".into(), "Notch".into());
        state.clear_offline_offer();
        assert_eq!(state.take_offline_play_offer("survival"), None);
    }
}

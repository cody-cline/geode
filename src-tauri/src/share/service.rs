//! Sharing while it runs: the pack server and its router mapping, kept in step
//! with what is shared and how.
//!
//! Nothing listens while nothing is shared. Sharing a first server starts the
//! pack server and, unless the admin chose to forward the port themselves,
//! asks the router to open it; unsharing the last stops both and gives the
//! port back. Every change to the running state is announced, so the UI never
//! has to poll to find out whether players can reach the admin.
//!
//! A listener that cannot start is not the end of it either. Something else
//! holding the port is usually letting go of it — most often a Geode that has
//! just quit and is still asking the router to close the port — so binding is
//! tried again on its own until it works, and only once that has gone on long
//! enough to mean something is really in the way is the admin told.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{watch, Mutex};

use crate::config::{PortMapping, ShareConfig};
use crate::error::{Error, Result};
use crate::events::{self, EventSink};
use crate::paths::Paths;
use crate::server::share::ServerShare;

use super::host::Host;
use super::identity::Identity;
use super::link::{Address, Link, Pin};
use super::portmap::{self, PortMapper, Reachability, Rouse};

/// Sent with a [`ShareStatus`] whenever the running state changes.
pub const STATUS_EVENT: &str = "geode://share-status";

/// Sent with a [`ListenNotice`] once when retrying has gone on long enough
/// that the admin has to do something, and once again if a later retry gets
/// the port after all. Twice at most per outage, and never the second without
/// the first: an outage nobody was told of has no coming back to announce,
/// and "listening again" against a problem the admin never saw reads as a
/// fault they missed. The Share tab's own line follows [`STATUS_EVENT`] and
/// says the rest.
pub const LISTEN_EVENT: &str = "geode://share-listen";

/// How often manual setup asks the router for its public address again, and
/// how soon it tries again after the router did not answer.
const DETECT_EVERY: Duration = Duration::from_secs(30 * 60);
const DETECT_RETRY: Duration = Duration::from_secs(60);

/// How long [`ShareService::recheck_router`] waits for the answer before
/// giving the admin back their page.
///
/// Long enough for the slowest honest case: two searches with their own
/// windows, then a request or two, each under its own timeout. A router that
/// is not there uses all of it.
const ASK_WITHIN: Duration = Duration::from_secs(45);

/// How long the listener may go on failing before the admin is told and the
/// screens stop saying Geode is still trying.
///
/// A minute. The likeliest reason the port is taken is a Geode that has just
/// quit and is still asking the router to close it, which takes ten to fifteen
/// seconds — well inside the retries at four, twelve and twenty-eight seconds
/// in, and the minute mark is exactly where the fifth attempt lands. Something
/// still holding the port then is another program or a firewall, and no amount
/// of waiting moves either.
const ESCALATE_AFTER: Duration = Duration::from_secs(60);

/// How soon a listener that could not start is tried again, and how long it
/// may go on failing before the admin is told. One set for the app; tests
/// shrink them so a retry takes milliseconds rather than a minute.
#[derive(Clone, Copy)]
struct Timings {
    /// The wait before the first retry, doubled by each further failure in a
    /// row up to `retry_limit`.
    retry: Duration,
    retry_limit: Duration,
    /// [`ESCALATE_AFTER`].
    escalate_after: Duration,
    /// How often the watcher looks again at this machine's own address on its
    /// network. Nothing announces that one: a new DHCP lease moves it while
    /// the mapping, the public address, the port and the certificate stay
    /// exactly as they were, and a Share tab left open would go on showing an
    /// address nobody on this network can reach.
    relook: Duration,
}

/// The same shape [`portmap`] retries a router with, and for the same reason:
/// whatever is in the way usually goes within seconds, and once it has not,
/// asking more often than once a minute gains nothing.
const TIMINGS: Timings = Timings {
    retry: Duration::from_secs(4),
    retry_limit: Duration::from_secs(60),
    escalate_after: ESCALATE_AFTER,
    relook: Duration::from_secs(60),
};

impl Default for Timings {
    fn default() -> Self {
        TIMINGS
    }
}

/// Everything the UI shows about whether players can reach this machine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareStatus {
    /// Which router setup the admin chose. The mapping below cannot say this
    /// on its own: one that never started because nothing listens is `off` in
    /// either mode, and telling an admin who chose automatic that setup is
    /// manual sends them to fix the wrong thing.
    pub mode: PortMapping,
    /// With manual setup, the public address the router gave when asked.
    pub detected: Option<Detected>,
    /// With manual setup, whether the router is still being asked for the
    /// first time — so no `detected` address means nothing yet, not that the
    /// router would not say.
    pub asking: bool,
    pub listening: bool,
    pub port: u16,
    pub listen_error: Option<String>,
    /// While something is shared and nothing listens: what trying again has
    /// come to so far. `None` when nothing is being tried, so a listen error
    /// with no retry beside it is one nobody is working on.
    pub retry: Option<Retrying>,
    pub mapping: portmap::Status,
    /// The address links are made with, when one is known.
    pub address: Option<String>,
    /// This machine's own address on the network it is on, with the pack
    /// port, for the players who are on that network too — and for the admin
    /// testing their own link, whose router may well refuse to send a
    /// connection to the public address back in to itself. Beside
    /// [`ShareStatus::address`], never instead of it.
    pub local_address: Option<String>,
    /// The certificate fingerprint players pin, as hex.
    pub fingerprint: String,
}

/// A listener that could not start, being tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Retrying {
    /// Attempts that have failed so far, the first one included.
    pub attempts: u32,
    /// Roughly how long until the next attempt.
    pub next_in_seconds: u64,
    /// Whether it has been failing for [`ESCALATE_AFTER`] — long enough that
    /// this is not something on its way out of the port, and the admin has
    /// been told so.
    pub needs_you: bool,
}

/// Sent to the admin when the listener has been down too long to keep quiet
/// about, and again if it comes back on its own.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListenNotice {
    /// Whether this is the recovery rather than the failure.
    listening: bool,
    port: u16,
    /// Attempts that had failed when this was sent.
    attempts: u32,
    /// Why it could not bind; `None` on the recovery.
    error: Option<String>,
}

/// A public address the router reported, without being asked to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    pub ip: IpAddr,
    pub reachability: Reachability,
}

/// What asking the router for its public address has come to so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Detection {
    /// Whether an ask has finished, answered or not.
    asked: bool,
    found: Option<Detected>,
    /// Where the router answered from. Manual setup holds no mapping, so this
    /// ask is the only thing that ever finds the router, and
    /// [`portmap::local_address`] has nothing else to measure this machine's
    /// own address against.
    gateway: Option<IpAddr>,
}

impl Detection {
    /// After another ask: where the router answered from, and what it said
    /// the public address is.
    ///
    /// Both are kept when this ask went unanswered: one request lost on busy
    /// Wi-Fi does not mean the address changed, and forgetting it would take a
    /// working link away. A router that has stopped answering is also still
    /// where this machine's own address faces, which is how the mapping treats
    /// it too.
    fn after(self, read: Option<(IpAddr, Detected)>) -> Self {
        let (gateway, found) = match read {
            Some((gateway, found)) => (Some(gateway), Some(found)),
            None => (None, None),
        };
        Self {
            asked: true,
            found: found.or(self.found),
            gateway: gateway.or(self.gateway),
        }
    }
}

#[derive(Default)]
pub struct ShareService {
    /// Whole reconciles, one at a time — held while the settings are read, so
    /// the last reconcile to run is always the one that saw the latest ones.
    /// A retry attempt takes it too, and so is never beside one.
    serial: Mutex<()>,
    inner: Mutex<Inner>,
    /// Never contact the router, not even to ask. Only tests set this: they run
    /// on a real network, and asking a real router anything is not theirs to do.
    quiet: bool,
    timings: Timings,
    local: Local,
}

/// Where the address for players on this same network comes from.
#[derive(Clone, Default)]
enum Local {
    /// The OS, asked which of this machine's addresses faces the router.
    #[default]
    Os,
    /// Whatever a test says this machine's address is, the router it was asked
    /// toward and all: the real one is whatever the routing table of the
    /// machine a test happens to run on says, which is nothing a test can be
    /// written against — `portmap` covers the question itself. Held behind a
    /// lock because a test moves it partway, which is the one thing about this
    /// address that nothing announces.
    #[cfg(test)]
    Told(std::sync::Arc<std::sync::Mutex<Option<IpAddr>>>),
}

impl Local {
    fn address(&self, gateway: Option<IpAddr>) -> Option<IpAddr> {
        match self {
            Local::Os => portmap::local_address(gateway),
            #[cfg(test)]
            Local::Told(told) => *told.lock().unwrap(),
        }
    }
}

/// Where the router is, however this setup found it.
///
/// Automatic setup has a mapping, which publishes it. Manual setup has no
/// mapper at all — it is the setup an admin lands in precisely because their
/// router would not open the port — so the ask that reads the public address
/// carries where it answered from, and that is the same router. Without
/// either, nothing: [`portmap::local_address`] says why an address guessed
/// toward the internet is worse than none.
fn gateway_of(mapped: Option<IpAddr>, detection: Option<Detection>) -> Option<IpAddr> {
    mapped.or_else(|| detection.and_then(|detection| detection.gateway))
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    /// Why the pack server could not start, until it next can.
    listen_error: Option<String>,
    /// Binding the port again until it works, while something is shared.
    retry: Option<Retry>,
    /// Bumped by every reconcile. A retry that comes back from its wait to
    /// find this changed was superseded while it waited and stops there, so
    /// two of them can never both be trying to bind.
    generation: u64,
    /// Loaded once. The identity never changes while the app runs.
    pin: Option<Pin>,
}

/// What retrying the listener has come to, and the task doing it.
struct Retry {
    /// Aborted when a reconcile supersedes this retry, or the app exits.
    task: tokio::task::JoinHandle<()>,
    /// The port being bound. A reconcile that still wants this one is the
    /// same outage going on; one that wants another port is a new problem,
    /// and everything below starts over.
    port: u16,
    /// Attempts that have failed so far, the first one included.
    attempts: u32,
    /// When the next attempt is due.
    next: Instant,
    /// When the listener first failed, which [`ESCALATE_AFTER`] runs from.
    since: Instant,
    /// The admin has been told this needs them. Once is enough: they cannot
    /// be told it a second time any more usefully.
    alerted: bool,
}

struct Running {
    host: Host,
    port: u16,
    mapper: Option<PortMapper>,
    /// With manual setup: what the router says the public address is.
    detected: Option<watch::Receiver<Detection>>,
    /// With manual setup: bringing that ask round now. Automatic setup has no
    /// such ask — its mapper carries its own.
    asking: Option<Rouse>,
    /// Re-announces the status as the mapping or the detected address changes.
    watcher: Option<tokio::task::JoinHandle<()>>,
}

impl Running {
    /// Whichever loop is talking to the router in this setup: the mapping's
    /// renewal, or the ask that reads the address when the admin forwards the
    /// port themselves. `None` while neither is running.
    fn rouse(&self) -> Option<Rouse> {
        self.mapper
            .as_ref()
            .map(PortMapper::rouse)
            .or_else(|| self.asking.clone())
    }
}

/// Whether anything is shared, as far as can be told.
enum Wanted {
    Share,
    Stop,
    /// A share file exists that could not be read. Stopping would cut players
    /// off from a server that is still shared; starting would guess.
    Unknown,
}

/// What bringing the running state in line left behind, and so what there is
/// to do next. Four answers rather than "is there something to retry": a
/// retry that ends because nothing is shared any more has not recovered, and
/// telling an admin it is listening again then would be a lie.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// A listener is up on the port the settings name.
    Listening,
    /// Something is shared and nothing is listening: what there is to try
    /// again.
    Down,
    /// Nothing is shared, so nothing should be listening.
    Idle,
    /// What is shared could not be read, so nothing was tried either way. A
    /// retry already running carries on — something was shared a moment ago —
    /// but none is ever started from here, which would be guessing on exactly
    /// the reading [`Wanted::Unknown`] exists to refuse.
    Unknown,
}

impl ShareService {
    /// Bring the running state in line with what is shared and the settings,
    /// reading the settings only once it is this call's turn.
    pub async fn reconcile(
        self: &Arc<Self>,
        state: &crate::state::AppState,
        sink: Option<EventSink>,
    ) {
        let _turn = self.serial.lock().await;
        let config = state.config().await.share;
        self.reconcile_taking_turn(&state.paths, &config, sink)
            .await;
    }

    /// The same, with the settings given. What tests call; the app goes through
    /// [`ShareService::reconcile`] so concurrent reconciles cannot apply stale
    /// settings last.
    pub async fn reconcile_with(
        self: &Arc<Self>,
        paths: &Paths,
        config: &ShareConfig,
        sink: Option<EventSink>,
    ) {
        let _turn = self.serial.lock().await;
        self.reconcile_taking_turn(paths, config, sink).await;
    }

    /// A reconcile with its turn already taken.
    ///
    /// Whatever was being retried is dropped first: these settings have the
    /// last word on what should be running, and the port the retry was binding
    /// may not even be the one wanted now.
    async fn reconcile_taking_turn(
        self: &Arc<Self>,
        paths: &Paths,
        config: &ShareConfig,
        sink: Option<EventSink>,
    ) {
        let superseded = self.supersede().await;
        if self.apply(paths, config, &sink).await == Outcome::Down {
            self.retry_from_now(paths, config, &sink, superseded).await;
        }
        self.announce(paths, config, &sink).await;
    }

    /// Start binding the port again until it works. The attempt that just
    /// failed is the first one counted.
    ///
    /// Unless `superseded` was already failing on this same port, in which
    /// case its count, its clock and whether the admin has been told go on
    /// from where they were. A reconcile is not news about the port: saving
    /// an unrelated setting, or sharing a second server, must not make ten
    /// minutes of failing read as a first try that usually clears in seconds,
    /// nor buy a second "this needs you" a minute later.
    async fn retry_from_now(
        self: &Arc<Self>,
        paths: &Paths,
        config: &ShareConfig,
        sink: &Option<EventSink>,
        superseded: Option<Retry>,
    ) {
        let carried = superseded.filter(|retry| retry.port == config.port);
        let attempts = carried.as_ref().map_or(0, |retry| retry.attempts) + 1;
        let wait = self.backoff(attempts);
        let now = Instant::now();
        let mut inner = self.inner.lock().await;
        let generation = inner.generation;
        // Spawned before it is recorded, which is safe because the loop waits
        // before it touches anything.
        let task = tokio::spawn(Arc::clone(self).retrying(
            paths.clone(),
            config.clone(),
            sink.clone(),
            generation,
            wait,
        ));
        inner.retry = Some(Retry {
            task,
            port: config.port,
            attempts,
            next: now + wait,
            since: carried.as_ref().map_or(now, |retry| retry.since),
            alerted: carried.is_some_and(|retry| retry.alerted),
        });
    }

    /// How long to wait after `attempts` failures in a row: the first wait,
    /// doubled by each further failure, up to the ceiling.
    fn backoff(&self, attempts: u32) -> Duration {
        let doubling = 1u32
            .checked_shl(attempts.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.timings
            .retry
            .saturating_mul(doubling)
            .min(self.timings.retry_limit)
    }

    /// Bind the port again until it works, nothing is shared any more, or a
    /// reconcile supersedes this.
    ///
    /// Each attempt takes the turn a reconcile takes and does the same work,
    /// so one that succeeds leaves the router mapping or detection, the
    /// announce watcher and the status exactly as a reconcile would.
    async fn retrying(
        self: Arc<Self>,
        paths: Paths,
        config: ShareConfig,
        sink: Option<EventSink>,
        generation: u64,
        mut wait: Duration,
    ) {
        loop {
            tokio::time::sleep(wait).await;
            let _turn = self.serial.lock().await;
            // Superseded while this waited for its turn: a reconcile has
            // already decided what should be running, and it was not this.
            if self.inner.lock().await.generation != generation {
                return;
            }

            let outcome = self.apply(&paths, &config, &sink).await;
            let mut notice = None;
            let mut last = false;
            {
                let mut inner = self.inner.lock().await;
                let Some(retry) = inner.retry.as_mut() else {
                    return;
                };
                match outcome {
                    Outcome::Down => {
                        retry.attempts += 1;
                        wait = self.backoff(retry.attempts);
                        retry.next = Instant::now() + wait;
                        // Told once, when waiting has plainly stopped being the
                        // answer — not once an attempt, which would be a toast a
                        // minute for as long as sharing is on.
                        let crossed =
                            !retry.alerted && retry.since.elapsed() >= self.timings.escalate_after;
                        retry.alerted |= crossed;
                        let attempts = retry.attempts;
                        if crossed {
                            notice = Some(ListenNotice {
                                listening: false,
                                port: config.port,
                                attempts,
                                error: inner.listen_error.clone(),
                            });
                        }
                    }
                    // Nothing was tried, because what is shared could not be
                    // read. No attempt to count against the port, and no
                    // reason to give up on a port something was shared on a
                    // moment ago: look again after the same wait.
                    Outcome::Unknown => retry.next = Instant::now() + wait,
                    Outcome::Listening | Outcome::Idle => {
                        // Only a recovery the admin can place: one from a
                        // failure they were told about. A port taken for four
                        // seconds and given back was never their business, and
                        // an end because nothing is shared any more is not
                        // sharing coming back at all.
                        let recovered = outcome == Outcome::Listening && retry.alerted;
                        let attempts = retry.attempts;
                        inner.retry = None;
                        if recovered {
                            notice = Some(ListenNotice {
                                listening: true,
                                port: config.port,
                                attempts,
                                error: None,
                            });
                        }
                        last = true;
                    }
                }
            }

            // The status first, so the screens are already right when the
            // notice arrives to point at them.
            self.announce(&paths, &config, &sink).await;
            if let Some(notice) = notice {
                events::emit(&sink, LISTEN_EVENT, &notice);
            }
            if last {
                return;
            }
        }
    }

    /// Drop whatever was being retried, and make any attempt already on its
    /// way back from a wait give up its turn. What was dropped is handed back,
    /// so a reconcile that still wants the same port can go on from where it
    /// had got to rather than starting the outage over.
    async fn supersede(&self) -> Option<Retry> {
        let mut inner = self.inner.lock().await;
        inner.generation += 1;
        let retry = inner.retry.take();
        if let Some(retry) = &retry {
            retry.task.abort();
        }
        retry
    }

    async fn announce(&self, paths: &Paths, config: &ShareConfig, sink: &Option<EventSink>) {
        let status = self.status(paths, config).await;
        events::emit(sink, STATUS_EVENT, &status);
    }

    /// Bring the running state in line with what is shared and the settings,
    /// and say what that came to.
    async fn apply(
        &self,
        paths: &Paths,
        config: &ShareConfig,
        sink: &Option<EventSink>,
    ) -> Outcome {
        let wanted = match crate::server::share::list(paths).await {
            Ok(shared) if !shared.servers.is_empty() => Wanted::Share,
            Ok(shared) if shared.unreadable.is_empty() => Wanted::Stop,
            _ => Wanted::Unknown,
        };
        let pin = self.pin(paths).await;
        let mut inner = self.inner.lock().await;

        let stale = inner
            .running
            .as_ref()
            .is_some_and(|running| matches!(wanted, Wanted::Stop) || running.port != config.port);
        if stale {
            if let Some(running) = inner.running.take() {
                stop(running).await;
            }
        }

        match wanted {
            Wanted::Stop => inner.listen_error = None,
            Wanted::Share if inner.running.is_none() => {
                match Host::start(paths.clone(), config.port, sink.clone()).await {
                    Ok(host) => {
                        inner.listen_error = None;
                        inner.running = Some(Running {
                            host,
                            port: config.port,
                            mapper: None,
                            detected: None,
                            asking: None,
                            watcher: None,
                        });
                    }
                    Err(e) => inner.listen_error = Some(e.to_string()),
                }
            }
            _ => {}
        }

        if let Some(running) = inner.running.as_mut() {
            // Whatever brought this reconcile about may have taken a code away
            // — a server unshared, or deleted — and a listener kept running
            // must not keep serving the connections that showed it.
            running.host.cut_revoked(paths).await;

            // Whichever way the router is handled, today's settings decide it,
            // and the watcher is restarted so what it announces uses them too.
            if let Some(watcher) = running.watcher.take() {
                watcher.abort();
            }
            match config.mapping {
                PortMapping::Auto => {
                    running.detected = None;
                    running.asking = None;
                    if running.mapper.is_none() {
                        running.mapper = Some(PortMapper::start(config.port));
                    }
                }
                PortMapping::Manual => {
                    if let Some(mapper) = running.mapper.take() {
                        mapper.stop().await;
                    }
                    if running.detected.is_none() && !self.quiet {
                        let (detected, asking) = detect();
                        running.detected = Some(detected);
                        running.asking = Some(asking);
                    }
                }
            }
            running.watcher = Some(tokio::spawn(announce(
                running.mapper.as_ref().map(PortMapper::status),
                running.detected.clone(),
                Nearby {
                    gateway: running.mapper.as_ref().map(PortMapper::gateway),
                    local: self.local.clone(),
                    relook: self.timings.relook,
                },
                config.clone(),
                pin,
                sink.clone(),
            )));
        }

        if inner.running.is_some() {
            return Outcome::Listening;
        }
        match wanted {
            Wanted::Share => Outcome::Down,
            Wanted::Stop => Outcome::Idle,
            Wanted::Unknown => Outcome::Unknown,
        }
    }

    /// Ask the router again now, and answer with what it said.
    ///
    /// The mapping is renewed, and the address under manual setup re-read, on
    /// a timer — so a change out on the network takes up to half an hour to
    /// show, which is no use to an admin who has just bridged a modem or
    /// turned UPnP on. This brings whichever of those two loops is running
    /// round now rather than asking the router itself: the mapping that
    /// already works is renewed by the request that always renews it, the
    /// listener is not touched, and several asks at once become one
    /// conversation with the router.
    ///
    /// Refused when nothing is talking to the router yet, which is the honest
    /// answer: there is no ask to hurry.
    pub async fn recheck_router(
        &self,
        paths: &Paths,
        config: &ShareConfig,
        sink: Option<EventSink>,
    ) -> Result<ShareStatus> {
        let rouse = {
            let inner = self.inner.lock().await;
            inner
                .running
                .as_ref()
                .and_then(Running::rouse)
                .ok_or_else(|| {
                    Error::msg(
                        "Geode has not asked your router anything yet. \
                         Share a server first, and it will.",
                    )
                })?
        };
        // Whatever came of it, the status is read afresh below: an ask that
        // ran out of time leaves the page showing the last thing the router
        // said rather than nothing, and the attempt reports itself when it
        // finishes.
        rouse.now(ASK_WITHIN).await;
        let status = self.status(paths, config).await;
        events::emit(&sink, STATUS_EVENT, &status);
        Ok(status)
    }

    pub async fn status(&self, paths: &Paths, config: &ShareConfig) -> ShareStatus {
        let pin = self.pin(paths).await;
        let inner = self.inner.lock().await;
        let running = inner.running.as_ref();
        let gateway = gateway_of(
            running
                .and_then(|running| running.mapper.as_ref())
                .and_then(|mapper| *mapper.gateway().borrow()),
            running
                .and_then(|running| running.detected.as_ref())
                .map(|detected| *detected.borrow()),
        );
        assemble(&inner, config, pin, self.local.address(gateway))
    }

    /// The link for a shared server, when there is an address to put in it.
    pub async fn link_for(
        &self,
        paths: &Paths,
        config: &ShareConfig,
        share: &ServerShare,
    ) -> Option<Link> {
        let status = self.status(paths, config).await;
        let address = reachable_address(config, &status.mapping, status.detected)?;
        // Not while nothing listens on the port: a link would lead nowhere,
        // however good the address in it.
        if !status.listening {
            return None;
        }
        let pin = match address {
            Address::Direct { .. } => Some(self.pin(paths).await?),
            Address::Https { .. } => None,
        };
        Some(Link {
            address,
            code: share.code.clone(),
            pin,
        })
    }

    /// The second link for a shared server: the one a player in the same house
    /// can click instead of typing this machine's address by hand.
    ///
    /// Only ever offered beside the link above, because it reaches no further
    /// than this network. It carries the same fingerprint, which is what makes
    /// it as safe as any other link — see [`local_address`].
    pub async fn local_link_for(
        &self,
        paths: &Paths,
        config: &ShareConfig,
        share: &ServerShare,
    ) -> Option<Link> {
        let status = self.status(paths, config).await;
        // Read back through the strict parser, like a link off the internet:
        // nothing here is special enough to skip it. The status only carries
        // an address while the listener is up, so there is no link to a port
        // nothing answers on.
        let address = Address::parse(&status.local_address?, config.port).ok()?;
        Some(Link {
            address,
            code: share.code.clone(),
            pin: Some(self.pin(paths).await?),
        })
    }

    /// Remember the address this server's links are being handed out with, as
    /// the admin takes one.
    ///
    /// Only when they take one: an admin who has sent nobody a link has no
    /// links to go stale, and nothing should be said to them when the address
    /// changes.
    pub async fn remember_link_address(
        &self,
        paths: &Paths,
        config: &ShareConfig,
        id: &str,
    ) -> Result<()> {
        if let Some(address) = self.status(paths, config).await.address {
            crate::server::share::save_link_address(paths, id, &address).await?;
        }
        Ok(())
    }

    /// Cut every preview and download whose server is no longer shared as it
    /// was when it came in. For a change to a share that needs no reconcile —
    /// a new code, or whether config is sent — but must still reach the
    /// players already being served, not only their next request.
    pub async fn cut_revoked(&self, paths: &Paths) {
        if let Some(running) = self.inner.lock().await.running.as_ref() {
            running.host.cut_revoked(paths).await;
        }
    }

    /// Stop everything and give the port back, for when the app exits. The
    /// mapping's removal is bounded, so a router that stops answering cannot
    /// hold the exit up; its lease is the safety net.
    pub async fn shutdown(&self) {
        // Behind the same turn a reconcile takes, so nothing is halfway
        // through starting a listener this would then not know to stop; and
        // anything still trying to bind is dropped before the port is given
        // back, rather than taking it again on the way out.
        let _turn = self.serial.lock().await;
        drop(self.supersede().await);
        let mut inner = self.inner.lock().await;
        if let Some(running) = inner.running.take() {
            stop(running).await;
        }
    }

    async fn pin(&self, paths: &Paths) -> Option<Pin> {
        if let Some(pin) = self.inner.lock().await.pin {
            return Some(pin);
        }
        let pin = Identity::load_or_create(paths).await.ok()?.pin();
        self.inner.lock().await.pin = Some(pin);
        Some(pin)
    }
}

async fn stop(mut running: Running) {
    if let Some(watcher) = running.watcher.take() {
        watcher.abort();
    }
    // The listener first: its connections are cut at once, where removing the
    // mapping can wait seconds on a slow router — and a download of a server
    // just unshared must not run on for them.
    let mapper = running.mapper.take();
    running.host.stop().await;
    if let Some(mapper) = mapper {
        mapper.stop().await;
    }
}

/// Ask the router for its public address now and then, changing nothing.
///
/// Manual setup means the admin forwards the port; it does not have to mean
/// they also look up their own address and type it in before a link exists.
///
/// The [`Rouse`] beside the answers brings this ask round now, for the admin
/// who has just changed something on their router and would otherwise wait
/// half an hour to see whether it worked.
fn detect() -> (watch::Receiver<Detection>, Rouse) {
    let (publish, detected) = watch::channel(Detection::default());
    let rouse = Rouse::new();
    let mut roused = rouse.roused();
    crate::rt::spawn(async move {
        loop {
            // Taken before the ask, so one arriving during it waits for the
            // next rather than reading this as its answer.
            let round = roused.taking();
            let read = portmap::read_external_ip().await.ok().map(|answer| {
                (
                    answer.gateway,
                    Detected {
                        ip: answer.external_ip,
                        reachability: portmap::classify(answer.external_ip),
                    },
                )
            });
            let wait = if read.is_some() {
                DETECT_EVERY
            } else {
                DETECT_RETRY
            };
            let next = publish.borrow().after(read);
            let listening = publish.send(next).is_ok();
            // After the answer is published, so an ask released here reads the
            // answer and not the one before it — but before giving up, or an
            // admin who pressed the button as the setup changed under them
            // would wait out the whole timeout for an answer nobody will send.
            roused.finished(round);
            if !listening {
                return; // nobody is listening any more
            }
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                // Somebody pressed "ask again".
                _ = roused.asked() => {}
                _ = publish.closed() => return,
            }
        }
    });
    (detected, rouse)
}

/// Where this machine's own address on its network is asked for, and how
/// often to ask again. Grouped because nothing announces that address: the
/// router it faces arrives on a channel, the answer itself does not, so the
/// watcher is the only thing that can notice it moved.
struct Nearby {
    gateway: Option<watch::Receiver<Option<IpAddr>>>,
    local: Local,
    relook: Duration,
}

/// Announce every change in the mapping or the detected address — and this
/// machine's own address on its network, which no channel here reports, so it
/// is looked at again on a timer and announced when it has moved.
async fn announce(
    mut mapping: Option<watch::Receiver<portmap::Status>>,
    mut detected: Option<watch::Receiver<Detection>>,
    nearby: Nearby,
    config: ShareConfig,
    pin: Option<Pin>,
    sink: Option<EventSink>,
) {
    let Nearby {
        gateway,
        local,
        relook,
    } = nearby;
    // What the status the reconcile has just announced said about this
    // machine's own address, so the first look round is measured against that
    // rather than against nothing.
    let mut announced = here(
        &local,
        gateway.as_ref().and_then(|g| *g.borrow()),
        detected.as_ref().map(|d| *d.borrow()),
        &config,
    );
    loop {
        let changed = async {
            match (&mut mapping, &mut detected) {
                (Some(m), _) => m.changed().await.is_ok(),
                (None, Some(d)) => d.changed().await.is_ok(),
                (None, None) => false,
            }
        };
        // Either something said so, or the clock came round: with nothing left
        // to follow there is nothing to look again for either, and the watcher
        // is done.
        let said = tokio::select! {
            changed = changed => {
                if !changed {
                    return;
                }
                true
            }
            _ = tokio::time::sleep(relook) => false,
        };
        let current = mapping
            .as_ref()
            .map(|m| m.borrow().clone())
            .unwrap_or(portmap::Status::Off);
        let detection = detected.as_ref().map(|d| *d.borrow());
        let found = detection.and_then(|d| d.found);
        let status = ShareStatus {
            mode: config.mapping,
            detected: found,
            asking: detection.is_some_and(|d| !d.asked),
            listening: true,
            port: config.port,
            listen_error: None,
            // This only runs while the listener is up, which is the one state
            // there is nothing to try again in.
            retry: None,
            address: reachable_address(&config, &current, found).map(|a| a.display()),
            local_address: here(
                &local,
                gateway.as_ref().and_then(|g| *g.borrow()),
                detection,
                &config,
            ),
            mapping: current,
            fingerprint: pin.map(|p| p.to_hex()).unwrap_or_default(),
        };
        // A look that found the same address as the last announcement is not
        // news; a change something announced is, whatever it was.
        if !said && status.local_address == announced {
            continue;
        }
        announced = status.local_address.clone();
        events::emit(&sink, STATUS_EVENT, &status);
    }
}

/// This machine's own address on its network, asked afresh: the router may
/// have been found after the watcher started — by the first mapping, or by the
/// first ask in manual setup — and the address facing it is the better answer.
fn here(
    local: &Local,
    gateway: Option<IpAddr>,
    detection: Option<Detection>,
    config: &ShareConfig,
) -> Option<String> {
    local
        .address(gateway_of(gateway, detection))
        .and_then(|ip| local_address(ip, config))
        .map(|a| a.display())
}

fn assemble(
    inner: &Inner,
    config: &ShareConfig,
    pin: Option<Pin>,
    local: Option<IpAddr>,
) -> ShareStatus {
    // Listening means listening on the port the settings name now. A listener
    // left on an old port is not something a link built from these settings
    // could reach.
    let running = inner
        .running
        .as_ref()
        .filter(|running| running.port == config.port);
    let mapping = running
        .and_then(|running| running.mapper.as_ref())
        .map(|mapper| mapper.status().borrow().clone())
        .unwrap_or(portmap::Status::Off);
    let detection = running
        .and_then(|running| running.detected.as_ref())
        .map(|d| *d.borrow());
    let detected = detection.and_then(|d| d.found);
    ShareStatus {
        mode: config.mapping,
        detected,
        asking: detection.is_some_and(|d| !d.asked),
        listening: running.is_some(),
        port: config.port,
        listen_error: inner.listen_error.clone(),
        retry: inner.retry.as_ref().map(|retry| Retrying {
            attempts: retry.attempts,
            // Rounded up, so a wait still to come never reads as none left.
            next_in_seconds: {
                let left = retry.next.saturating_duration_since(Instant::now());
                (left.as_millis() as u64).div_ceil(1000)
            },
            needs_you: retry.alerted,
        }),
        // Nothing to give players while nothing listens, even with a public
        // address set.
        address: running
            .and_then(|_| reachable_address(config, &mapping, detected))
            .map(|a| a.display()),
        // Nor while nothing listens, for the same reason: an address with
        // nothing behind it is not somewhere a player can be sent.
        local_address: running
            .and(local)
            .and_then(|ip| local_address(ip, config))
            .map(|a| a.display()),
        mapping,
        fingerprint: pin.map(|p| p.to_hex()).unwrap_or_default(),
    }
}

/// The address players should use, if one is known to work.
///
/// The admin's own public address wins whenever it is set: they know about
/// the domain or tunnel in front of this machine, and the router does not.
/// Otherwise only an address the router reported *and* that is actually public
/// is offered — a link built from a carrier-grade NAT address would be a link
/// that cannot work, handed out as if it could.
pub(crate) fn reachable_address(
    config: &ShareConfig,
    mapping: &portmap::Status,
    detected: Option<Detected>,
) -> Option<Address> {
    if let Some(text) = config
        .public_address
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Address::parse(text, config.port).ok();
    }
    let (ip, port) = match (mapping, detected) {
        (
            portmap::Status::Mapped {
                external_ip,
                port,
                reachability: Reachability::Public,
            },
            _,
        ) => (*external_ip, *port),
        // Manual setup: the admin forwarded the share port themselves, so the
        // address the router reports is reached on that port.
        (
            portmap::Status::Off,
            Some(Detected {
                ip,
                reachability: Reachability::Public,
            }),
        ) if config.mapping == PortMapping::Manual => (ip, config.port),
        _ => return None,
    };
    // Through the parser rather than built by hand, so an IPv6 address is
    // written the same way everywhere a link is read.
    Address::parse(&std::net::SocketAddr::new(ip, port).to_string(), port).ok()
}

/// The address players on this same network use, on the pack port.
///
/// Safe to hand out as a link of its own because trust is pinned to the
/// certificate and not to a host name: a player who connects here is held to
/// the very same fingerprint as one coming in from the internet, so a LAN link
/// proves it reached this machine exactly as any other link does. The listener
/// already takes every interface, so nothing new has to listen for it.
///
/// Through the same parser as the public address, for the same reason.
fn local_address(ip: IpAddr, config: &ShareConfig) -> Option<Address> {
    Address::parse(
        &std::net::SocketAddr::new(ip, config.port).to_string(),
        config.port,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    //! Sharing end to end, over real TLS on loopback: an admin shares a server,
    //! a player previews, downloads and imports it.
    //!
    //! Every test here runs with the mapping set to **manual**. Automatic
    //! mapping asks the real router on the real network to open a port, which
    //! no test has any business doing; `portmap` is tested against a mock
    //! gateway instead.

    use super::*;
    use crate::config::{PortMapping, ShareConfig};
    use crate::instance::content::{ContentKind, Origin};
    use crate::instance::jar::tests::fabric_jar;
    use crate::net::Progress;
    use crate::paths::Scratch;
    use crate::server::share::{self as server_share, ServerShare};
    use crate::server::{Server, ServerKind};
    use crate::share::code::Code;
    use crate::share::join;
    use crate::share::link::{Address, Link, Pin};
    use std::io::Read;
    use std::path::Path;

    /// A shared Fabric server on disk, with things that must reach a player and
    /// things that must never leave the admin's machine.
    async fn shared_server(paths: &Paths) -> (Server, ServerShare) {
        let mut server = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
        server.id = "survival".into();
        server.build = "0.16.9".into();
        let share = ServerShare {
            code: Code::generate(),
            include_config: false,
            settings: Default::default(),
            picked: Vec::new(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
        };
        let run = paths.server_run(&server.id);
        let files: Vec<(&str, Vec<u8>)> = vec![
            (
                "server.properties",
                b"enable-rcon=true\nrcon.password=hunter2\n".to_vec(),
            ),
            ("ops.json", br#"[{"name":"admin"}]"#.to_vec()),
            ("config/tokens.toml", b"discord = \"hunter2\"\n".to_vec()),
            ("mods/homemade.jar", fabric_jar("homemade", "*")),
            ("mods/server-only.jar", fabric_jar("serveronly", "server")),
        ];
        for (rel, bytes) in files {
            let path = run.join(rel);
            tokio::fs::create_dir_all(path.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(path, bytes).await.unwrap();
        }
        server.save(paths).await.unwrap();
        server_share::save(paths, &server.id, &share).await.unwrap();
        (server, share)
    }

    /// The service as the app runs it, minus any contact with the router.
    /// Shared, as the app shares it, so a retry has something to run against.
    fn service() -> Arc<ShareService> {
        Arc::new(ShareService {
            quiet: true,
            ..Default::default()
        })
    }

    /// An answer to which of this machine's addresses faces its own network
    /// that a test can give and then change, since the real one is whatever
    /// the routing table of the machine it runs on says; `portmap` covers the
    /// question itself.
    fn told(local: Option<&str>) -> (Local, std::sync::Arc<std::sync::Mutex<Option<IpAddr>>>) {
        let cell = std::sync::Arc::new(std::sync::Mutex::new(local.map(|ip| ip.parse().unwrap())));
        (Local::Told(cell.clone()), cell)
    }

    /// The same as [`service`], with that answer.
    fn service_on_network(local: Option<&str>) -> Arc<ShareService> {
        Arc::new(ShareService {
            quiet: true,
            local: told(local).0,
            ..Default::default()
        })
    }

    /// The same, retrying a listener in milliseconds rather than a minute:
    /// the first retry after `retry`, doubling to twice that, and the admin
    /// told once `escalate_after` of failing has gone by.
    fn service_retrying(retry: Duration, escalate_after: Duration) -> Arc<ShareService> {
        Arc::new(ShareService {
            quiet: true,
            timings: Timings {
                retry,
                retry_limit: retry * 2,
                escalate_after,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn pin_of(paths: &Paths) -> Pin {
        Identity::load_or_create(paths).await.unwrap().pin()
    }

    /// A link the way an admin's Geode writes one, read back the way a
    /// player's Geode reads one — so the real parser is in the path.
    fn link(port: u16, code: &Code, pin: Pin) -> Link {
        let written = Link {
            address: Address::Direct {
                host: "127.0.0.1".into(),
                port,
            },
            code: code.clone(),
            pin: Some(pin),
        };
        Link::parse(&written.to_url()).expect("a link Geode writes parses")
    }

    fn entries(archive: &Path) -> Vec<(String, Vec<u8>)> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        (0..zip.len())
            .map(|i| {
                let mut entry = zip.by_index(i).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (entry.name().to_string(), bytes)
            })
            .collect()
    }

    /// The whole feature, as a player meets it: preview, download, import.
    #[tokio::test]
    async fn a_player_joins_a_shared_server_end_to_end() {
        let admin_scratch = Scratch::new("e2e-join");
        let admin = admin_scratch.paths.clone();
        let (server, mut share) = shared_server(&admin).await;
        // How the admin wants players to see this server. The game address,
        // not the pack's: this server's packs come down the loopback port the
        // host below binds, and Minecraft connects to 25566.
        share.listing = Some(crate::share::manifest::Listing {
            name: "Survival Club".into(),
            address: "play.example.com:25566".into(),
        });
        server_share::save(&admin, &server.id, &share).await.unwrap();

        let host = Host::start(admin.clone(), 0, None).await.unwrap();
        let link = link(host.local_addr.port(), &share.code, pin_of(&admin).await);

        // What the player is shown before deciding.
        let preview = join::preview(link.clone()).await.unwrap();
        assert_eq!(preview.info.name, "Survival");
        // The listing crossed the wire in the description, which is what
        // keeps the row in step on every later Play.
        assert_eq!(
            preview.info.listing.as_ref().map(|l| (l.name.as_str(), l.address.as_str())),
            Some(("Survival Club", "play.example.com:25566"))
        );
        assert_eq!(preview.info.mc_version, "1.21.1");
        assert_eq!(preview.info.loader, "fabric");
        assert_eq!(preview.info.loader_version, "0.16.9");
        assert_eq!(
            preview.info.mods, 2,
            "every mod in the server's mods folder: {:?}",
            preview.info
        );
        assert_eq!(preview.info.server_mods, 1, "{:?}", preview.info);
        assert_eq!(preview.verified, join::Verified::Pinned);

        // What actually crosses the network.
        let player_scratch = Scratch::new("e2e-join-player");
        let player = player_scratch.paths.clone();
        let archive = player.root.join("incoming.geodepack");
        join::download(&preview.link, &archive, &Progress::silent("t"))
            .await
            .unwrap();

        let sent = entries(&archive);
        let names: Vec<&str> = sent.iter().map(|(n, _)| n.as_str()).collect();
        for jar in [
            "overrides/mods/homemade.jar",
            "overrides/mods/server-only.jar",
        ] {
            assert!(names.contains(&jar), "{jar} was not sent: {names:?}");
        }
        for never in ["server.properties", "ops.json", "tokens.toml"] {
            assert!(
                !names.iter().any(|n| n.contains(never)),
                "{never} was sent: {names:?}"
            );
        }
        // Belt and braces: the secret appears in no entry's contents either.
        for (name, bytes) in &sent {
            assert!(
                !bytes.windows(7).any(|w| w == b"hunter2"),
                "a secret travelled inside {name}"
            );
        }

        // And it imports as the server's, through the same path as the app.
        let described = crate::instance::modpack::preview(archive.clone())
            .await
            .unwrap();
        let mut instance = crate::instance::create(
            &player,
            described.name,
            described.mc_version,
            described.loader,
            described.loader_version,
        )
        .await
        .unwrap();
        let missing = crate::share::pack::import(
            // Nothing here is fetched, and the pass that names what arrived
            // must not go asking: a client that cannot reach anything is what
            // says so.
            &reqwest::Client::builder()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
                .build()
                .unwrap(),
            &player,
            &mut instance,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(missing.is_empty(), "{missing:?}");
        assert_eq!(instance.loader, crate::meta::Loader::Fabric);
        assert_eq!(instance.loader_version, "0.16.9");

        let mods = player.game_dir(&instance.id).join("mods");
        assert!(mods.join("homemade.jar").is_file());
        let record = instance
            .content
            .iter()
            .find(|r| r.file_name == "homemade.jar")
            .expect("the server's jar has a record");
        assert_eq!(record.origin, Origin::Server);
        let server_only = instance
            .content
            .iter()
            .find(|r| r.file_name == "server-only.jar")
            .expect("the server-side jar has a record");
        assert_eq!(
            server_only.side,
            Some(crate::instance::jar::Side::Server),
            "the server's classification did not arrive"
        );
        crate::instance::content::remove_in(
            &mods,
            &mut instance.content,
            ContentKind::Mod,
            "homemade.jar",
        )
        .await
        .expect_err("a player cannot delete the server's mod");

        // And the server is in the player's multiplayer list, with Play
        // pointed at it — so the import ends at a server they can join rather
        // than a title screen and an empty list.
        assert_eq!(
            instance.joined_listing_address.as_deref(),
            Some("play.example.com:25566")
        );
        assert_eq!(
            instance.overrides.quick_play_server.as_deref(),
            Some("play.example.com:25566")
        );
        let list = std::fs::read(crate::instance::multiplayer::file(
            &player.game_dir(&instance.id),
        ))
        .expect("no multiplayer list was written");
        let text = String::from_utf8_lossy(&list);
        assert!(text.contains("Survival Club"), "{text}");
        assert!(text.contains("play.example.com:25566"), "{text}");
        // The port this very pack came down is Geode's, and the game has
        // nothing listening on it.
        let pack_port = host.local_addr.port().to_string();
        assert!(!text.contains(&pack_port), "the pack port reached the list: {text}");

        host.stop().await;
    }

    /// Everything that should turn a player away does, and says why.
    #[tokio::test]
    async fn a_wrong_password_the_wrong_machine_and_a_replaced_password_are_refused() {
        let scratch = Scratch::new("e2e-refuse");
        let admin = scratch.paths.clone();
        let (server, share) = shared_server(&admin).await;
        let host = Host::start(admin.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&admin).await;
        let original = share.code.clone();

        let err = join::preview(link(port, &Code::generate(), pin))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );

        let impostor = Pin([7u8; 32]);
        let err = join::preview(link(port, &original, impostor))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("did not prove"), "{err}");

        // A new password cuts off the old one on the very next request.
        let share = server_share::replace_password(&admin, &server.id)
            .await
            .unwrap();
        let err = join::preview(link(port, &original, pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );
        join::preview(link(port, &share.code, pin)).await.unwrap();

        // And a whole-server save made from a copy loaded before the rotation —
        // a backup finishing, an install saving — cannot bring the old
        // password back, because the password does not live in the file it
        // writes.
        let stale = server.clone();
        stale.save(&admin).await.unwrap();
        let err = join::preview(link(port, &original, pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );

        // Stopping sharing cuts everyone off, with the password kept.
        let current = share.code.clone();
        server_share::stop(&admin, &server.id).await.unwrap();
        let err = join::preview(link(port, &current, pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );

        // Sharing again brings the same link back, and not the replaced one.
        server_share::start(&admin, &server.id).await.unwrap();
        join::preview(link(port, &current, pin)).await.unwrap();
        let err = join::preview(link(port, &original, pin)).await.unwrap_err();
        assert!(
            err.to_string().contains("did not accept this password"),
            "{err}"
        );

        // Nothing but the two paths is served, even with a good connection.
        let client =
            crate::share::trust::TrustedClient::new(crate::share::trust::Trust::Pinned(pin))
                .unwrap();
        let response = client
            .client
            .get(format!("https://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

        host.stop().await;
    }

    /// A typed address trusts the first certificate it sees, and the download
    /// that follows is pinned to exactly that one.
    #[tokio::test]
    async fn a_typed_address_is_pinned_to_the_machine_that_first_answered() {
        let scratch = Scratch::new("e2e-typed");
        let admin = scratch.paths.clone();
        let (_, share) = shared_server(&admin).await;
        let host = Host::start(admin.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();

        let typed = join::JoinInput::Parts {
            address: format!("  127.0.0.1:{port} "),
            code: share.code.display(),
        }
        .into_link(25590)
        .unwrap();
        assert!(typed.pin.is_none());

        // Unverified links do not get a download.
        let player = Scratch::new("e2e-typed-player");
        let archive = player.paths.root.join("p.geodepack");
        join::download(&typed, &archive, &Progress::silent("t"))
            .await
            .expect_err("a direct address with no fingerprint must preview first");

        let preview = join::preview(typed).await.unwrap();
        assert_eq!(preview.verified, join::Verified::FirstUse);
        assert_eq!(preview.link.pin, Some(pin_of(&admin).await));
        join::download(&preview.link, &archive, &Progress::silent("t"))
            .await
            .unwrap();

        host.stop().await;
    }

    fn manual(port: u16, public_address: Option<String>) -> ShareConfig {
        let config = ShareConfig {
            port,
            mapping: PortMapping::Manual,
            public_address,
        };
        // The guard every test here relies on: never ask a real router.
        assert_eq!(config.mapping, PortMapping::Manual);
        config
    }

    /// A port for the service to be told to listen on.
    ///
    /// Deliberately not a `:0` bind read back and dropped: that number comes
    /// from the range the kernel hands out for every other `:0` bind, so one
    /// of `host.rs`'s listeners could be given it in the moment between this
    /// call and the service binding it, and the service would come up on a
    /// port already taken. Numbers below that range are only ever taken by
    /// something that asked for them by name — 24_000 is below the 32768 the
    /// default `net.ipv4.ip_local_port_range` starts at, so a machine tuned
    /// lower needs these numbers lowered with it.
    ///
    /// Two things keep callers apart, because the gap between this check and
    /// the service's own bind is still a gap. The counter separates tests in
    /// one run; the process id separates runs, which matters because two
    /// `cargo test` processes on one machine would otherwise both start at the
    /// same number, both be told it is free, and the second service to bind
    /// would fail on a port already in use.
    fn free_port() -> u16 {
        static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
        let base = first_port(std::process::id());
        loop {
            let port = base + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert!(port < 32_000, "no port left below the ephemeral range");
            if port_is_free(port) {
                return port;
            }
        }
    }

    /// Where this process's candidates start: a stretch of the range picked by
    /// process id, wide enough for every `free_port` call one run makes.
    fn first_port(pid: u32) -> u16 {
        24_000 + (pid % 200) as u16 * 32
    }

    /// Asked the way the service asks it, rather than re-stated.
    ///
    /// This used to bind the IPv4 wildcard while the service holds the IPv6
    /// one dual-stack. Linux calls that a clash and the two agreed; Windows
    /// does not, so a port the service was listening on read back as free.
    /// Borrowing `share::host::listen` means the question cannot drift from the
    /// answer again — and the socket is dropped the moment it is made.
    fn port_is_free(port: u16) -> bool {
        crate::share::host::listen(port).is_ok()
    }

    /// Two `cargo test` runs at once — one worktree each, which is how this
    /// repo gets worked on — have to be handed different candidates, since the
    /// counter inside `free_port` is per process and cannot tell them apart.
    /// Every start also has to stay below the ephemeral range, whatever the
    /// pid.
    #[test]
    fn two_runs_on_one_machine_do_not_start_at_the_same_port() {
        let starts: std::collections::HashSet<u16> = (1..=400u32).map(first_port).collect();
        assert!(
            starts.len() > 100,
            "400 pids share only {} starting ports",
            starts.len()
        );
        for pid in [1, 4_194_304, u32::MAX] {
            let start = first_port(pid);
            assert!(
                (24_000..31_000).contains(&start),
                "pid {pid} starts at {start}"
            );
        }
    }

    /// The service listens exactly while something is shared, follows a port
    /// change, gives the port back, and builds working links.
    #[tokio::test]
    async fn sharing_listens_only_while_something_is_shared() {
        let scratch = Scratch::new("e2e-service");
        let admin = scratch.paths.clone();
        let (server, share) = shared_server(&admin).await;
        let service = service();

        // Nothing shared: nothing listens.
        server_share::stop(&admin, &server.id).await.unwrap();
        let first = free_port();
        let config = manual(first, Some(format!("127.0.0.1:{first}")));
        service.reconcile_with(&admin, &config, None).await;
        assert!(!service.status(&admin, &config).await.listening);

        // Shared again: listening, with the same password, and a link that
        // works.
        assert_eq!(
            server_share::start(&admin, &server.id).await.unwrap(),
            share
        );
        service.reconcile_with(&admin, &config, None).await;
        let status = service.status(&admin, &config).await;
        assert!(status.listening, "{status:?}");
        assert_eq!(
            status.mapping,
            portmap::Status::Off,
            "manual means the router is left alone"
        );
        assert_eq!(
            status.address.as_deref(),
            Some(format!("127.0.0.1:{first}").as_str())
        );
        assert!(!port_is_free(first));

        let link = service
            .link_for(&admin, &config, &share)
            .await
            .expect("an address is set");
        assert_eq!(link.pin, Some(pin_of(&admin).await));
        join::preview(link).await.unwrap();

        // No public address and no mapping: no link, rather than a wrong one.
        let blind = manual(first, None);
        assert!(service.link_for(&admin, &blind, &share).await.is_none());

        // A new port moves the listener and frees the old one.
        let second = free_port();
        let moved = manual(second, Some(format!("127.0.0.1:{second}")));
        service.reconcile_with(&admin, &moved, None).await;
        assert!(port_is_free(first), "the old port was not given back");
        let link = service.link_for(&admin, &moved, &share).await.unwrap();
        join::preview(link).await.unwrap();

        // A share file that cannot be read is not the same as nothing shared:
        // stopping would cut players off from a server that still is.
        tokio::fs::write(server_share::path(&admin, &server.id), b"{ not json")
            .await
            .unwrap();
        service.reconcile_with(&admin, &moved, None).await;
        assert!(
            service.status(&admin, &moved).await.listening,
            "an unreadable share stopped the listener"
        );
        server_share::save(&admin, &server.id, &share)
            .await
            .unwrap();

        // Stopped again: stops, and the port is free.
        server_share::stop(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &moved, None).await;
        assert!(!service.status(&admin, &moved).await.listening);
        assert!(port_is_free(second), "the port was not given back");

        // And sharing again listens again, on the same password.
        server_share::start(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &moved, None).await;
        assert!(service.status(&admin, &moved).await.listening);
        let link = service.link_for(&admin, &moved, &share).await.unwrap();
        join::preview(link).await.unwrap();

        service.shutdown().await;
    }

    /// Unsharing one server while another stays shared keeps the listener up
    /// — and cuts off a download of the one unshared there and then, not only
    /// its next request.
    #[tokio::test]
    async fn unsharing_a_server_cuts_off_its_downloads_while_another_stays_shared() {
        use crate::share::host::tests::{ask_for_pack, read_rest};

        let scratch = Scratch::new("e2e-unshare-cut");
        let admin = scratch.paths.clone();
        let (server, share) = shared_server(&admin).await;
        // Megabytes that do not compress, so the download outlasts every buffer
        // between the two ends and is still under way when the server is unshared.
        let mut filler = vec![0u8; 4 * 1024 * 1024];
        getrandom::fill(&mut filler).unwrap();
        tokio::fs::write(admin.server_run(&server.id).join("mods/filler.jar"), filler)
            .await
            .unwrap();
        let mut other = Server::new("Creative".into(), ServerKind::Fabric, "1.21.1".into());
        other.id = "creative".into();
        other.build = "0.16.9".into();
        other.save(&admin).await.unwrap();
        let other_share = ServerShare {
            code: Code::generate(),
            include_config: false,
            settings: Default::default(),
            picked: Vec::new(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
        };
        server_share::save(&admin, &other.id, &other_share)
            .await
            .unwrap();

        let service = service();
        let config = manual(free_port(), None);
        service.reconcile_with(&admin, &config, None).await;
        assert!(service.status(&admin, &config).await.listening);

        let (mut tls, received, length) = ask_for_pack(config.port, &share.code).await;
        server_share::stop(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &config, None).await;
        assert!(
            service.status(&admin, &config).await.listening,
            "the listener stopped with a server still shared"
        );

        let (received, _) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "a download of an unshared server ran to its end"
        );

        service.shutdown().await;
    }

    /// Unsharing the last shared server stops the listener, and removing the
    /// router's mapping on the way can wait seconds on a slow router. A
    /// download of the server unshared is cut before that wait, not after.
    #[tokio::test]
    async fn stopping_cuts_off_downloads_before_waiting_on_the_router() {
        use crate::share::host::tests::{ask_for_pack, outgoing_files, read_rest};
        use crate::share::portmap::tests::a_mapping_whose_removal_hangs;

        let scratch = Scratch::new("e2e-stop-cut");
        let admin = scratch.paths.clone();
        let (server, share) = shared_server(&admin).await;
        // Megabytes that do not compress, so the download outlasts every buffer
        // between the two ends and is still under way when sharing stops.
        let mut filler = vec![0u8; 4 * 1024 * 1024];
        getrandom::fill(&mut filler).unwrap();
        tokio::fs::write(admin.server_run(&server.id).join("mods/filler.jar"), filler)
            .await
            .unwrap();

        let host = Host::start(admin.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();
        let (mapper, _router, removal) = a_mapping_whose_removal_hangs().await;
        let running = Running {
            host,
            port,
            mapper: Some(mapper),
            detected: None,
            asking: None,
            watcher: None,
        };

        let (mut tls, received, length) = ask_for_pack(port, &share.code).await;
        let stopping = tokio::spawn(stop(running));
        // Nothing is read meanwhile, so the download is still waiting on the
        // player, and only cutting it removes its pack file.
        let waiting = std::time::Instant::now();
        while outgoing_files(&admin) > 0 {
            assert!(
                waiting.elapsed() < removal / 2,
                "the download ran on while the router was waited on"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!stopping.is_finished(), "the router was not waited on");
        let (received, _) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "a download ran to its end as sharing stopped"
        );
        stopping.await.unwrap();
    }

    /// A new code needs no reconcile, so what cuts off a download started with
    /// the old one is the service checking codes again once the new one is
    /// saved — which is what replacing a code does.
    #[tokio::test]
    async fn a_new_code_cuts_off_downloads_through_the_service() {
        use crate::share::host::tests::{ask_for_pack, read_rest};

        let scratch = Scratch::new("e2e-rotate-cut");
        let admin = scratch.paths.clone();
        let (server, mut share) = shared_server(&admin).await;
        // Megabytes that do not compress, so the download outlasts every buffer
        // between the two ends and is still under way when the code changes.
        let mut filler = vec![0u8; 4 * 1024 * 1024];
        getrandom::fill(&mut filler).unwrap();
        tokio::fs::write(admin.server_run(&server.id).join("mods/filler.jar"), filler)
            .await
            .unwrap();

        let service = service();
        let config = manual(free_port(), None);
        service.reconcile_with(&admin, &config, None).await;
        assert!(service.status(&admin, &config).await.listening);

        let (mut tls, received, length) = ask_for_pack(config.port, &share.code).await;
        share.code = Code::generate();
        server_share::save(&admin, &server.id, &share)
            .await
            .unwrap();
        service.cut_revoked(&admin).await;

        let (received, _) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "a download ran to its end on a code replaced"
        );

        service.shutdown().await;
    }

    /// A link is only ever built from an address players can actually reach.
    /// The router saying yes is not enough: behind carrier-grade NAT or a second
    /// router, the address it reports goes nowhere, and a link made from it
    /// would be handed out as if it worked.
    #[test]
    fn links_are_only_built_from_an_address_that_works() {
        let mapped = |ip: &str, reachability| portmap::Status::Mapped {
            external_ip: ip.parse().unwrap(),
            port: 25590,
            reachability,
        };
        let auto = ShareConfig {
            port: 25590,
            mapping: PortMapping::Auto,
            public_address: None,
        };

        assert_eq!(
            reachable_address(&auto, &mapped("203.0.113.7", Reachability::Public), None)
                .map(|a| a.display()),
            Some("203.0.113.7:25590".into())
        );
        for (ip, unreachable) in [
            ("100.64.1.2", Reachability::Cgnat),
            ("192.168.1.2", Reachability::DoubleNat),
            ("0.0.0.0", Reachability::NoIpv4),
        ] {
            assert_eq!(
                reachable_address(&auto, &mapped(ip, unreachable), None),
                None,
                "{ip}"
            );
        }
        for status in [
            portmap::Status::Off,
            portmap::Status::Searching,
            portmap::Status::Failed(portmap::Failure::NoGateway),
        ] {
            assert_eq!(reachable_address(&auto, &status, None), None, "{status:?}");
        }

        // The admin's own address wins, even over a working mapping: they know
        // about the tunnel or domain in front of this machine, the router does not.
        let tunnelled = ShareConfig {
            public_address: Some("https://pack.example.com".into()),
            ..auto.clone()
        };
        assert_eq!(
            reachable_address(
                &tunnelled,
                &mapped("203.0.113.7", Reachability::Public),
                None
            ),
            Some(Address::Https {
                url: "https://pack.example.com".into()
            })
        );
        // And it is what makes a link possible at all behind CGNAT.
        let behind_cgnat = ShareConfig {
            public_address: Some("mc.example.com".into()),
            ..auto
        };
        assert_eq!(
            reachable_address(
                &behind_cgnat,
                &mapped("100.64.1.2", Reachability::Cgnat),
                None
            )
            .map(|a| a.display()),
            Some("mc.example.com:25590".into())
        );
    }

    /// Manual setup still gets a link without the admin typing their address:
    /// the router is asked, and a public answer is used on the forwarded port.
    /// A private or shared answer is not.
    #[test]
    fn manual_setup_links_from_a_detected_public_address_only() {
        let manual_setup = ShareConfig {
            port: 38033,
            mapping: PortMapping::Manual,
            public_address: None,
        };
        let detected = |ip: &str, reachability| {
            Some(Detected {
                ip: ip.parse().unwrap(),
                reachability,
            })
        };

        assert_eq!(
            reachable_address(
                &manual_setup,
                &portmap::Status::Off,
                detected("203.0.113.7", Reachability::Public)
            )
            .map(|a| a.display()),
            Some("203.0.113.7:38033".into()),
            "the forwarded port, not a default"
        );
        for private in [
            detected("100.64.1.2", Reachability::Cgnat),
            detected("192.168.1.2", Reachability::DoubleNat),
        ] {
            assert_eq!(
                reachable_address(&manual_setup, &portmap::Status::Off, private),
                None
            );
        }
        assert_eq!(
            reachable_address(&manual_setup, &portmap::Status::Off, None),
            None
        );

        // Automatic setup never uses a detection: the mapping is its source.
        let auto_setup = ShareConfig {
            mapping: PortMapping::Auto,
            ..manual_setup
        };
        assert_eq!(
            reachable_address(
                &auto_setup,
                &portmap::Status::Off,
                detected("203.0.113.7", Reachability::Public)
            ),
            None
        );
    }

    /// A player in the same house gets this machine's own address and a link
    /// that carries it, beside the public one and never instead of it. It is
    /// also what the admin can try when their own link comes back refused,
    /// which is what a router that will not loop a connection back in looks
    /// like.
    #[tokio::test]
    async fn players_on_this_network_get_this_machines_own_address_and_a_link_for_it() {
        let scratch = Scratch::new("e2e-local-link");
        let admin = scratch.paths.clone();
        let (server, share) = shared_server(&admin).await;
        let service = service_on_network(Some("192.168.1.50"));

        let port = free_port();
        let config = manual(port, Some("203.0.113.7".into()));
        service.reconcile_with(&admin, &config, None).await;

        let status = service.status(&admin, &config).await;
        assert_eq!(
            status.address.as_deref(),
            Some(format!("203.0.113.7:{port}").as_str()),
            "the address links are made with changed"
        );
        assert_eq!(
            status.local_address.as_deref(),
            Some(format!("192.168.1.50:{port}").as_str())
        );

        // Read back through the parser a link off the internet goes through,
        // and pinned to the same certificate: that is what lets a player on
        // this network reach the very machine the public link names.
        let local = service
            .local_link_for(&admin, &config, &share)
            .await
            .expect("this machine's own address is known");
        let read_back = Link::parse(&local.to_url()).expect("a link Geode writes parses");
        assert_eq!(
            read_back.address,
            Address::Direct {
                host: "192.168.1.50".into(),
                port
            }
        );
        assert_eq!(read_back.pin, Some(pin_of(&admin).await));
        assert_eq!(read_back.code, share.code);

        let public = service.link_for(&admin, &config, &share).await.unwrap();
        assert_eq!(
            public.address,
            Address::Direct {
                host: "203.0.113.7".into(),
                port
            },
            "the local address was handed out as the main one"
        );
        assert_eq!(public.pin, read_back.pin);

        // Nothing shared, so nothing listens: no address of either kind, since
        // either would be somewhere a player gets refused.
        server_share::stop(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &config, None).await;
        assert_eq!(service.status(&admin, &config).await.local_address, None);
        let gone = service.local_link_for(&admin, &config, &share).await;
        assert!(gone.is_none(), "a link to a port nothing answers on");

        service.shutdown().await;
    }

    /// A machine with no address of its own that this network could reach says
    /// so by saying nothing, and the public link is unaffected.
    #[tokio::test]
    async fn a_machine_with_no_local_address_offers_no_local_link() {
        let scratch = Scratch::new("e2e-no-local");
        let admin = scratch.paths.clone();
        let (_, share) = shared_server(&admin).await;
        let service = service_on_network(None);

        let port = free_port();
        let config = manual(port, Some("203.0.113.7".into()));
        service.reconcile_with(&admin, &config, None).await;

        let status = service.status(&admin, &config).await;
        assert!(status.listening, "{status:?}");
        assert_eq!(status.local_address, None);
        let none = service.local_link_for(&admin, &config, &share).await;
        assert!(none.is_none());
        assert!(
            service.link_for(&admin, &config, &share).await.is_some(),
            "the public link went with it"
        );

        service.shutdown().await;
    }

    /// What the note about an address that has moved on goes by: the address
    /// the last link taken points at. Only taking a link writes it, and taking
    /// one at the address of the day is what clears the note again.
    #[tokio::test]
    async fn the_address_links_were_taken_at_is_kept_until_a_link_is_taken_again() {
        let scratch = Scratch::new("e2e-link-address");
        let admin = scratch.paths.clone();
        let (server, _) = shared_server(&admin).await;
        let service = service();

        let port = free_port();
        let first = manual(port, Some("203.0.113.7".into()));
        service.reconcile_with(&admin, &first, None).await;

        // Nothing until a link is taken: an admin who has sent nobody a link
        // has no links to go stale, and is told nothing when the address
        // changes.
        assert_eq!(server_share::link_address(&admin, &server.id).await, None);

        service
            .remember_link_address(&admin, &first, &server.id)
            .await
            .unwrap();
        let taken = server_share::link_address(&admin, &server.id).await;
        assert_eq!(taken, Some(format!("203.0.113.7:{port}")));
        assert_eq!(
            taken,
            service.status(&admin, &first).await.address,
            "a link just taken would read as pointing somewhere stale"
        );

        // The address moves on, which is the whole of the note's condition:
        // what links were built from is no longer what Geode sees.
        let moved = manual(port, Some("198.51.100.4".into()));
        service.reconcile_with(&admin, &moved, None).await;
        let now = service.status(&admin, &moved).await.address;
        assert_eq!(now, Some(format!("198.51.100.4:{port}")));
        assert_ne!(server_share::link_address(&admin, &server.id).await, now);

        // And taking the new link puts it right.
        service
            .remember_link_address(&admin, &moved, &server.id)
            .await
            .unwrap();
        assert_eq!(server_share::link_address(&admin, &server.id).await, now);

        // With nothing shared there is no address to take a link at, and the
        // last one stands: unsharing is not a link anybody was sent.
        server_share::stop(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &moved, None).await;
        service
            .remember_link_address(&admin, &moved, &server.id)
            .await
            .unwrap();
        assert_eq!(server_share::link_address(&admin, &server.id).await, now);

        service.shutdown().await;
    }

    /// A port something else holds is reported, not a crash.
    #[tokio::test]
    async fn a_port_already_in_use_is_reported() {
        let scratch = Scratch::new("e2e-busy");
        let admin = scratch.paths.clone();
        let (_, share) = shared_server(&admin).await;
        let squatter = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let taken = squatter.local_addr().unwrap().port();

        let service = service();
        let config = manual(taken, None);
        service.reconcile_with(&admin, &config, None).await;
        let status = service.status(&admin, &config).await;
        assert!(!status.listening);
        assert!(
            status
                .listen_error
                .as_deref()
                .is_some_and(|e| e.contains(&taken.to_string())),
            "{status:?}"
        );

        // A public address set in Settings does not make a link to a port
        // nothing is listening on.
        let tunnelled = manual(taken, Some("mc.example.com".into()));
        service.reconcile_with(&admin, &tunnelled, None).await;
        let status = service.status(&admin, &tunnelled).await;
        assert!(!status.listening);
        assert_eq!(
            status.address, None,
            "an address to read out for a dead port"
        );
        assert!(
            service.link_for(&admin, &tunnelled, &share).await.is_none(),
            "a link handed out for a dead port"
        );
        // Reported, and not left at that: the port is being tried again.
        assert!(status.retry.is_some(), "{status:?}");

        service.shutdown().await;
        drop(squatter);
    }

    /// Whether the service is listening within `wait`, asked as the Share tab
    /// asks. Retries happen on a task of their own, so nothing tells a test
    /// when one has run.
    async fn listens_within(
        service: &ShareService,
        paths: &Paths,
        config: &ShareConfig,
        wait: Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + wait;
        loop {
            if service.status(paths, config).await.listening {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The first retry the status reports that `done` accepts, or a panic
    /// after five seconds.
    /// Wait for the admin to have been told something, rather than for the
    /// state that precedes it.
    ///
    /// The retry sets its status flag and then emits, so a machine under load
    /// can satisfy [`listens_within`] with the notice still in flight — which
    /// made the count below a race rather than an assertion.
    async fn told_at_least(notices: impl Fn() -> Vec<serde_json::Value>, many: usize) -> Vec<serde_json::Value> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let told = notices();
            if told.len() >= many {
                return told;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {} of {many} notices arrived: {told:?}",
                told.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_for_retry(
        service: &ShareService,
        paths: &Paths,
        config: &ShareConfig,
        done: impl Fn(&Retrying) -> bool,
    ) -> Retrying {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let status = service.status(paths, config).await;
            if let Some(retry) = status.retry.filter(&done) {
                return retry;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the retry never got there: {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A port held by something else, as a Geode that has just quit holds one
    /// while it asks the router to close it.
    fn squatted() -> (std::net::TcpListener, u16) {
        let squatter = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = squatter.local_addr().unwrap().port();
        (squatter, port)
    }

    /// The listener that could not start tries again on its own: whatever held
    /// the port lets go, and sharing comes up without a reconcile in between —
    /// nobody restarts Geode, nobody changes the port.
    #[tokio::test]
    async fn a_listener_kept_off_its_port_starts_on_its_own_once_the_port_is_free() {
        let scratch = Scratch::new("e2e-retry");
        let admin = scratch.paths.clone();
        let (_, share) = shared_server(&admin).await;
        let (squatter, taken) = squatted();

        let service = service_retrying(Duration::from_millis(100), Duration::from_secs(60));
        let config = manual(taken, Some(format!("127.0.0.1:{taken}")));
        service.reconcile_with(&admin, &config, None).await;
        let status = service.status(&admin, &config).await;
        assert!(!status.listening, "{status:?}");
        assert_eq!(status.retry.map(|r| r.attempts), Some(1), "{status:?}");

        drop(squatter);
        assert!(
            listens_within(&service, &admin, &config, Duration::from_secs(5)).await,
            "the listener never came up on its own"
        );

        // And it came up the way a reconcile leaves it: nothing to say about
        // the port any more, and a link players can use.
        let status = service.status(&admin, &config).await;
        assert!(
            status.retry.is_none() && status.listen_error.is_none(),
            "{status:?}"
        );
        let link = service
            .link_for(&admin, &config, &share)
            .await
            .expect("an address is set");
        join::preview(link).await.unwrap();

        service.shutdown().await;
    }

    /// Unsharing the last shared server ends the retry. There is nothing left
    /// to listen for, so the port coming free afterwards must not quietly
    /// bring a listener up behind the admin's back.
    #[tokio::test]
    async fn unsharing_the_last_server_ends_the_retry() {
        let scratch = Scratch::new("e2e-retry-unshare");
        let admin = scratch.paths.clone();
        let (server, _) = shared_server(&admin).await;
        let (squatter, taken) = squatted();

        let service = service_retrying(Duration::from_millis(100), Duration::from_secs(60));
        let config = manual(taken, None);
        service.reconcile_with(&admin, &config, None).await;
        assert!(service.status(&admin, &config).await.retry.is_some());

        server_share::stop(&admin, &server.id).await.unwrap();
        service.reconcile_with(&admin, &config, None).await;
        let status = service.status(&admin, &config).await;
        assert!(
            status.retry.is_none(),
            "still trying with nothing shared: {status:?}"
        );
        assert!(status.listen_error.is_none(), "{status:?}");

        drop(squatter);
        assert!(
            !listens_within(&service, &admin, &config, Duration::from_millis(500)).await,
            "a listener came up for a server nobody shares"
        );
        assert!(port_is_free(taken), "the port was taken after unsharing");

        service.shutdown().await;
    }

    /// A port change while a retry is in flight replaces it: the new port is
    /// listened on, the old one is left alone however free it becomes, and
    /// what is running is one listener with the router handled as the settings
    /// ask — not a second one from the retry that was superseded.
    #[tokio::test]
    async fn a_port_change_supersedes_a_retry_in_flight() {
        let scratch = Scratch::new("e2e-retry-moved");
        let admin = scratch.paths.clone();
        let (_, share) = shared_server(&admin).await;
        let (squatter, taken) = squatted();

        let service = service_retrying(Duration::from_millis(100), Duration::from_secs(60));
        let stuck = manual(taken, None);
        service.reconcile_with(&admin, &stuck, None).await;
        assert!(service.status(&admin, &stuck).await.retry.is_some());

        let second = free_port();
        let moved = manual(second, Some(format!("127.0.0.1:{second}")));
        service.reconcile_with(&admin, &moved, None).await;
        let status = service.status(&admin, &moved).await;
        assert!(status.listening, "{status:?}");
        assert!(
            status.retry.is_none(),
            "a retry outlived the port change: {status:?}"
        );
        assert_eq!(
            status.mapping,
            portmap::Status::Off,
            "manual means the router is left alone"
        );

        // The superseded retry is gone rather than merely asleep: the old port
        // coming free never gets it a second listener.
        drop(squatter);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            port_is_free(taken),
            "the superseded retry took the old port"
        );
        let link = service.link_for(&admin, &moved, &share).await.unwrap();
        join::preview(link).await.unwrap();

        service.shutdown().await;
    }

    /// What the status says while the port stays taken: trying again, with the
    /// attempt count and the wait the screens read out, and only once it has
    /// been failing for the threshold does it say this needs the admin.
    #[tokio::test]
    async fn the_status_says_trying_again_and_then_that_it_needs_you() {
        let scratch = Scratch::new("e2e-retry-status");
        let admin = scratch.paths.clone();
        shared_server(&admin).await;
        let (squatter, taken) = squatted();

        let service = service_retrying(Duration::from_millis(300), Duration::from_millis(900));
        let config = manual(taken, None);
        service.reconcile_with(&admin, &config, None).await;

        let first = service
            .status(&admin, &config)
            .await
            .retry
            .expect("the first failure is already being tried again");
        assert_eq!(first.attempts, 1);
        assert!(!first.needs_you, "{first:?}");
        assert_eq!(
            first.next_in_seconds, 1,
            "a wait still to come read as none left: {first:?}"
        );

        // A second attempt, still inside the threshold: more tries, same tone.
        let trying = wait_for_retry(&service, &admin, &config, |r| r.attempts > 1).await;
        assert!(
            !trying.needs_you,
            "escalated before the threshold: {trying:?}"
        );

        // Past it, and the reason is there to name.
        let needing = wait_for_retry(&service, &admin, &config, |r| r.needs_you).await;
        assert!(needing.attempts >= trying.attempts, "{needing:?}");
        let status = service.status(&admin, &config).await;
        assert!(
            status
                .listen_error
                .as_deref()
                .is_some_and(|e| e.contains(&taken.to_string())),
            "{status:?}"
        );

        service.shutdown().await;
        drop(squatter);
    }

    /// Everything the notices say, in the order an admin meets them: nothing
    /// while the first attempt works, one notice however many attempts fail
    /// past the threshold, and one more when a retry finally gets the port.
    #[tokio::test]
    async fn the_admin_is_told_once_that_it_needs_them_and_once_when_it_comes_back() {
        let scratch = Scratch::new("e2e-retry-told");
        let admin = scratch.paths.clone();
        shared_server(&admin).await;
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: Option<EventSink> = Some(recorder.clone());
        let notices = || -> Vec<serde_json::Value> {
            recorder
                .events()
                .into_iter()
                .filter(|(topic, _)| topic == LISTEN_EVENT)
                .map(|(_, payload)| payload)
                .collect()
        };

        // A listener that starts first time is not news.
        let ordinary = service();
        let free = manual(free_port(), None);
        ordinary.reconcile_with(&admin, &free, sink.clone()).await;
        assert!(ordinary.status(&admin, &free).await.listening);
        assert!(notices().is_empty(), "{:?}", recorder.topics());
        ordinary.shutdown().await;

        let (squatter, taken) = squatted();
        let service = service_retrying(Duration::from_millis(100), Duration::from_millis(250));
        let config = manual(taken, None);
        service.reconcile_with(&admin, &config, sink.clone()).await;

        // Several attempts past the threshold, and one notice for all of them.
        wait_for_retry(&service, &admin, &config, |r| r.attempts >= 5).await;
        let told = notices();
        assert_eq!(told.len(), 1, "one notice, not one an attempt: {told:?}");
        assert_eq!(told[0]["listening"], serde_json::json!(false));
        assert_eq!(told[0]["port"], serde_json::json!(taken));
        assert!(
            told[0]["error"]
                .as_str()
                .is_some_and(|e| e.contains(&taken.to_string())),
            "{told:?}"
        );

        drop(squatter);
        assert!(listens_within(&service, &admin, &config, Duration::from_secs(5)).await);
        let told = told_at_least(notices, 2).await;
        assert_eq!(told.len(), 2, "{told:?}");
        assert_eq!(told[1]["listening"], serde_json::json!(true));
        assert_eq!(told[1]["error"], serde_json::Value::Null);
        // And the screens were told to follow along, each attempt.
        assert!(
            recorder
                .topics()
                .iter()
                .filter(|t| *t == STATUS_EVENT)
                .count()
                > 2,
            "{:?}",
            recorder.topics()
        );

        service.shutdown().await;
    }

    /// Every notice sent so far, newest last.
    fn listen_notices(recorder: &crate::events::Recorder) -> Vec<serde_json::Value> {
        recorder
            .events()
            .into_iter()
            .filter(|(topic, _)| topic == LISTEN_EVENT)
            .map(|(_, payload)| payload)
            .collect()
    }

    /// A port that comes free before the admin was ever told it was taken is
    /// not something to announce the end of. This is the case the retry was
    /// built for — reopening Geode while the old one still holds the port —
    /// and an admin who has just opened the app and been told of no problem
    /// cannot read "listening again" as anything but a fault they missed.
    #[tokio::test]
    async fn a_port_that_comes_back_before_the_admin_was_told_says_nothing() {
        let scratch = Scratch::new("e2e-retry-quiet");
        let admin = scratch.paths.clone();
        shared_server(&admin).await;
        let (squatter, taken) = squatted();
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: Option<EventSink> = Some(recorder.clone());

        // A threshold it never reaches: this outage is over in milliseconds.
        let service = service_retrying(Duration::from_millis(100), Duration::from_secs(60));
        let config = manual(taken, None);
        service.reconcile_with(&admin, &config, sink.clone()).await;
        assert!(service.status(&admin, &config).await.retry.is_some());

        drop(squatter);
        assert!(listens_within(&service, &admin, &config, Duration::from_secs(5)).await);
        assert_eq!(
            listen_notices(&recorder),
            Vec::<serde_json::Value>::new(),
            "an admin who was told of no problem was told it was over"
        );

        service.shutdown().await;
    }

    /// A retry that comes back to find nothing shared any more has not
    /// recovered: it has nothing left to do. Unsharing the last server writes
    /// the share file before it reconciles, so an attempt whose wait ends in
    /// that gap is the one that reads it — and it must not congratulate the
    /// admin on a listener that does not exist.
    #[tokio::test]
    async fn a_retry_that_finds_nothing_shared_stops_without_saying_it_came_back() {
        let scratch = Scratch::new("e2e-retry-gone");
        let admin = scratch.paths.clone();
        let (server, _) = shared_server(&admin).await;
        let (squatter, taken) = squatted();
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: Option<EventSink> = Some(recorder.clone());

        // Past the threshold first, so the failure notice is already sent and
        // the only thing left that could speak is the recovery.
        let service = service_retrying(Duration::from_millis(100), Duration::from_millis(250));
        let config = manual(taken, None);
        service.reconcile_with(&admin, &config, sink.clone()).await;
        wait_for_retry(&service, &admin, &config, |r| r.needs_you).await;
        assert_eq!(listen_notices(&recorder).len(), 1);

        // The share file alone, as `unshare_server` writes it before taking
        // the turn a reconcile takes.
        server_share::stop(&admin, &server.id).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while service.status(&admin, &config).await.retry.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "the retry went on with nothing shared"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let told = listen_notices(&recorder);
        assert_eq!(told.len(), 1, "sharing was said to be back on: {told:?}");
        let status = service.status(&admin, &config).await;
        assert!(
            !status.listening && status.listen_error.is_none(),
            "{status:?}"
        );

        service.shutdown().await;
        drop(squatter);
    }

    /// A reconcile that leaves the port alone is not news about the port. The
    /// admin saves a setting, or shares a second server, while the first is
    /// still failing: the count and the clock go on, so the screens keep
    /// saying this needs them rather than reverting to a first try that
    /// usually clears in seconds, and no second toast arrives a minute later.
    #[tokio::test]
    async fn a_reconcile_that_keeps_the_port_does_not_start_the_outage_over() {
        let scratch = Scratch::new("e2e-retry-kept");
        let admin = scratch.paths.clone();
        shared_server(&admin).await;
        let (squatter, taken) = squatted();
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: Option<EventSink> = Some(recorder.clone());

        let service = service_retrying(Duration::from_millis(100), Duration::from_millis(250));
        let config = manual(taken, None);
        service.reconcile_with(&admin, &config, sink.clone()).await;
        let stuck = wait_for_retry(&service, &admin, &config, |r| r.needs_you).await;
        assert_eq!(listen_notices(&recorder).len(), 1);

        // Saving the settings with the port untouched — the admin changing
        // the mapping mode, or typing a public address.
        let saved = manual(taken, Some("mc.example.com".into()));
        service.reconcile_with(&admin, &saved, sink.clone()).await;
        let after = service
            .status(&admin, &saved)
            .await
            .retry
            .expect("still failing, so still being tried");
        assert!(after.needs_you, "the alert was taken back: {after:?}");
        assert!(
            after.attempts > stuck.attempts,
            "the count started over: {stuck:?} then {after:?}"
        );

        // And long enough past the threshold for a second alert to have been
        // due, had the clock been restarted.
        wait_for_retry(&service, &admin, &saved, |r| {
            r.attempts >= after.attempts + 4
        })
        .await;
        let told = listen_notices(&recorder);
        assert_eq!(told.len(), 1, "told a second time: {told:?}");

        service.shutdown().await;
        drop(squatter);
    }

    /// The threshold runs from when the listener first failed, not from the
    /// last reconcile. An admin trying things while the port stays taken —
    /// saving the settings again, sharing another server — would otherwise
    /// put the alert off every time they touched anything, and never be told
    /// at all as long as they kept trying.
    #[tokio::test]
    async fn reconciling_while_the_port_stays_taken_does_not_put_the_alert_off() {
        let scratch = Scratch::new("e2e-retry-clock");
        let admin = scratch.paths.clone();
        shared_server(&admin).await;
        let (squatter, taken) = squatted();
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: Option<EventSink> = Some(recorder.clone());

        let service = service_retrying(Duration::from_millis(50), Duration::from_millis(400));
        let config = manual(taken, None);

        // Reconciled over and over for four times the threshold, and never so
        // fast that no attempt fits between two of them.
        let began = std::time::Instant::now();
        while began.elapsed() < Duration::from_millis(1600) {
            service.reconcile_with(&admin, &config, sink.clone()).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let told = listen_notices(&recorder);
        assert_eq!(
            told.len(),
            1,
            "the alert was put off by reconciling: {told:?}"
        );

        service.shutdown().await;
        drop(squatter);
    }

    /// Asking the router again now and then must not forget what it said
    /// before because one ask went unanswered; and before the first ask is
    /// over, nothing has been learned either way.
    #[test]
    fn an_unanswered_ask_keeps_the_address_the_router_gave_before() {
        let public = Detected {
            ip: "203.0.113.7".parse().unwrap(),
            reachability: Reachability::Public,
        };
        let moved = Detected {
            ip: "198.51.100.4".parse().unwrap(),
            ..public
        };
        let router: IpAddr = "192.168.1.1".parse().unwrap();
        let replaced: IpAddr = "10.0.0.1".parse().unwrap();
        let start = Detection::default();
        assert!(!start.asked);

        let unanswered = start.after(None);
        assert_eq!(
            unanswered,
            Detection {
                asked: true,
                found: None,
                gateway: None,
            }
        );
        let answered = unanswered.after(Some((router, public)));
        assert_eq!(answered.found, Some(public));
        assert_eq!(
            answered.gateway,
            Some(router),
            "where the router answered from was thrown away"
        );
        assert_eq!(
            answered.after(None).found,
            Some(public),
            "one lost ask forgot the address"
        );
        assert_eq!(
            answered.after(None).gateway,
            Some(router),
            "one lost ask forgot the router, and with it the address for the house"
        );
        assert_eq!(
            answered.after(Some((replaced, moved))),
            Detection {
                asked: true,
                found: Some(moved),
                gateway: Some(replaced),
            },
            "a new address or a new router was not taken"
        );
    }

    /// Manual router setup holds no mapping — it is the setup an admin lands
    /// in because their router would not open the port — so where the router
    /// is comes from the ask that reads the public address. Without it the
    /// only thing left to ask toward is the way out, which on a machine
    /// holding a VPN open answers with the tunnel: see
    /// [`portmap::local_address`].
    #[test]
    fn where_the_router_is_comes_from_the_mapping_or_from_the_ask_that_read_the_address() {
        let mapped: IpAddr = "192.168.1.1".parse().unwrap();
        let asked: IpAddr = "192.168.4.1".parse().unwrap();
        let detection = |gateway: Option<IpAddr>| {
            Some(Detection {
                asked: true,
                found: None,
                gateway,
            })
        };

        // Automatic setup: the mapping publishes it.
        assert_eq!(gateway_of(Some(mapped), None), Some(mapped));
        // Manual setup: no mapper at all, and the ask carries it.
        assert_eq!(gateway_of(None, detection(Some(asked))), Some(asked));
        // Both, which is a mapping being taken down as the settings change:
        // the mapping is the one still talking to the router.
        assert_eq!(
            gateway_of(Some(mapped), detection(Some(asked))),
            Some(mapped)
        );
        // Nothing has found a router, so there is nothing to ask toward.
        assert_eq!(gateway_of(None, None), None);
        assert_eq!(gateway_of(None, detection(None)), None);
    }

    /// The address for players on this network changes with nothing else: a
    /// DHCP lease renews and this machine moves while the mapping, the public
    /// address, the port and the certificate all stay as they were. Nothing
    /// announces that, so the watcher looks again, and a Share tab left open
    /// is told rather than going on showing an address nobody can reach.
    #[tokio::test]
    async fn an_address_that_moved_on_this_network_is_announced_on_its_own() {
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let sink: Option<EventSink> = Some(recorder.clone());
        let (local, cell) = told(Some("192.168.1.50"));
        let config = manual(25590, Some("203.0.113.7".into()));
        let (publish, detected) = watch::channel(Detection::default());

        let watcher = tokio::spawn(announce(
            None,
            Some(detected),
            Nearby {
                gateway: None,
                local,
                relook: Duration::from_millis(20),
            },
            config,
            None,
            sink,
        ));
        let announced = || -> Vec<serde_json::Value> {
            recorder
                .events()
                .into_iter()
                .filter(|(topic, _)| topic == STATUS_EVENT)
                .map(|(_, payload)| payload)
                .collect()
        };

        // Looking again and finding the same address is not news.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(announced().is_empty(), "{:?}", announced());

        *cell.lock().unwrap() = Some("192.168.1.81".parse().unwrap());
        let moved = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(last) = announced().last() {
                    if last["localAddress"] == serde_json::json!("192.168.1.81:25590") {
                        return last.clone();
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the address that moved was never announced");
        // The rest of the status is what it was, so nothing else had to change
        // for this to be told.
        assert_eq!(moved["address"], serde_json::json!("203.0.113.7:25590"));
        let so_far = announced().len();

        // And still nothing while it stays put.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(announced().len(), so_far, "{:?}", announced());

        // A change something does announce still goes out as it did.
        publish.send_replace(Detection::default().after(None));
        tokio::time::timeout(Duration::from_secs(5), async {
            while announced().len() == so_far {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("an ask that finished was not announced");

        watcher.abort();
    }
}

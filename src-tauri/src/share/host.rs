//! Serving shared servers' packs to players: the admin's side of a join.
//!
//! One TLS listener on one port, for every server this machine shares. A
//! request names no server; the share code in its `Authorization` header is
//! what picks one, so the port reveals nothing about how many servers are
//! behind it and a guessed server id gets nobody anywhere.
//!
//! ```text
//! GET /geode/v1/info   Authorization: Bearer <code>   → PackInfo as JSON
//! GET /geode/v1/pack   Authorization: Bearer <code>   → the client .geodepack
//! ```
//!
//! Anything else is a 404, and a missing or wrong code is a 401 after a short,
//! fixed delay — the same body and the same delay whichever was wrong.
//!
//! This is reachable from the whole internet by design, so it is written to be
//! cheap to refuse: a cap on connections from any one source and on
//! connections in all, both checked before any TLS is spent on them, with room
//! made for a newcomer by dropping the oldest connection that has not shown a
//! code, from the busiest source; short deadlines on the TLS handshake and the
//! request headers; no keep-alive; a queue for building packs; and nothing
//! built or read from disk until a code has matched.
//!
//! A code is not a promise of good behaviour either. It is in every player's
//! link, and links get passed around, so a connection that showed one is held
//! to limits of its own: a download that falls below a slow minimum pace is
//! cut; only so many downloads run at once, from any one source and in all,
//! keeping only so much of the disk; a preview is answered from a description
//! worked out moments ago; and every connection is cut the moment its server's
//! share is no longer the one it showed its code against, or the listener
//! stops.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::TryStreamExt;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Notify, Semaphore};
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;
use tokio_util::io::ReaderStream;

use crate::error::{Error, Result};
use crate::events::{self, EventSink};
use crate::net::Progress;
use crate::paths::Paths;
use crate::server::share::{ServerShare, Shared};
use crate::server::Server;

/// The request paths, shared with the client in [`super::join`].
pub const INFO_PATH: &str = "/geode/v1/info";
pub const PACK_PATH: &str = "/geode/v1/pack";

/// Sent with a [`PackFailed`] when a player's request could not be answered
/// because the server's pack would not build. What the player is told carries
/// no reason at all, so without this nobody but a terminal ever learns of it.
pub const PACK_FAILED_EVENT: &str = "geode://share-pack-failed";

/// Connections one source may hold at once; past this, its new ones are closed
/// unread. A player makes one at a time — the preview, then the download — so
/// this still lets a household behind one address join together, while
/// somebody opening connections and sending nothing gets this many and no
/// more. Not applied to the machine itself or the local network, where one
/// address can be every player at once: see [`stands_for_many`].
const PER_SOURCE: usize = 8;

/// Connections served at once, from every source together, kept well inside
/// the few hundred open files some systems allow a process by default.
///
/// Reaching it does not shut anybody out. A newcomer takes the place of a
/// connection that has not shown a code yet, from whichever source holds the
/// most — so filling the listener to keep a player out takes a connection from
/// each of this many addresses, not [`PER_SOURCE`] from a handful. When every
/// place is held by a connection that did show one, the newcomer takes the
/// oldest of those that is between requests instead, which its client simply
/// opens again; a connection sending a pack or waiting on a description is
/// never taken, and nobody is ever closed unread. Nor can connections that
/// showed a code fill it for long: at most [`MAX_DOWNLOADS`] of them are
/// downloads and at most [`MAX_PREVIEWS`] are waiting on a description, which
/// holds its place until that description has been handed on — worked out for
/// it, or answered from one worked out in the last few seconds, see
/// [`PREVIEW_FRESH`].
const MAX_CONNECTIONS: usize = 128;

/// How long a connection may take to finish TLS, and then to send its request
/// headers. Both short: a real client does either in well under a second even
/// across an ocean, and until it has, nothing says it is a player at all.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

/// The window an answer's pace is measured over. Once the answer has started,
/// it must hand on at least [`MIN_PROGRESS`] in every window of this length, or
/// the connection is cut.
///
/// A download is held open by the player reading it, so without this one that
/// stops reading — or whose network vanished without a word — keeps its
/// connection, and the pack file being sent, for as long as it likes; and so
/// does one that reads a few kilobytes now and then, which asking only for
/// *some* progress would never catch.
///
/// Measured window by window from when the answer started, not from when the
/// connection opened, so a big pack on a slow upload is never cut while it is
/// still moving. Two minutes, because data handed on sits in buffers until the
/// player's side takes it, and a slow link takes a while to empty them. For the
/// same reason an answer that has handed on all of itself is not held to the
/// pace any more — only emptying those buffers is left — but it is cut if that
/// has still not finished a window later.
const STALL_LIMIT: Duration = Duration::from_secs(2 * 60);

/// The least an answer must hand on in each [`STALL_LIMIT`] window: about two
/// kilobytes a second.
///
/// Safely below any real player's link. A 1 Mbit upload — slower than almost
/// any connection an admin shares from — split evenly between
/// [`MAX_DOWNLOADS`] players still gives each nearly twice this, and a player
/// on any ordinary connection takes many times it. What does fall below it is
/// a reader that barely reads, which is exactly what it is there to cut.
const MIN_PROGRESS: u64 = 256 * 1024;

/// Pack downloads at once, from every source together. Well below
/// [`MAX_CONNECTIONS`], so however many a code holder starts, there are always
/// places left for a player's preview and for newcomers to show their code.
///
/// A download counts from the moment its code matches — waiting its turn to be
/// built, being built, being sent — until its connection ends. When every one
/// is taken, a new one is told the server is busy, and nobody is dropped for
/// it, not even the slowest. Players sharing an upload too narrow for all of
/// them are slow together, so dropping the slowest for each new request cuts
/// one of them short every time and none finishes; and it would not keep a
/// code holder out, whose requests still waiting for their packs, or only just
/// started, have no pace to judge.
///
/// So someone with the code who keeps asking can keep these places busy: from
/// eight addresses, at [`DOWNLOADS_PER_SOURCE`] each; from fewer when a pack of
/// several GiB reaches [`OUTGOING_LIMIT`] first; or from one through a tunnel.
/// They still cannot keep a player from a preview, and a new code cuts every
/// one of them at once.
const MAX_DOWNLOADS: usize = 32;

/// Downloads one source may run at once: half the connections it may hold.
/// Enough for a household behind one address to download together, and it
/// means keeping every download place busy takes eight addresses, not four.
/// Like [`PER_SOURCE`], not applied where one address stands for many.
const DOWNLOADS_PER_SOURCE: usize = PER_SOURCE / 2;

/// Previews waiting on a description at once, from every source together.
///
/// A description is worked out one at a time and reused for [`PREVIEW_FRESH`],
/// so a queue deeper than this hands nobody their answer any sooner: it only
/// holds places while it waits. Past it a preview that would have to wait is
/// told the server is busy answering others, at once, the way a download past
/// [`MAX_DOWNLOADS`] is — while one whose description was worked out in the
/// last few seconds joins no queue and is answered as ever.
/// Between them they leave [`MAX_CONNECTIONS`] less this many and
/// [`MAX_DOWNLOADS`] — most of the listener — places that nobody with the code
/// can occupy, however many connections they open.
const MAX_PREVIEWS: usize = 16;

/// Bytes of built packs kept on disk for downloads under way, together.
///
/// Every download sends a pack built for it alone, kept until its connection
/// ends, so without this someone with the code reading each of
/// [`MAX_DOWNLOADS`] downloads at the minimum pace keeps as many copies of a
/// big pack on the admin's disk for days. Enough for every download at once of
/// a pack up to 256 MiB, and for four at once of the largest a player's Geode
/// accepts.
///
/// A download holds room from the moment it counts: as much as its server's
/// pack took when last built, so one there is no room for is refused before
/// its pack is built rather than after; then as much as its own pack takes.
/// Past the limit a new download is told the server is busy, and a pack
/// bigger than the limit by itself is not sent at all.
const OUTGOING_LIMIT: u64 = 8 << 30;

/// How long a server's description is answered from the one last worked out.
///
/// Working one out reads every jar in the server's `mods/` and `client-mods/`,
/// and hashes the ones added by hand and any config sent for its revision,
/// which for a few hundred mods takes a while, and a preview's connection holds
/// its place for all of it — a connection that showed a code is never dropped
/// for a newcomer. Worked out one at a time and reused for this long, a flood
/// of previews costs one read of the mods every few seconds, and is otherwise
/// answered at once.
///
/// A revision answered from here can be a few seconds older than the server,
/// so a check made inside that window can be told nothing has changed when
/// something just did. Nothing is missed by it: what a player keeps is the
/// revision carried by the pack they actually received, never the one a
/// description named, so the next check — seconds later — compares against the
/// server as it is now and updates then.
const PREVIEW_FRESH: Duration = Duration::from_secs(5);

/// Packs being built at once. Building one hashes and compresses a server's
/// mods, which is real work for the admin's disk and processor, and every
/// request for a pack starts one; past this they wait their turn.
const MAX_BUILDS: usize = 2;

/// Added before every refusal of a code. Guessing is already hopeless against
/// fifty bits; this just makes it hopeless more slowly.
const REFUSAL_DELAY: Duration = Duration::from_millis(400);

/// What a download is told when there is no room for it: every download place
/// taken, or every one its address may have, or [`OUTGOING_LIMIT`] reached.
/// Not when to try again, which is when some download ends — in seconds, or in
/// hours for a big pack on a slow upload.
const BUSY: &str = "this server is busy sending its pack to other players — try again later";

/// What a preview is told when every place for one is taken: [`MAX_PREVIEWS`]
/// are already waiting on a description. Not the same wait as [`BUSY`], and not
/// the same words: one description is being worked out for all of them, so
/// asking again in a moment is answered from it.
const DESCRIBING: &str = "this server is busy answering other players — try again in a moment";

/// The limits a code holder's connections are held to, together so a test can
/// shrink them.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// [`STALL_LIMIT`].
    window: Duration,
    /// [`MIN_PROGRESS`].
    min_progress: u64,
    /// [`MAX_DOWNLOADS`].
    downloads: usize,
    /// [`MAX_PREVIEWS`].
    previews: usize,
    /// [`OUTGOING_LIMIT`].
    outgoing: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            window: STALL_LIMIT,
            min_progress: MIN_PROGRESS,
            downloads: MAX_DOWNLOADS,
            previews: MAX_PREVIEWS,
            outgoing: OUTGOING_LIMIT,
        }
    }
}

type Body = UnsyncBoxBody<Bytes, std::io::Error>;

/// What a player sees before deciding to import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackInfo {
    pub name: String,
    pub mc_version: String,
    /// The client loader: `vanilla`, `fabric`, `forge`, `neoforge`.
    pub loader: String,
    pub loader_version: String,
    /// Mods in the pack, counting references as well as bundled jars.
    pub mods: usize,
    /// Of `mods`, how many only the server needs. A player may keep these
    /// turned off. Zero from a Geode older than this.
    #[serde(default)]
    pub server_mods: usize,
    /// The server software, for display: `paper`, `neoforge`, …
    pub server_kind: String,
    /// Changes exactly when what a player receives changes, so a joined
    /// instance can tell whether it is behind. Empty from a Geode older than
    /// this.
    #[serde(default)]
    pub revision: String,
    /// The server's id on the admin's machine, so a player's Geode recognises
    /// the same server under a new password. Empty from a Geode older than
    /// this.
    #[serde(default)]
    pub server_id: String,
    /// How the server should appear in its players' multiplayer list: shown to
    /// a player before they import, and re-read on every Play afterwards.
    ///
    /// This is the carrier that keeps the row in step. It is deliberately
    /// *not* part of `revision`: a rename would otherwise re-download the
    /// whole pack for every player, and the revision already leaves the
    /// server's own name out for that very reason. `None` from a Geode older
    /// than this, and from an admin who has not set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<super::manifest::Listing>,
}

/// A running pack server.
pub struct Host {
    pub local_addr: SocketAddr,
    gate: Arc<Gate>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Host {
    /// Listen on `port` over IPv4 and IPv6 both, and serve until [`Host::stop`].
    ///
    /// Port 0 binds any free port, which is what tests use; the address actually
    /// bound is in `local_addr`.
    ///
    /// `sink` is where what only the admin can put right is reported: a pack
    /// that would not build. Nothing a player does is announced through it.
    pub async fn start(paths: Paths, port: u16, sink: Option<EventSink>) -> Result<Self> {
        Self::start_with(paths, port, Limits::default(), sink).await
    }

    /// [`Host::start`], with limits a test can wait out.
    async fn start_with(
        paths: Paths,
        port: u16,
        limits: Limits,
        sink: Option<EventSink>,
    ) -> Result<Self> {
        let identity = super::identity::Identity::load_or_create(&paths).await?;
        let acceptor = TlsAcceptor::from(identity.server_config()?);

        let listener = listen(port)
            .and_then(TcpListener::from_std)
            .map_err(|e| {
                Error::msg(format!(
                    "could not listen on port {port} for sharing: {e} — another program may be using it"
                ))
            })?;
        let local_addr = listener.local_addr()?;

        // A pack is built to a file per request and removed once sent. A crash
        // mid-send leaves one behind; clear those rather than accumulate them.
        let _ = tokio::fs::remove_dir_all(outgoing(&paths)).await;

        let (stop, mut stopped) = watch::channel(false);
        let gate = Arc::new(Gate {
            limits,
            ..Gate::default()
        });
        let builds = Arc::new(Semaphore::new(MAX_BUILDS));
        let previews = Arc::new(Previews::default());
        let alerts = Arc::new(Alerts {
            sink,
            told: Mutex::default(),
        });
        let admitted_by = gate.clone();

        let task = tokio::spawn(async move {
            loop {
                let (tcp, peer) = tokio::select! {
                    _ = stopped.changed() => break,
                    accepted = listener.accept() => match accepted {
                        Ok(accepted) => accepted,
                        // Out of file descriptors and the like. Pause rather
                        // than spin on an error that will repeat immediately.
                        Err(_) => {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    },
                };
                // Over a limit: close it unread, before any TLS is spent on it.
                let Some(admitted) = gate.admit(peer.ip()) else {
                    continue;
                };
                let admitted = Arc::new(admitted);
                let acceptor = acceptor.clone();
                let paths = paths.clone();
                let (builds, previews, alerts) = (builds.clone(), previews.clone(), alerts.clone());
                tokio::spawn(async move {
                    // Dropping the connection drops the response it was
                    // sending, and with it the pack file that response was
                    // streaming from.
                    tokio::select! {
                        () = serve(
                            tcp, acceptor, paths, builds, previews, alerts, admitted.clone(),
                        ) => {}
                        () = admitted.cut.notified() => {}
                        () = admitted.stalled() => {}
                    }
                });
            }
        });

        Ok(Self {
            local_addr,
            gate: admitted_by,
            stop,
            task,
        })
    }

    /// Cut every connection whose server's share is no longer the one its code
    /// matched: the server unshared, given a new code, or changed in whether
    /// it sends `config/`.
    ///
    /// Shares are read fresh for every request, so a change turns the next
    /// request away, or answers it as changed, without this. What this is for
    /// is the preview or download already under way, which would otherwise run
    /// on with a code the admin has taken back — or send a `config/` the admin
    /// has stopped sending. Nothing is cut when what is shared cannot be read.
    pub async fn cut_revoked(&self, paths: &Paths) {
        if let Ok(shared) = crate::server::share::list(paths).await {
            self.gate.revoke(&shared);
        }
    }

    /// Stop accepting, and cut every connection this host let in. The port is
    /// released as soon as this returns, and a download in flight does not run
    /// on beside whatever listens next.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
        self.gate.cut_all();
    }
}

/// A listening socket on every address, IPv4 and IPv6 alike.
///
/// Settings accepts an IPv6 public address, and a name with only an AAAA
/// record, and an admin behind carrier-grade NAT may be reachable no other
/// way — a listener on IPv4 alone refuses every player those links send. One
/// dual-stack socket rather than a listener per family, so IPv4 players arrive
/// as IPv4-mapped addresses through the same accept loop and are counted
/// against the same limits. `IPV6_V6ONLY` is cleared by hand: Linux leaves it
/// off, but Windows turns it on.
///
/// A machine with no IPv6 at all cannot make that socket, and gets IPv4 alone
/// rather than no sharing. Any other failure — the port being taken, above
/// all — is reported as it is, not taken as a reason to fall back to IPv4 and
/// quietly refuse the players who arrive over IPv6.
fn listen(port: u16) -> std::io::Result<std::net::TcpListener> {
    let v4 = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    let dual = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))
        .and_then(|socket| socket.set_only_v6(false).map(|()| socket));
    match dual {
        Ok(socket) => match bound(socket, SocketAddr::from((Ipv6Addr::UNSPECIFIED, port))) {
            Err(e) if e.kind() == ErrorKind::AddrNotAvailable => {
                bound(Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?, v4)
            }
            listening => listening,
        },
        Err(_) => bound(Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?, v4),
    }
}

fn bound(socket: Socket, addr: SocketAddr) -> std::io::Result<std::net::TcpListener> {
    // What tokio's own bind does, and for the same reason: a port given up a
    // moment ago, its last connections still waiting out TIME_WAIT, can be
    // listened on again. Not on Windows, where the same option lets a second
    // program take a port that is in use.
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

/// Connections being served, oldest first.
#[derive(Default)]
struct Gate {
    occupancy: Mutex<Occupancy>,
    limits: Limits,
}

#[derive(Default)]
struct Occupancy {
    next_id: u64,
    /// How many times shares have been checked again by [`Gate::revoke`], so a
    /// request can tell that one was while it looked its own code up.
    revocations: u64,
    /// At most [`MAX_CONNECTIONS`], so a scan of it is cheap.
    places: Vec<Place>,
    /// Bytes each server's pack took when last built, by server id: the room
    /// a new download of it holds on disk until its own pack is built.
    sizes: HashMap<String, u64>,
}

struct Place {
    id: u64,
    source: IpAddr,
    /// Whether the source can be many players at once, so is not held to one
    /// source's share: see [`stands_for_many`].
    many: bool,
    /// What the connection's code opened, once it showed one that did: a
    /// player, never dropped to make room for anyone, and cut once that server
    /// is no longer shared as it was.
    proof: Option<Proof>,
    /// Asked for a pack with that code, so counts against [`MAX_DOWNLOADS`].
    downloading: bool,
    /// Waiting on its server's description, so counts against
    /// [`MAX_PREVIEWS`], until that description has been handed on.
    describing: bool,
    /// Bytes of disk held for the pack it sends, which stays there until the
    /// connection ends: counted against [`OUTGOING_LIMIT`].
    holds: u64,
    cut: Arc<Notify>,
}

/// The shared server a connection's code matched, and its share as it was
/// then.
struct Proof {
    server: String,
    share: ServerShare,
}

/// How much of its answer a connection has handed on, window by window.
#[derive(Default)]
struct Flow {
    /// When the current window began: when the answer started, then the end of
    /// each window after it. `None` until the answer starts.
    window_began: Option<Instant>,
    /// Bytes handed on in the current window.
    this_window: u64,
    /// When the whole answer had been handed on, leaving only what sits in
    /// buffers on its way to the player.
    finished: Option<Instant>,
}

impl Occupancy {
    /// Bytes of disk every download together holds for its pack.
    fn held(&self) -> u64 {
        self.places.iter().map(|place| place.holds).sum()
    }

    /// Take a place away, and tell its connection to end.
    fn cut(&mut self, index: usize) {
        self.places.remove(index).cut.notify_one();
    }
}

impl Gate {
    fn lock(&self) -> MutexGuard<'_, Occupancy> {
        self.occupancy.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Let a connection from `peer` in, unless its source already holds its
    /// share, or every place is taken by a player.
    ///
    /// When the listener is full, the connection dropped for the newcomer is
    /// the oldest without a code from the source holding the most. Someone
    /// filling the listener from a few addresses loses their own connections
    /// first, and a player's one connection outlasts every source with two.
    /// When every place is held by a connection that did show a code, the
    /// oldest between requests goes instead — never one sending a pack or
    /// waiting on a description. There is always one of those to take:
    /// [`MAX_DOWNLOADS`] and [`MAX_PREVIEWS`] together stay well inside
    /// [`MAX_CONNECTIONS`], so the two of them can never hold every place.
    /// Raise either until they can, and a newcomer is closed unread again.
    ///
    /// A source is ranked by what it holds up to [`PER_SOURCE`], never past
    /// it: a source that stands for many skips that cap, and without the
    /// clamp one of those holding nine would outrank every address in the
    /// world, which can hold eight, and lose its newcomers first.
    fn admit(self: &Arc<Self>, peer: IpAddr) -> Option<Admitted> {
        let (source, many) = (source_of(peer), stands_for_many(peer.to_canonical()));
        let mut occupancy = self.lock();
        let held_by =
            |places: &[Place], source: IpAddr| places.iter().filter(|p| p.source == source).count();

        if !many && held_by(&occupancy.places, source) >= PER_SOURCE {
            return None;
        }
        if occupancy.places.len() >= MAX_CONNECTIONS {
            let oldest_from_busiest = |places: &[Place], spare: fn(&Place) -> bool| {
                places
                    .iter()
                    .enumerate()
                    .filter(|(_, place)| spare(place))
                    .max_by_key(|(i, place)| {
                        (held_by(places, place.source).min(PER_SOURCE), Reverse(*i))
                    })
                    .map(|(i, _)| i)
            };
            let spare = oldest_from_busiest(&occupancy.places, |place| place.proof.is_none())
                .or_else(|| {
                    oldest_from_busiest(&occupancy.places, |place| {
                        !place.downloading && !place.describing
                    })
                })?;
            occupancy.cut(spare);
        }

        let id = occupancy.next_id;
        occupancy.next_id += 1;
        let cut = Arc::new(Notify::new());
        occupancy.places.push(Place {
            id,
            source,
            many,
            proof: None,
            downloading: false,
            describing: false,
            holds: 0,
            cut: cut.clone(),
        });
        Some(Admitted {
            gate: self.clone(),
            id,
            cut,
            flow: Mutex::default(),
        })
    }

    fn revocations(&self) -> u64 {
        self.lock().revocations
    }

    /// Cut every connection whose server is no longer shared as it was when
    /// its code matched, going by `shared`: with another code, or changed in
    /// whether it sends `config/`, which a pack built before the change may
    /// hold. Turning that on cuts too, rather than keep sending packs without
    /// what the admin now sends.
    ///
    /// A server missing from `shared` has been unshared — unless it is missing
    /// because its own share file could not be read. Its players are left be
    /// then, the way the listener is left running, rather than cut off from a
    /// server that may well still be shared. Another server's unreadable file
    /// says nothing about this one. A share that was changed is always known
    /// for what it is.
    fn revoke(&self, shared: &Shared) {
        let mut occupancy = self.lock();
        occupancy.revocations += 1;
        let still_opens =
            |proof: &Proof| match shared.servers.iter().find(|(id, _)| *id == proof.server) {
                Some((_, share)) => *share == proof.share,
                None => shared.unreadable.contains(&proof.server),
            };
        occupancy.places.retain(|place| {
            let keep = place.proof.as_ref().map_or(true, still_opens);
            if !keep {
                place.cut.notify_one();
            }
            keep
        });
    }

    /// Cut every connection, for a listener that is stopping.
    fn cut_all(&self) {
        for place in self.lock().places.drain(..) {
            place.cut.notify_one();
        }
    }
}

/// A connection's place at the [`Gate`], given back when it is dropped.
struct Admitted {
    gate: Arc<Gate>,
    id: u64,
    /// Told when the place is taken away: given to a newcomer, its server no
    /// longer shared as it was, or the listener stopped.
    cut: Arc<Notify>,
    flow: Mutex<Flow>,
}

impl Admitted {
    /// The connection showed a code that opens `server`, shared as `share`.
    ///
    /// Refused — look the code up again — when shares were checked again since
    /// `revocations` was read, which is done before the lookup. The lookup may
    /// have found a share the admin changed just before that check, and the
    /// check came too early to find this connection holding it; taking it as
    /// proof now would let a download run on with a code already taken back.
    fn prove(&self, server: &str, share: &ServerShare, revocations: u64) -> bool {
        let mut occupancy = self.gate.lock();
        if occupancy.revocations != revocations {
            return false;
        }
        if let Some(place) = occupancy
            .places
            .iter_mut()
            .find(|place| place.id == self.id)
        {
            place.proof = Some(Proof {
                server: server.to_string(),
                share: share.clone(),
            });
        }
        true
    }

    /// Count the connection as a download, holding room on disk for a pack as
    /// big as its server's was when last built.
    ///
    /// False, with nothing counted, when every download place is taken, or
    /// every one its source may have, or that room is not there — or when this
    /// connection has lost its own place already.
    fn start_download(&self) -> bool {
        let limits = self.gate.limits;
        let mut occupancy = self.gate.lock();
        let Some(index) = occupancy
            .places
            .iter()
            .position(|place| place.id == self.id)
        else {
            return false;
        };
        let place = &occupancy.places[index];
        let expected = place
            .proof
            .as_ref()
            .and_then(|proof| occupancy.sizes.get(&proof.server))
            .copied()
            .unwrap_or(0);
        let downloads = occupancy.places.iter().filter(|p| p.downloading);
        let from_source = downloads.clone().filter(|p| p.source == place.source);
        if downloads.count() >= limits.downloads
            || (!place.many && from_source.count() >= DOWNLOADS_PER_SOURCE)
            || occupancy.held() + expected > limits.outgoing
        {
            return false;
        }
        let place = &mut occupancy.places[index];
        place.downloading = true;
        place.holds = expected;
        true
    }

    /// Count the connection as a preview waiting on its server's description.
    ///
    /// False, with nothing counted, when every preview place is taken — or
    /// when this connection has lost its own place already.
    fn start_preview(&self) -> bool {
        let limits = self.gate.limits;
        let mut occupancy = self.gate.lock();
        if occupancy.places.iter().filter(|p| p.describing).count() >= limits.previews {
            return false;
        }
        let Some(place) = occupancy
            .places
            .iter_mut()
            .find(|place| place.id == self.id)
        else {
            return false;
        };
        place.describing = true;
        true
    }

    /// The description has been handed on, so the connection stops counting:
    /// an answered preview holds no place for the next one.
    fn finish_preview(&self) {
        let mut occupancy = self.gate.lock();
        if let Some(place) = occupancy
            .places
            .iter_mut()
            .find(|place| place.id == self.id)
        {
            place.describing = false;
        }
    }

    /// Hold room on disk for this download's built pack, `bytes` long, in place
    /// of what it held until now, and remember the size for the next download
    /// of its server.
    ///
    /// False, with the pack not to be sent, when that would take what every
    /// download holds past [`OUTGOING_LIMIT`], or when this connection has lost
    /// its own place already.
    fn hold(&self, bytes: u64) -> bool {
        let limits = self.gate.limits;
        let mut occupancy = self.gate.lock();
        let Some(index) = occupancy
            .places
            .iter()
            .position(|place| place.id == self.id)
        else {
            return false;
        };
        if let Some(proof) = &occupancy.places[index].proof {
            let server = proof.server.clone();
            occupancy.sizes.insert(server, bytes);
        }
        if occupancy.held() - occupancy.places[index].holds + bytes > limits.outgoing {
            return false;
        }
        occupancy.places[index].holds = bytes;
        true
    }

    fn flow(&self) -> MutexGuard<'_, Flow> {
        self.flow.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The answer handed on `bytes` more.
    fn sent(&self, bytes: usize) {
        let mut flow = self.flow();
        flow.window_began.get_or_insert_with(Instant::now);
        flow.this_window += bytes as u64;
    }

    /// The answer has handed on all of itself.
    fn finished(&self) {
        self.flow().finished.get_or_insert_with(Instant::now);
    }

    /// Finishes when the connection has fallen too far behind to keep: a whole
    /// window in which its answer handed on less than the minimum, or a whole
    /// window since it handed on the last of itself.
    ///
    /// Before the answer starts there is no pace to judge and no deadline
    /// here. [`HANDSHAKE_TIMEOUT`] and [`HEADER_TIMEOUT`] end that wait while
    /// the request has not even been read; after it, the build finishing or
    /// the mods being read is what ends it, and what is bounded is not how
    /// long one connection waits but how many can be waiting at all — the
    /// [`MAX_BUILDS`] queue a pack waits in, the [`MAX_PREVIEWS`] queue a
    /// description waits in. A deadline of its own here would cut the preview
    /// of a large cold server, or a pack queued behind a long build, while
    /// both were going exactly as they should.
    async fn stalled(&self) {
        let Limits {
            window,
            min_progress,
            ..
        } = self.gate.limits;
        loop {
            let wake = {
                let mut flow = self.flow();
                let now = Instant::now();
                match (flow.finished, flow.window_began) {
                    (Some(finished), _) if now >= finished + window => return,
                    (Some(finished), _) => finished + window,
                    (None, None) => now + window,
                    (None, Some(began)) if now < began + window => began + window,
                    (None, Some(_)) if flow.this_window < min_progress => return,
                    (None, Some(began)) => {
                        flow.this_window = 0;
                        flow.window_began = Some(began + window);
                        began + window * 2
                    }
                }
            };
            tokio::time::sleep_until(wake).await;
        }
    }
}

impl Drop for Admitted {
    fn drop(&mut self) {
        let mut occupancy = self.gate.lock();
        // Not there if it was given to a newcomer.
        occupancy.places.retain(|place| place.id != self.id);
    }
}

/// The source a connection is counted against.
///
/// An IPv4 player reaches the dual-stack listener as an IPv4-mapped IPv6
/// address, and is turned back into IPv4 here. That matters twice over: a host
/// counts the same however it arrives, and every IPv4 address in the world is
/// not lumped into the one `::ffff:0:0/56` prefix below.
///
/// IPv6 is counted by `/56`, not by address. A home is commonly handed a `/56`
/// and a machine a `/64` inside one, and every address in them is that
/// subscriber's to use, so counting by anything narrower would let one of them
/// take a source's share many times over just by connecting from as many
/// addresses as it has.
fn source_of(peer: IpAddr) -> IpAddr {
    match peer.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 72))),
        v4 => v4,
    }
}

/// Whether one address here can be many players at once, so is not held to
/// [`PER_SOURCE`]: this machine, which is where a tunnel's agent or a reverse
/// proxy on it connects from, and the local network, where a proxy on another
/// machine, a router looping players back in, or a LAN party all arrive as
/// one address. Nobody on the internet can connect from one of these, except
/// through something the admin set up.
fn stands_for_many(peer: IpAddr) -> bool {
    match peer {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            // Unique local `fc00::/7`, and link-local `fe80::/10`.
            v6.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
    }
}

/// One connection, start to finish: the handshake, its one request, the answer.
async fn serve(
    tcp: TcpStream,
    acceptor: TlsAcceptor,
    paths: Paths,
    builds: Arc<Semaphore>,
    previews: Arc<Previews>,
    alerts: Arc<Alerts>,
    admitted: Arc<Admitted>,
) {
    let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await else {
        return;
    };
    let service = hyper::service::service_fn(move |request| {
        let (paths, builds, previews, alerts, admitted) = (
            paths.clone(),
            builds.clone(),
            previews.clone(),
            alerts.clone(),
            admitted.clone(),
        );
        async move {
            let response = respond(&paths, &builds, &previews, &alerts, &admitted, request).await;
            Ok::<_, Infallible>(response.map(|body| watched(body, admitted)))
        }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .keep_alive(false)
        .serve_connection(TokioIo::new(tls), service)
        .await;
}

/// A body that counts what is taken from it as handed on — which is only as
/// fast as the player reads, since nothing more is taken while what was
/// already sent sits unread — and marks the answer finished when it is
/// dropped, which the connection does once it has taken all of it.
fn watched(body: Body, admitted: Arc<Admitted>) -> Body {
    let finish = Finish(admitted);
    body.map_frame(move |frame| {
        if let Some(data) = frame.data_ref() {
            finish.0.sent(data.len());
        }
        frame
    })
    .boxed_unsync()
}

/// Marks an answer finished when dropped, along with the body it rides in.
struct Finish(Arc<Admitted>);

impl Drop for Finish {
    fn drop(&mut self) {
        self.0.finished();
    }
}

async fn respond(
    paths: &Paths,
    builds: &Arc<Semaphore>,
    previews: &Previews,
    alerts: &Alerts,
    admitted: &Admitted,
    request: Request<Incoming>,
) -> Response<Body> {
    let path = request.uri().path();
    if request.method() != Method::GET || (path != INFO_PATH && path != PACK_PATH) {
        return message(StatusCode::NOT_FOUND, "not found");
    }
    let want_pack = path == PACK_PATH;

    let server = loop {
        let revocations = admitted.gate.revocations();
        let Some((id, share, server)) = shared_server(paths, bearer(&request)).await else {
            tokio::time::sleep(REFUSAL_DELAY).await;
            return message(
                StatusCode::UNAUTHORIZED,
                "that password is not accepted here",
            );
        };
        if admitted.prove(&id, &share, revocations) {
            break server;
        }
    };

    // Said in the server's own words, and only after the code matched: it is
    // written for the player, and it contains nothing about this machine.
    if let Err(e) = super::pack::client_loader(&server) {
        return message(StatusCode::CONFLICT, &e.to_string());
    }

    // Said at once, with no delay: this player's code is good, and the sooner
    // they know to come back, the sooner they do.
    if want_pack && !admitted.start_download() {
        return message(StatusCode::SERVICE_UNAVAILABLE, BUSY);
    }

    let outcome = if want_pack {
        pack(paths, &server, builds).await.and_then(|response| {
            let bytes = response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|length| length.to_str().ok()?.parse().ok())
                .expect("a pack is sent with its length");
            // Never sent, however few others are downloading, so not "busy":
            // only the admin can do anything about it.
            let limit = admitted.gate.limits.outgoing;
            if bytes > limit {
                return Err(Error::msg(format!(
                    "its pack is {bytes} bytes, more than the {limit} kept on disk for downloads"
                )));
            }
            // Refused, the response goes, and the pack file with it.
            Ok(if admitted.hold(bytes) {
                response
            } else {
                message(StatusCode::SERVICE_UNAVAILABLE, BUSY)
            })
        })
    } else if let Some(info) = previews.fresh(&server.id) {
        // Already worked out, and handed over without waiting on anything, so
        // it costs no preview place: the ceiling is on the queue behind the
        // description being worked out, and this request never joins it.
        // However deep that queue is, a player whose answer is in hand gets it.
        Ok(json(StatusCode::OK, &info))
    } else {
        if !admitted.start_preview() {
            return message(StatusCode::SERVICE_UNAVAILABLE, DESCRIBING);
        }
        let (paths, described) = (paths.clone(), server.clone());
        let work = async move { super::pack::server_pack_info(&paths, &described).await };
        let answer = previews.describe(&server.id, work).await;
        // As soon as it has an answer, not when its connection ends: the next
        // preview waiting to be let in can have this place now. From here
        // until the answer is written this connection is a candidate for a
        // newcomer's place, and losing it there costs a description it has to
        // ask for again — cheaper than holding one of the few preview places
        // for as long as a code holder chooses not to read their answer.
        admitted.finish_preview();
        answer.map(|info| json(StatusCode::OK, &info))
    };
    outcome.unwrap_or_else(|e| {
        // The real reason stays on this machine. It can name paths under the
        // admin's home directory, which are nobody else's business — so it
        // goes to the terminal for a headless run, and to the admin's own
        // screen, and nowhere near the answer below.
        eprintln!("geode: sharing {} failed: {e}", server.id);
        alerts.failed(&server, &e.to_string());
        message(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the server could not prepare its pack — ask its admin to check Geode",
        )
    })
}

/// The code a request presented, if it presented one.
fn bearer(request: &Request<Incoming>) -> Option<&str> {
    request
        .headers()
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// The shared server this code opens, read fresh from disk so sharing,
/// unsharing and a new code take effect on the very next request.
///
/// Read from the share files, which nothing but sharing writes, rather than
/// from `server.json`, which a backup or an install can rewrite with a copy
/// loaded before the code last changed.
///
/// Comes with the server's id as sharing knows it and the share it has, which
/// is what [`Gate::revoke`] later checks the connection against.
async fn shared_server(paths: &Paths, code: Option<&str>) -> Option<(String, ServerShare, Server)> {
    let code = code?;
    let shared = crate::server::share::list(paths).await.ok()?;
    let (id, share) = shared
        .servers
        .into_iter()
        .find(|(_, share)| share.code.matches(code))?;
    let server = crate::server::load(paths, &id).await.ok()?;
    Some((id, share, server))
}

/// How often one server's failures are worth saying again.
///
/// The notice is a toast, which is gone from the screen in seconds; a rule
/// that said it once a listener run would make an admin who was away from
/// their machine for those seconds as uninformed as the `eprintln` left them,
/// which is the silent failure this exists to end. A minute is long enough
/// that a server failing every player over and over is still one notice they
/// can read, and short enough that the next player to press Play tells an
/// admin who has come back.
const ALERT_AGAIN_AFTER: Duration = Duration::from_secs(60);

/// What the admin is told when a server cannot answer its players.
///
/// A player whose pack will not build is told only to ask the admin, so this
/// is the whole of what the admin gets. Every joined player's Geode asks again
/// each time they press Play, and a server that cannot build its pack fails
/// every one of them — so **each server is named at most once every
/// [`ALERT_AGAIN_AFTER`]**, which is enough to say that it is broken and is
/// not a toast per player.
struct Alerts {
    sink: Option<EventSink>,
    /// By server id, when each was last reported.
    told: Mutex<HashMap<String, Instant>>,
}

/// Why a server could not answer, for the admin's own screen.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackFailed {
    server_id: String,
    server_name: String,
    /// The reason in full, paths and all: this goes nowhere but here.
    error: String,
}

impl Alerts {
    fn failed(&self, server: &Server, error: &str) {
        {
            let mut told = self.told.lock().unwrap_or_else(|p| p.into_inner());
            let now = Instant::now();
            if told
                .get(&server.id)
                .is_some_and(|last| now.duration_since(*last) < ALERT_AGAIN_AFTER)
            {
                return;
            }
            told.insert(server.id.clone(), now);
        }
        events::emit(
            &self.sink,
            PACK_FAILED_EVENT,
            &PackFailed {
                server_id: server.id.clone(),
                server_name: server.name.clone(),
                error: error.to_string(),
            },
        );
    }
}

/// Shared servers' descriptions, as last worked out.
#[derive(Default)]
struct Previews {
    /// By server id, with when each was worked out.
    known: Arc<Mutex<HashMap<String, (Instant, PackInfo)>>>,
    /// Held while a description is worked out, so only one is at a time.
    working: Arc<tokio::sync::Mutex<()>>,
}

impl Previews {
    /// `server`'s description: the one last worked out, if that was within
    /// [`PREVIEW_FRESH`], or else what `work` finds once no other description
    /// is being worked out.
    ///
    /// The work is a task of its own that keeps its turn, and keeps what it
    /// found, even once the request waiting on it is dropped. It reads the
    /// server's mods on a blocking thread, which dropping a future does not
    /// stop: a work owned by the request would hand its turn on while that
    /// thread still read, so hanging up over and over would read the mods as
    /// many times at once.
    async fn describe<W>(&self, server: &str, work: W) -> Result<PackInfo>
    where
        W: Future<Output = Result<PackInfo>> + Send + 'static,
    {
        if let Some(info) = self.fresh(server) {
            return Ok(info);
        }
        let turn = self.working.clone().lock_owned().await;
        // Whoever had the turn before may have just worked this one out.
        if let Some(info) = self.fresh(server) {
            return Ok(info);
        }
        let (known, server) = (self.known.clone(), server.to_string());
        tokio::spawn(async move {
            let _turn = turn;
            let info = work.await?;
            known
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(server, (Instant::now(), info.clone()));
            Ok(info)
        })
        .await
        .map_err(|e| Error::msg(format!("describing the server stopped: {e}")))?
    }

    fn fresh(&self, server: &str) -> Option<PackInfo> {
        let known = self.known.lock().unwrap_or_else(|p| p.into_inner());
        let (at, info) = known.get(server)?;
        (at.elapsed() < PREVIEW_FRESH).then(|| info.clone())
    }
}

/// Build the server's pack and stream it, removing the file once it is sent.
async fn pack(paths: &Paths, server: &Server, builds: &Arc<Semaphore>) -> Result<Response<Body>> {
    // Waiting for a turn is given up along with the request, so a player who
    // hangs up while queued starts no build.
    let turn = builds
        .clone()
        .acquire_owned()
        .await
        .expect("the build queue is never closed");
    let dir = outgoing(paths);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}-{}.geodepack", server.id, uuid::Uuid::new_v4()));

    // Once started, the build is a task of its own that keeps its turn and its
    // file's cleanup until it has truly finished. The pack is written on a
    // blocking thread, which dropping a future does not stop: a build owned by
    // the request would give up its turn while that thread was still writing,
    // so hanging up mid-build would start builds without limit, and its file
    // could be removed before the thread created it and then left behind.
    let build = tokio::spawn({
        let (paths, server, path) = (paths.clone(), server.clone(), path.clone());
        async move {
            let _turn = turn;
            let cleanup = TempFile(path.clone());
            super::pack::export_server(&paths, &server, path, &Progress::silent("share"))
                .await
                .map(|()| cleanup)
        }
    });
    let cleanup = build
        .await
        .map_err(|e| Error::msg(format!("building the pack stopped: {e}")))??;

    let file = tokio::fs::File::open(&path).await?;
    let len = file.metadata().await?.len();
    // The cleanup rides inside the stream, so the file lives exactly as long as
    // the response that is sending it — through a slow download, and no longer.
    let stream = ReaderStream::new(file).map_ok(move |chunk| {
        let _held = &cleanup;
        Frame::data(chunk)
    });

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/zip")
        .header(CONTENT_LENGTH, len)
        .header(CACHE_CONTROL, "no-store")
        .body(StreamBody::new(stream).boxed_unsync())
        .expect("a response built from fixed, valid parts"))
}

fn outgoing(paths: &Paths) -> PathBuf {
    paths.share_dir().join("outgoing")
}

/// Removes a file when dropped.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn json<T: Serialize>(status: StatusCode, value: &T) -> Response<Body> {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .header(CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::from(bytes)).map_err(|never| match never {}).boxed_unsync())
        .expect("a response built from fixed, valid parts")
}

fn message(status: StatusCode, text: &str) -> Response<Body> {
    json(status, &serde_json::json!({ "error": text }))
}

#[cfg(test)]
pub(super) mod tests {
    //! The listener against the connections that would keep players out: one
    //! source holding every slot, a download nobody reads or reads barely at
    //! all, a crowd of pack requests, a code taken back while it is still in
    //! use — and a player who can only reach it over IPv6.

    use std::time::Instant;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::paths::Scratch;
    use crate::server::ServerKind;
    use crate::share::code::Code;
    use crate::share::identity::Identity;
    use crate::share::link::Pin;
    use crate::share::trust::{Trust, TrustedClient};

    /// A shared Fabric server on disk, with `filler` bytes of a jar that does
    /// not compress.
    async fn share_a_server(paths: &Paths, filler: usize) -> (Server, Code) {
        let mut server = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
        server.id = "survival".into();
        server.build = "0.16.9".into();
        let code = Code::generate();
        if filler > 0 {
            let mut bytes = vec![0u8; filler];
            getrandom::fill(&mut bytes).unwrap();
            let mods = paths.server_run(&server.id).join("mods");
            tokio::fs::create_dir_all(&mods).await.unwrap();
            tokio::fs::write(mods.join("filler.jar"), bytes).await.unwrap();
        }
        server.save(paths).await.unwrap();
        crate::server::share::save(
            paths,
            &server.id,
            &ServerShare {
                code: code.clone(),
                include_config: false,
                listing: None,
                shared: true,
            },
        )
        .await
        .unwrap();
        (server, code)
    }

    async fn pin_of(paths: &Paths) -> Pin {
        Identity::load_or_create(paths).await.unwrap().pin()
    }

    /// What a player's Geode asks first, the way it asks: pinned TLS, the code
    /// as a bearer token.
    async fn info(host: &str, port: u16, pin: Pin, code: &Code) -> reqwest::Result<PackInfo> {
        TrustedClient::new(Trust::Pinned(pin))
            .unwrap()
            .client
            .get(format!("https://{host}:{port}{INFO_PATH}"))
            .bearer_auth(code.raw())
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }

    pub(crate) fn outgoing_files(paths: &Paths) -> usize {
        std::fs::read_dir(outgoing(paths)).map_or(0, |entries| entries.count())
    }

    /// A TCP connection to the host on loopback, made from `local`.
    fn connect_from(local: Ipv4Addr, port: u16, recv_buffer: Option<usize>) -> TcpStream {
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
        if let Some(size) = recv_buffer {
            // Before connecting, or the window it offers is already agreed.
            socket.set_recv_buffer_size(size).unwrap();
        }
        socket.bind(&SocketAddr::from((local, 0)).into()).unwrap();
        socket
            .connect(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)).into())
            .unwrap();
        socket.set_nonblocking(true).unwrap();
        TcpStream::from_std(socket.into()).unwrap()
    }

    /// Whether the host has closed this connection, waiting up to `wait` to
    /// find out. The host never speaks first, so anything but silence is it
    /// hanging up.
    async fn closed_within(socket: &mut TcpStream, wait: Duration) -> bool {
        tokio::time::timeout(wait, socket.read(&mut [0u8; 1])).await.is_ok()
    }

    /// Takes any certificate. The test using it is about how long the host
    /// keeps a connection; trusting the host is `trust.rs`'s to test.
    #[derive(Debug)]
    struct AnyCertificate;

    impl ServerCertVerifier for AnyCertificate {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> std::result::Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    /// TLS to the host over a connection whose receive buffer is `recv_buffer`
    /// bytes, so a reader that stops reading stops the host's sending soon
    /// after — rather than after however many megabytes loopback will buffer.
    async fn tls_to(port: u16, recv_buffer: usize) -> tokio_rustls::client::TlsStream<TcpStream> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate))
            .with_no_client_auth();
        let tcp = connect_from(Ipv4Addr::LOCALHOST, port, Some(recv_buffer));
        tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("geode-share.invalid").unwrap(), tcp)
            .await
            .unwrap()
    }

    /// Read a response up to the end of its headers. Returns the headers and
    /// how many bytes of body came with them.
    async fn read_head(tls: &mut tokio_rustls::client::TlsStream<TcpStream>) -> (String, usize) {
        let mut received = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = tls.read(&mut chunk).await.unwrap();
            assert!(n > 0, "the host hung up before answering");
            received.extend_from_slice(&chunk[..n]);
            if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&received[..end]).to_string();
                return (head, received.len() - end - 4);
            }
        }
    }

    /// Ask for the pack over a connection with a small receive buffer, and read
    /// only its headers — so the host is left with the download under way,
    /// waiting on a reader. Returns the connection, how many bytes of the pack
    /// have arrived, and how many there are.
    pub(crate) async fn ask_for_pack(
        port: u16,
        code: &Code,
    ) -> (tokio_rustls::client::TlsStream<TcpStream>, usize, usize) {
        let mut tls = tls_to(port, 4096).await;
        let request = format!(
            "GET {PACK_PATH} HTTP/1.1\r\nHost: geode\r\nAuthorization: Bearer {}\r\n\r\n",
            code.raw()
        );
        tls.write_all(request.as_bytes()).await.unwrap();
        let (head, received) = read_head(&mut tls).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let length = head
            .lines()
            .find_map(|line| {
                let line = line.to_ascii_lowercase();
                line.strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .expect("the pack's length is sent");
        (tls, received, length)
    }

    /// Read until the host is done sending. Returns how many bytes of the body
    /// arrived in all, and the error the end came with: `UnexpectedEof` for a
    /// connection cut without TLS's goodbye.
    pub(crate) async fn read_rest(
        tls: &mut tokio_rustls::client::TlsStream<TcpStream>,
        mut received: usize,
    ) -> (usize, Option<ErrorKind>) {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match tls.read(&mut chunk).await {
                Ok(0) => return (received, None),
                Ok(n) => received += n,
                Err(e) => return (received, Some(e.kind())),
            }
        }
    }

    /// Wait for every pack file to be removed, which happens once the response
    /// sending it is dropped.
    async fn outgoing_files_removed(paths: &Paths) -> bool {
        let waiting = Instant::now();
        while outgoing_files(paths) > 0 {
            if waiting.elapsed() > Duration::from_secs(5) {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    #[test]
    fn a_source_is_one_ipv4_address_or_one_ipv6_prefix() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();

        // IPv4 through the dual-stack listener is IPv4, one address at a time.
        assert_eq!(source_of(ip("::ffff:203.0.113.7")), ip("203.0.113.7"));
        assert_eq!(source_of(ip("203.0.113.7")), ip("203.0.113.7"));
        assert_ne!(
            source_of(ip("::ffff:203.0.113.7")),
            source_of(ip("::ffff:203.0.113.8"))
        );

        // IPv6 is one /56, however many addresses inside it connect.
        assert_eq!(source_of(ip("2001:db8:1:2::5")), ip("2001:db8:1::"));
        assert_eq!(
            source_of(ip("2001:db8:1:2::5")),
            source_of(ip("2001:db8:1:ff:1:2:3:4"))
        );
        assert_ne!(
            source_of(ip("2001:db8:1:2::5")),
            source_of(ip("2001:db8:1:100::5"))
        );
    }

    /// A source's share, and a place given back when its connection ends —
    /// without which every source would be shut out for good after its first
    /// few.
    #[test]
    fn the_gate_counts_by_source_and_forgets_what_has_left() {
        let gate = Arc::new(Gate::default());
        let one: IpAddr = "::ffff:198.51.100.1".parse().unwrap();

        let mut from_one: Vec<Admitted> =
            (0..PER_SOURCE).map(|_| gate.admit(one).unwrap()).collect();
        assert!(gate.admit(one).is_none(), "one source past its limit");
        assert!(
            gate.admit("198.51.100.1".parse().unwrap()).is_none(),
            "the same source over IPv4"
        );
        assert!(
            gate.admit("198.51.100.2".parse().unwrap()).is_some(),
            "another source"
        );
        drop(from_one.pop());
        assert!(
            gate.admit(one).is_some(),
            "a place given back is free again"
        );
        drop(from_one);

        assert!(gate.lock().places.is_empty());
    }

    /// Everyone arriving through a tunnel's agent or a proxy comes from one
    /// address, and so does a household behind a router that loops them back
    /// in; holding those to one source's share would let eight players in.
    #[test]
    fn this_machine_and_the_local_network_are_not_held_to_one_sources_share() {
        for address in [
            "127.0.0.1",
            "::ffff:127.0.0.1",
            "::1",
            "192.168.1.20",
            "10.0.0.5",
            "172.16.4.1",
            "169.254.3.3",
            "fd12:3456::1",
            "fe80::1",
        ] {
            let gate = Arc::new(Gate::default());
            let held: Vec<Option<Admitted>> = (0..PER_SOURCE * 4)
                .map(|_| gate.admit(address.parse().unwrap()))
                .collect();
            assert!(held.iter().all(Option::is_some), "{address} was limited");
        }
        for address in ["203.0.113.1", "100.64.0.1", "2001:db8::1", "::"] {
            assert!(!stands_for_many(address.parse().unwrap()), "{address}");
        }
    }

    /// Whether the connection has been told to end. Asking takes the telling
    /// with it, so each connection is asked once.
    fn cut_off(admitted: &Admitted) -> bool {
        use futures::FutureExt;
        admitted.cut.notified().now_or_never().is_some()
    }

    /// A share with `code`, sending no `config/`.
    fn shared_with(code: &Code) -> ServerShare {
        ServerShare {
            code: code.clone(),
            include_config: false,
            listing: None,
            shared: true,
        }
    }

    /// A connection let in from `peer` that showed `code`, for `server`.
    fn proven(gate: &Arc<Gate>, peer: IpAddr, server: &str, code: &Code) -> Admitted {
        let admitted = gate.admit(peer).expect("a connection was turned away");
        assert!(admitted.prove(server, &shared_with(code), gate.revocations()));
        admitted
    }

    fn downloads(gate: &Gate) -> usize {
        gate.lock()
            .places
            .iter()
            .filter(|place| place.downloading)
            .count()
    }

    fn v4(n: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0xcb00_7100 + n))
    }

    /// The listener full of connections that have shown no code, from a few
    /// sources at their share each: a newcomer still gets in, and the place it
    /// takes is always from a source holding more than the players do — never a
    /// player's one connection, and never a connection that showed a code while
    /// one that has not is there to take. Full of connections that all showed
    /// one, the newcomer takes the oldest between requests instead: a download
    /// under way is still not touched, and nobody is closed unread.
    #[test]
    fn a_full_gate_makes_room_from_the_busiest_source_and_never_from_a_player() {
        let gate = Arc::new(Gate::default());
        let code = Code::generate();

        let downloading = proven(&gate, v4(200), "survival", &code);
        assert!(downloading.start_download());
        let waiting = gate.admit(v4(201)).unwrap();
        let crowd: Vec<Admitted> = (0..MAX_CONNECTIONS - 2)
            .map(|n| gate.admit(v4((n / PER_SOURCE) as u32)).unwrap())
            .collect();

        // Each newcomer is a fresh address, so its own share never stops it.
        let mut newcomers = Vec::new();
        for n in 0..100 {
            newcomers.push(
                gate.admit(v4(1000 + n))
                    .expect("a newcomer was turned away"),
            );
            assert_eq!(gate.lock().places.len(), MAX_CONNECTIONS);
        }
        let dropped: Vec<bool> = crowd.iter().map(cut_off).collect();
        assert_eq!(dropped.iter().filter(|&&d| d).count(), 100);
        // The oldest of the first source went first.
        assert!(dropped[0]);
        assert!(
            !cut_off(&downloading),
            "a player downloading lost its place"
        );
        assert!(
            !cut_off(&waiting),
            "a player's one connection went before a source with two"
        );
        assert!(newcomers.iter().all(|n| !cut_off(n)));

        drop((crowd, newcomers, waiting));
        assert_eq!(
            gate.lock().places.len(),
            1,
            "given-up places are gone"
        );

        // Every place held by someone who showed a code: the oldest of them
        // holding nothing in flight goes, which its client opens again, rather
        // than the newcomer being closed unread.
        let players: Vec<Admitted> = (1..MAX_CONNECTIONS as u32)
            .map(|n| proven(&gate, v4(2000 + n), "survival", &code))
            .collect();
        let newcomer = gate.admit(v4(9999)).expect("a newcomer was closed unread");
        assert!(
            !cut_off(&downloading),
            "a download under way was dropped for a newcomer"
        );
        assert!(
            cut_off(&players[0]),
            "the place given up was not the oldest between requests"
        );
        assert!(players.iter().skip(1).all(|p| !cut_off(p)));
        drop((downloading, newcomer));
    }

    /// A source that stands for many is held to no share of the listener, so a
    /// tunnel or a local network can hold more places than any address in the
    /// world may. Ranked by every one of them it would be the busiest for good,
    /// and the players behind it would lose their newcomers first, to the very
    /// crowd the ranking is there to answer. It is ranked by what any source
    /// may hold and no further.
    #[test]
    fn an_exempt_source_is_not_the_first_to_lose_a_newcomer() {
        let gate = Arc::new(Gate::default());
        let tunnel: IpAddr = "127.0.0.1".parse().unwrap();

        // Fourteen addresses at their share and a fifteenth just under it, and
        // then the tunnel with one place more than any of them may hold — and
        // newer than all of them. The fourteen at their share tie for busiest,
        // the tie broken by age, so the place to go is the very first.
        let capped: Vec<Admitted> = (0..MAX_CONNECTIONS - PER_SOURCE - 1)
            .map(|n| gate.admit(v4((n / PER_SOURCE) as u32)).unwrap())
            .collect();
        let exempt: Vec<Admitted> = (0..PER_SOURCE + 1)
            .map(|_| gate.admit(tunnel).expect("a source that stands for many"))
            .collect();
        assert_eq!(gate.lock().places.len(), MAX_CONNECTIONS);

        let newcomer = gate.admit(v4(9999)).expect("a newcomer was turned away");
        assert!(
            exempt.iter().all(|place| !cut_off(place)),
            "a tunnel's newcomer went before one from a source at its share"
        );
        assert!(
            cut_off(&capped[0]),
            "the place given up was not the oldest of a source at its share"
        );
        drop(newcomer);
    }

    /// A code taken back cuts the connections that showed it, and nothing else:
    /// not another server's players, not connections that showed no code.
    #[test]
    fn a_code_taken_back_cuts_the_connections_that_showed_it() {
        let gate = Arc::new(Gate::default());
        let (old, new, other) = (Code::generate(), Code::generate(), Code::generate());
        let shared = |servers: &[(&str, &Code)], unreadable: &[&str]| Shared {
            servers: servers
                .iter()
                .map(|(id, code)| (id.to_string(), shared_with(code)))
                .collect(),
            unreadable: unreadable.iter().map(|id| id.to_string()).collect(),
        };

        let on_old = proven(&gate, v4(1), "survival", &old);
        let on_other = proven(&gate, v4(2), "creative", &other);
        let no_code = gate.admit(v4(3)).unwrap();

        // Checked again with nothing changed: nobody is cut.
        gate.revoke(&shared(&[("survival", &old), ("creative", &other)], &[]));
        assert_eq!(gate.lock().places.len(), 3);

        // Survival's code replaced.
        gate.revoke(&shared(&[("survival", &new), ("creative", &other)], &[]));
        assert!(cut_off(&on_old), "a download ran on with a replaced code");
        assert_eq!(gate.lock().places.len(), 2, "its place was not given back");

        // Creative missing because its own share file cannot be read: it may
        // well still be shared, so its players are left be.
        gate.revoke(&shared(&[("survival", &new)], &["creative"]));
        assert_eq!(gate.lock().places.len(), 2);
        // Another server's file that cannot be read says nothing about
        // creative: its absence means it was unshared.
        gate.revoke(&shared(&[("survival", &new)], &["old"]));
        assert!(
            cut_off(&on_other),
            "a download ran on from an unshared server"
        );
        assert!(
            !cut_off(&no_code),
            "a connection that showed no code was cut"
        );

        // A request that looked its code up before a check, and shows it after,
        // is sent to look it up again rather than taken at its word.
        let late = gate.admit(v4(4)).unwrap();
        let before = gate.revocations();
        gate.revoke(&shared(&[("survival", &new)], &[]));
        assert!(
            !late.prove("survival", &shared_with(&old), before),
            "a code looked up before it was taken back was accepted after"
        );
        assert!(late.prove("survival", &shared_with(&new), gate.revocations()));
    }

    /// Whether a server sends its `config/` is read when its pack is built, so
    /// a pack built with it keeps it while it streams. Stopping sending it
    /// cuts the connections that came in before, as a new code does; so does
    /// starting, since their packs lack what the admin now sends.
    #[test]
    fn a_change_to_whether_config_is_sent_cuts_the_connections_that_came_before() {
        let gate = Arc::new(Gate::default());
        let code = Code::generate();
        let without = shared_with(&code);
        let with = ServerShare {
            include_config: true,
            listing: None,
            ..without.clone()
        };
        let shared = |share: &ServerShare| Shared {
            servers: vec![("survival".to_string(), share.clone())],
            unreadable: Vec::new(),
        };

        let sending_config = gate.admit(v4(1)).unwrap();
        assert!(sending_config.prove("survival", &with, gate.revocations()));
        let not_sending = proven(&gate, v4(2), "survival", &code);

        gate.revoke(&shared(&with));
        assert!(!cut_off(&sending_config));
        assert!(
            cut_off(&not_sending),
            "a pack without config/ ran on once config/ was sent"
        );
        gate.revoke(&shared(&without));
        assert!(
            cut_off(&sending_config),
            "a pack with config/ ran on once config/ stopped being sent"
        );
    }

    /// Stopping the listener leaves no connection behind.
    #[test]
    fn stopping_cuts_every_connection() {
        let gate = Arc::new(Gate::default());
        let code = Code::generate();
        let player = proven(&gate, v4(1), "survival", &code);
        let no_code = gate.admit(v4(2)).unwrap();
        gate.cut_all();
        assert!(cut_off(&player) && cut_off(&no_code));
        assert!(gate.lock().places.is_empty());
    }

    /// A body of `count` frames of `size` bytes, taken as the test takes them.
    fn frames(size: usize, count: usize) -> Body {
        let bytes = Bytes::from(vec![7u8; size]);
        StreamBody::new(futures::stream::iter(
            std::iter::repeat_with(move || Ok(Frame::data(bytes.clone()))).take(count),
        ))
        .boxed_unsync()
    }

    /// A download read a little every so often never stops moving, and asking
    /// only for some progress kept it for good. What counts is how much it
    /// hands on: a trickle is cut the moment its first window ends, however
    /// steadily its frames keep coming.
    #[tokio::test(start_paused = true)]
    async fn a_trickle_is_cut_when_its_window_ends_though_it_never_stops() {
        use tokio::time::Instant;
        let gate = Arc::new(Gate::default());
        let admitted = Arc::new(gate.admit("203.0.113.1".parse().unwrap()).unwrap());

        let mut stalled = Box::pin(admitted.stalled());
        // Not started: a pack being built for ten minutes is not a stall.
        assert!(tokio::time::timeout(STALL_LIMIT * 5, &mut stalled)
            .await
            .is_err());

        // 16 KiB every ten seconds: 192 KiB a window, short of the minimum.
        let mut body = watched(frames(16 * 1024, 1000), admitted.clone());
        assert!(body.frame().await.is_some());
        let started = Instant::now();
        while tokio::time::timeout(Duration::from_secs(10), &mut stalled)
            .await
            .is_err()
        {
            assert!(
                started.elapsed() < STALL_LIMIT * 5,
                "a trickle was never cut"
            );
            assert!(body.frame().await.is_some());
        }
        assert_eq!(started.elapsed(), STALL_LIMIT);
    }

    /// A download that keeps up with the minimum is never cut, however long it
    /// runs. Once all of it has been handed on it need not keep up any more —
    /// what is left sits in buffers on its way to the player — but that gets
    /// one window more, not forever.
    #[tokio::test(start_paused = true)]
    async fn a_download_that_keeps_up_is_never_cut_and_a_finished_one_gets_one_window_more() {
        use tokio::time::Instant;
        let gate = Arc::new(Gate::default());
        let admitted = Arc::new(gate.admit("203.0.113.1".parse().unwrap()).unwrap());
        let mut stalled = Box::pin(admitted.stalled());

        // 64 KiB every ten seconds: 768 KiB a window, for ten windows.
        let mut body = watched(frames(64 * 1024, 121), admitted.clone());
        for _ in 0..120 {
            assert!(body.frame().await.is_some());
            assert!(
                tokio::time::timeout(Duration::from_secs(10), &mut stalled)
                    .await
                    .is_err(),
                "cut off while keeping up"
            );
        }

        // Ten seconds into a fresh window, the last frame is taken and the body
        // is done with: far less than the minimum in this window, and still not
        // cut when it ends, only a whole window after the last of it went.
        assert!(tokio::time::timeout(Duration::from_secs(10), &mut stalled)
            .await
            .is_err());
        assert!(body.frame().await.is_some());
        drop(body);
        let finished = Instant::now();
        tokio::time::timeout(STALL_LIMIT * 2, &mut stalled)
            .await
            .expect("an answer handed on in full was kept for good");
        assert_eq!(finished.elapsed(), STALL_LIMIT);
    }

    /// `count` downloads, each from an address of its own, numbered from
    /// `from`.
    fn start_downloads(gate: &Arc<Gate>, code: &Code, from: u32, count: usize) -> Vec<Admitted> {
        (from..from + count as u32)
            .map(|n| {
                let download = proven(gate, v4(n), "survival", code);
                assert!(download.start_download(), "download {n} was turned away");
                download
            })
            .collect()
    }

    /// Every download place taken: a new download is told to wait, and nobody
    /// is cut for it, however slowly the others go. Players sharing an upload
    /// too narrow for all of them go slowly together — here a 2 Mbit upload
    /// split between every place, under 8 KiB a second each for ten minutes —
    /// and cutting the slowest for a newcomer cut one of them short at every
    /// click, so none ever finished. A download that ends gives its place back.
    #[tokio::test(start_paused = true)]
    async fn a_new_download_cuts_nobody_however_slowly_the_others_go() {
        let gate = Arc::new(Gate::default());
        let code = Code::generate();
        let mut held = start_downloads(&gate, &code, 0, MAX_DOWNLOADS);
        for _ in 0..600 {
            for download in &held {
                download.sent(7 * 1024 + 512);
            }
            tokio::time::advance(Duration::from_secs(1)).await;
        }

        let newcomer = proven(&gate, v4(1000), "survival", &code);
        assert!(
            !newcomer.start_download(),
            "a download was let in past the limit"
        );
        assert!(
            held.iter().all(|download| !cut_off(download)),
            "a download was cut for a newcomer"
        );
        assert_eq!(downloads(&gate), MAX_DOWNLOADS);

        drop(held.pop());
        assert!(
            newcomer.start_download(),
            "a place given back was not free again"
        );
    }

    /// One address gets only its share of the download places, so keeping
    /// every one busy — reading, or not, or waiting for packs to be built —
    /// takes many addresses. A player from another address still gets one.
    /// Through a tunnel, where every player is one address, the share does not
    /// apply: see the test of a code holder behind one.
    #[test]
    fn one_address_gets_only_its_share_of_the_download_places() {
        let gate = Arc::new(Gate::default());
        let code = Code::generate();
        let theirs: Vec<Admitted> = (0..PER_SOURCE)
            .map(|_| proven(&gate, v4(1), "survival", &code))
            .collect();
        let started = theirs
            .iter()
            .filter(|download| download.start_download())
            .count();
        assert_eq!(
            started, DOWNLOADS_PER_SOURCE,
            "one address took more than its share"
        );
        let player = proven(&gate, v4(2), "survival", &code);
        assert!(
            player.start_download(),
            "a player from another address was turned away"
        );
    }

    /// Every download keeps its own copy of the pack on disk, and holds room
    /// for it from the start — as much as its server's pack took when last
    /// built. So past the limit a new one is refused before its pack is built,
    /// not a whole build later, as each of a crowd arriving at once used to be.
    /// A pack built bigger than the room left is not sent, and nobody is
    /// dropped to make room for anything.
    #[test]
    fn packs_kept_on_disk_are_held_to_a_limit() {
        let limits = Limits {
            outgoing: 10 << 20,
            ..Limits::default()
        };
        let gate = Arc::new(Gate {
            limits,
            ..Gate::default()
        });
        let code = Code::generate();

        // No pack built yet to go by: the first holds nothing until its own is.
        let first = start_downloads(&gate, &code, 0, 1).remove(0);
        assert_eq!(gate.lock().held(), 0);
        assert!(first.hold(4 << 20), "a pack inside the limit was refused");

        // Then each holds room for 4 MiB before its pack is built, and the one
        // there is no room for is refused at once.
        let second = start_downloads(&gate, &code, 1, 1).remove(0);
        assert_eq!(gate.lock().held(), 8 << 20);
        let third = proven(&gate, v4(2), "survival", &code);
        assert!(
            !third.start_download(),
            "a download was let in whose pack could not be kept"
        );

        // Built bigger than the room it held, past the limit: not sent, and
        // nobody is dropped for it.
        assert!(!second.hold(7 << 20), "a pack was kept past the limit");
        assert!(!cut_off(&first), "a download was dropped to make room");

        // Its size is what the next download holds room for.
        drop(second);
        assert!(
            !third.start_download(),
            "a download held room for less than its server's pack last took"
        );
        drop(first);
        assert!(third.start_download());
        assert_eq!(gate.lock().held(), 7 << 20);
    }

    /// Over the network, with room on disk for one copy of the pack: once the
    /// first player's pack is built, a second player is told the server is
    /// busy without another being built for them, while the first download
    /// runs to its end.
    #[tokio::test]
    async fn a_pack_with_no_room_on_disk_is_not_sent() {
        let scratch = Scratch::new("host-outgoing");
        let (_, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let limits = Limits {
            outgoing: 6 << 20,
            ..Limits::default()
        };
        let host = Host::start_with(scratch.paths.clone(), 0, limits, None)
            .await
            .unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&scratch.paths).await;

        let (mut first, received, length) = ask_for_pack(port, &code).await;
        assert!(length > 4 << 20, "the pack is smaller than its filler");

        let response = TrustedClient::new(Trust::Pinned(pin))
            .unwrap()
            .client
            .get(format!("https://127.0.0.1:{port}{PACK_PATH}"))
            .bearer_auth(code.raw())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let said: serde_json::Value = response.json().await.unwrap();
        assert_eq!(said["error"], BUSY);
        assert_eq!(
            outgoing_files(&scratch.paths),
            1,
            "a pack with no room was kept"
        );

        let (received, _) = read_rest(&mut first, received).await;
        assert_eq!(received, length, "the download under way was cut");

        host.stop().await;
    }

    /// A pack bigger than the room on disk for every download together is not
    /// sent to anybody, however few are downloading. Saying the server is busy
    /// would have the player try again for nothing, and tell the admin nothing:
    /// the player is told to ask the admin instead, and the admin's Geode says
    /// why.
    #[tokio::test]
    async fn a_pack_bigger_than_the_room_for_every_download_is_not_sent() {
        let scratch = Scratch::new("host-too-large");
        let (_, code) = share_a_server(&scratch.paths, 2 * 1024 * 1024).await;
        let limits = Limits {
            outgoing: 1 << 20,
            ..Limits::default()
        };
        let host = Host::start_with(scratch.paths.clone(), 0, limits, None)
            .await
            .unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&scratch.paths).await;

        let response = TrustedClient::new(Trust::Pinned(pin))
            .unwrap()
            .client
            .get(format!("https://127.0.0.1:{port}{PACK_PATH}"))
            .bearer_auth(code.raw())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        let said: serde_json::Value = response.json().await.unwrap();
        assert_ne!(said["error"], BUSY);
        assert!(
            outgoing_files_removed(&scratch.paths).await,
            "a pack that was not sent was kept"
        );

        host.stop().await;
    }

    /// The player is told nothing but to ask the admin, so the admin has to be
    /// told something: which server, and why. Once a run, however many players
    /// ask — every one of them keeps asking each time they press Play.
    #[tokio::test]
    async fn a_pack_that_will_not_build_tells_the_admin_which_server_and_says_it_once() {
        let scratch = Scratch::new("host-build-told");
        let (_, code) = share_a_server(&scratch.paths, 2 * 1024 * 1024).await;
        // Room for no pack of this server's at all, which is a failure every
        // request after the first repeats exactly.
        let limits = Limits {
            outgoing: 1 << 20,
            ..Limits::default()
        };
        let recorder = Arc::new(crate::events::Recorder::default());
        let sink: Option<EventSink> = Some(recorder.clone());
        let host = Host::start_with(scratch.paths.clone(), 0, limits, sink)
            .await
            .unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&scratch.paths).await;

        for asked in 1..=2 {
            let response = TrustedClient::new(Trust::Pinned(pin))
                .unwrap()
                .client
                .get(format!("https://127.0.0.1:{port}{PACK_PATH}"))
                .bearer_auth(code.raw())
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                "request {asked}"
            );
            // And the player is told exactly as little as before.
            let said: serde_json::Value = response.json().await.unwrap();
            assert_eq!(
                said["error"],
                "the server could not prepare its pack — ask its admin to check Geode"
            );
        }

        let told: Vec<serde_json::Value> = recorder
            .events()
            .into_iter()
            .filter(|(topic, _)| topic == PACK_FAILED_EVENT)
            .map(|(_, payload)| payload)
            .collect();
        assert_eq!(told.len(), 1, "one notice a server a run: {told:?}");
        assert_eq!(told[0]["serverId"], serde_json::json!("survival"));
        assert_eq!(told[0]["serverName"], serde_json::json!("Survival"));
        assert!(
            told[0]["error"]
                .as_str()
                .is_some_and(|e| e.contains("kept on disk")),
            "{told:?}"
        );

        host.stop().await;
    }

    /// The rule that keeps one broken server from being a toast a player, and
    /// the other half of it: a notice is a toast that leaves the screen in
    /// seconds, so a server still failing once the interval is up is worth
    /// telling an admin who was not at their machine for those seconds.
    #[tokio::test]
    async fn a_server_that_keeps_failing_is_said_again_once_the_interval_is_up() {
        let recorder = Arc::new(crate::events::Recorder::default());
        let alerts = Alerts {
            sink: Some(recorder.clone()),
            told: Mutex::default(),
        };
        let named = |server: &Server| {
            recorder
                .events()
                .into_iter()
                .filter(|(topic, payload)| {
                    topic == PACK_FAILED_EVENT && payload["serverId"] == server.id.as_str()
                })
                .count()
        };
        let mut survival = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
        survival.id = "survival".into();
        let mut creative = survival.clone();
        creative.id = "creative".into();
        creative.name = "Creative".into();

        alerts.failed(&survival, "no room on disk");
        alerts.failed(&survival, "no room on disk");
        assert_eq!(named(&survival), 1, "a toast a player");
        // Another server failing is its own news, whenever it happens.
        alerts.failed(&creative, "no room on disk");
        assert_eq!(named(&creative), 1);

        // The interval gone by, wound back rather than waited out.
        alerts.told.lock().unwrap().insert(
            survival.id.clone(),
            tokio::time::Instant::now() - ALERT_AGAIN_AFTER,
        );
        alerts.failed(&survival, "no room on disk");
        assert_eq!(
            named(&survival),
            2,
            "a server still failing said nothing to an admin who missed the first"
        );
        assert_eq!(named(&creative), 1, "one server's clock moved another's");
    }

    /// What working a server's description out finds, for a test that supplies
    /// the work itself.
    fn a_description() -> PackInfo {
        PackInfo {
            name: "Survival".into(),
            mc_version: "1.21.1".into(),
            loader: "fabric".into(),
            loader_version: "0.16.9".into(),
            mods: 300,
            server_mods: 0,
            server_kind: "fabric".into(),
            revision: "r".into(),
            server_id: "survival".into(),
            listing: None,
        }
    }

    /// A flood of previews reads a server's mods once, not once each — and a
    /// player hanging up while they are read starts no second reading beside
    /// the first. A description older than [`PREVIEW_FRESH`] is worked out
    /// again.
    #[tokio::test(start_paused = true)]
    async fn previews_read_a_servers_mods_once_however_many_ask() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let previews = Arc::new(Previews::default());
        let readings = Arc::new(AtomicUsize::new(0));
        let ask = || {
            let (previews, readings) = (previews.clone(), readings.clone());
            tokio::spawn(async move {
                let work = async move {
                    readings.fetch_add(1, Ordering::SeqCst);
                    // A few hundred jars.
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    Ok(a_description())
                };
                previews.describe("survival", work).await
            })
        };
        let read = || readings.load(Ordering::SeqCst);

        // The first to ask hangs up while the mods are being read.
        let first = ask();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(read(), 1);
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());

        // A crowd asks meanwhile.
        let crowd: Vec<_> = (0..100).map(|_| ask()).collect();
        for asked in crowd {
            assert_eq!(asked.await.unwrap().unwrap().mods, 300);
        }
        assert_eq!(read(), 1, "the mods were read more than once");

        // A second before it is too old, and once it is.
        tokio::time::sleep(PREVIEW_FRESH - Duration::from_secs(1)).await;
        ask().await.unwrap().unwrap();
        assert_eq!(read(), 1);
        tokio::time::sleep(Duration::from_secs(1)).await;
        ask().await.unwrap().unwrap();
        assert_eq!(read(), 2, "a description was kept past its time");
    }

    /// The other half of the attack a code holder could still run: prove every
    /// place and ask each of them for a description, so they all queue behind
    /// the one being worked out and the listener is full of connections that
    /// showed a code. Only [`MAX_PREVIEWS`] of them wait; the rest are told the
    /// server is busy answering, at once, and their connections end. A player
    /// arriving while it is still full takes the place of one between requests
    /// rather than being closed unread, and once the description is handed on
    /// they ask for their own.
    #[test]
    fn a_gate_full_of_previews_still_lets_a_new_player_in() {
        let gate = Arc::new(Gate::default());
        let code = Code::generate();
        let held: Vec<Admitted> = (0..MAX_CONNECTIONS as u32)
            .map(|n| proven(&gate, v4(n), "survival", &code))
            .collect();
        let waiting = held.iter().filter(|c| c.start_preview()).count();
        assert_eq!(waiting, MAX_PREVIEWS, "a queue deeper than the ceiling");

        let player = gate.admit(v4(9000)).expect("a player was closed unread");
        assert!(player.prove("survival", &shared_with(&code), gate.revocations()));
        assert_eq!(
            held.iter()
                .take(MAX_PREVIEWS)
                .filter(|c| cut_off(c))
                .count(),
            0,
            "a preview waiting on a description was dropped for a newcomer"
        );
        assert!(
            cut_off(&held[MAX_PREVIEWS]),
            "the place given up was not the oldest between requests"
        );

        for answered in held.iter().take(MAX_PREVIEWS) {
            answered.finish_preview();
        }
        assert!(
            player.start_preview(),
            "a player waited behind previews already answered"
        );
    }

    /// A large cold server's description takes a while, and the preview waiting
    /// on it keeps its place for all of it: neither the ceiling nor a listener
    /// filling up around it cuts a wait that is going exactly as it should.
    #[tokio::test(start_paused = true)]
    async fn a_slow_description_is_still_answered() {
        let gate = Arc::new(Gate::default());
        let code = Code::generate();
        let previews = Arc::new(Previews::default());

        let player = proven(&gate, v4(1), "survival", &code);
        assert!(player.start_preview());
        let asked = tokio::spawn({
            let previews = previews.clone();
            async move {
                let work = async {
                    // A few hundred jars, a cold cache and a slow disk.
                    tokio::time::sleep(Duration::from_secs(90)).await;
                    Ok(a_description())
                };
                previews.describe("survival", work).await
            }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Every other place taken by a code holder, and then newcomers past it.
        let _crowd: Vec<Admitted> = (0..MAX_CONNECTIONS as u32 - 1)
            .map(|n| proven(&gate, v4(100 + n), "survival", &code))
            .collect();
        let _newcomers: Vec<Admitted> = (0..50)
            .map(|n| {
                gate.admit(v4(1000 + n))
                    .expect("a newcomer was closed unread")
            })
            .collect();
        assert!(
            !cut_off(&player),
            "a description under way was cut to make room"
        );

        let described = tokio::time::timeout(Duration::from_secs(300), asked)
            .await
            .expect("the description never arrived")
            .unwrap()
            .unwrap();
        assert_eq!(described.mods, 300);
    }

    /// Through a tunnel every player is 127.0.0.1, which no source's share
    /// applies to. Someone there with the code asks for the pack on every
    /// connection they can open: they get [`MAX_DOWNLOADS`] downloads, and
    /// every other request is told to wait and its connection ends. Previews
    /// are answered from a description already worked out, so what they can
    /// keep in the places left, beyond a moment, are connections that have not
    /// shown a code — which a newcomer takes. So a player's preview still gets
    /// in.
    #[test]
    fn a_code_holder_behind_a_tunnel_cannot_keep_a_player_from_a_preview() {
        let tunnel: IpAddr = "127.0.0.1".parse().unwrap();
        let gate = Arc::new(Gate::default());
        let code = Code::generate();

        let mut held = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let connection = proven(&gate, tunnel, "survival", &code);
            if connection.start_download() {
                held.push(connection);
            } else {
                drop(connection);
                held.push(gate.admit(tunnel).unwrap());
            }
        }
        assert_eq!(gate.lock().places.len(), MAX_CONNECTIONS);
        assert_eq!(downloads(&gate), MAX_DOWNLOADS);

        let player = gate.admit(tunnel).expect("a player's preview was shut out");
        assert!(player.prove("survival", &shared_with(&code), gate.revocations()));
        assert_eq!(held.iter().filter(|c| cut_off(c)).count(), 1);
        assert_eq!(
            downloads(&gate),
            MAX_DOWNLOADS,
            "a download made room for a preview"
        );
    }

    /// The attack the listener used to fall to: one machine opens more
    /// connections than it serves and says nothing on any of them. Now a player
    /// still gets in, and the connections that make room are the idle ones,
    /// oldest first.
    ///
    /// On loopback, which is not held to one source's share, so every idle
    /// connection is let in and the listener really is full. Linux only,
    /// because only Linux answers on all of `127.0.0.0/8`, which is what lets
    /// 127.0.0.2 be a second machine on one computer.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_listener_full_of_idle_connections_still_lets_a_player_in() {
        let scratch = Scratch::new("host-crowd");
        let (_, code) = share_a_server(&scratch.paths, 0).await;
        let host = Host::start(scratch.paths.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&scratch.paths).await;

        let crowd = Ipv4Addr::new(127, 0, 0, 2);
        let extra = 4;
        let mut idle: Vec<TcpStream> = (0..MAX_CONNECTIONS + extra)
            .map(|_| connect_from(crowd, port, None))
            .collect();
        // The last few each took the place of one before them; once the last
        // of those has been closed, every connection has been through the gate.
        assert!(
            closed_within(&mut idle[extra - 1], Duration::from_secs(3)).await,
            "the host never made room for the last connection"
        );

        let served = info("127.0.0.1", port, pin, &code).await;
        assert_eq!(
            served.map(|info| info.name).map_err(|e| format!("{e:?}")),
            Ok("Survival".to_string()),
            "a player was locked out by idle connections"
        );

        // The player took one more. Well inside the handshake deadline, so no
        // other connection has been closed for taking too long — which is why
        // they are all looked at at once, not one after another.
        let closed = futures::future::join_all(
            idle.iter_mut()
                .map(|socket| closed_within(socket, Duration::from_millis(50))),
        )
        .await;
        for (i, closed) in closed.into_iter().enumerate() {
            assert_eq!(closed, i <= extra, "idle connection {i}");
        }

        host.stop().await;
    }

    /// A player downloading through a tunnel shares its one address with
    /// everyone else coming through it, so is the oldest connection from the
    /// busiest source when a crowd arrives the same way. Having shown the code
    /// is what keeps its download going.
    #[tokio::test]
    async fn a_download_under_way_is_never_dropped_to_make_room() {
        let scratch = Scratch::new("host-proven");
        let (_, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let host = Host::start(scratch.paths.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();

        let mut tls = tls_to(port, 4096).await;
        let request = format!(
            "GET {PACK_PATH} HTTP/1.1\r\nHost: geode\r\nAuthorization: Bearer {}\r\n\r\n",
            code.raw()
        );
        tls.write_all(request.as_bytes()).await.unwrap();
        let (head, mut received) = read_head(&mut tls).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let length: usize = head
            .lines()
            .find_map(|line| {
                let line = line.to_ascii_lowercase();
                line.strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap();

        let extra = 4;
        let mut idle: Vec<TcpStream> = (0..MAX_CONNECTIONS + extra)
            .map(|_| connect_from(Ipv4Addr::LOCALHOST, port, None))
            .collect();
        assert!(
            closed_within(&mut idle[extra - 1], Duration::from_secs(3)).await,
            "the host never made room for the last connection"
        );

        let mut chunk = [0u8; 64 * 1024];
        loop {
            match tls.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => received += n,
            }
        }
        assert_eq!(received, length, "the download was dropped for a newcomer");

        host.stop().await;
    }

    /// A player who asks for the pack and then stops reading would otherwise
    /// keep the connection, and the pack file on the admin's disk, for as long
    /// as they left the socket open.
    ///
    /// The first window is not the one that cuts it: filling the buffers
    /// between the two ends hands on far more than the minimum. The window
    /// after, with nothing read, is.
    #[tokio::test]
    async fn a_download_nobody_reads_is_cut_off_once_it_stalls() {
        let scratch = Scratch::new("host-stall");
        // Megabytes that do not compress, so the pack outlasts every buffer
        // between the two ends — a little under two, measured on loopback —
        // and the host really is left waiting to send.
        let (_, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let limit = Duration::from_secs(3);
        let limits = Limits {
            window: limit,
            ..Limits::default()
        };
        let host = Host::start_with(scratch.paths.clone(), 0, limits, None)
            .await
            .unwrap();

        let mut tls = tls_to(host.local_addr.port(), 4096).await;
        let request = format!(
            "GET {PACK_PATH} HTTP/1.1\r\nHost: geode\r\nAuthorization: Bearer {}\r\n\r\n",
            code.raw()
        );
        tls.write_all(request.as_bytes()).await.unwrap();
        let (head, mut received) = read_head(&mut tls).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let length: usize = head
            .lines()
            .find_map(|line| {
                let line = line.to_ascii_lowercase();
                line.strip_prefix("content-length:").map(|value| value.trim().parse().unwrap())
            })
            .expect("the pack's length is sent");

        // Nothing more is read. The host is still sending, so its file is there,
        // and stays there past the first window: waiting on a player is not yet
        // a stall.
        let stopped = Instant::now();
        tokio::time::sleep_until((stopped + limit * 3 / 2).into()).await;
        assert_eq!(
            outgoing_files(&scratch.paths),
            1,
            "cut off in its first window, or never sending"
        );

        tokio::time::sleep_until((stopped + limit * 2 + Duration::from_millis(1500)).into()).await;
        assert_eq!(
            outgoing_files(&scratch.paths),
            0,
            "a download nobody reads still holds its pack file after a window with nothing sent"
        );

        // Reading again gets what was already on its way, then the end — short
        // of the whole pack, and without TLS's goodbye, because it was cut off.
        let mut chunk = [0u8; 64 * 1024];
        let ended = loop {
            match tls.read(&mut chunk).await {
                Ok(0) => break None,
                Ok(n) => received += n,
                Err(e) => break Some(e.kind()),
            }
        };
        assert!(received < length, "all {length} bytes arrived");
        assert_eq!(ended, Some(ErrorKind::UnexpectedEof));

        host.stop().await;
    }

    /// A host on a port from below the range the system hands out for port 0.
    ///
    /// A port from that range can be one another test was handed a moment ago
    /// by binding port 0 and letting go again — which is how
    /// `share::service`'s tests pick theirs — and taking it makes that test's
    /// listener fail to start. The longer a listener is held, the likelier
    /// that is, and the two tests below hold one for seconds at a time (the
    /// stall test above does too, and takes the same chance by asking for port
    /// 0). Below the range no such promise exists, but a port there may
    /// already be somebody's, so try a few.
    async fn host_on_a_quiet_port(paths: &Paths, limits: Limits) -> Host {
        let mut refused = None;
        for port in 24700..24710 {
            match Host::start_with(paths.clone(), port, limits, None).await {
                Ok(host) => return host,
                Err(e) => refused = Some(e.to_string()),
            }
        }
        panic!("no free port to listen on: {}", refused.unwrap_or_default());
    }

    /// Read a download at `bytes` every `window`, until the whole pack has
    /// arrived or the host hangs up. Returns how many bytes of the body
    /// arrived in all, and the error the end came with.
    async fn read_at_a_pace(
        tls: &mut tokio_rustls::client::TlsStream<TcpStream>,
        mut received: usize,
        length: usize,
        bytes: usize,
        window: Duration,
    ) -> (usize, Option<ErrorKind>) {
        let mut chunk = [0u8; 16 * 1024];
        while received < length {
            let next = tokio::time::Instant::now() + window;
            let mut taken = 0;
            while taken < bytes && received < length {
                let want = chunk.len().min(bytes - taken);
                match tls.read(&mut chunk[..want]).await {
                    Ok(0) => return (received, None),
                    Ok(n) => (taken, received) = (taken + n, received + n),
                    Err(e) => return (received, Some(e.kind())),
                }
            }
            tokio::time::sleep_until(next).await;
        }
        (received, None)
    }

    /// A player on a slow link reads steadily — four times the floor in every
    /// window, which is still a trickle — and is never cut. Over a real socket,
    /// because what the watchdog counts as handed on is what hyper could write,
    /// and hyper writes only what the player has drained: the guarantee is
    /// about a real download's pacing, not about frames a test hands it.
    #[tokio::test]
    async fn a_download_read_slowly_but_steadily_is_not_cut_off() {
        let scratch = Scratch::new("host-steady");
        // More than every buffer between the two ends holds, so most of the
        // pack is handed on at the pace the reader sets, window after window.
        let (_, code) = share_a_server(&scratch.paths, 8 * 1024 * 1024).await;
        let window = Duration::from_secs(1);
        let limits = Limits {
            window,
            min_progress: 512 * 1024,
            ..Limits::default()
        };
        let host = host_on_a_quiet_port(&scratch.paths, limits).await;

        let (mut tls, received, length) = ask_for_pack(host.local_addr.port(), &code).await;
        let (received, _) =
            read_at_a_pace(&mut tls, received, length, 2 * 1024 * 1024, window).await;
        assert_eq!(
            received, length,
            "a download that never stopped moving was cut off"
        );

        host.stop().await;
    }

    /// And the same reader a quarter of the floor instead: the first window
    /// fills the buffers between the two ends and passes, and the next one,
    /// which is the reader's own pace, does not.
    #[tokio::test]
    async fn a_download_read_below_the_floor_is_cut_off() {
        let scratch = Scratch::new("host-below");
        let (_, code) = share_a_server(&scratch.paths, 8 * 1024 * 1024).await;
        let window = Duration::from_secs(1);
        let limits = Limits {
            window,
            min_progress: 512 * 1024,
            ..Limits::default()
        };
        let host = host_on_a_quiet_port(&scratch.paths, limits).await;

        let (mut tls, received, length) = ask_for_pack(host.local_addr.port(), &code).await;
        let (received, ended) =
            read_at_a_pace(&mut tls, received, length, 128 * 1024, window).await;
        assert!(
            received < length,
            "all {length} bytes arrived to a reader below the floor"
        );
        assert_eq!(ended, Some(ErrorKind::UnexpectedEof));

        host.stop().await;
    }

    /// A new code cuts off everyone holding the old one: not only on their next
    /// request, but in the middle of a download they already started.
    #[tokio::test]
    async fn a_new_code_cuts_off_a_download_started_with_the_old_one() {
        let scratch = Scratch::new("host-rotate");
        let (server, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let host = Host::start(scratch.paths.clone(), 0, None).await.unwrap();

        let (mut tls, mut received, length) = ask_for_pack(host.local_addr.port(), &code).await;
        // Under way, and slowly: a little is read, and then no more for now.
        let mut chunk = [0u8; 16 * 1024];
        received += tls.read(&mut chunk).await.unwrap();

        let replaced = ServerShare {
            code: Code::generate(),
            include_config: false,
            listing: None,
            shared: true,
        };
        crate::server::share::save(&scratch.paths, &server.id, &replaced)
            .await
            .unwrap();
        host.cut_revoked(&scratch.paths).await;

        // What was already on its way arrives, then the end, short of the pack.
        let (received, ended) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "all {length} bytes arrived on a code taken back"
        );
        assert_eq!(ended, Some(ErrorKind::UnexpectedEof));
        assert!(
            outgoing_files_removed(&scratch.paths).await,
            "the pack file outlived its download"
        );

        host.stop().await;
    }

    /// Unsharing a server cuts off its downloads under way, and so does
    /// stopping the listener, which used to leave them running beside whatever
    /// listened next.
    #[tokio::test]
    async fn unsharing_or_stopping_cuts_off_downloads_under_way() {
        let scratch = Scratch::new("host-unshare");
        let (server, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let host = Host::start(scratch.paths.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();

        let (mut tls, received, length) = ask_for_pack(port, &code).await;
        crate::server::share::stop(&scratch.paths, &server.id)
            .await
            .unwrap();
        host.cut_revoked(&scratch.paths).await;
        let (received, ended) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "all {length} bytes arrived from a server unshared"
        );
        assert_eq!(ended, Some(ErrorKind::UnexpectedEof));
        assert!(outgoing_files_removed(&scratch.paths).await);

        // Shared again, with a new code, and a download of it under way.
        let (_, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let (mut tls, received, length) = ask_for_pack(port, &code).await;
        host.stop().await;
        let (received, ended) = read_rest(&mut tls, received).await;
        assert!(
            received < length,
            "all {length} bytes arrived from a listener stopped"
        );
        assert_eq!(ended, Some(ErrorKind::UnexpectedEof));
        assert!(
            outgoing_files_removed(&scratch.paths).await,
            "the pack file outlived its download"
        );
    }

    /// Over the network, with every download place taken: a player asking for
    /// the pack is told the server is busy, at
    /// once and in words their Geode shows them; a crowd of connections from
    /// the same address still does not keep a preview out; and the download
    /// that was there first runs to its end.
    #[tokio::test]
    async fn a_busy_server_says_so_and_still_answers_a_preview() {
        let scratch = Scratch::new("host-busy");
        let (_, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let limits = Limits {
            downloads: 1,
            ..Limits::default()
        };
        let host = Host::start_with(scratch.paths.clone(), 0, limits, None)
            .await
            .unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&scratch.paths).await;

        let (mut first, received, length) = ask_for_pack(port, &code).await;

        let response = TrustedClient::new(Trust::Pinned(pin))
            .unwrap()
            .client
            .get(format!("https://127.0.0.1:{port}{PACK_PATH}"))
            .bearer_auth(code.raw())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let said: serde_json::Value = response.json().await.unwrap();
        assert_eq!(said["error"], BUSY);

        let extra = 4;
        let mut idle: Vec<TcpStream> = (0..MAX_CONNECTIONS + extra)
            .map(|_| connect_from(Ipv4Addr::LOCALHOST, port, None))
            .collect();
        assert!(
            closed_within(&mut idle[extra - 1], Duration::from_secs(3)).await,
            "the host never made room for the last connection"
        );
        let served = info("127.0.0.1", port, pin, &code).await;
        assert_eq!(
            served.map(|info| info.name).map_err(|e| format!("{e:?}")),
            Ok("Survival".to_string()),
            "a player's preview was locked out"
        );

        let (received, _) = read_rest(&mut first, received).await;
        assert_eq!(
            received, length,
            "the download under way was cut for a newcomer"
        );

        host.stop().await;
    }

    /// Over the network, with every place for waiting on a description taken:
    /// a player asking for one is told the server is busy answering others, at
    /// once and in words of its own — not the ones about sending a pack.
    #[tokio::test]
    async fn a_preview_past_the_ceiling_is_told_the_server_is_answering_others() {
        let scratch = Scratch::new("host-describing");
        let (_, code) = share_a_server(&scratch.paths, 0).await;
        let limits = Limits {
            previews: 0,
            ..Limits::default()
        };
        let host = Host::start_with(scratch.paths.clone(), 0, limits, None)
            .await
            .unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&scratch.paths).await;

        let response = TrustedClient::new(Trust::Pinned(pin))
            .unwrap()
            .client
            .get(format!("https://127.0.0.1:{port}{INFO_PATH}"))
            .bearer_auth(code.raw())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let said: serde_json::Value = response.json().await.unwrap();
        assert_eq!(said["error"], DESCRIBING);

        host.stop().await;
    }

    /// And the player whose answer the server already has, with the same place
    /// taken: the ceiling is on the queue behind the description being worked
    /// out, so one that is already worked out is handed over as ever rather
    /// than refused for a queue it would never join.
    #[tokio::test]
    async fn a_description_already_worked_out_is_answered_past_the_ceiling() {
        let scratch = Scratch::new("host-inhand");
        let (_, code) = share_a_server(&scratch.paths, 0).await;
        let limits = Limits {
            previews: 1,
            ..Limits::default()
        };
        let host = Host::start_with(scratch.paths.clone(), 0, limits, None)
            .await
            .unwrap();
        let port = host.local_addr.port();
        let pin = pin_of(&scratch.paths).await;

        // One player's preview works the description out, and it stays good
        // for a few seconds after that.
        assert!(info("127.0.0.1", port, pin, &code).await.is_ok());

        // Then somebody else takes the one place there is for waiting on one.
        let waiting = proven(&host.gate, v4(1), "survival", &code);
        assert!(waiting.start_preview());

        let served = info("127.0.0.1", port, pin, &code).await;
        assert_eq!(
            served.map(|info| info.name).map_err(|e| format!("{e:?}")),
            Ok("Survival".to_string()),
            "a player was refused a description the server had in hand"
        );

        host.stop().await;
    }

    /// Links can carry an IPv6 address, so the listener has to answer on one —
    /// and on IPv4 still, from the same port.
    #[tokio::test]
    async fn players_are_served_over_ipv4_and_ipv6_alike() {
        let scratch = Scratch::new("host-dual");
        let (_, code) = share_a_server(&scratch.paths, 0).await;
        let host = Host::start(scratch.paths.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();
        assert_ne!(port, 0, "port 0 asks for any free port, and local_addr says which");
        let pin = pin_of(&scratch.paths).await;

        for address in ["127.0.0.1", "[::1]"] {
            let served = info(address, port, pin, &code).await;
            assert_eq!(
                served.map(|info| info.name).map_err(|e| format!("{e:?}")),
                Ok("Survival".to_string()),
                "over {address}"
            );
        }

        host.stop().await;
    }

    /// Every request for a pack starts a build, so they take turns: while the
    /// turns are all taken, a request builds nothing.
    #[tokio::test]
    async fn a_pack_is_not_built_until_it_has_a_turn() {
        let scratch = Scratch::new("host-queue");
        let (server, _) = share_a_server(&scratch.paths, 0).await;
        let builds = Arc::new(Semaphore::new(1));
        let taken = builds.clone().acquire_owned().await.unwrap();

        let waiting = tokio::spawn({
            let (paths, builds) = (scratch.paths.clone(), builds.clone());
            async move { pack(&paths, &server, &builds).await }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!waiting.is_finished(), "a pack was built while every turn was taken");
        assert_eq!(outgoing_files(&scratch.paths), 0);

        drop(taken);
        let response = tokio::time::timeout(Duration::from_secs(30), waiting)
            .await
            .expect("the pack was built once its turn came")
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(outgoing_files(&scratch.paths), 1, "kept while its response lives");
        drop(response);
        assert_eq!(outgoing_files(&scratch.paths), 0, "and removed with it");
    }

    /// A player hanging up mid-build does not stop the writer, which works on
    /// a thread of its own — so the build keeps its turn until the writer is
    /// done, or hanging up over and over would start builds without limit, and
    /// its file goes once it is.
    #[tokio::test]
    async fn a_build_whose_player_hung_up_keeps_its_turn_until_it_is_done() {
        let scratch = Scratch::new("host-hangup");
        // Enough that the writer is still compressing when the player leaves.
        let (server, _) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let builds = Arc::new(Semaphore::new(1));

        let request = tokio::spawn({
            let (paths, builds) = (scratch.paths.clone(), builds.clone());
            async move { pack(&paths, &server, &builds).await }
        });
        let writing = Instant::now();
        while outgoing_files(&scratch.paths) == 0 {
            assert!(writing.elapsed() < Duration::from_secs(30), "the writer never started");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert_eq!(builds.available_permits(), 0, "the turn was given back while the writer still ran");

        let finishing = Instant::now();
        while builds.available_permits() == 0 {
            assert!(finishing.elapsed() < Duration::from_secs(30), "the build never finished");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(outgoing_files(&scratch.paths), 0, "the abandoned build left its file behind");
    }
}

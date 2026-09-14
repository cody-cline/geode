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
//! made for a newcomer by dropping an idle connection from the busiest source;
//! short deadlines on the TLS handshake and the request headers, and a
//! download cut off once it stops moving; no keep-alive; a queue for building
//! packs; and nothing built or read from disk until a code has matched.

use std::cmp::Reverse;
use std::convert::Infallible;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
use crate::net::Progress;
use crate::paths::Paths;
use crate::server::Server;

/// The request paths, shared with the client in [`super::join`].
pub const INFO_PATH: &str = "/geode/v1/info";
pub const PACK_PATH: &str = "/geode/v1/pack";

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
/// each of this many addresses, not [`PER_SOURCE`] from a handful.
const MAX_CONNECTIONS: usize = 128;

/// How long a connection may take to finish TLS, and then to send its request
/// headers. Both short: a real client does either in well under a second even
/// across an ocean, and until it has, nothing says it is a player at all.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an answer may go without sending anything before the connection is
/// cut. A download is held open by the player reading it, so without this one
/// that stops reading — or whose network vanished without a word — keeps its
/// connection, and the pack file being sent, for as long as it likes.
///
/// Measured from when the answer last handed data on, not from when the
/// connection opened, so a big pack on a slow upload is never cut while it is
/// still moving. Two minutes, because data handed on sits in buffers until the
/// player's side takes it, and a slow link takes a while to empty them.
const STALL_LIMIT: Duration = Duration::from_secs(2 * 60);

/// Packs being built at once. Building one hashes and compresses a server's
/// mods, which is real work for the admin's disk and processor, and every
/// request for a pack starts one; past this they wait their turn.
const MAX_BUILDS: usize = 2;

/// Added before every refusal of a code. Guessing is already hopeless against
/// fifty bits; this just makes it hopeless more slowly.
const REFUSAL_DELAY: Duration = Duration::from_millis(400);

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
    /// The server software, for display: `paper`, `neoforge`, …
    pub server_kind: String,
}

/// A running pack server.
pub struct Host {
    pub local_addr: SocketAddr,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Host {
    /// Listen on `port` over IPv4 and IPv6 both, and serve until [`Host::stop`].
    ///
    /// Port 0 binds any free port, which is what tests use; the address actually
    /// bound is in `local_addr`.
    pub async fn start(paths: Paths, port: u16) -> Result<Self> {
        Self::start_with(paths, port, STALL_LIMIT).await
    }

    /// [`Host::start`], with a stall limit a test can wait out.
    async fn start_with(paths: Paths, port: u16, stall_limit: Duration) -> Result<Self> {
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
        let gate = Arc::new(Gate::default());
        let builds = Arc::new(Semaphore::new(MAX_BUILDS));

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
                let builds = builds.clone();
                tokio::spawn(async move {
                    // Dropping the connection drops the response it was
                    // sending, and with it the pack file that response was
                    // streaming from.
                    tokio::select! {
                        () = serve(tcp, acceptor, paths, builds, admitted.clone()) => {}
                        () = admitted.evicted.notified() => {}
                        () = admitted.stalled(stall_limit) => {}
                    }
                });
            }
        });

        Ok(Self {
            local_addr,
            stop,
            task,
        })
    }

    /// Stop accepting. The port is released as soon as this returns; a
    /// download already in flight is left to finish, or to stall.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
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
struct Gate(Mutex<Occupancy>);

#[derive(Default)]
struct Occupancy {
    next_id: u64,
    /// At most [`MAX_CONNECTIONS`], so a scan of it is cheap.
    places: Vec<Place>,
}

struct Place {
    id: u64,
    source: IpAddr,
    /// Showed a code that opens a shared server: a player, never dropped to
    /// make room.
    proven: bool,
    evicted: Arc<Notify>,
}

impl Gate {
    /// Let a connection from `peer` in, unless its source already holds its
    /// share, or every place is taken by a player.
    ///
    /// When the listener is full, the connection dropped for the newcomer is
    /// the oldest without a code from the source holding the most. Someone
    /// filling the listener from a few addresses loses their own connections
    /// first, and a player's one connection outlasts every source with two.
    fn admit(self: &Arc<Self>, peer: IpAddr) -> Option<Admitted> {
        let source = source_of(peer);
        let mut occupancy = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let held_by =
            |places: &[Place], source: IpAddr| places.iter().filter(|p| p.source == source).count();

        if !stands_for_many(peer.to_canonical()) && held_by(&occupancy.places, source) >= PER_SOURCE
        {
            return None;
        }
        if occupancy.places.len() >= MAX_CONNECTIONS {
            let (oldest_from_busiest, _) = occupancy
                .places
                .iter()
                .enumerate()
                .filter(|(_, place)| !place.proven)
                .max_by_key(|(i, place)| (held_by(&occupancy.places, place.source), Reverse(*i)))?;
            occupancy
                .places
                .remove(oldest_from_busiest)
                .evicted
                .notify_one();
        }

        let id = occupancy.next_id;
        occupancy.next_id += 1;
        let evicted = Arc::new(Notify::new());
        occupancy.places.push(Place {
            id,
            source,
            proven: false,
            evicted: evicted.clone(),
        });
        Some(Admitted {
            gate: self.clone(),
            id,
            evicted,
            last_sent: Mutex::new(None),
        })
    }
}

/// A connection's place at the [`Gate`], given back when it is dropped.
struct Admitted {
    gate: Arc<Gate>,
    id: u64,
    /// Told when the place was given to a newcomer.
    evicted: Arc<Notify>,
    /// When the answer last handed on any data; `None` until it starts.
    last_sent: Mutex<Option<Instant>>,
}

impl Admitted {
    /// The connection showed a code that matched.
    fn prove(&self) {
        let mut occupancy = self.gate.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(place) = occupancy
            .places
            .iter_mut()
            .find(|place| place.id == self.id)
        {
            place.proven = true;
        }
    }

    fn sent(&self) {
        *self.last_sent.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
    }

    /// Finishes once the answer has started and then sent nothing for `limit`.
    /// Before it starts — a handshake, a request, a pack being built — the
    /// deadlines on those, and the build finishing, are what end the wait.
    async fn stalled(&self, limit: Duration) {
        loop {
            let last_sent = *self.last_sent.lock().unwrap_or_else(|p| p.into_inner());
            match last_sent {
                Some(at) if at.elapsed() >= limit => return,
                Some(at) => tokio::time::sleep_until(at + limit).await,
                None => tokio::time::sleep(limit).await,
            }
        }
    }
}

impl Drop for Admitted {
    fn drop(&mut self) {
        let mut occupancy = self.gate.0.lock().unwrap_or_else(|p| p.into_inner());
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
    admitted: Arc<Admitted>,
) {
    let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await else {
        return;
    };
    let service = hyper::service::service_fn(move |request| {
        let (paths, builds, admitted) = (paths.clone(), builds.clone(), admitted.clone());
        async move {
            let response = respond(&paths, &builds, &admitted, request).await;
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

/// A body that marks the connection as still sending each time data is taken
/// from it — which is only as fast as the player reads, since nothing more is
/// taken while what was already sent sits unread.
fn watched(body: Body, admitted: Arc<Admitted>) -> Body {
    body.map_frame(move |frame| {
        admitted.sent();
        frame
    })
    .boxed_unsync()
}

async fn respond(
    paths: &Paths,
    builds: &Arc<Semaphore>,
    admitted: &Admitted,
    request: Request<Incoming>,
) -> Response<Body> {
    let path = request.uri().path();
    if request.method() != Method::GET || (path != INFO_PATH && path != PACK_PATH) {
        return message(StatusCode::NOT_FOUND, "not found");
    }
    let want_pack = path == PACK_PATH;

    let Some(server) = shared_server(paths, bearer(&request)).await else {
        tokio::time::sleep(REFUSAL_DELAY).await;
        return message(StatusCode::UNAUTHORIZED, "that code is not accepted here");
    };
    admitted.prove();

    // Said in the server's own words, and only after the code matched: it is
    // written for the player, and it contains nothing about this machine.
    if let Err(e) = super::pack::client_loader(&server) {
        return message(StatusCode::CONFLICT, &e.to_string());
    }

    let outcome = if want_pack {
        pack(paths, &server, builds).await
    } else {
        super::pack::server_pack_info(paths, &server)
            .await
            .map(|info| json(StatusCode::OK, &info))
    };
    outcome.unwrap_or_else(|e| {
        // The real reason stays on this machine. It can name paths under the
        // admin's home directory, which are nobody else's business.
        eprintln!("geode: sharing {} failed: {e}", server.id);
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
async fn shared_server(paths: &Paths, code: Option<&str>) -> Option<Server> {
    let code = code?;
    let shared = crate::server::share::list(paths).await.ok()?;
    let (id, _) = shared
        .servers
        .into_iter()
        .find(|(_, share)| share.code.matches(code))?;
    crate::server::load(paths, &id).await.ok()
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
mod tests {
    //! The listener against the connections that would keep players out: one
    //! source holding every slot, a download nobody reads, a crowd of pack
    //! requests — and a player who can only reach it over IPv6.

    use std::time::Instant;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::server::share::ServerShare;
    use crate::server::ServerKind;
    use crate::share::code::Code;
    use crate::share::identity::Identity;
    use crate::share::link::Pin;
    use crate::share::trust::{Trust, TrustedClient};

    /// A data directory of its own under the system's temporary directory,
    /// removed when the test ends however it ends.
    struct Scratch {
        paths: Paths,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("geode-host-{tag}-{}", uuid::Uuid::new_v4()));
            Self { paths: Paths { root } }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.paths.root);
        }
    }

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

    fn outgoing_files(paths: &Paths) -> usize {
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

        assert!(gate.0.lock().unwrap().places.is_empty());
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

    fn evicted(admitted: &Admitted) -> bool {
        use futures::FutureExt;
        admitted.evicted.notified().now_or_never().is_some()
    }

    /// The listener full of connections that have shown no code, from a few
    /// sources at their share each: a newcomer still gets in, and the place it
    /// takes is always from a source holding more than the players do — never a
    /// player's one connection, and never a connection that showed a code.
    #[test]
    fn a_full_gate_makes_room_from_the_busiest_source_and_never_from_a_player() {
        let v4 = |n: u32| IpAddr::V4(Ipv4Addr::from(0xcb00_7100 + n));
        let gate = Arc::new(Gate::default());

        let downloading = gate.admit(v4(200)).unwrap();
        downloading.prove();
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
            assert_eq!(gate.0.lock().unwrap().places.len(), MAX_CONNECTIONS);
        }
        let dropped: Vec<bool> = crowd.iter().map(evicted).collect();
        assert_eq!(dropped.iter().filter(|&&d| d).count(), 100);
        // The oldest of the first source went first.
        assert!(dropped[0]);
        assert!(
            !evicted(&downloading),
            "a player downloading lost its place"
        );
        assert!(
            !evicted(&waiting),
            "a player's one connection went before a source with two"
        );
        assert!(newcomers.iter().all(|n| !evicted(n)));

        drop((crowd, newcomers, waiting));
        assert_eq!(
            gate.0.lock().unwrap().places.len(),
            1,
            "given-up places are gone"
        );

        // Every place held by someone who showed a code: nothing to drop.
        let players: Vec<Admitted> = (1..MAX_CONNECTIONS as u32)
            .map(|n| {
                let player = gate.admit(v4(2000 + n)).unwrap();
                player.prove();
                player
            })
            .collect();
        assert!(
            gate.admit(v4(9999)).is_none(),
            "a player was dropped for a newcomer"
        );
        assert!(players.iter().all(|p| !evicted(p)));
        drop(downloading);
    }

    /// Only what the player takes counts as moving, and the wait is measured
    /// from the last of it — so a download that keeps moving is never cut, and
    /// one that stops is cut the limit after it stopped.
    #[tokio::test(start_paused = true)]
    async fn an_answer_is_stalled_only_once_it_stops_sending() {
        use tokio::time::Instant;
        let limit = Duration::from_secs(60);
        let gate = Arc::new(Gate::default());
        let admitted = Arc::new(gate.admit("203.0.113.1".parse().unwrap()).unwrap());

        let mut stalled = Box::pin(admitted.stalled(limit));
        // Not started: a pack being built for ten minutes is not a stall.
        assert!(tokio::time::timeout(limit * 10, &mut stalled)
            .await
            .is_err());

        let bytes = Bytes::from(vec![7u8; 16 * 1024]);
        let mut body = watched(
            StreamBody::new(futures::stream::iter(
                std::iter::repeat_with(move || Ok(Frame::data(bytes.clone()))).take(40),
            ))
            .boxed_unsync(),
            admitted.clone(),
        );
        let mut last_frame = Instant::now();
        for _ in 0..40 {
            assert!(body.frame().await.is_some());
            last_frame = Instant::now();
            assert!(
                tokio::time::timeout(limit / 2, &mut stalled).await.is_err(),
                "cut off while still sending"
            );
        }
        tokio::time::timeout(limit * 2, &mut stalled)
            .await
            .expect("never cut off after it stopped sending");
        assert_eq!(last_frame.elapsed(), limit);
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
        let scratch = Scratch::new("crowd");
        let (_, code) = share_a_server(&scratch.paths, 0).await;
        let host = Host::start(scratch.paths.clone(), 0).await.unwrap();
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
        let scratch = Scratch::new("proven");
        let (_, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let host = Host::start(scratch.paths.clone(), 0).await.unwrap();
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
    #[tokio::test]
    async fn a_download_nobody_reads_is_cut_off_once_it_stalls() {
        let scratch = Scratch::new("stall");
        // Megabytes that do not compress, so the pack outlasts every buffer
        // between the two ends — a little under two, measured on loopback —
        // and the host really is left waiting to send.
        let (_, code) = share_a_server(&scratch.paths, 4 * 1024 * 1024).await;
        let limit = Duration::from_secs(3);
        let host = Host::start_with(scratch.paths.clone(), 0, limit)
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
        // and stays there for a while: waiting on a player is not yet a stall.
        let stopped = Instant::now();
        tokio::time::sleep_until((stopped + limit / 2).into()).await;
        assert_eq!(
            outgoing_files(&scratch.paths),
            1,
            "cut off before the limit, or never sending"
        );

        tokio::time::sleep_until((stopped + limit + Duration::from_millis(1500)).into()).await;
        assert_eq!(
            outgoing_files(&scratch.paths),
            0,
            "a download nobody reads still holds its pack file past the stall limit"
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

    /// Links can carry an IPv6 address, so the listener has to answer on one —
    /// and on IPv4 still, from the same port.
    #[tokio::test]
    async fn players_are_served_over_ipv4_and_ipv6_alike() {
        let scratch = Scratch::new("dual");
        let (_, code) = share_a_server(&scratch.paths, 0).await;
        let host = Host::start(scratch.paths.clone(), 0).await.unwrap();
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
        let scratch = Scratch::new("queue");
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
        let scratch = Scratch::new("hangup");
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

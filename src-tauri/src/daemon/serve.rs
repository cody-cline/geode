//! The management listener: the socket, and the ceilings on it.
//!
//! One plain HTTP listener on one port. **No TLS of its own**: it expects to be
//! reached over a loopback-published port, a reverse proxy, a tunnel or a VPN,
//! and [`DEFAULT_LISTEN`](super::settings::DEFAULT_LISTEN) is loopback so that
//! is the shape by default. That is a different decision from
//! [`crate::share::host`], which faces the open internet and therefore carries
//! its own certificate; this port is the admin's own, and the thing in front of
//! it is theirs to choose.
//!
//! What it serves is [`super::api`]'s. This file is the part below that: the
//! accept loop, and the ceilings `share/host.rs` holds itself to on anything a
//! stranger can make it allocate — a cap on connections being served, checked
//! before anything is spent on one, and a deadline on reading request headers
//! so an opened socket that says nothing cannot hold a place. A connection
//! over the cap is closed unread rather than queued.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::{watch, Semaphore};

use crate::error::{Error, Result};

use super::{api, Host};

/// How many connections are served at once.
///
/// A management API has one client, or a handful. The ceiling is not a capacity
/// plan — it is the number past which somebody opening sockets is doing it on
/// purpose, and it costs them nothing to find out that it will not work.
const MAX_CONNECTIONS: usize = 64;

/// How long a connection has to send its request headers.
///
/// Also the idle limit between requests on a kept-alive connection, which is
/// what stops an opened socket holding one of the places above for as long as
/// it likes. A console socket is past the headers by the time this would
/// apply, so watching a quiet server is not idling.
const HEADER_TIMEOUT: Duration = Duration::from_secs(15);

/// A running management listener.
pub struct Listener {
    /// Where it actually bound. Port 0 asks the OS to choose, which is what
    /// tests do, and this is then the port it chose.
    pub local_addr: SocketAddr,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Listener {
    pub async fn start(addr: SocketAddr, host: Arc<Host>) -> Result<Self> {
        let listener = TcpListener::bind(addr).await.map_err(|e| {
            Error::msg(format!(
                "could not listen on {addr}: {e} — another program may be using that port"
            ))
        })?;
        let local_addr = listener.local_addr()?;

        let (stop, mut stopped) = watch::channel(false);
        let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let cut = stop.subscribe();

        let task = tokio::spawn(async move {
            loop {
                let (tcp, _peer) = tokio::select! {
                    _ = stopped.changed() => break,
                    accepted = listener.accept() => match accepted {
                        Ok(accepted) => accepted,
                        // Out of file descriptors and the like. Pause rather
                        // than spin on an error that repeats immediately.
                        Err(_) => {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    },
                };
                // Over the ceiling: closed unread, before anything is spent on
                // it. Dropping the socket is the refusal — an answer would
                // cost the very thing being protected.
                let Ok(permit) = connections.clone().try_acquire_owned() else {
                    continue;
                };
                let mut cut = cut.clone();
                let host = host.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let service = hyper::service::service_fn(move |request| {
                        let host = host.clone();
                        async move {
                            Ok::<_, std::convert::Infallible>(
                                api::respond(&host, request).await,
                            )
                        }
                    });
                    // `with_upgrades`, because the console route hands its
                    // connection over to a WebSocket; without it the 101 goes
                    // out and the upgrade never completes.
                    let served = hyper::server::conn::http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEADER_TIMEOUT)
                        .serve_connection(TokioIo::new(tcp), service)
                        .with_upgrades();
                    // Cut when the host is stopping. Not for the port's sake —
                    // that is freed by the accept loop above ending, which
                    // drops the listening socket — but because without it a
                    // connection sitting between requests is held while the
                    // servers below are being shut down. A client cut this way
                    // reconnects; one left hanging waits on a host that has
                    // gone.
                    //
                    // A console socket is not among them, and that is right
                    // rather than overlooked: `with_upgrades` hands the
                    // connection over and *finishes*, so this future is
                    // already done by the time the cut arrives, and the socket
                    // lives on in the task `console::open` spawned. Which is
                    // what an admin wants — the shutdown below is the part
                    // worth watching, and they see every chunk saved and every
                    // exit, until the process itself goes.
                    tokio::select! {
                        _ = served => {}
                        _ = cut.changed() => {}
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

    /// Stop accepting and cut what is being served. The port is free, and no
    /// request is still in flight, once this returns. Console sockets are the
    /// exception, and deliberately: see the cut above.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

#[cfg(test)]
pub(super) mod tests {
    //! Against a listener on a port the OS chose, spoken to over a real
    //! socket, with its data in a scratch directory. Nothing here reaches the
    //! network or starts a Minecraft server.

    use std::io::{Read, Write};
    use std::net::TcpStream;

    use crate::paths::Scratch;

    use super::super::token::Token;
    use super::*;

    /// A token no test should be able to guess by accident.
    pub const TOKEN: &str = "f3a9c1d7e5b204860000";

    /// A host over `scratch`, with nothing in it.
    pub fn host(scratch: &Scratch) -> Arc<Host> {
        Arc::new(Host::new(
            Arc::new(crate::state::AppState::new(
                scratch.paths.clone(),
                crate::config::Config::default(),
            )),
            Token::new(TOKEN),
            None,
        ))
    }

    pub async fn started(host: Arc<Host>) -> Listener {
        Listener::start(SocketAddr::from(([127, 0, 0, 1], 0)), host)
            .await
            .expect("loopback, port chosen by the OS")
    }

    /// One request, one answer, over a plain socket — so what is asserted is
    /// what went over the wire and not what a client library made of it.
    pub fn ask(addr: SocketAddr, request: &str) -> String {
        let mut socket = TcpStream::connect(addr).expect("the listener is up");
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        socket.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        let _ = socket.read_to_string(&mut answer);
        answer
    }

    /// A request carrying the host's token.
    pub fn signed(addr: SocketAddr, method: &str, path: &str, body: Option<&str>) -> String {
        let body = body.unwrap_or_default();
        ask(
            addr,
            &format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    pub fn get(addr: SocketAddr, path: &str) -> String {
        ask(
            addr,
            &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
        )
    }

    pub fn body_of(answer: &str) -> &str {
        answer.split("\r\n\r\n").nth(1).unwrap_or_default()
    }

    /// What the container healthcheck does, with no credentials of any kind.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_answers_with_no_token() {
        let scratch = Scratch::new("daemon-health");
        let listening = started(host(&scratch)).await;
        let answer = get(listening.local_addr, "/health");
        assert!(answer.starts_with("HTTP/1.1 200 "), "{answer}");
        assert!(answer.contains(r#"{"ok":true}"#), "{answer}");
        listening.stop().await;
    }

    /// The ceiling, from the outside: hold every place with sockets that say
    /// nothing, and the next one is closed unread rather than queued behind
    /// them. Then let one go, and the listener is answering again — the cap is
    /// on connections at once, not on connections ever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_so_many_connections_are_served_at_once() {
        let scratch = Scratch::new("daemon-ceiling");
        let listening = started(host(&scratch)).await;

        let mut held = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            held.push(TcpStream::connect(listening.local_addr).expect("within the ceiling"));
        }
        // Let the accept loop take all of them before asking for one more.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let over = get(listening.local_addr, "/health");
        assert!(
            over.is_empty(),
            "a connection over the ceiling should be closed unread, got {over:?}"
        );

        held.pop();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let after = get(listening.local_addr, "/health");
        assert!(after.starts_with("HTTP/1.1 200 "), "{after}");

        listening.stop().await;
    }

    /// And a console socket is deliberately *not* among what the stop cuts, so
    /// the admin watching a server sees it save and exit rather than losing
    /// the connection the moment the shutdown starts.
    ///
    /// The mirror of the test below it, and here rather than beside the
    /// console's own tests because what it pins is this file's cut: an upgrade
    /// finishes the connection future, so the cut is already over by the time
    /// it fires, and anything added here that reached an upgraded socket would
    /// break this.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_console_socket_keeps_watching_while_the_servers_are_stopped() {
        let scratch = Scratch::new("daemon-console-cut");
        let listening = started(host(&scratch)).await;

        let mut socket = TcpStream::connect(listening.local_addr).expect("the listener is up");
        socket
            .write_all(
                b"GET /v1/servers/survival/console HTTP/1.1\r\nHost: localhost\r\n\
                  Authorization: Bearer f3a9c1d7e5b204860000\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                  Sec-WebSocket-Version: 13\r\n\r\n",
            )
            .unwrap();
        let mut answer = [0u8; 512];
        let read = socket.read(&mut answer).expect("the handshake reply");
        assert!(
            String::from_utf8_lossy(&answer[..read]).starts_with("HTTP/1.1 101 "),
            "{:?}",
            String::from_utf8_lossy(&answer[..read])
        );

        listening.stop().await;

        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let after = socket.read(&mut answer);
        assert!(
            matches!(&after, Err(e) if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )),
            "the console should still be open and quiet, got {after:?}"
        );
    }

    /// A container stop has to leave the port free, not leave it held by a
    /// client that has not hung up.
    ///
    /// Proved by nothing answering there any more, rather than by binding the
    /// port again: the OS hands these out as ephemeral ports, and a rebind
    /// races every other test in this binary asking for one — which it lost,
    /// reporting "address already in use" for a port this code had released.
    /// A refused connection says what the test means, and says it about this
    /// listener rather than about the OS's bookkeeping.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_port_is_free_once_it_has_stopped() {
        let scratch = Scratch::new("daemon-port");
        let listening = started(host(&scratch)).await;
        let addr = listening.local_addr;
        let _idle = TcpStream::connect(addr).expect("a client that never says anything");
        listening.stop().await;

        let refused = TcpStream::connect(addr);
        assert!(
            refused.is_err(),
            "something is still listening on {addr} after the host stopped"
        );
    }

    /// And a client holding a connection open between requests is cut, rather
    /// than left waiting on a host that has gone.
    ///
    /// A separate test from the one above, because that one does not reliably
    /// notice: the port is freed by the accept loop ending, so whether a
    /// rebind succeeds says little about what happened to a connection
    /// already being served. This is the half that does. Asserted as
    /// end-of-stream well inside [`HEADER_TIMEOUT`], because a connection
    /// merely left alone closes when that runs out, and a test that waited
    /// that long could not tell the two apart.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_connection_held_open_between_requests_is_cut_when_the_host_stops() {
        let scratch = Scratch::new("daemon-cut");
        let listening = started(host(&scratch)).await;

        // One request answered, then kept alive — so the connection is being
        // served and is idle, which is the state the cut is for.
        let mut socket = TcpStream::connect(listening.local_addr).expect("the listener is up");
        socket
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
            .unwrap();
        let mut answer = [0u8; 256];
        let read = socket.read(&mut answer).expect("the health answer");
        assert!(
            String::from_utf8_lossy(&answer[..read]).starts_with("HTTP/1.1 200 "),
            "{:?}",
            String::from_utf8_lossy(&answer[..read])
        );

        let waited = std::time::Instant::now();
        listening.stop().await;

        // Generous enough not to be flaky, far short of the idle timeout that
        // would close this connection on its own.
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let after = socket.read(&mut answer);
        assert!(
            matches!(after, Ok(0)),
            "the connection should have been closed, got {after:?}"
        );
        assert!(
            waited.elapsed() < HEADER_TIMEOUT,
            "it closed after {:?}, which is the idle timeout expiring rather than the host cutting it",
            waited.elapsed()
        );
    }
}

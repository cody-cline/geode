//! Where the host's events go, and the socket an admin watches them on.
//!
//! The host's end of [`crate::events`], and the counterpart to
//! [`crate::desktop`]. Everything the library reports goes two places at once:
//! standard output, one JSON object per line, which is where a container's
//! logs come from; and a fan-out that every open console socket reads.
//!
//! Both always, not one or the other. `docker logs` is the only record of a
//! host nobody was watching, and a socket that only showed what happened while
//! it was open would make the log the real console and this a convenience.
//!
//! # The socket
//!
//! ```text
//! GET /v1/servers/{id}/console   Upgrade: websocket   → every line that server says
//! ```
//!
//! Text in is a console command for that server, exactly as if it had been
//! typed into the app's console. JSON out is `{"topic":…,"payload":…}`, the
//! same events the app's webview receives, filtered to the one server
//! ([`concerns`]) so watching one server is not watching all of them.
//!
//! Its ceilings, in the spirit of `share/host.rs`: only so many sockets at
//! once, a bounded backlog for a reader that falls behind — told, rather than
//! quietly missing lines — and a cap on how much one incoming message may be,
//! because a console command is a line and anything longer is not one.
//!
//! One thing it does not do, said rather than implied: there is no keepalive.
//! A socket watching a *talking* server notices a dead peer on the next line
//! it fails to send, but one watching a silent server holds its place until
//! TCP gives up, which can be hours. So a laptop that closed its lid can cost
//! a place out of [`MAX_SOCKETS`] for a while. Left alone because opening one
//! of these takes the host's token, so this is the admin's own connection
//! rather than a stranger's, and a ping every few seconds to every console is
//! a cost paid always against a nuisance paid rarely.

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use hyper::header::{
    CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION, UPGRADE,
};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use sha1::{Digest, Sha1};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::protocol::{Message, Role, WebSocketConfig};

use crate::events::Events;

use super::api::{self, Body};
use super::Host;

/// Events kept for a socket that has fallen behind.
///
/// A starting server says a few hundred lines in a second or two, so a backlog
/// this size rides out a reader that stalls briefly. One that stalls for longer
/// is told how many lines it missed rather than silently shown a console with
/// holes in it.
const BACKLOG: usize = 1024;

/// Console sockets open at once, over every server together.
///
/// Each holds a subscription and a task. An admin has one console open, or a
/// few; past this somebody is opening sockets on purpose.
pub const MAX_SOCKETS: usize = 16;

/// The most one incoming message may be.
///
/// A console command is a line someone typed. Nothing a server accepts is
/// anywhere near this, and the point of a limit is that the far end cannot
/// choose how much this process allocates.
const MAX_MESSAGE: usize = 8 * 1024;

/// The magic string RFC 6455 mixes into the key, so a reply proves the server
/// understood the handshake rather than merely echoing.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// One thing that happened, on its way to every socket watching.
#[derive(Debug)]
pub struct Event {
    pub topic: String,
    pub payload: serde_json::Value,
}

/// The host's event sink: standard output, and every open socket.
pub struct Console {
    lines: broadcast::Sender<Arc<Event>>,
}

impl Default for Console {
    fn default() -> Self {
        Self {
            lines: broadcast::channel(BACKLOG).0,
        }
    }
}

impl Console {
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.lines.subscribe()
    }
}

impl Events for Console {
    fn emit(&self, topic: &str, payload: serde_json::Value) {
        let event = Arc::new(Event {
            topic: topic.to_string(),
            payload,
        });
        println!(
            "{}",
            serde_json::json!({ "topic": event.topic, "payload": event.payload })
        );
        // Nobody watching is the ordinary case, not a failure.
        let _ = self.lines.send(event);
    }
}

/// Whether an event belongs in `id`'s console.
///
/// Two ways it can. Most say which server they are about — a log line, an
/// exit. Progress does not: it is keyed by the job, and the jobs a server's
/// own start and backup create are named after it, which is how "Downloading
/// Java 21" reaches the console of the server that is waiting for it.
///
/// Pure, and the part of the socket worth proving: a rule that let another
/// server's lines through would put one server's console in another's, and a
/// rule too strict would leave an admin watching a server start in silence.
pub fn concerns(id: &str, event: &Event) -> bool {
    let field = |name: &str| event.payload.get(name).and_then(|v| v.as_str());
    if field("serverId") == Some(id) {
        return true;
    }
    match field("id") {
        Some(job) => job == format!("server:{id}") || job == format!("server-backup:{id}"),
        None => false,
    }
}

/// Answer a console request: either the 101 that hands this connection over, or
/// the same refusal as any other route.
///
/// The upgrade itself is awaited on a task of its own, because hyper only
/// completes it after this response has gone out.
pub fn open(
    host: &Arc<Host>,
    id: String,
    request: &mut Request<hyper::body::Incoming>,
) -> Response<Body> {
    // A browser that asked for a plain GET of this path gets told what is
    // actually here, rather than a blank 404 that reads like a typo.
    let upgrading = header(request, &UPGRADE).is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let version = header(request, &SEC_WEBSOCKET_VERSION) == Some("13");
    let Some(key) = header(request, &SEC_WEBSOCKET_KEY) else {
        return api::message(StatusCode::BAD_REQUEST, "this route is a WebSocket");
    };
    if !upgrading || !version {
        return api::message(StatusCode::BAD_REQUEST, "this route is a WebSocket");
    }
    let accept = accept_key(key);

    // Taken before the 101, so a socket over the ceiling is refused in a way
    // the client can read rather than accepted and dropped a moment later.
    let Ok(permit) = host.sockets.clone().try_acquire_owned() else {
        return api::message(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many consoles are open on this host",
        );
    };

    let upgraded = hyper::upgrade::on(request);
    let host = host.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let Ok(upgraded) = upgraded.await else {
            return;
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            TokioIo::new(upgraded),
            Role::Server,
            Some(config),
        )
        .await;
        pump(host, id, socket).await;
    });

    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, "upgrade")
        .header(UPGRADE, "websocket")
        .header(SEC_WEBSOCKET_ACCEPT, accept)
        .body(Body::default())
        .expect("a 101 built from fixed, valid parts")
}

/// Carry events out and commands in until one end stops.
async fn pump<S>(host: Arc<Host>, id: String, socket: tokio_tungstenite::WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut events = host.console.subscribe();
    let (mut out, mut incoming) = socket.split();

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => {
                    if !concerns(&id, &event) {
                        continue;
                    }
                    let line = serde_json::json!({
                        "topic": event.topic,
                        "payload": event.payload,
                    });
                    if out.send(Message::text(line.to_string())).await.is_err() {
                        break;
                    }
                }
                // Behind by more than the backlog. Said out loud: a console
                // with a gap in it and no mention of one is a console that
                // lies about what the server said.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    let notice = serde_json::json!({
                        "topic": "geode://console-gap",
                        "payload": { "serverId": id, "missed": missed },
                    });
                    if out.send(Message::text(notice.to_string())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            message = incoming.next() => match message {
                // A line typed into the console, sent to the server exactly as
                // the app sends one. A command for a stopped server is
                // dropped by the registry, which is what the app does too.
                Some(Ok(Message::Text(text))) => {
                    let command = text.trim();
                    if !command.is_empty() {
                        host.state.servers.send(&id, command);
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                // Ping and Pong are answered by the protocol itself; a binary
                // frame is not a console command and is ignored rather than
                // treated as one.
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
        }
    }
    let _ = out.close().await;
}

fn header<'r>(
    request: &'r Request<hyper::body::Incoming>,
    name: &hyper::header::HeaderName,
) -> Option<&'r str> {
    request.headers().get(name)?.to_str().ok()
}

/// The reply RFC 6455 asks for: the client's key and the protocol's magic
/// string, hashed and base64'd.
fn accept_key(key: &str) -> String {
    use base64::Engine as _;
    let mut hash = Sha1::new();
    hash.update(key.as_bytes());
    hash.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(topic: &str, payload: serde_json::Value) -> Event {
        Event {
            topic: topic.to_string(),
            payload,
        }
    }

    /// The example RFC 6455 prints, so this is checked against the standard
    /// rather than against itself.
    #[test]
    fn the_handshake_reply_is_the_one_the_standard_asks_for() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    /// One server's console must show that server, and a starting server's
    /// downloads, which are named after the job rather than the server.
    #[test]
    fn a_console_shows_its_own_servers_lines_and_the_jobs_named_after_it() {
        for shown in [
            event(
                "geode://server-log",
                serde_json::json!({ "serverId": "survival", "line": "Done", "stream": "out" }),
            ),
            event(
                "geode://server-exit",
                serde_json::json!({ "serverId": "survival", "code": 0 }),
            ),
            event(
                "geode://progress",
                serde_json::json!({ "id": "server:survival", "stage": "Downloading Java" }),
            ),
            event(
                "geode://progress",
                serde_json::json!({ "id": "server-backup:survival", "stage": "Archiving" }),
            ),
        ] {
            assert!(concerns("survival", &shown), "{:?}", shown.payload);
        }
    }

    /// The half that matters more: another server's console is not this one.
    /// `survival-2` is in the list because a prefix match would let it through,
    /// which is how one admin's console ends up showing somebody else's world.
    #[test]
    fn a_console_shows_nothing_belonging_to_another_server() {
        for hidden in [
            event(
                "geode://server-log",
                serde_json::json!({ "serverId": "creative", "line": "Done" }),
            ),
            event(
                "geode://server-log",
                serde_json::json!({ "serverId": "survival-2", "line": "Done" }),
            ),
            event(
                "geode://progress",
                serde_json::json!({ "id": "server:creative" }),
            ),
            event(
                "geode://progress",
                serde_json::json!({ "id": "server:survival-2" }),
            ),
            // An instance being installed on this host is nobody's console.
            event(
                "geode://progress",
                serde_json::json!({ "id": "install:survival" }),
            ),
            event("geode://daemon", serde_json::json!({ "message": "stopping" })),
        ] {
            assert!(!concerns("survival", &hidden), "{:?}", hidden.payload);
        }
    }

    /// The sink's own contract: what the library emits reaches a socket that
    /// subscribed before it happened.
    ///
    /// Bounded, because an unbounded `recv` on a sink that fans out to nobody
    /// does not fail — it waits for ever, and a test that hangs reports
    /// nothing at all.
    #[tokio::test]
    async fn an_event_reaches_a_console_that_is_already_watching() {
        let console = Console::default();
        let mut watching = console.subscribe();
        console.emit(
            "geode://server-log",
            serde_json::json!({ "serverId": "survival", "line": "Done (8.2s)" }),
        );
        let seen = tokio::time::timeout(std::time::Duration::from_secs(5), watching.recv())
            .await
            .expect("the event should reach a watching console")
            .expect("the event was sent");
        assert_eq!(seen.topic, "geode://server-log");
        assert!(concerns("survival", &seen));
    }
}

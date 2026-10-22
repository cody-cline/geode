//! The management API: what the app does to a server, over HTTP.
//!
//! ```text
//! GET    /health                                   no token; the healthcheck
//!
//! GET    /v1/servers                               every server here
//! POST   /v1/servers                               {name, kind, mcVersion} → 202
//! GET    /v1/servers/{id}
//! PATCH  /v1/servers/{id}                          {name?, notes?, settings?}
//! DELETE /v1/servers/{id}
//! POST   /v1/servers/{id}/install                  → 202; a retry after a failure
//! POST   /v1/servers/{id}/eula                     accept Mojang's EULA
//! POST   /v1/servers/{id}/start
//! POST   /v1/servers/{id}/stop                     asked to stop, so it saves
//! POST   /v1/servers/{id}/kill                     only when it will not
//! POST   /v1/servers/{id}/command                  {command}
//! GET    /v1/servers/{id}/properties               server.properties
//! PUT    /v1/servers/{id}/properties               [{key, value}]
//! GET    /v1/servers/{id}/content/{kind}           mods, plugins, datapacks…
//! GET    /v1/servers/{id}/tasks                    scheduled backups and commands
//! PUT    /v1/servers/{id}/tasks                    the whole list, edited as one
//! GET    /v1/servers/{id}/backups
//! POST   /v1/servers/{id}/backups                  → 202
//! DELETE /v1/servers/{id}/backups/{name}
//! POST   /v1/servers/{id}/backups/{name}/restore
//! GET    /v1/servers/{id}/console                  the WebSocket; see `console`
//! ```
//!
//! # Nothing here reimplements a command
//!
//! Every route below is an envelope: it checks the token, parses what arrived,
//! and calls the function in `server::` that the matching Tauri command calls.
//! `server::run::start` is what both Start buttons press,
//! `server::backup::create` is what both Backup buttons press, and so on. A
//! second copy of any of that would be a second answer, and the two would
//! drift.
//!
//! # Facing a network
//!
//! Held to `share/host.rs`'s rules, which [`super::serve`] and this file split
//! between them:
//!
//! * **The token first.** Before the path is matched, before a body is read,
//!   before anything is read from disk. A request without it cannot even learn
//!   whether a route exists.
//! * **One refusal.** A missing token, a wrong token and a token for a route
//!   that does not exist are the same 401, the same body, after the same fixed
//!   delay. A route that does not exist and a route asked with the wrong
//!   method are the same 404.
//! * **A ceiling on anything a stranger can make this allocate.** A request
//!   body stops at [`MAX_BODY`]; connections and header deadlines are
//!   [`super::serve`]'s; console sockets and their messages are
//!   [`super::console`]'s.
//!
//! One thing is deliberately *not* uniform. Once a token has matched, an
//! operation that fails answers with the real reason — "this server has no jar
//! yet", "stop the server before restoring a backup" — because the holder of
//! the token administers this machine, and `Error`'s text is written for a
//! person to read. Hiding it from them would only mean reading container logs
//! to find out why a button did nothing.
//!
//! # What a transfer would need
//!
//! Not built, and [`super`] says why and what the decision is. Nothing here
//! forecloses it: a server arriving would be a new id under `servers/`, which
//! the list reads from disk every time and so cannot be stale, and the routes
//! are shaped per server rather than as one document to replace.

use std::sync::Arc;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::instance::content::ContentKind;
use crate::net::Progress;
use crate::server::{self, ServerKind};

use super::{console, Host};

pub type Body = Full<Bytes>;

/// The most a request body may be.
///
/// Everything sent here is a handful of fields or a list of a few dozen
/// settings. Generous for all of it, and far too small to be worth sending as
/// a way of filling this machine's memory.
pub const MAX_BODY: usize = 64 * 1024;

/// How long a refusal takes, whatever was wrong with it.
///
/// The same reason `share/host.rs` waits: a token checked in constant time
/// still answers faster when there was no token to check at all, and a fixed
/// floor under every refusal removes that difference. It also makes guessing
/// slow, which is worth more here than the millisecond it costs an admin.
const REFUSAL_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// The longest a server id may be.
///
/// Ids are made from a server's name by `instance::slugify`, so a real one is
/// a few words. The limit is here because the id arrives in a path from the
/// network and ends up in a file path.
const MAX_ID: usize = 80;

/// The longest a Minecraft version may be.
///
/// The longest Mojang has published is a snapshot name; this is generous for
/// every one of them and, like [`MAX_ID`], is here because the value arrives
/// over the network and ends up in a file name.
const MAX_VERSION: usize = 32;

/// A server, as the API describes it.
///
/// The stored record, plus the two things a caller cannot work out from it:
/// whether a process is attached right now, and whether its software is
/// actually in place — which is not the same as having a jar, since modern
/// Forge and NeoForge servers have none.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerState {
    #[serde(flatten)]
    server: server::Server,
    running: bool,
    installed: bool,
}

impl ServerState {
    fn of(server: server::Server, host: &Host) -> Self {
        Self {
            running: host.state.servers.is_running(&server.id),
            installed: server.is_installed(),
            server,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewServer {
    name: String,
    kind: String,
    mc_version: String,
}

/// The same shape the app's `update_server` takes, minus the schedules, which
/// have a route of their own so there is only one way to set them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerPatch {
    name: Option<String>,
    notes: Option<String>,
    settings: Option<server::ServerSettings>,
}

#[derive(Deserialize)]
struct ConsoleCommand {
    command: String,
}

/// Answer one request.
pub async fn respond(host: &Arc<Host>, mut request: Request<Incoming>) -> Response<Body> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    // The one route with no token, because `docker-compose.yml`'s healthcheck
    // calls it with no credentials. Safe to leave open because it reads
    // nothing — not a file, not a server, not the configuration — and says
    // nothing but that the process is answering. A stopped server is still a
    // healthy host, which is why counting processes would be wrong here.
    if method == Method::GET && path == "/health" {
        return json(StatusCode::OK, &serde_json::json!({ "ok": true }));
    }

    // Before the path is looked at and before a body is read.
    if !authorised(host, &request) {
        tokio::time::sleep(REFUSAL_DELAY).await;
        return message(StatusCode::UNAUTHORIZED, "unauthorised");
    }

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // Whatever stands where a server id goes is re-parsed once, here, so no
    // route below can forget to — the same reason `backup::resolve` is the one
    // place a backup name is checked. Something that could not be an id names
    // nothing, and is refused exactly as an unknown route is.
    let id = match segments.get(2) {
        None => String::new(),
        Some(raw) => match server_id(raw) {
            Some(id) => id,
            None => return not_found(),
        },
    };

    // Answered before the body is taken, because this is the one route that
    // needs the connection itself rather than what was sent on it.
    if method == Method::GET && matches!(segments.as_slice(), ["v1", "servers", _, "console"]) {
        return console::open(host, id, &mut request);
    }

    // Read once, under one ceiling, rather than per route — a route that
    // forgot to would be a route with no ceiling at all.
    let Ok(body) = body_of(request).await else {
        return too_large();
    };

    match (&method, segments.as_slice()) {
        (&Method::GET, ["v1", "servers"]) => list(host).await,
        (&Method::POST, ["v1", "servers"]) => create(host, &body).await,

        (&Method::GET, ["v1", "servers", _]) => get(host, &id).await,
        (&Method::PATCH, ["v1", "servers", _]) => patch(host, &id, &body).await,
        (&Method::DELETE, ["v1", "servers", _]) => delete(host, &id).await,

        (&Method::POST, ["v1", "servers", _, "install"]) => install(host, &id).await,
        (&Method::POST, ["v1", "servers", _, "eula"]) => eula(host, &id).await,
        (&Method::POST, ["v1", "servers", _, "start"]) => start(host, &id).await,
        (&Method::POST, ["v1", "servers", _, "stop"]) => stop(host, &id).await,
        (&Method::POST, ["v1", "servers", _, "kill"]) => kill(host, &id).await,
        (&Method::POST, ["v1", "servers", _, "command"]) => command(host, &id, &body).await,

        (&Method::GET, ["v1", "servers", _, "properties"]) => properties(host, &id).await,
        (&Method::PUT, ["v1", "servers", _, "properties"]) => {
            write_properties(host, &id, &body).await
        }

        (&Method::GET, ["v1", "servers", _, "content", kind]) => content(host, &id, kind).await,

        (&Method::GET, ["v1", "servers", _, "tasks"]) => tasks(host, &id).await,
        (&Method::PUT, ["v1", "servers", _, "tasks"]) => write_tasks(host, &id, &body).await,

        (&Method::GET, ["v1", "servers", _, "backups"]) => backups(host, &id).await,
        (&Method::POST, ["v1", "servers", _, "backups"]) => back_up(host, &id).await,
        // The name is not checked here: `backup::resolve` already refuses
        // anything that is not a plain `.zip` beside that server's others, and
        // one guard every caller goes through is worth more than a second one
        // here that could disagree with it.
        (&Method::DELETE, ["v1", "servers", _, "backups", name]) => {
            answer(server::backup::delete(&host.state.paths, &id, name).await)
        }
        (&Method::POST, ["v1", "servers", _, "backups", name, "restore"]) => {
            restore(host, &id, name).await
        }

        // One refusal for everything else, so a route that never existed, one
        // that will exist tomorrow, and the right route asked with the wrong
        // method are impossible to tell apart.
        _ => not_found(),
    }
}

/// Whether the request carries this host's token.
///
/// Compared in constant time by [`super::token::Token`], over digests, so
/// neither how much of the token was right nor how long the real one is can be
/// read off how long this takes.
fn authorised(host: &Host, request: &Request<Incoming>) -> bool {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    host.token.matches(presented)
}

/// A server id, or nothing.
///
/// `server::load` joins what it is given straight onto `servers/`, which is
/// safe for the app — the id came from its own list — and is not safe for a
/// path off the network. So an id from a request is re-parsed here against
/// what `instance::slugify` can produce, which contains no `.` and no
/// separator and therefore cannot name anything outside one server's folder.
fn server_id(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.len() > MAX_ID
        || !raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return None;
    }
    Some(raw.to_string())
}

/// A Minecraft version, or nothing.
///
/// The other thing a create takes from the network that reaches the disk, and
/// so re-parsed for the same reason an id is. `server::meta` puts it both in
/// the URL it asks for a build and in the name of the file it writes into the
/// server's folder — `minecraft_server.{version}.jar`,
/// `purpur-{version}-{build}.jar` — and `install::install_jar` joins that name
/// onto the run directory. Nothing in the app sends anything but a version off
/// a published list; this is what stands in for that list here.
///
/// The shape is every version id Mojang has ever published: digits, letters,
/// dots, dashes and the underscore of `1.16_combat-6`. No separator of either
/// platform is in it, which is what makes a name built from it unable to leave
/// the folder, however the string is arranged.
fn mc_version(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.len() > MAX_VERSION
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return None;
    }
    Some(raw.to_string())
}

// ---------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------

async fn list(host: &Arc<Host>) -> Response<Body> {
    match server::list(&host.state.paths).await {
        Ok(servers) => {
            let states: Vec<ServerState> = servers
                .into_iter()
                .map(|server| ServerState::of(server, host))
                .collect();
            json(StatusCode::OK, &states)
        }
        Err(e) => refused(e),
    }
}

async fn get(host: &Arc<Host>, id: &str) -> Response<Body> {
    match server::load(&host.state.paths, id).await {
        Ok(server) => json(StatusCode::OK, &ServerState::of(server, host)),
        Err(e) => refused(e),
    }
}

/// Create the record, then fetch its software on a task of its own.
///
/// Answered before the download because resolving and fetching a server's
/// software takes minutes — and for Spigot, compiles it from source. A reply
/// held open for that is a reply most clients give up on. What happens next is
/// on the console as `geode://progress` for `server:{id}`, which is where the
/// app watches an install too.
async fn create(host: &Arc<Host>, body: &Bytes) -> Response<Body> {
    let new: NewServer = match parsed(body) {
        Ok(new) => new,
        Err(why) => return message(StatusCode::BAD_REQUEST, &why),
    };
    let Some(kind) = ServerKind::parse(&new.kind) else {
        return message(
            StatusCode::BAD_REQUEST,
            &format!("unknown server type '{}'", new.kind),
        );
    };
    let Some(version) = mc_version(&new.mc_version) else {
        return message(
            StatusCode::BAD_REQUEST,
            &format!(
                "'{}' is not a Minecraft version — they look like 1.21.1 or 24w14a",
                new.mc_version
            ),
        );
    };

    match server::create(&host.state.paths, new.name, kind, version).await {
        Ok(server) => {
            // The id is made from the name, and it is how every other route
            // reaches this server — so one too long for [`MAX_ID`] would be a
            // server that lists but can never be started, stopped or deleted
            // from here. Taken back rather than left stranded on disk, and
            // checked on the id itself because it is what `unique_id` settled
            // on and not merely what the name slugifies to.
            if server_id(&server.id).is_none() {
                let _ = server::delete(&host.state.paths, &server.id).await;
                return message(
                    StatusCode::BAD_REQUEST,
                    &format!(
                        "that name is too long: it makes an id of {} characters and this host handles at most {MAX_ID}",
                        server.id.len()
                    ),
                );
            }
            let id = server.id.clone();
            installing(host, id);
            json(StatusCode::ACCEPTED, &ServerState::of(server, host))
        }
        Err(e) => refused(e),
    }
}

/// Fetch a server's software again, for one whose first attempt failed.
async fn install(host: &Arc<Host>, id: &str) -> Response<Body> {
    // Read before the task starts, so an id for no server is refused here
    // rather than only appearing as a failure on the console.
    match server::load(&host.state.paths, id).await {
        Ok(_) => {
            installing(host, id.to_string());
            accepted()
        }
        Err(e) => refused(e),
    }
}

/// The install itself, on a task, reporting as the app's does.
fn installing(host: &Arc<Host>, id: String) {
    let host = host.clone();
    tokio::spawn(async move {
        let progress = Progress::new(host.sink.clone(), format!("server:{id}"));
        let mut server = match server::load(&host.state.paths, &id).await {
            Ok(server) => server,
            Err(e) => {
                progress.fail(&e.to_string()).await;
                return;
            }
        };
        match server::install::install_jar(
            &host.state.http,
            &host.state.paths,
            &mut server,
            &progress,
        )
        .await
        {
            // The folder stays on failure, as it does in the app: a server
            // with no software is something to retry, and deleting it would
            // throw away the name and settings somebody chose.
            Ok(()) => progress.finish().await,
            Err(e) => progress.fail(&e.to_string()).await,
        }
    });
}

async fn patch(host: &Arc<Host>, id: &str, body: &Bytes) -> Response<Body> {
    let patch: ServerPatch = match parsed(body) {
        Ok(patch) => patch,
        Err(why) => return message(StatusCode::BAD_REQUEST, &why),
    };
    let mut server = match server::load(&host.state.paths, id).await {
        Ok(server) => server,
        Err(e) => return refused(e),
    };
    if let Some(name) = patch.name {
        let name = name.trim().to_string();
        if name.is_empty() {
            return message(StatusCode::BAD_REQUEST, "the server needs a name");
        }
        server.name = name;
    }
    if let Some(notes) = patch.notes {
        server.notes = notes;
    }
    if let Some(settings) = patch.settings {
        server.settings = settings;
    }
    match server.save(&host.state.paths).await {
        Ok(()) => json(StatusCode::OK, &ServerState::of(server, host)),
        Err(e) => refused(e),
    }
}

async fn delete(host: &Arc<Host>, id: &str) -> Response<Body> {
    if host.state.servers.is_running(id) {
        return message(StatusCode::BAD_REQUEST, "stop the server before deleting it");
    }
    answer(server::delete(&host.state.paths, id).await)
}

async fn eula(host: &Arc<Host>, id: &str) -> Response<Body> {
    let mut server = match server::load(&host.state.paths, id).await {
        Ok(server) => server,
        Err(e) => return refused(e),
    };
    match server::accept_eula(&host.state.paths, &mut server).await {
        Ok(()) => json(StatusCode::OK, &ServerState::of(server, host)),
        Err(e) => refused(e),
    }
}

/// The same function the app's Start button calls, with this host's heap
/// ceiling applied to it.
async fn start(host: &Arc<Host>, id: &str) -> Response<Body> {
    match server::run::start(
        &host.sink,
        &host.state.paths,
        &host.state.http,
        &host.state.servers,
        id,
        host.max_heap_mb,
    )
    .await
    {
        Ok(started) => json(StatusCode::OK, &started),
        Err(e) => refused(e),
    }
}

/// Ask the server to stop the way an operator would, so it saves first.
async fn stop(host: &Arc<Host>, id: &str) -> Response<Body> {
    let stopping = host.state.servers.stop(id);
    json(StatusCode::OK, &serde_json::json!({ "stopping": stopping }))
}

async fn kill(host: &Arc<Host>, id: &str) -> Response<Body> {
    let killed = host.state.servers.kill(id);
    json(StatusCode::OK, &serde_json::json!({ "killed": killed }))
}

async fn command(host: &Arc<Host>, id: &str, body: &Bytes) -> Response<Body> {
    let sent: ConsoleCommand = match parsed(body) {
        Ok(sent) => sent,
        Err(why) => return message(StatusCode::BAD_REQUEST, &why),
    };
    let delivered = host.state.servers.send(id, sent.command.trim());
    json(StatusCode::OK, &serde_json::json!({ "sent": delivered }))
}

async fn properties(host: &Arc<Host>, id: &str) -> Response<Body> {
    match server::properties::read(&host.state.paths, id).await {
        Ok(properties) => json(StatusCode::OK, &properties),
        Err(e) => refused(e),
    }
}

async fn write_properties(host: &Arc<Host>, id: &str, body: &Bytes) -> Response<Body> {
    let changes: Vec<server::properties::Property> = match parsed(body) {
        Ok(changes) => changes,
        Err(why) => return message(StatusCode::BAD_REQUEST, &why),
    };
    answer(server::properties::write(&host.state.paths, id, &changes).await)
}

async fn content(host: &Arc<Host>, id: &str, kind: &str) -> Response<Body> {
    let Some(kind) = ContentKind::parse(kind) else {
        return message(
            StatusCode::BAD_REQUEST,
            &format!("unknown content type '{kind}'"),
        );
    };
    let server = match server::load(&host.state.paths, id).await {
        Ok(server) => server,
        Err(e) => return refused(e),
    };
    let dir = server::content_dir(&host.state.paths, id, kind).await;
    match crate::instance::content::list_in(&dir, &server.content, kind, &host.state.jars).await {
        Ok(items) => json(StatusCode::OK, &items),
        Err(e) => refused(e),
    }
}

async fn tasks(host: &Arc<Host>, id: &str) -> Response<Body> {
    match server::load(&host.state.paths, id).await {
        Ok(server) => json(StatusCode::OK, &server.schedules),
        Err(e) => refused(e),
    }
}

/// The whole list, edited as a unit — there are only ever a handful.
async fn write_tasks(host: &Arc<Host>, id: &str, body: &Bytes) -> Response<Body> {
    let mut schedules: Vec<server::schedule::Schedule> = match parsed(body) {
        Ok(schedules) => schedules,
        Err(why) => return message(StatusCode::BAD_REQUEST, &why),
    };
    let mut server = match server::load(&host.state.paths, id).await {
        Ok(server) => server,
        Err(e) => return refused(e),
    };
    // A new task is stamped as if it had just run, so adding one does not
    // immediately fire it. The same thing `update_server` does, for the same
    // reason.
    let now = chrono::Utc::now().timestamp();
    for schedule in &mut schedules {
        if schedule.last_run.is_none() {
            schedule.last_run = Some(now);
        }
    }
    server.schedules = schedules;
    match server.save(&host.state.paths).await {
        Ok(()) => json(StatusCode::OK, &server.schedules),
        Err(e) => refused(e),
    }
}

async fn backups(host: &Arc<Host>, id: &str) -> Response<Body> {
    match server::backup::list(&host.state.paths, id).await {
        Ok(backups) => json(StatusCode::OK, &backups),
        Err(e) => refused(e),
    }
}

/// Take a backup on a task of its own.
///
/// Answered before the archive exists, for the same reason an install is:
/// zipping a world is minutes of work, and the progress is on the console.
async fn back_up(host: &Arc<Host>, id: &str) -> Response<Body> {
    let server = match server::load(&host.state.paths, id).await {
        Ok(server) => server,
        Err(e) => return refused(e),
    };
    let host = host.clone();
    tokio::spawn(async move {
        let progress = Progress::new(host.sink.clone(), format!("server-backup:{}", server.id));
        match server::backup::create(&host.state.paths, &server.id, &progress).await {
            Ok(_) => {
                progress.finish().await;
                // Retention is the point of the setting; applying it when a
                // new backup lands keeps the folder bounded without a sweeper.
                if let Some(keep) = server.settings.keep_backups {
                    let _ =
                        server::backup::prune(&host.state.paths, &server.id, keep as usize).await;
                }
            }
            Err(e) => progress.fail(&e.to_string()).await,
        }
    });
    accepted()
}

/// Unpack a backup over the server's files.
///
/// Refused while it is running: restoring underneath a live server would have
/// it writing over what was just put back.
async fn restore(host: &Arc<Host>, id: &str, name: &str) -> Response<Body> {
    if host.state.servers.is_running(id) {
        return message(
            StatusCode::BAD_REQUEST,
            "stop the server before restoring a backup",
        );
    }
    answer(server::backup::restore(&host.state.paths, id, name).await)
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

async fn body_of(request: Request<Incoming>) -> std::result::Result<Bytes, ()> {
    Limited::new(request.into_body(), MAX_BODY)
        .collect()
        .await
        .map(|collected| collected.to_bytes())
        .map_err(|_| ())
}

/// Returns why rather than a built refusal, so neither this nor its callers
/// carry a whole HTTP response around in an `Err`.
fn parsed<T: serde::de::DeserializeOwned>(body: &Bytes) -> std::result::Result<T, String> {
    serde_json::from_slice(body)
        .map_err(|e| format!("that is not what this route expects: {e}"))
}

/// An operation the host would not do, said in the words `Error` already has
/// for it — those are written for a person, and the person here holds the
/// token.
fn refused(e: crate::error::Error) -> Response<Body> {
    message(StatusCode::BAD_REQUEST, &e.to_string())
}

fn answer(result: crate::error::Result<()>) -> Response<Body> {
    match result {
        Ok(()) => json(StatusCode::OK, &serde_json::json!({ "ok": true })),
        Err(e) => refused(e),
    }
}

fn accepted() -> Response<Body> {
    json(StatusCode::ACCEPTED, &serde_json::json!({ "started": true }))
}

fn not_found() -> Response<Body> {
    message(StatusCode::NOT_FOUND, "not found")
}

fn too_large() -> Response<Body> {
    message(StatusCode::PAYLOAD_TOO_LARGE, "that request is too large")
}

pub fn json<T: Serialize>(status: StatusCode, value: &T) -> Response<Body> {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .header(CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::from(bytes)))
        .expect("a response built from fixed, valid parts")
}

pub fn message(status: StatusCode, text: &str) -> Response<Body> {
    json(status, &serde_json::json!({ "error": text }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a real id looks like, and the shapes that would reach outside one
    /// server's folder if `server::load` were handed them.
    #[test]
    fn an_id_from_the_network_cannot_name_anything_but_a_server() {
        for real in ["survival", "sky-block-2", "s", "server-10"] {
            assert_eq!(server_id(real).as_deref(), Some(real));
        }
        for refused in [
            "",
            "..",
            "../config",
            "../../.ssh/id_rsa",
            "sur/vival",
            "survival/../../config",
            "survival.json",
            "Survival",          // slugify only ever produces lowercase
            "survival\\config",  // a separator on the other platform
            "survival\0",
            "sur vival",
            &"s".repeat(MAX_ID + 1),
        ] {
            assert!(server_id(refused).is_none(), "{refused:?} is not an id");
        }
    }

    /// Every shape Mojang has published, and the shapes that would put a jar
    /// somewhere other than the server's folder if `install_jar` were handed
    /// them.
    #[test]
    fn a_version_from_the_network_cannot_name_a_path() {
        for real in [
            "1.21.1",
            "1.21.1-pre1",
            "24w14a",
            "b1.7.3",
            "rd-132211",
            "1.16_combat-6",
            "1.RV-Pre1",
        ] {
            assert_eq!(mc_version(real).as_deref(), Some(real));
        }
        for refused in [
            "",
            "../../../tmp/evil",
            "/etc/cron.d/x",
            "1.21.1/../../x",
            "..\\windows",
            "1.21.1 ",
            "1.21.1\0",
            &"1".repeat(MAX_VERSION + 1),
        ] {
            assert!(mc_version(refused).is_none(), "{refused:?} is not a version");
        }
    }

    // -----------------------------------------------------------------------
    // Over a real socket, against a scratch directory. Nothing below reaches
    // the network or starts a Minecraft server.
    // -----------------------------------------------------------------------

    use crate::paths::Scratch;
    use crate::server::ServerKind;
    use std::collections::HashSet;

    use super::super::serve::tests::{body_of, get, host, signed, started, TOKEN};

    /// One route of each shape, including the ones that would change or delete
    /// something. Written out rather than derived from the router, because the
    /// point is to notice a route added without a guard — and a list generated
    /// from the router would grow a hole in step with it.
    const EVERY_ROUTE: &[(&str, &str)] = &[
        ("GET", "/v1/servers"),
        ("POST", "/v1/servers"),
        ("GET", "/v1/servers/survival"),
        ("PATCH", "/v1/servers/survival"),
        ("DELETE", "/v1/servers/survival"),
        ("POST", "/v1/servers/survival/install"),
        ("POST", "/v1/servers/survival/eula"),
        ("POST", "/v1/servers/survival/start"),
        ("POST", "/v1/servers/survival/stop"),
        ("POST", "/v1/servers/survival/kill"),
        ("POST", "/v1/servers/survival/command"),
        ("GET", "/v1/servers/survival/properties"),
        ("PUT", "/v1/servers/survival/properties"),
        ("GET", "/v1/servers/survival/content/mod"),
        ("GET", "/v1/servers/survival/tasks"),
        ("PUT", "/v1/servers/survival/tasks"),
        ("GET", "/v1/servers/survival/backups"),
        ("POST", "/v1/servers/survival/backups"),
        ("DELETE", "/v1/servers/survival/backups/old.zip"),
        ("POST", "/v1/servers/survival/backups/old.zip/restore"),
        ("GET", "/v1/servers/survival/console"),
    ];

    /// A server on disk, so a refusal has something it could have reached.
    async fn with_a_server(scratch: &Scratch) -> crate::server::Server {
        crate::server::create(
            &scratch.paths,
            "Survival".into(),
            ServerKind::Paper,
            "1.21.1".into(),
        )
        .await
        .expect("a scratch directory to write into")
    }

    fn unsigned(addr: std::net::SocketAddr, method: &str, path: &str) -> String {
        super::super::serve::tests::ask(
            addr,
            &format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        )
    }

    fn wrongly_signed(addr: std::net::SocketAddr, method: &str, path: &str) -> String {
        super::super::serve::tests::ask(
            addr,
            &format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer \
                 0000000000000000000000\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        )
    }

    /// The whole point of the token. Every route, with none and with a wrong
    /// one, and an unknown route besides — all the same refusal, so a stranger
    /// cannot even learn which routes exist. And the server is still there
    /// afterwards, because the `DELETE` in that list was refused before
    /// anything was read from disk, let alone removed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_route_does_anything_without_the_token() {
        let scratch = Scratch::new("daemon-auth");
        let server = with_a_server(&scratch).await;
        let listening = started(host(&scratch)).await;
        let addr = listening.local_addr;

        let mut refusals = Vec::new();
        for (method, path) in EVERY_ROUTE {
            refusals.push((*method, *path, unsigned(addr, method, path)));
            refusals.push((*method, *path, wrongly_signed(addr, method, path)));
        }
        // A route that does not exist, so "wrong token" and "no such route"
        // are not told apart either.
        refusals.push(("GET", "/v1/nothing", unsigned(addr, "GET", "/v1/nothing")));

        for (method, path, answer) in &refusals {
            assert!(
                answer.starts_with("HTTP/1.1 401 "),
                "{method} {path} answered {answer}"
            );
        }
        let bodies: HashSet<&str> = refusals
            .iter()
            .map(|(_, _, answer)| body_of(answer))
            .collect();
        assert_eq!(bodies.len(), 1, "every refusal must read alike: {bodies:?}");

        assert!(
            crate::server::load(&scratch.paths, &server.id).await.is_ok(),
            "the refused DELETE must not have touched the server"
        );
        listening.stop().await;
    }

    /// Past the token, a route that never existed, a route that will exist
    /// tomorrow, and the right route asked the wrong way are still the same
    /// answer — including an id that could only be an attempt to read
    /// something else.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unknown_route_is_the_same_refusal_however_it_is_unknown() {
        let scratch = Scratch::new("daemon-routes");
        let listening = started(host(&scratch)).await;
        let addr = listening.local_addr;

        let refusals = [
            signed(addr, "GET", "/", None),
            signed(addr, "GET", "/v1", None),
            signed(addr, "GET", "/v1/instances", None),
            // The right route, the wrong method.
            signed(addr, "DELETE", "/v1/servers", None),
            signed(addr, "GET", "/v1/servers/survival/start", None),
            // Ids that would name something outside one server's folder if
            // they were taken at face value.
            signed(addr, "GET", "/v1/servers/..", None),
            signed(addr, "GET", "/v1/servers/../../config.json", None),
            signed(addr, "GET", "/v1/servers/..%2f..%2fconfig.json", None),
            signed(addr, "GET", "/v1/servers/Survival/properties", None),
        ];

        for answer in &refusals {
            assert!(answer.starts_with("HTTP/1.1 404 "), "{answer}");
        }
        let bodies: HashSet<&str> = refusals.iter().map(|a| body_of(a)).collect();
        assert_eq!(bodies.len(), 1, "every refusal must read alike: {bodies:?}");
        listening.stop().await;
    }

    /// `/health` is the one open route, and it must stay open because the
    /// container healthcheck calls it with no credentials at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_is_the_only_route_that_needs_no_token() {
        let scratch = Scratch::new("daemon-open");
        let listening = started(host(&scratch)).await;
        let answer = get(listening.local_addr, "/health");
        assert!(answer.starts_with("HTTP/1.1 200 "), "{answer}");
        listening.stop().await;
    }

    /// A body is the one thing a caller chooses the size of, so it has a
    /// ceiling. Refused rather than collected, and the server is unchanged.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_body_past_the_ceiling_is_refused_rather_than_held() {
        let scratch = Scratch::new("daemon-body");
        let server = with_a_server(&scratch).await;
        let listening = started(host(&scratch)).await;

        let huge = format!(
            r#"{{"name":"{}","notes":""}}"#,
            "n".repeat(MAX_BODY + 1024)
        );
        let answer = signed(
            listening.local_addr,
            "PATCH",
            &format!("/v1/servers/{}", server.id),
            Some(&huge),
        );
        assert!(answer.starts_with("HTTP/1.1 413 "), "{answer}");

        let unchanged = crate::server::load(&scratch.paths, &server.id).await.unwrap();
        assert_eq!(unchanged.name, "Survival");
        listening.stop().await;
    }

    /// The list, and an edit, against the same files the app reads — so this
    /// proves the routes reach `server::` and not a copy of it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_api_lists_and_edits_the_servers_on_this_host() {
        let scratch = Scratch::new("daemon-crud");
        let server = with_a_server(&scratch).await;
        let listening = started(host(&scratch)).await;
        let addr = listening.local_addr;

        let listed = signed(addr, "GET", "/v1/servers", None);
        assert!(listed.starts_with("HTTP/1.1 200 "), "{listed}");
        let listed: serde_json::Value = serde_json::from_str(body_of(&listed)).unwrap();
        assert_eq!(listed[0]["id"], server.id.as_str());
        assert_eq!(listed[0]["name"], "Survival");
        // Nothing is running, and a server whose jar was never fetched is not
        // installed — both facts a caller cannot work out from the record.
        assert_eq!(listed[0]["running"], false);
        assert_eq!(listed[0]["installed"], false);

        let renamed = signed(
            addr,
            "PATCH",
            &format!("/v1/servers/{}", server.id),
            Some(r#"{"name":"Survival Two","notes":"moved here"}"#),
        );
        assert!(renamed.starts_with("HTTP/1.1 200 "), "{renamed}");
        let reloaded = crate::server::load(&scratch.paths, &server.id).await.unwrap();
        assert_eq!(reloaded.name, "Survival Two");
        assert_eq!(reloaded.notes, "moved here");

        listening.stop().await;
    }

    /// A name long enough to outgrow [`MAX_ID`] must not leave a server on
    /// disk that this API will not touch: it would list, and every route for
    /// it would answer the same 404 as a route that does not exist.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_name_too_long_to_address_makes_no_server_at_all() {
        let scratch = Scratch::new("daemon-long-name");
        let listening = started(host(&scratch)).await;
        let addr = listening.local_addr;

        // Over [`MAX_ID`] and well inside the 255 bytes a filename may be, so
        // what refuses this is the guard and not the filesystem.
        let name = "survival ".repeat(13);
        assert!(crate::instance::slugify(&name).len() > MAX_ID);
        let answer = signed(
            addr,
            "POST",
            "/v1/servers",
            Some(&format!(
                r#"{{"name":"{name}","kind":"paper","mcVersion":"1.21.1"}}"#
            )),
        );
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
        assert!(body_of(&answer).contains("too long"), "{}", body_of(&answer));

        let listed = signed(addr, "GET", "/v1/servers", None);
        let listed: serde_json::Value = serde_json::from_str(body_of(&listed)).unwrap();
        assert_eq!(
            listed.as_array().map(Vec::len),
            Some(0),
            "a server nothing here can reach was left behind: {listed}"
        );

        // And a name that does fit is still made, so the guard is on the
        // length and not on the route.
        let fine = signed(
            addr,
            "POST",
            "/v1/servers",
            Some(r#"{"name":"Survival","kind":"paper","mcVersion":"1.21.1"}"#),
        );
        assert!(fine.starts_with("HTTP/1.1 202 "), "{fine}");
        listening.stop().await;
    }

    /// And the route turns one away rather than storing it and finding out
    /// when the install writes the jar.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_version_that_is_a_path_makes_no_server_at_all() {
        let scratch = Scratch::new("daemon-version");
        let listening = started(host(&scratch)).await;
        let addr = listening.local_addr;

        let answer = signed(
            addr,
            "POST",
            "/v1/servers",
            Some(r#"{"name":"Survival","kind":"paper","mcVersion":"../../../../tmp/x"}"#),
        );
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
        assert!(
            body_of(&answer).contains("not a Minecraft version"),
            "{}",
            body_of(&answer)
        );

        let listed = signed(addr, "GET", "/v1/servers", None);
        let listed: serde_json::Value = serde_json::from_str(body_of(&listed)).unwrap();
        assert_eq!(listed.as_array().map(Vec::len), Some(0), "{listed}");
        listening.stop().await;
    }

    /// `server.properties` through the API is the same file the app edits.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn properties_written_through_the_api_are_read_back_from_the_file() {
        let scratch = Scratch::new("daemon-props");
        let server = with_a_server(&scratch).await;
        let listening = started(host(&scratch)).await;
        let path = format!("/v1/servers/{}/properties", server.id);

        let written = signed(
            listening.local_addr,
            "PUT",
            &path,
            Some(r#"[{"key":"motd","value":"Geode host"},{"key":"max-players","value":"12"}]"#),
        );
        assert!(written.starts_with("HTTP/1.1 200 "), "{written}");

        let read = signed(listening.local_addr, "GET", &path, None);
        let read: Vec<crate::server::properties::Property> =
            serde_json::from_str(body_of(&read)).unwrap();
        let motd = read.iter().find(|p| p.key == "motd").expect("motd was set");
        assert_eq!(motd.value, "Geode host");
        // Written through `properties::write`, so it is on disk and not only
        // in an answer.
        let on_disk = crate::server::properties::read(&scratch.paths, &server.id)
            .await
            .unwrap();
        assert!(on_disk.iter().any(|p| p.key == "max-players" && p.value == "12"));

        listening.stop().await;
    }

    /// A refused operation says why, in the words `Error` already has for it:
    /// the holder of the token administers this machine, and "400" on its own
    /// would send them to the container logs to find out what went wrong.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn starting_a_server_with_no_software_says_so() {
        let scratch = Scratch::new("daemon-start");
        let server = with_a_server(&scratch).await;
        let listening = started(host(&scratch)).await;

        let answer = signed(
            listening.local_addr,
            "POST",
            &format!("/v1/servers/{}/start", server.id),
            None,
        );
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
        assert!(
            body_of(&answer).contains("install one before starting it"),
            "it should say what to do: {}",
            body_of(&answer)
        );
        assert!(
            !crate::server::run::ServerRegistry::default().is_running(&server.id),
            "nothing should have started"
        );
        listening.stop().await;
    }

    /// Scheduled work is the reason a host is worth having, and a task added
    /// through the API must behave like one added in the app: stamped as if it
    /// had just run, so saving the list does not immediately fire everything
    /// in it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_task_added_through_the_api_does_not_fire_the_moment_it_is_saved() {
        let scratch = Scratch::new("daemon-tasks");
        let server = with_a_server(&scratch).await;
        let listening = started(host(&scratch)).await;
        let path = format!("/v1/servers/{}/tasks", server.id);

        let saved = signed(
            listening.local_addr,
            "PUT",
            &path,
            Some(
                r#"[{"id":"nightly","action":{"kind":"backup"},"everyMinutes":1440,"enabled":true}]"#,
            ),
        );
        assert!(saved.starts_with("HTTP/1.1 200 "), "{saved}");

        let stored = crate::server::load(&scratch.paths, &server.id).await.unwrap();
        let task = stored.schedules.first().expect("the task was saved");
        assert_eq!(task.id, "nightly");
        assert!(task.last_run.is_some(), "an unstamped task runs at once");
        assert!(
            !crate::server::schedule::is_due(task, chrono::Utc::now().timestamp()),
            "a task just added is not already due"
        );

        let read = signed(listening.local_addr, "GET", &path, None);
        assert!(read.contains("nightly"), "{read}");
        listening.stop().await;
    }

    /// The console route is a WebSocket. A plain `GET` of it says so rather
    /// than reading as a route that is not there, which is the one case where
    /// being specific helps: the token has already matched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_console_route_says_it_is_a_websocket() {
        let scratch = Scratch::new("daemon-console");
        let server = with_a_server(&scratch).await;
        let listening = started(host(&scratch)).await;

        let answer = signed(
            listening.local_addr,
            "GET",
            &format!("/v1/servers/{}/console", server.id),
            None,
        );
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
        assert!(body_of(&answer).contains("WebSocket"), "{answer}");
        listening.stop().await;
    }

    /// And it upgrades when asked properly — the handshake, over a real
    /// socket, so what is asserted is what a client would receive.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_console_request_with_the_token_is_handed_the_socket() {
        let scratch = Scratch::new("daemon-upgrade");
        let server = with_a_server(&scratch).await;
        let listening = started(host(&scratch)).await;

        let answer = super::super::serve::tests::ask(
            listening.local_addr,
            &format!(
                "GET /v1/servers/{}/console HTTP/1.1\r\nHost: localhost\r\n\
                 Authorization: Bearer {TOKEN}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
                server.id
            ),
        );
        assert!(answer.starts_with("HTTP/1.1 101 "), "{answer}");
        assert!(
            answer.contains("sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            "{answer}"
        );
        listening.stop().await;
    }
}

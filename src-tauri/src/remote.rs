//! The `geoded` hosts this copy of Geode can reach, and how it talks to them.
//!
//! Not to be confused with [`crate::share::host`], which is the listener
//! *players* reach to download a server's pack. This is the other direction and
//! the other audience: the admin's own machine in a cupboard or on a rented
//! box, running [`crate::daemon`], reached over its management port with the
//! token that machine was given. One token per host, held by whoever
//! administers it, which is the whole of its access control — `daemon/token.rs`
//! says why that is the shape.
//!
//! # The address, and what crosses it
//!
//! `geoded` serves plain HTTP and expects a loopback publish, a reverse proxy,
//! a tunnel or a VPN in front of it (`daemon/serve.rs` says why that is a
//! different answer from the pack listener's). So an address typed here without
//! a scheme is assumed to be `http://`, and the UI says plainly what that
//! means: over plain HTTP the token crosses the network in the clear, and the
//! thing in front of the host is the admin's to choose. An `https://` address
//! is taken at its word and verified the ordinary way, against the system's
//! roots.
//!
//! # The token
//!
//! Kept in `hosts.json` beside `accounts.json`, written
//! through the same owner-only helper, and never sent across the IPC boundary:
//! [`HostView`] has no token field at all, which is what every command here
//! returns. Whoever holds a host's token can do everything on that host,
//! which is `daemon/token.rs`'s decision rather than this module's, and the
//! panel says so rather than implying otherwise.

use futures::TryStreamExt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::net::Progress;
use crate::paths::Paths;
use crate::server::transfer::{self, Arrived, Measure, Mode};

/// How long a management request may take to reach a host, and how long it may
/// go quiet mid-answer before Geode gives up on it.
///
/// There is deliberately no deadline on a request as a whole: a 10 GB world up
/// a domestic uplink takes as long as it takes, and the person watching can
/// cancel it at any point. What a stalled socket is caught by is the read
/// timeout, which is about a connection that has stopped saying anything rather
/// than about one that is merely slow.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// A host, as `hosts.json` holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub id: String,
    /// What the admin calls it. Nothing is derived from it.
    pub name: String,
    /// Normalised by [`base`] when it was added, so everything here can join
    /// a path onto it without thinking about it again.
    pub address: String,
    pub token: String,
}

/// A host as the frontend sees it: everything but the token.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostView {
    pub id: String,
    pub name: String,
    pub address: String,
    /// Whether the token travels in the clear to reach it, so the UI can say
    /// so where the admin is deciding rather than in documentation.
    pub plain_http: bool,
}

impl Host {
    fn view(&self) -> HostView {
        HostView {
            id: self.id.clone(),
            name: self.name.clone(),
            address: self.address.clone(),
            plain_http: self.address.starts_with("http://"),
        }
    }
}

/// What `hosts.json` holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hosts {
    pub hosts: Vec<Host>,
}

impl Hosts {
    pub async fn load(paths: &Paths) -> Result<Self> {
        match tokio::fs::read(paths.hosts_file()).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| Error::json("hosts.json", e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    async fn save(&self, paths: &Paths) -> Result<()> {
        tokio::fs::create_dir_all(&paths.root).await?;
        crate::auth::store::write_private(
            &paths.hosts_file(),
            &serde_json::to_vec_pretty(self)?,
        )
        .await
    }

    pub fn views(&self) -> Vec<HostView> {
        self.hosts.iter().map(Host::view).collect()
    }

    pub fn get(&self, id: &str) -> Result<&Host> {
        self.hosts
            .iter()
            .find(|host| host.id == id)
            .ok_or_else(|| Error::msg("that host is no longer in your list — add it again"))
    }
}

/// Remember a host, with the token it answers to.
pub async fn add(paths: &Paths, name: String, address: String, token: String) -> Result<Vec<HostView>> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::msg("give the host a name, so you can tell it apart later"));
    }
    let address = base(&address)?;
    let token = token.trim().to_string();
    // The same floor the host itself holds its token to. Checked here as well
    // so a typo is caught where it can be corrected, rather than as a uniform
    // 401 from a host that will not say what was wrong.
    if token.len() < crate::daemon::token::MIN_LENGTH {
        return Err(Error::msg(format!(
            "that token is too short — a host's token is at least {} characters, the one \
             its compose file was given",
            crate::daemon::token::MIN_LENGTH
        )));
    }

    let mut hosts = Hosts::load(paths).await?;
    if let Some(existing) = hosts.hosts.iter_mut().find(|host| host.address == address) {
        // The same machine, said again. Replacing it is what somebody pasting
        // a rotated token means, and a second row for one host would leave
        // them guessing which of the two to pick.
        existing.name = name;
        existing.token = token;
    } else {
        hosts.hosts.push(Host {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            address,
            token,
        });
    }
    hosts.save(paths).await?;
    Ok(hosts.views())
}

/// Forget a host. The servers on it are untouched; this is only this machine's
/// note of how to reach it.
pub async fn forget(paths: &Paths, id: &str) -> Result<Vec<HostView>> {
    let mut hosts = Hosts::load(paths).await?;
    hosts.hosts.retain(|host| host.id != id);
    hosts.save(paths).await?;
    Ok(hosts.views())
}

/// What the admin typed, as something a path can be joined onto.
///
/// A bare `box.example:9090` is `http://box.example:9090`, because that is what
/// `geoded` serves and pretending otherwise would fail with a TLS error instead
/// of a readable one. Anything with a path, a query or credentials in it is
/// refused rather than quietly trimmed: a host is an origin, and an address
/// that looked like it pointed somewhere specific but did not would be a
/// confusing thing to have accepted.
pub fn base(address: &str) -> Result<String> {
    let typed = address.trim().trim_end_matches('/');
    if typed.is_empty() {
        return Err(Error::msg("give the host's address, like box.example:9090"));
    }
    let with_scheme = if typed.contains("://") {
        typed.to_string()
    } else {
        format!("http://{typed}")
    };
    let url = url::Url::parse(&with_scheme)
        .map_err(|_| unreachable_address(address))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::msg(format!(
            "'{address}' is not an address Geode can reach — a host is http:// or https://"
        )));
    }
    let Some(host) = url.host_str() else {
        return Err(unreachable_address(address));
    };
    if !url.path().is_empty() && url.path() != "/" {
        return Err(Error::msg(format!(
            "give the host's address only, without a path — '{}' rather than '{address}'",
            format_args!("{}://{host}", url.scheme())
        )));
    }
    if url.query().is_some() || !url.username().is_empty() {
        return Err(unreachable_address(address));
    }
    match url.port() {
        Some(port) => Ok(format!("{}://{host}:{port}", url.scheme())),
        None => Ok(format!("{}://{host}", url.scheme())),
    }
}

fn unreachable_address(address: &str) -> Error {
    Error::msg(format!(
        "'{address}' is not an address Geode can reach — it looks like box.example:9090"
    ))
}

/// Built once and shared: a client holds its own connection pool.
fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(crate::net::USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .expect("http client")
    })
}

/// A server on a host, as the app lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteServer {
    pub id: String,
    pub name: String,
    pub kind_name: String,
    pub mc_version: String,
    pub running: bool,
    pub installed: bool,
}

/// What a host's list route answers with. Only the fields the app shows; serde
/// ignores the rest, so a host a version ahead still lists.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    id: String,
    name: String,
    kind: crate::server::ServerKind,
    mc_version: String,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    installed: bool,
}

/// Every server on a host.
pub async fn servers(host: &Host) -> Result<Vec<RemoteServer>> {
    let listed: Vec<Listed> = asked(host, client().get(route(host, "")))
        .await?
        .json()
        .await
        .map_err(|e| Error::msg(format!("{} did not answer with a list of servers: {e}", host.name)))?;
    Ok(listed
        .into_iter()
        .map(|server| RemoteServer {
            id: server.id,
            name: server.name,
            kind_name: server.kind.display_name().to_string(),
            mc_version: server.mc_version,
            running: server.running,
            installed: server.installed,
        })
        .collect())
}

/// How big bringing one of a host's servers here would be.
pub async fn measure(host: &Host, id: &str, mode: Mode) -> Result<Measure> {
    let path = format!("{id}/transfer/size{}", query(mode));
    asked(host, client().get(route(host, &path)))
        .await?
        .json()
        .await
        .map_err(|e| Error::msg(format!("{} did not say how big {id} is: {e}", host.name)))
}

/// Send one of this machine's servers to a host.
///
/// The bundle is streamed as it is built — nothing is staged here and nothing
/// is held in memory. The host verifies every file before it answers, so a 201
/// means the server is in place there, which is the only thing that makes a
/// move's delete safe to do afterwards.
pub async fn push(
    paths: &Paths,
    host: &Host,
    id: &str,
    mode: Mode,
    progress: &Progress,
) -> Result<Arrived> {
    let measured = transfer::measure(paths, id, mode).await?;
    if measured.bytes > transfer::MAX_BUNDLE_BYTES {
        return Err(Error::msg(
            "that server is larger than a transfer may be — move its backups off it first",
        ));
    }
    progress.stage("Sending the server", measured.bytes).await;

    // What the bundle refused with, if it refuses.
    //
    // A push can fail from the inside: the bundle is the request body, and
    // `transfer::write_bundle` turning something down — a file whose length
    // moved while it was being read, a file it cannot open — reaches the
    // socket as an error in that stream. `reqwest` then fails the call in
    // words of its own about sending a request, keeping ours as the innermost
    // of four causes, and `unreachable` below prints only the outermost. So
    // the sentence is taken here, where it is still the only thing in hand,
    // and reported in place of the plumbing: it is the one part that tells the
    // person what still has the world open.
    let refused = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let bundle = {
        let refused = refused.clone();
        transfer::bundle(paths, id, mode, progress.clone()).inspect_err(move |e| {
            if let Ok(mut refused) = refused.lock() {
                *refused = Some(e.to_string());
            }
        })
    };
    let sent = client()
        .post(route(host, &format!("{id}/transfer")))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", host.token))
        .header(reqwest::header::CONTENT_TYPE, "application/x-tar")
        .header(transfer::DECLARED_BYTES, measured.bytes.to_string())
        .body(reqwest::Body::wrap_stream(bundle))
        .send();

    let sent = crate::share::join::unless_cancelled(progress, sent).await?;
    let answer = match refusal_or(host, sent).await {
        Ok(answer) => answer,
        Err(e) => {
            let refused = refused.lock().ok().and_then(|reason| reason.clone());
            return Err(refused.map_or(e, Error::msg));
        }
    };
    answer
        .json()
        .await
        .map_err(|e| Error::msg(format!("{} did not say what arrived: {e}", host.name)))
}

/// Bring one of a host's servers here.
///
/// Verified on this side by [`transfer::receive`] before anything is put in
/// place, so a failure leaves nothing here and the host's copy untouched.
pub async fn pull(
    paths: &Paths,
    host: &Host,
    id: &str,
    mode: Mode,
    progress: &Progress,
) -> Result<Arrived> {
    progress.stage("Asking the host for the server", 0).await;
    let path = format!("{id}/transfer{}", query(mode));
    let answer = asked(host, client().get(route(host, &path))).await?;

    // What the host said it was about to send, which is what the progress bar
    // is measured against. Counted as it lands all the same.
    let declared = answer
        .headers()
        .get(transfer::DECLARED_BYTES)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);

    let body = answer.bytes_stream();
    transfer::receive(paths, id, Box::pin(body), declared, progress).await
}

/// Delete a server on a host — the second half of a move, sent only once what
/// arrived here has been verified.
pub async fn delete_server(host: &Host, id: &str) -> Result<()> {
    asked(host, client().delete(route(host, id))).await?;
    Ok(())
}

/// `{address}/v1/servers/{rest}`.
fn route(host: &Host, rest: &str) -> String {
    format!("{}/v1/servers/{}", host.address, rest.trim_start_matches('/'))
}

fn query(mode: Mode) -> &'static str {
    match mode {
        Mode::Copy => "",
        Mode::Move => "?mode=move",
    }
}

/// Send a request with the host's token, and turn anything but success into
/// something a person can read.
async fn asked(host: &Host, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    let sent = request
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", host.token))
        .send()
        .await;
    refusal_or(host, sent).await
}

/// One place where a host's answer becomes this app's error.
///
/// A host's own refusals are written for the person holding its token, so they
/// are passed through rather than replaced — but said to be the host's, because
/// "stop the server before transferring it" means a different machine here than
/// it does there.
async fn refusal_or(
    host: &Host,
    sent: std::result::Result<reqwest::Response, reqwest::Error>,
) -> Result<reqwest::Response> {
    let answer = sent.map_err(|e| unreachable(host, e))?;
    if answer.status().is_success() {
        return Ok(answer);
    }
    let status = answer.status();
    // A host says nothing at all about why a token was refused, by design, so
    // this is the one case where the status has to be translated rather than
    // relayed.
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::msg(format!(
            "{} did not accept the token — paste the current one in Settings → Server hosts",
            host.name
        )));
    }
    #[derive(Deserialize)]
    struct Refusal {
        error: String,
    }
    let said = answer
        .json::<Refusal>()
        .await
        .map(|refusal| refusal.error)
        .unwrap_or_else(|_| format!("it answered {status}"));
    Err(Error::msg(format!("{}: {said}", host.name)))
}

fn unreachable(host: &Host, e: reqwest::Error) -> Error {
    if e.is_connect() || e.is_timeout() {
        return Error::msg(format!(
            "could not reach {} at {} — check it is running, and that the port is open to this machine",
            host.name, host.address
        ));
    }
    Error::msg(format!("could not talk to {}: {e}", host.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    #[test]
    fn an_address_is_an_origin_and_nothing_else() {
        // What somebody actually types.
        assert_eq!(base("box.example:9090").unwrap(), "http://box.example:9090");
        assert_eq!(base(" box.example:9090/ ").unwrap(), "http://box.example:9090");
        assert_eq!(base("http://10.0.0.4:9090").unwrap(), "http://10.0.0.4:9090");
        // A proxy in front of it, which is the posture `daemon/serve.rs` asks
        // for, and the scheme is then taken at its word.
        assert_eq!(base("https://geode.example").unwrap(), "https://geode.example");

        for refused in [
            "",
            "   ",
            // A path would look like it pointed somewhere specific.
            "box.example:9090/v1/servers",
            "ftp://box.example",
            "box.example:9090?token=hunter2",
        ] {
            assert!(base(refused).is_err(), "{refused:?} should not be accepted");
        }
    }

    #[tokio::test]
    async fn a_host_is_remembered_without_its_token_crossing_the_boundary() {
        let scratch = Scratch::new("remote-hosts");
        let paths = &scratch.paths;
        let token = "f3a9c1d7e5b204860000";

        let views = add(paths, "Cupboard".into(), "box.example:9090".into(), token.into())
            .await
            .unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].address, "http://box.example:9090");
        assert!(views[0].plain_http, "the UI has to be able to say so");

        // What the frontend is handed cannot be made to say the token, whatever
        // it does with it.
        let json = serde_json::to_string(&views).unwrap();
        assert!(!json.contains(token), "the token reached the frontend: {json}");

        // And it is on disk for the next launch.
        let hosts = Hosts::load(paths).await.unwrap();
        assert_eq!(hosts.get(&views[0].id).unwrap().token, token);

        let empty = forget(paths, &views[0].id).await.unwrap();
        assert!(empty.is_empty());
        assert!(Hosts::load(paths).await.unwrap().hosts.is_empty());
    }

    #[tokio::test]
    async fn the_same_host_said_twice_is_one_host_with_the_newer_token() {
        let scratch = Scratch::new("remote-again");
        let paths = &scratch.paths;
        add(paths, "Cupboard".into(), "box.example:9090".into(), "f3a9c1d7e5b204860000".into())
            .await
            .unwrap();
        let views = add(
            paths,
            "The cupboard".into(),
            "http://box.example:9090/".into(),
            "0000f3a9c1d7e5b20486".into(),
        )
        .await
        .unwrap();

        assert_eq!(views.len(), 1, "one machine, one row");
        assert_eq!(views[0].name, "The cupboard");
        let hosts = Hosts::load(paths).await.unwrap();
        assert_eq!(hosts.hosts[0].token, "0000f3a9c1d7e5b20486");
    }

    /// A push can fail from the inside: the bundle is the request body, and
    /// `transfer::write_bundle` turning something down fails the request with
    /// the sentence written for the person four causes deep inside
    /// `reqwest`'s own words about sending a request. That sentence is the one
    /// thing telling them what still has the world open, so it is what the
    /// dialog has to show.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refusal_from_the_bundle_reaches_the_person_rather_than_the_plumbing() {
        use std::os::unix::fs::PermissionsExt;

        let here = Scratch::new("remote-push-refused");
        let mut server = crate::server::Server::new(
            "Survival".into(),
            crate::server::ServerKind::Paper,
            "1.21.1".into(),
        );
        server.id = "survival".into();
        tokio::fs::create_dir_all(here.paths.server_run("survival")).await.unwrap();
        let world = here.paths.server_run("survival").join("level.dat");
        tokio::fs::write(&world, b"a world").await.unwrap();
        server.save(&here.paths).await.unwrap();

        // A file the plan can measure and the bundle cannot read, which is
        // what makes this refusal happen with the request already on the wire.
        std::fs::set_permissions(&world, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&world).is_ok() {
            // Running as root, which ignores the mode — and then there is
            // nothing here to refuse.
            return;
        }

        // Something listening, so this is a request that failed while it was
        // being sent rather than one that never connected.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut read = Vec::new();
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut socket, &mut read).await;
            }
        });
        let host = Host {
            id: "cupboard".into(),
            name: "the cupboard".into(),
            address: format!("http://{addr}"),
            token: "f3a9c1d7e5b204860000".into(),
        };

        let reported = push(&here.paths, &host, "survival", Mode::Copy, &Progress::silent("t"))
            .await
            .expect_err("the bundle could not read the world");
        assert!(
            reported.to_string().contains("level.dat"),
            "the reason the transfer stopped never reached the person: {reported}"
        );
        assert!(
            reported.to_string().contains("could not read"),
            "it should say what went wrong with it: {reported}"
        );
    }

    #[tokio::test]
    async fn a_token_too_short_to_be_a_token_is_refused_here_rather_than_there() {
        let scratch = Scratch::new("remote-token");
        let refused = add(
            &scratch.paths,
            "Cupboard".into(),
            "box.example:9090".into(),
            "changeme".into(),
        )
        .await
        .expect_err("a host would answer 401 and say nothing about why");
        assert!(refused.to_string().contains("too short"), "{refused}");
    }
}

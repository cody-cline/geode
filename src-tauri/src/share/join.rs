//! Joining a shared server: the player's side.
//!
//! Resolve how to trust the address, ask the server what it is sharing, show
//! the player, and on their say-so download the pack and import it through the
//! same sequence as any other `.geodepack` — so a joined server gets the
//! origin tagging, reference resolution and every other import rule for free.

use std::path::Path;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};
use crate::meta::Loader;
use crate::net::Progress;

use super::host::{PackInfo, INFO_PATH, PACK_PATH};
use super::link::{Address, Link};
use super::trust::{Trust, TrustedClient};

/// The most a pack download may be. Packs name their mods rather than carry
/// them, so a real one is megabytes. This bounds the download alone; what the
/// archive expands to on import is bounded separately, where it is extracted.
pub(super) const MAX_PACK_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The most any other reply may be: a pack description, or a refusal saying
/// why. Both are a few hundred bytes of JSON. These are held in memory to be
/// parsed, so one that runs past this is refused as it arrives — a server does
/// not get to make a player's Geode hold gigabytes to find out it is junk.
const MAX_REPLY_BYTES: usize = 64 * 1024;

/// The whole of a preview's request. Describing a pack builds nothing, so a
/// server that takes longer than this to say what it shares has stopped, and
/// the player is still looking at the dialog waiting to hear.
const PREVIEW_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How often a wait on the server looks to see whether the player cancelled.
/// Cancelling only raises a flag, and a server that has gone quiet sends
/// nothing that would wake the wait to notice it.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// How the connection to the server was verified, for the confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Verified {
    /// Matched the fingerprint in the link, or the one remembered from joining
    /// the same typed address before.
    Pinned,
    /// Checked by the domain's certificate authority.
    Domain,
    /// Typed by hand: trusted now, pinned from here on.
    FirstUse,
}

/// What a player is shown before importing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinPreview {
    pub info: PackInfo,
    pub address: String,
    pub verified: Verified,
    /// The link as it will be stored and reused: for a typed address, now
    /// carrying the fingerprint that was seen.
    pub link: Link,
    /// An instance already joined from this server, so the player is offered
    /// that rather than a second copy. [`preview`] only talks to the server
    /// and leaves this empty; see [`existing_instance`].
    pub existing: Option<ExistingInstance>,
}

/// An instance that was joined from the server being looked at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistingInstance {
    pub id: String,
    pub name: String,
    /// The instance was recognised by the server it was joined from rather
    /// than by its password, so this link carries a newer one: opening the
    /// instance saves it, and its updates keep coming.
    pub password_changed: bool,
    /// This link's fingerprint is not the one the instance holds. A machine
    /// keeps its certificate, so that is an admin who reinstalled Geode — and
    /// it is also what someone else answering at the same address looks like.
    /// It is worth saying wherever opening the link saves it, because from
    /// then on this instance takes its updates from whoever that is.
    pub certificate_changed: bool,
}

/// What a player gave to join: a link, or an address and a code typed apart.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum JoinInput {
    Link { link: String },
    Parts { address: String, code: String },
}

impl JoinInput {
    pub fn into_link(self, default_port: u16) -> Result<Link> {
        match self {
            JoinInput::Link { link } => Link::parse(link.trim()),
            JoinInput::Parts { address, code } => {
                Link::from_parts(address.trim(), &code, default_port)
            }
        }
    }
}

/// Where an archive being joined is downloaded to, and cleared at startup.
pub fn incoming_dir(paths: &crate::paths::Paths) -> std::path::PathBuf {
    paths.share_dir().join("incoming")
}

/// Remove anything a join left behind when Geode stopped mid-download. Only
/// one Geode runs at a time, so nothing in here can still be in use at launch.
pub async fn clear_incoming(paths: &crate::paths::Paths) {
    // Nothing to report if it fails: every join downloads under a name of its
    // own, and the next start tries again.
    let _ = tokio::fs::remove_dir_all(incoming_dir(paths)).await;
}

/// The fingerprint remembered for a typed address, from an instance already
/// joined through it — which is what makes "trusted from now on" true.
///
/// Remembered per address, whatever the code: one machine serves every server
/// it shares with one certificate. An `https://` address has none, because its
/// certificate authority vouches for it, and the certificate in front of a
/// domain or tunnel is renewed as a matter of course. Nor is there one if the
/// instances cannot be read, and the address is a first use again.
pub async fn remembered_pin(paths: &crate::paths::Paths, address: &Address) -> Option<super::link::Pin> {
    if matches!(address, Address::Https { .. }) {
        return None;
    }
    crate::instance::list(paths)
        .await
        .ok()?
        .into_iter()
        .filter_map(|instance| {
            let joined = instance.joined?;
            (joined.address == *address).then_some((instance.created_at, joined.pin?))
        })
        // A machine keeps its certificate for life, so instances only disagree
        // when it was replaced. A typed address is held to what was remembered,
        // so only a link from the admin brings in the new one — and the newest
        // instance is the join that knows about the replacement.
        .max_by_key(|(created_at, _)| *created_at)
        .map(|(_, pin)| pin)
}

/// An instance already joined from this same server, if there is one.
///
/// The same address and the same code, and not the same fingerprint: a
/// machine whose certificate was replaced is still sharing the same pack. When
/// several instances were joined from it, the one played most recently — the
/// order instances are listed in, and the one the player most likely means.
///
/// A link with a replaced password matches none of them by code, so the
/// server's own id is tried next: the admin handing out a new link is the one
/// way a player gets one, and joining again would leave them a second instance
/// and the first one never updated again. An instance joined before servers
/// sent their id has none recorded until its first update, and is only found
/// by its password until then.
pub async fn existing_instance(
    paths: &crate::paths::Paths,
    link: &Link,
    server_id: &str,
) -> Option<ExistingInstance> {
    let instances = crate::instance::list(paths).await.ok()?;
    let at_address = |instance: &&crate::instance::Instance| {
        instance
            .joined
            .as_ref()
            .is_some_and(|joined| joined.address == link.address)
    };
    let by_password = instances.iter().filter(at_address).find(|instance| {
        instance
            .joined
            .as_ref()
            .is_some_and(|joined| joined.code.matches(link.code.raw()))
    });
    let (instance, password_changed) = match by_password {
        Some(instance) => (instance, false),
        None if server_id.is_empty() => return None,
        None => (
            instances
                .iter()
                .filter(at_address)
                .find(|instance| instance.joined_server_id.as_deref() == Some(server_id))?,
            true,
        ),
    };
    Some(ExistingInstance {
        id: instance.id.clone(),
        name: instance.name.clone(),
        password_changed,
        // Only a link that names a fingerprint disagrees with the one the
        // instance holds. A typed address carries none until the preview has
        // seen one, and an `https://` one never does.
        certificate_changed: link.pin.is_some_and(|offered| {
            instance
                .joined
                .as_ref()
                .and_then(|joined| joined.pin)
                .is_some_and(|held| held != offered)
        }),
    })
}

/// Refuse a pack that is not what the player approved: a different Minecraft
/// version, loader or loader version from what the preview showed.
///
/// The description and the pack are two requests, and an admin can change the
/// server between them — or a hostile one can describe one thing and send
/// another. The name and the mod count are left alone: a renamed server is
/// still the one approved, and a pack counts its files differently from the
/// way a description counts mods.
pub fn check_matches(approved: &PackInfo, described: &crate::instance::modpack::PackPreview) -> Result<()> {
    let same = approved.mc_version == described.mc_version
        && Loader::parse(&approved.loader) == Some(described.loader)
        && approved.loader_version == described.loader_version;
    if same {
        return Ok(());
    }
    Err(Error::msg(format!(
        "the server's pack changed since you looked (it was {}, and is now {}) — look it up again",
        setup(
            &approved.mc_version,
            &approved.loader,
            &approved.loader_version
        ),
        setup(
            &described.mc_version,
            described.loader.as_str(),
            &described.loader_version
        ),
    )))
}

/// `Minecraft 1.21.1`, or `Minecraft 1.21.1 with Fabric 0.16.9`.
fn setup(mc_version: &str, loader: &str, loader_version: &str) -> String {
    match Loader::parse(loader) {
        Some(Loader::Vanilla) if loader_version.is_empty() => format!("Minecraft {mc_version}"),
        parsed => {
            let name = parsed.map_or(loader, |known| known.display_name());
            format!("Minecraft {mc_version} with {name} {loader_version}")
                .trim_end()
                .to_string()
        }
    }
}

/// How a link says its server should be trusted.
fn trust_for(link: &Link) -> Trust {
    match (&link.address, link.pin) {
        (Address::Https { .. }, _) => Trust::Domain,
        (Address::Direct { .. }, Some(pin)) => Trust::Pinned(pin),
        (Address::Direct { .. }, None) => Trust::FirstUse,
    }
}

/// Ask the server what it is sharing, and settle how far it can be trusted.
///
/// For a typed address this is the first use, and the fingerprint seen here is
/// written into the returned link — so the download that follows is pinned to
/// the same machine that answered now, not to whoever answers next.
pub async fn preview(mut link: Link) -> Result<JoinPreview> {
    let trust = trust_for(&link);
    let client = TrustedClient::new(trust)?;
    // Nothing cancels a preview; its deadline is what ends one.
    let uncancelled = Progress::silent("preview");

    let response = request(
        &client,
        &link,
        INFO_PATH,
        Some(PREVIEW_DEADLINE),
        &uncancelled,
    )
    .await?;
    let info: PackInfo = serde_json::from_slice(&read_reply(response, &uncancelled).await?)
        .map_err(|_| Error::msg("the server's answer was not a Geode pack description"))?;

    let verified = match trust {
        Trust::Pinned(_) => Verified::Pinned,
        Trust::Domain => Verified::Domain,
        Trust::FirstUse => {
            link.pin = Some(client.seen_pin().ok_or_else(|| {
                Error::msg("the server did not present a certificate to remember")
            })?);
            Verified::FirstUse
        }
    };

    Ok(JoinPreview {
        info,
        address: link.address.display(),
        verified,
        link,
        existing: None,
    })
}

/// Download a server's pack to `dest`.
///
/// Refuses a direct address without a fingerprint: by this point the preview
/// has filled one in, and a link that still lacks it has skipped the step that
/// decides who to trust.
pub async fn download(link: &Link, dest: &Path, progress: &Progress) -> Result<()> {
    if matches!(link.address, Address::Direct { .. }) && link.pin.is_none() {
        return Err(Error::msg(
            "this server has not been verified yet — open its link again to see what it shares",
        ));
    }
    let client = TrustedClient::new(trust_for(link))?;
    // Said before asking rather than once the server answers. The server
    // builds the whole pack before it sends a byte, which for a big one takes
    // a while, and until a job has said something there is nothing on screen
    // for the player to cancel.
    progress.stage("Waiting for the server", 0).await;
    // No overall deadline here: a big pack on a slow upload takes as long as it
    // takes. The client's per-read timeout still catches one that stalls, the
    // player can cancel at any point, and the admin's side cuts a download that
    // hands on less than a slow minimum pace — about two kilobytes a second,
    // measured over two minutes at a time — however long it has been running.
    let response = request(&client, link, PACK_PATH, None, progress).await?;

    if response.content_length().is_some_and(|len| len > MAX_PACK_BYTES) {
        return Err(Error::msg("this server's pack is too large to download"));
    }
    progress
        .stage("Downloading the server's pack", response.content_length().unwrap_or(0))
        .await;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::File::create(dest).await?;
    let mut received: u64 = 0;
    let mut body = response.bytes_stream();
    while let Some(chunk) = unless_cancelled(progress, body.next()).await? {
        let chunk = chunk.map_err(|e| Error::msg(format!("the download was interrupted: {e}")))?;
        received += chunk.len() as u64;
        // Counted as it arrives, not trusted from the header: a server can say
        // one length and send another.
        if received > MAX_PACK_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(dest).await;
            return Err(Error::msg("this server's pack is too large to download"));
        }
        file.write_all(&chunk).await?;
        progress.advance(chunk.len() as u64, "pack").await;
    }
    file.flush().await?;
    Ok(())
}

/// One authenticated request, with the failures a player can act on said
/// plainly rather than as transport errors.
async fn request(
    client: &TrustedClient,
    link: &Link,
    path: &str,
    deadline: Option<std::time::Duration>,
    progress: &Progress,
) -> Result<reqwest::Response> {
    let url = format!("{}{path}", link.address.base_url());
    let mut builder = client.client.get(&url).bearer_auth(link.code.raw());
    if let Some(deadline) = deadline {
        builder = builder.timeout(deadline);
    }
    let response = unless_cancelled(progress, builder.send())
        .await?
        .map_err(|e| connect_error(link, e))?;

    match response.status() {
        s if s.is_success() => Ok(response),
        reqwest::StatusCode::UNAUTHORIZED => Err(Error::PasswordRefused),
        reqwest::StatusCode::NOT_FOUND => Err(Error::msg(
            "something answered at that address, but it is not a Geode server sharing a pack",
        )),
        status => {
            // A Geode server explains itself in `{"error": "…"}`; say that.
            // Anything else it sent, however much, is only a reason to fall
            // back on the status.
            let said = match read_reply(response, progress).await {
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                body => body
                    .ok()
                    .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string)),
            };
            Err(Error::msg(said.unwrap_or_else(|| {
                format!("the server refused the request ({status})")
            })))
        }
    }
}

/// A reply that has to be held whole to be read, collected as it arrives and
/// refused the moment it passes [`MAX_REPLY_BYTES`] — not once the server has
/// finished sending however much it likes.
async fn read_reply(response: reqwest::Response, progress: &Progress) -> Result<Vec<u8>> {
    let too_long =
        || Error::msg("the server's answer was far longer than any a Geode server sends");
    if response
        .content_length()
        .is_some_and(|len| len > MAX_REPLY_BYTES as u64)
    {
        return Err(too_long());
    }
    let mut body = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = unless_cancelled(progress, body.next()).await? {
        let chunk =
            chunk.map_err(|e| Error::msg(format!("the server's answer was interrupted: {e}")))?;
        if bytes.len() + chunk.len() > MAX_REPLY_BYTES {
            return Err(too_long());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Wait for `work` unless the player cancels first.
///
/// Every wait on the server goes through this. Checking the flag only when a
/// chunk arrives would leave a cancel waiting on the server too — for as long
/// as the read timeout while it is quiet, and indefinitely against one that
/// sends a byte just often enough to stay inside it.
pub(super) async fn unless_cancelled<T>(
    progress: &Progress,
    work: impl std::future::Future<Output = T>,
) -> Result<T> {
    progress.check_cancelled()?;
    tokio::pin!(work);
    let mut poll = tokio::time::interval(CANCEL_POLL);
    loop {
        tokio::select! {
            done = &mut work => return Ok(done),
            _ = poll.tick() => progress.check_cancelled()?,
        }
    }
}

fn connect_error(link: &Link, error: reqwest::Error) -> Error {
    let address = link.address.display();
    // A certificate that does not match is the one failure worth being loud
    // about: it means something other than the admin's machine answered.
    let text = format!("{error:?}");
    if text.contains("certificate") || text.contains("Certificate") || text.contains("BadSignature") {
        return Error::msg(format!(
            "{address} did not prove it is the server this link was made for, so nothing was downloaded"
        ));
    }
    if error.is_timeout() || error.is_connect() {
        return Error::msg(format!(
            "could not reach {address} — the server may be offline, or its port may not be open to the internet"
        ));
    }
    Error::msg(format!("could not talk to {address}: {error}"))
}

#[cfg(test)]
mod tests {
    //! Against a sharing machine that says exactly what a test scripts, byte
    //! for byte, over real TLS with a real pinned certificate — so everything
    //! but what the server says is as it should be.

    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use super::super::code::Code;
    use super::super::identity::Identity;
    use super::super::link::Pin;
    use super::*;
    use crate::paths::Scratch;

    /// Plenty for anything a client does against loopback, and far short of
    /// what a client that waited on the server would sit out: half a minute
    /// for a preview, five minutes for a download.
    const PROMPTLY: Duration = Duration::from_secs(5);

    /// A machine that answers every request with `reply`, sent as written,
    /// and then holds the connection open without another byte.
    async fn answering(reply: Vec<u8>) -> Link {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["geode-share.invalid".to_string()]).unwrap();
        let identity = Identity {
            cert_der: cert.der().to_vec(),
            key_der: signing_key.serialize_der(),
        };
        let acceptor = TlsAcceptor::from(identity.server_config().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let (acceptor, reply) = (acceptor.clone(), reply.clone());
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    // The whole request first, so nothing is answered early.
                    let (mut seen, mut buf) = (Vec::new(), [0u8; 1024]);
                    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tls.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                    if tls.write_all(&reply).await.is_ok() && tls.flush().await.is_ok() {
                        std::future::pending::<()>().await;
                    }
                });
            }
        });
        Link {
            address: Address::Direct {
                host: "127.0.0.1".into(),
                port,
            },
            code: Code::generate(),
            pin: Some(identity.pin()),
        }
    }

    /// A complete reply.
    fn whole(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// The start of a reply sent in chunks: one chunk of `body` and no final
    /// empty one, so as far as the client can tell there is more to come.
    fn unending(status: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out.extend_from_slice(b"\r\n");
        out
    }

    async fn promptly<T>(what: &str, work: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(PROMPTLY, work)
            .await
            .unwrap_or_else(|_| panic!("{what} was still waiting on the server after {PROMPTLY:?}"))
    }

    /// A reply too long to be anything a Geode server sends is refused as
    /// soon as it is too long — not buffered until the server stops, which
    /// here it never does.
    #[tokio::test]
    async fn a_reply_too_long_for_a_geode_server_is_refused_as_it_arrives() {
        let junk = vec![b' '; MAX_REPLY_BYTES * 2];

        // A description that never ends.
        let link = answering(unending("200 OK", &junk)).await;
        let err = promptly("an endless description", preview(link))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("far longer"), "{err}");

        // One that says up front it is a gigabyte, and then goes quiet.
        let link = answering(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{{",
                1u64 << 30
            )
            .into_bytes(),
        )
        .await;
        let err = promptly("a gigabyte description", preview(link))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("far longer"), "{err}");

        // A refusal that never ends, on the download — which has no deadline
        // of its own to end it. What it said is dropped for the status.
        let link = answering(unending("500 Internal Server Error", &junk)).await;
        let scratch = Scratch::new("join-refusal");
        let err = promptly(
            "an endless refusal",
            download(
                &link,
                &scratch.paths.root.join("p.geodepack"),
                &Progress::silent("t"),
            ),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("refused the request (500"),
            "{err}"
        );
    }

    /// Waits until `recorder` has heard the job enter `stage`.
    async fn until_stage(recorder: &crate::events::Recorder, stage: &str) {
        promptly(&format!("the {stage:?} stage"), async {
            while !recorder
                .events()
                .iter()
                .any(|(_, event)| event["stage"] == stage)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
    }

    /// A player can cancel a join however quiet the server is: before it has
    /// answered at all, partway through the pack, or partway through saying
    /// why it refused. And the job is on screen to be cancelled from the start,
    /// not only once the server has answered.
    #[tokio::test]
    async fn a_join_can_be_cancelled_while_the_server_says_nothing() {
        let quiet = [
            // Builds its pack, or has gone away, and never answers.
            ("silent", Vec::new(), "Waiting for the server"),
            // Starts the pack, then stalls.
            (
                "stalled",
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nPK".to_vec(),
                "Downloading the server's pack",
            ),
            // Starts to say why it refused, then stalls.
            (
                "refusing",
                unending("500 Internal Server Error", b"{\"error\": \""),
                "Waiting for the server",
            ),
        ];
        for (what, reply, stage) in quiet {
            let link = answering(reply).await;
            let scratch = Scratch::new(&format!("join-{what}"));
            let recorder = std::sync::Arc::new(crate::events::Recorder::default());
            let progress = Progress::new(recorder.clone(), "join:t");
            let join = tokio::spawn({
                let progress = progress.clone();
                let dest = scratch.paths.root.join("p.geodepack");
                async move { download(&link, &dest, &progress).await }
            });

            until_stage(&recorder, stage).await;
            if what == "refusing" {
                // Nothing on screen marks the refusal arriving; give loopback
                // ample time to deliver its first bytes.
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            progress
                .cancel_flag()
                .store(true, std::sync::atomic::Ordering::Relaxed);

            let outcome = promptly(&format!("cancelling the {what} join"), join)
                .await
                .unwrap();
            assert!(
                matches!(outcome, Err(Error::Cancelled)),
                "{what}: {outcome:?}"
            );
        }
    }

    /// What a join left behind when Geode stopped partway is cleared, and
    /// nothing else sharing keeps beside it is touched.
    #[tokio::test]
    async fn clearing_incoming_removes_leftover_archives_and_only_those() {
        let scratch = Scratch::new("join-incoming");
        let paths = scratch.paths.clone();
        let incoming = incoming_dir(&paths);
        std::fs::create_dir_all(&incoming).unwrap();
        std::fs::write(incoming.join("0b1c.geodepack"), b"half a pack").unwrap();
        std::fs::write(paths.share_dir().join("identity.key"), b"the machine's key").unwrap();

        clear_incoming(&paths).await;

        assert!(!incoming.exists(), "the leftover archive is still there");
        assert!(
            paths.share_dir().join("identity.key").is_file(),
            "clearing reached past incoming"
        );

        // Nothing left behind is not an error either.
        clear_incoming(&paths).await;
    }

    /// An instance on disk, joined from `link` if there is one.
    async fn instance(
        paths: &crate::paths::Paths,
        id: &str,
        link: Option<Link>,
        created_at: i64,
        last_played: Option<i64>,
    ) {
        let mut instance = crate::instance::Instance::new(
            format!("Joined {id}"),
            "1.21.1".into(),
            crate::meta::Loader::Fabric,
            "0.16.9".into(),
        );
        instance.id = id.into();
        instance.created_at = created_at;
        instance.last_played = last_played;
        instance.joined = link;
        instance.save(paths).await.unwrap();
    }

    fn direct(host: &str, port: u16, code: &Code, pin: Option<Pin>) -> Link {
        Link {
            address: Address::Direct {
                host: host.into(),
                port,
            },
            code: code.clone(),
            pin,
        }
    }

    /// A typed address is held to the certificate its earlier join saw: the
    /// same address typed however it is typed, and nothing else — not another
    /// port on the same host, not an address nobody joined, and never an
    /// `https://` one.
    #[tokio::test]
    async fn a_typed_address_remembers_the_certificate_it_was_joined_with() {
        let scratch = Scratch::new("join-remembered");
        let paths = scratch.paths.clone();
        let code = Code::generate();
        let (first, replaced, other, tunnel) =
            (Pin([1; 32]), Pin([2; 32]), Pin([3; 32]), Pin([4; 32]));
        let https = Address::Https {
            url: "https://pack.example.com".into(),
        };

        instance(
            &paths,
            "first",
            Some(direct("203.0.113.7", 25590, &code, Some(first))),
            100,
            Some(900),
        )
        .await;
        // The admin replaced the machine's certificate and sent a new link.
        instance(
            &paths,
            "replaced",
            Some(direct("203.0.113.7", 25590, &code, Some(replaced))),
            200,
            None,
        )
        .await;
        instance(
            &paths,
            "elsewhere",
            Some(direct("203.0.113.8", 25590, &code, Some(other))),
            300,
            None,
        )
        .await;
        // A pin beside an `https://` address, as only a hand-edited file has.
        let edited = Link {
            address: https.clone(),
            code: code.clone(),
            pin: Some(tunnel),
        };
        instance(&paths, "tunnel", Some(edited), 400, None).await;
        instance(&paths, "made-by-hand", None, 500, None).await;

        let typed = JoinInput::Parts {
            address: " 203.0.113.7 ".into(),
            code: code.display(),
        }
        .into_link(25590)
        .unwrap()
        .address;
        assert_eq!(remembered_pin(&paths, &typed).await, Some(replaced));
        assert_eq!(
            remembered_pin(
                &paths,
                &Address::Direct {
                    host: "203.0.113.8".into(),
                    port: 25590
                }
            )
            .await,
            Some(other)
        );

        for unknown in [
            Address::Direct {
                host: "203.0.113.7".into(),
                port: 25591,
            },
            Address::Direct {
                host: "203.0.113.9".into(),
                port: 25590,
            },
            https,
        ] {
            assert_eq!(remembered_pin(&paths, &unknown).await, None, "{unknown:?}");
        }
        // With no instances at all there is nothing to remember.
        assert_eq!(
            remembered_pin(&Scratch::new("join-none").paths, &typed).await,
            None
        );
    }

    /// Opening the same server's link again finds the instance already joined
    /// from it — by address and code, however the code was typed, and whether
    /// or not the certificate has changed since — rather than making another.
    #[tokio::test]
    async fn the_instance_already_joined_from_a_server_is_found_again() {
        let scratch = Scratch::new("join-existing");
        let paths = scratch.paths.clone();
        let (code, other_code) = (Code::generate(), Code::generate());
        let at = |host: &str, code: &Code, pin| Some(direct(host, 25590, code, pin));

        instance(
            &paths,
            "survival",
            at("203.0.113.7", &code, Some(Pin([1; 32]))),
            100,
            Some(10),
        )
        .await;
        instance(
            &paths,
            "survival-2",
            at("203.0.113.7", &code, Some(Pin([2; 32]))),
            200,
            Some(50),
        )
        .await;
        instance(
            &paths,
            "creative",
            at("203.0.113.7", &other_code, Some(Pin([1; 32]))),
            300,
            Some(99),
        )
        .await;
        instance(
            &paths,
            "elsewhere",
            at("203.0.113.8", &code, Some(Pin([3; 32]))),
            400,
            Some(99),
        )
        .await;
        instance(&paths, "made-by-hand", None, 500, Some(1000)).await;

        // Typed in lower case, with no fingerprint yet: the same server.
        let typed = JoinInput::Parts {
            address: "203.0.113.7".into(),
            code: code.display().to_lowercase(),
        }
        .into_link(25590)
        .unwrap();
        let found = existing_instance(&paths, &typed, "").await;
        assert_eq!(
            found,
            Some(ExistingInstance {
                id: "survival-2".into(),
                name: "Joined survival-2".into(),
                password_changed: false,
                certificate_changed: false,
            }),
            "the one played last"
        );

        let found =
            existing_instance(&paths, &direct("203.0.113.7", 25590, &other_code, None), "").await;
        assert_eq!(found.map(|e| e.id).as_deref(), Some("creative"));

        for (what, link) in [
            (
                "another code",
                direct("203.0.113.7", 25590, &Code::generate(), None),
            ),
            ("another address", direct("203.0.113.9", 25590, &code, None)),
            ("another port", direct("203.0.113.7", 25591, &code, None)),
        ] {
            assert_eq!(existing_instance(&paths, &link, "").await, None, "{what}");
        }
    }

    /// An admin who replaces a server's password hands out a new link, and
    /// nothing in it matches what the instance holds. The server it names is
    /// what finds that instance again, so the player saves the new password
    /// instead of joining into a second one.
    #[tokio::test]
    async fn an_instance_is_found_again_under_a_new_password() {
        let scratch = Scratch::new("join-relink");
        let paths = scratch.paths.clone();
        let (code1, code2) = (Code::generate(), Code::generate());
        let (machine, stranger) = (Pin([1; 32]), Pin([2; 32]));
        instance(
            &paths,
            "a",
            Some(direct("203.0.113.7", 25590, &code1, Some(machine))),
            100,
            Some(10),
        )
        .await;
        let mut joined = crate::instance::load(&paths, "a").await.unwrap();
        joined.joined_server_id = Some("s1".into());
        joined.save(&paths).await.unwrap();

        let found = |link: Link, server_id: &'static str| {
            let paths = paths.clone();
            async move {
                existing_instance(&paths, &link, server_id)
                    .await
                    .map(|e| (e.id, e.password_changed, e.certificate_changed))
            }
        };

        // The password it was joined with: found, with nothing to save.
        assert_eq!(
            found(direct("203.0.113.7", 25590, &code1, Some(machine)), "s1").await,
            Some(("a".into(), false, false))
        );
        // A password it has never seen, from the server it was joined from.
        assert_eq!(
            found(direct("203.0.113.7", 25590, &code2, Some(machine)), "s1").await,
            Some(("a".into(), true, false))
        );
        // The same, from a machine holding a certificate this instance has
        // never seen. Still the instance to offer — an admin who reinstalled
        // Geode has a new one — but the dialog says so, because opening it
        // hands this instance's updates to whoever that is.
        assert_eq!(
            found(direct("203.0.113.7", 25590, &code2, Some(stranger)), "s1").await,
            Some(("a".into(), true, true))
        );
        // Another server on the same machine is another server, and so is the
        // same server's id at an address this instance never joined.
        assert_eq!(
            found(direct("203.0.113.7", 25590, &code2, Some(machine)), "s2").await,
            None
        );
        assert_eq!(
            found(direct("203.0.113.9", 25590, &code2, Some(machine)), "s1").await,
            None
        );
    }

    fn described(mc_version: &str, loader: Loader, loader_version: &str) -> PackInfo {
        PackInfo {
            name: "Survival".into(),
            mc_version: mc_version.into(),
            loader: loader.as_str().into(),
            loader_version: loader_version.into(),
            mods: 1,
            server_mods: 0,
            server_kind: "fabric".into(),
            revision: String::new(),
            server_id: String::new(),
        }
    }

    /// An admin on an older Geode sends a description without a revision, a
    /// server id or a count of server-side mods. It still reads, as a server
    /// that says nothing about any of them.
    #[test]
    fn a_pack_info_from_an_older_geode_still_reads() {
        let info: PackInfo = serde_json::from_value(serde_json::json!({
            "name": "Survival", "mcVersion": "1.21.1", "loader": "fabric",
            "loaderVersion": "0.16.9", "mods": 3, "serverKind": "fabric",
        }))
        .unwrap();
        assert_eq!(info.revision, "");
        assert_eq!(info.server_id, "");
        assert_eq!(info.server_mods, 0);
        assert_eq!(info.mods, 3);
    }

    fn pack(
        mc_version: &str,
        loader: Loader,
        loader_version: &str,
    ) -> crate::instance::modpack::PackPreview {
        crate::instance::modpack::PackPreview {
            name: "Survival (renamed since)".into(),
            version: "1.0.0".into(),
            summary: None,
            mc_version: mc_version.into(),
            loader,
            loader_version: loader_version.into(),
            file_count: 7,
            format: "mrpack".into(),
            needs_curseforge_key: false,
        }
    }

    /// The pack that arrives is installed only if it is the setup the player
    /// said yes to, and each way it can differ is caught on its own.
    #[test]
    fn a_pack_is_refused_unless_it_is_the_setup_the_player_approved() {
        let fabric = described("1.21.1", Loader::Fabric, "0.16.9");
        check_matches(&fabric, &pack("1.21.1", Loader::Fabric, "0.16.9")).expect("the same setup");
        let vanilla = described("1.21.1", Loader::Vanilla, "");
        check_matches(&vanilla, &pack("1.21.1", Loader::Vanilla, ""))
            .expect("the same vanilla setup");

        for (what, approved, arrived) in [
            (
                "Minecraft version",
                &fabric,
                pack("1.21.4", Loader::Fabric, "0.16.9"),
            ),
            ("loader", &fabric, pack("1.21.1", Loader::Quilt, "0.16.9")),
            (
                "loader version",
                &fabric,
                pack("1.21.1", Loader::Fabric, "0.16.10"),
            ),
            // Described as a plain server with nothing to load mods with.
            (
                "vanilla turned modded",
                &vanilla,
                pack("1.21.1", Loader::Fabric, "0.16.9"),
            ),
        ] {
            let err = check_matches(approved, &arrived)
                .expect_err(what)
                .to_string();
            assert!(
                err.contains("changed since you looked") && err.contains("look it up again"),
                "{what}: {err}"
            );
        }

        let err = check_matches(&vanilla, &pack("1.21.1", Loader::Fabric, "0.16.9")).unwrap_err();
        assert!(
            err.to_string().contains(
                "it was Minecraft 1.21.1, and is now Minecraft 1.21.1 with Fabric 0.16.9"
            ),
            "{err}"
        );
    }

    /// The frontend reads `existing` as `{ id, name }` or `null`, never absent.
    #[test]
    fn a_preview_names_an_existing_instance_or_says_null() {
        let mut preview = JoinPreview {
            info: described("1.21.1", Loader::Fabric, "0.16.9"),
            address: "203.0.113.7:25590".into(),
            verified: Verified::Pinned,
            link: direct("203.0.113.7", 25590, &Code::generate(), Some(Pin([1; 32]))),
            existing: None,
        };
        assert_eq!(
            serde_json::to_value(&preview).unwrap()["existing"],
            serde_json::Value::Null
        );

        preview.existing = Some(ExistingInstance {
            id: "survival".into(),
            name: "Survival".into(),
            password_changed: true,
            certificate_changed: true,
        });
        assert_eq!(
            serde_json::to_value(&preview).unwrap()["existing"],
            serde_json::json!({
                "id": "survival",
                "name": "Survival",
                "passwordChanged": true,
                "certificateChanged": true,
            })
        );
    }

    /// A gzip bomb: well inside the limit on the wire, megabytes once
    /// inflated. It is read as the bytes that arrived, which are not a pack
    /// description — never inflated into one too long to be.
    #[tokio::test]
    async fn a_compressed_reply_is_read_as_sent_never_inflated() {
        use std::io::Write;
        let inflated = vec![0u8; 8 * 1024 * 1024];
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&inflated).unwrap();
        let bomb = encoder.finish().unwrap();
        assert!(
            bomb.len() < MAX_REPLY_BYTES,
            "the bomb must fit the limit on the wire"
        );

        let link = answering(whole("200 OK", "Content-Encoding: gzip\r\n", &bomb)).await;
        let err = promptly("a gzip bomb", preview(link)).await.unwrap_err();

        assert!(
            err.to_string().contains("not a Geode pack description"),
            "{err}"
        );
    }
}

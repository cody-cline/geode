//! Downloading a pack a link names, with no credentials at all.
//!
//! The recipient of a `geode://pack` link has no account anywhere and no API
//! key — Geode ships none, and the address in the link is whatever the sharer
//! put the file at. A chat attachment, a release asset, their own web space:
//! the one thing they all have in common is that nothing here can ask them a
//! question or prove who it is.
//!
//! So what comes back is **judged, not believed**:
//!
//! - what comes back is checked four ways. The status, the content type, the
//!   first four bytes (`PK\x03\x04`), and the SHA-256 the link carries. A web
//!   page is not a pack, and a host serving an interstitial, a preview page or
//!   a sign-in wall is the ordinary case rather than the strange one;
//! - when the address answers with a page, Geode **refuses** and sends the
//!   person down the path that already works — open it in a browser and import
//!   the file — rather than scraping the page for a real link on their behalf;
//! - [`probe`] makes the same request from the *sharer's* machine before a
//!   link is handed out, so the mistake is caught by the person who can fix it.
//!
//! The client here carries no `Authorization`, no cookies and no compression:
//! every byte counted is a byte that arrived, which is the same argument
//! [`super::trust`] makes for its own client.
//!
//! The ceiling and the cancel polling are [`super::join`]'s, reused rather
//! than restated, so there is one answer to "how big may a pack be".

use std::path::Path;

use futures::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};
use crate::net::{Progress, USER_AGENT};

use super::join::{unless_cancelled, MAX_PACK_BYTES};
use super::packlink::{PackHash, PackSource};

/// What every zip file starts with. Checked on the first chunk, before a file
/// is created, so a web page never lands on disk at all.
const ZIP_MAGIC: &[u8; 4] = b"PK\x03\x04";

/// Enough of the body to see what it is.
const SNIFF: usize = 512;

/// How long one request may wait for an answer. The body itself is not held to
/// a deadline: a big pack on a slow line takes as long as it takes.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Whether a link actually serves the pack, as the sharer's own machine found
/// out by fetching it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PublicCheck {
    /// The link served the archive.
    Served,
    /// The link served a web page — the page a file sits on rather than a
    /// link that downloads it, or a share that has gone private.
    WebPage,
    /// The check itself could not be made: no answer at all, or one that says
    /// nothing about what a recipient gets.
    Unknown,
}

/// A client with nothing of the user in it.
///
/// No `Authorization`, because the recipient has nothing to send and the
/// sharer must not: the point of the self-check is to see what a stranger
/// sees. No cookies either — reqwest is built here without its `cookies`
/// feature, so there is no store to turn off, and a signed-in browser is
/// exactly the case that works when the link does not. And nothing
/// decompressed, so every byte counted against the ceiling is a byte that
/// actually arrived.
fn plain() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()?)
}

/// Download the pack a link names to `dest`, verifying its hash.
///
/// Leaves nothing behind on any failure: a part file is removed, and so is a
/// finished file whose hash did not match.
pub async fn pack(
    source: &PackSource,
    hash: &PackHash,
    dest: &Path,
    progress: &Progress,
) -> Result<()> {
    pack_within(source, hash, dest, progress, MAX_PACK_BYTES).await
}

/// The same, with the ceiling as a parameter.
///
/// Only so a test can push a body past it without pushing two gigabytes
/// through a socket. Everything in production goes through [`pack`], which
/// passes [`MAX_PACK_BYTES`].
async fn pack_within(
    source: &PackSource,
    hash: &PackHash,
    dest: &Path,
    progress: &Progress,
    ceiling: u64,
) -> Result<()> {
    let client = plain()?;
    let mut refusals = Vec::new();

    for url in &source.download_urls() {
        progress.stage("Downloading the pack", 0).await;
        match attempt(&client, url, hash, dest, progress, ceiling).await {
            Ok(()) => return Ok(()),
            // Not the pack. The next shape may be.
            Err(Attempt::Refused(why)) => refusals.push(why),
            Err(Attempt::Fatal(e)) => return Err(e),
        }
    }
    Err(refused(source, &refusals))
}

/// Why one candidate URL did not produce the pack.
enum Attempt {
    /// Not the pack, and why not. Another URL is worth trying.
    Refused(Refusal),
    /// Worth telling the user about now: trying another URL would get the
    /// same bytes, or there is nothing left to try for.
    Fatal(Error),
}

/// What a candidate URL answered with, when it was not the pack.
///
/// Told apart because each one is a different thing for the person holding the
/// link to do, and because a web page is the *least* likely of them on a given
/// day — naming it for all three sent people off to a browser for a download
/// that had simply been cut off.
enum Refusal {
    /// The request never finished: a reset connection, a name that did not
    /// resolve, a read that timed out.
    Interrupted(String),
    /// An answer arrived, and it was not a success.
    Status(u16),
    /// A 200 that is a web page rather than an archive — a preview page, an
    /// interstitial, a sign-in wall — by content type or by first bytes.
    Page,
}

/// What to say when no candidate URL produced the pack.
///
/// The first refusal that is not a page decides. A page is what a host serves
/// *instead* of the file — an interstitial, or a share gone private — so it is
/// only the answer when nothing more definite came back; a dropped connection
/// or a 404 says something about the link itself.
fn refused(source: &PackSource, refusals: &[Refusal]) -> Error {
    let host = source.host();
    match refusals.iter().find(|why| !matches!(why, Refusal::Page)) {
        Some(Refusal::Interrupted(detail)) => Error::msg(format!(
            "the download from {host} was interrupted: {detail}"
        )),
        // Deleted, or not shared any more. Either way the person who sent the
        // link is the only one who can fix it.
        Some(Refusal::Status(403 | 404 | 410)) => Error::msg(format!(
            "{host} no longer has that pack — it was deleted, or it is not shared any more. \
             Ask whoever sent the link for a fresh one."
        )),
        Some(Refusal::Status(status)) => Error::msg(format!(
            "{host} answered {status} instead of serving the pack. Try the link again later, or \
             ask whoever sent it for a fresh one."
        )),
        // A page where an archive should be. Usually a share that has gone
        // private, or a host that serves an interstitial before the file.
        _ => Error::msg(format!(
            "{host} served a web page instead of the pack. The file may no longer be shared. \
             Open the link in your browser: if it downloads, use Add instance → Import a pack; \
             if not, ask whoever sent it for a fresh link."
        )),
    }
}

async fn attempt(
    client: &reqwest::Client,
    url: &str,
    want: &PackHash,
    dest: &Path,
    progress: &Progress,
    ceiling: u64,
) -> std::result::Result<(), Attempt> {
    let response = unless_cancelled(progress, client.get(url).send())
        .await
        .map_err(Attempt::Fatal)?
        .map_err(|e| Attempt::Refused(Refusal::Interrupted(e.to_string())))?;

    let status = response.status();
    if !status.is_success() {
        return Err(Attempt::Refused(Refusal::Status(status.as_u16())));
    }
    if is_html(&response) {
        return Err(Attempt::Refused(Refusal::Page));
    }
    // Checked, and then ignored in favour of counting what arrives: a server
    // can say one length and send another.
    if response.content_length().is_some_and(|len| len > ceiling) {
        return Err(Attempt::Fatal(too_large()));
    }
    progress
        .stage(
            "Downloading the pack",
            response.content_length().unwrap_or(0),
        )
        .await;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Attempt::Fatal(e.into()))?;
    }
    let part = dest.with_extension("part");
    // Nothing is opened until the first bytes have proved they are an archive,
    // so a web page never reaches the disk.
    let mut file: Option<tokio::fs::File> = None;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut head: Vec<u8> = Vec::new();
    let mut body = response.bytes_stream();

    let outcome = async {
        while let Some(chunk) = unless_cancelled(progress, body.next())
            .await
            .map_err(Attempt::Fatal)?
        {
            let chunk = chunk.map_err(|e| Attempt::Refused(Refusal::Interrupted(e.to_string())))?;
            if file.is_none() {
                // Held, not written, and never dropped: too little to judge
                // yet, and these are the first bytes of the pack and of the
                // hash it is checked against. A body can arrive in pieces
                // smaller than the magic.
                head.extend_from_slice(&chunk);
                if head.len() < ZIP_MAGIC.len() {
                    continue;
                }
                if !head.starts_with(ZIP_MAGIC) {
                    return Err(Attempt::Refused(Refusal::Page));
                }
                file = Some(
                    tokio::fs::File::create(&part)
                        .await
                        .map_err(|e| Attempt::Fatal(e.into()))?,
                );
            }
            // Everything held while the magic was in doubt, and then each
            // chunk as it comes.
            let bytes: &[u8] = if head.is_empty() { &chunk } else { &head };

            received += bytes.len() as u64;
            if received > ceiling {
                return Err(Attempt::Fatal(too_large()));
            }
            hasher.update(bytes);
            let file = file.as_mut().expect("a file once the magic matched");
            file.write_all(bytes)
                .await
                .map_err(|e| Attempt::Fatal(e.into()))?;
            progress.advance(bytes.len() as u64, "pack").await;
            head = Vec::new();
        }

        let Some(mut file) = file else {
            // Nothing arrived, or not even four bytes of it.
            return Err(Attempt::Refused(Refusal::Page));
        };
        file.flush().await.map_err(|e| Attempt::Fatal(e.into()))?;
        drop(file);

        let got = hex::encode(hasher.finalize());
        if got != want.to_hex() {
            return Err(Attempt::Fatal(Error::msg(
                "this is not the pack the link names — its contents do not match, so nothing \
                 was imported. Ask whoever sent it for a fresh link.",
            )));
        }
        tokio::fs::rename(&part, dest)
            .await
            .map_err(|e| Attempt::Fatal(e.into()))?;
        Ok(())
    }
    .await;

    if outcome.is_err() {
        // Written to a sibling and renamed on success, so a failure never
        // leaves a truncated archive that looks complete.
        let _ = tokio::fs::remove_file(&part).await;
    }
    outcome
}

/// Fetch a public link the way its recipient will, and say what came back.
///
/// One extra request on the sharing machine, right after the permission is
/// set, and it exercises precisely the recipient's path — which is the one
/// thing in this transport that cannot be unit-tested against the real
/// service.
pub async fn probe(url: &str) -> PublicCheck {
    let Ok(client) = plain() else {
        return PublicCheck::Unknown;
    };
    let response = match client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes=0-{}", SNIFF - 1))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return PublicCheck::Unknown,
    };
    // Not a web page: a status says nothing about what a recipient gets. This
    // endpoint is documented nowhere and the check asks it for a byte range,
    // so a 416, a 403 or a 500 here is a check that could not be made — not
    // evidence that the pack is too large to serve.
    if !response.status().is_success() {
        return PublicCheck::Unknown;
    }
    if is_html(&response) {
        return PublicCheck::WebPage;
    }

    let mut head = Vec::new();
    let mut body = response.bytes_stream();
    while let Some(Ok(chunk)) = body.next().await {
        head.extend_from_slice(&chunk);
        // Four bytes answer the question. However much a server that ignored
        // the range wants to send, that is all that is read before the
        // response is dropped.
        if head.len() >= ZIP_MAGIC.len() {
            break;
        }
    }
    if head.starts_with(ZIP_MAGIC) {
        PublicCheck::Served
    } else {
        PublicCheck::WebPage
    }
}

/// A real answer is `application/zip` or `application/octet-stream`. HTML is
/// the page a file sits on, a sign-in wall, or an error page.
fn is_html(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|kind| {
            let kind = kind.to_ascii_lowercase();
            kind.starts_with("text/html") || kind.starts_with("application/xhtml+xml")
        })
}

fn too_large() -> Error {
    Error::msg("this pack is too large to download")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};

    use http_body_util::Full;
    use hyper::body::{Bytes, Incoming};
    use hyper::{Request, Response};
    use hyper_util::rt::{TokioIo, TokioTimer};
    use tokio::net::TcpListener;

    /// A reply a test scripts, by the path it is asked for.
    #[derive(Clone)]
    struct Answer {
        status: u16,
        content_type: Option<&'static str>,
        body: Vec<u8>,
    }

    impl Answer {
        fn zip(body: Vec<u8>) -> Self {
            Self {
                status: 200,
                content_type: Some("application/zip"),
                body,
            }
        }

        fn html() -> Self {
            Self {
                status: 200,
                content_type: Some("text/html; charset=utf-8"),
                body: b"<html><body>Sign in to download this file.</body></html>"
                    .to_vec(),
            }
        }
    }

    /// A server that answers `/one` and `/two` with whatever a test says, and
    /// counts what was asked for.
    struct Serving {
        base: String,
        asked: Arc<Mutex<Vec<String>>>,
    }

    async fn serving(answers: Vec<(&'static str, Answer)>) -> Serving {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let table: Arc<Vec<(&'static str, Answer)>> = Arc::new(answers);

        let (loop_asked, loop_table) = (asked.clone(), table.clone());
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let (asked, table) = (loop_asked.clone(), loop_table.clone());
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |r: Request<Incoming>| {
                        let (asked, table) = (asked.clone(), table.clone());
                        async move {
                            let path = r.uri().path().to_string();
                            asked.lock().unwrap().push(path.clone());
                            let answer = table
                                .iter()
                                .find(|(at, _)| *at == path)
                                .map(|(_, answer)| answer.clone());
                            let Some(answer) = answer else {
                                return Ok::<_, Infallible>(
                                    Response::builder()
                                        .status(404)
                                        .body(Full::new(Bytes::new()))
                                        .unwrap(),
                                );
                            };
                            let mut builder = Response::builder().status(answer.status);
                            if let Some(kind) = answer.content_type {
                                builder = builder.header("content-type", kind);
                            }
                            Ok::<_, Infallible>(
                                builder.body(Full::new(Bytes::from(answer.body))).unwrap(),
                            )
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .timer(TokioTimer::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Serving { base, asked }
    }

    /// Bytes that start like a zip and are stable, so a hash can be written
    /// down once.
    fn archive(len: usize) -> Vec<u8> {
        let mut bytes = ZIP_MAGIC.to_vec();
        bytes.extend((0..len.saturating_sub(4)).map(|i| (i % 251) as u8));
        bytes
    }

    fn hash_of(bytes: &[u8]) -> PackHash {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        PackHash::from_hex(&hex::encode(hasher.finalize())).unwrap()
    }

    fn one_url(base: &str) -> PackSource {
        PackSource {
            url: format!("{base}/one"),
        }
    }

    #[tokio::test]
    async fn a_served_archive_is_written_and_its_hash_checked() {
        let bytes = archive(5000);
        let serving = serving(vec![("/one", Answer::zip(bytes.clone()))]).await;
        let scratch = Scratch::new("fetch-ok");
        let dest = scratch.paths.root.join("incoming/one.geodepack");

        pack(
            &one_url(&serving.base),
            &hash_of(&bytes),
            &dest,
            &Progress::silent("pack-link"),
        )
        .await
        .unwrap();

        assert_eq!(tokio::fs::read(&dest).await.unwrap(), bytes);
        assert!(!dest.with_extension("part").exists());
        assert_eq!(*serving.asked.lock().unwrap(), vec!["/one".to_string()]);
    }

    /// A `url=` source derives nothing: exactly the URL in the link, once.
    #[tokio::test]
    async fn a_url_source_asks_for_exactly_what_the_link_says() {
        let bytes = archive(64);
        let serving = serving(vec![("/deep/path", Answer::zip(bytes.clone()))]).await;
        let scratch = Scratch::new("fetch-url");
        let dest = scratch.paths.root.join("one.geodepack");

        let source = PackSource {
            url: format!("{}/deep/path?export=download", serving.base),
        };
        pack(
            &source,
            &hash_of(&bytes),
            &dest,
            &Progress::silent("pack-link"),
        )
        .await
        .unwrap();
        assert_eq!(
            *serving.asked.lock().unwrap(),
            vec!["/deep/path".to_string()]
        );
    }

    #[tokio::test]
    async fn a_web_page_on_the_first_shape_makes_the_second_be_tried() {
        // Pointed at the fake by host override, which is what makes this
        // testable without reaching a real host.
        let bytes = archive(300);
        let serving = serving(vec![
            ("/first", Answer::html()),
            ("/second", Answer::zip(bytes.clone())),
        ])
        .await;
        let scratch = Scratch::new("fetch-second");
        let dest = scratch.paths.root.join("one.geodepack");

        let client = plain().unwrap();
        let first = attempt(
            &client,
            &format!("{}/first", serving.base),
            &hash_of(&bytes),
            &dest,
            &Progress::silent("p"),
            MAX_PACK_BYTES,
        )
        .await;
        assert!(matches!(first, Err(Attempt::Refused(Refusal::Page))));
        assert!(!dest.exists(), "a web page landed on disk");

        let second = attempt(
            &client,
            &format!("{}/second", serving.base),
            &hash_of(&bytes),
            &dest,
            &Progress::silent("p"),
            MAX_PACK_BYTES,
        )
        .await;
        assert!(second.is_ok(), "the second shape should have worked");
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), bytes);
    }

    /// Each refusal is a different thing for the person holding the link to
    /// do, and the virus-scan page is the one that must not be named for the
    /// others: it sends them to a browser for a download that was working.
    ///
    /// A pure test: which refusal is reported is a decision about what came
    /// back, and needs no server to make.
    #[test]
    fn what_no_candidate_serving_the_pack_is_blamed_on_depends_on_what_happened() {
        let elsewhere = PackSource {
            url: "https://files.example.com/survival.geodepack".into(),
        };

        // A page where an archive should be: the one case that says to open
        // it in a browser, because that is what distinguishes a share that
        // went private from a host that is simply down.
        let said = refused(&elsewhere, &[Refusal::Page]).to_string();
        assert!(said.contains("Import a pack"), "{said}");

        // One of them answered with a page, but the other never finished. The
        // connection is what to say, not the page.
        for refusals in [
            vec![
                Refusal::Interrupted("connection reset".into()),
                Refusal::Page,
            ],
            vec![
                Refusal::Page,
                Refusal::Interrupted("connection reset".into()),
            ],
        ] {
            let said = refused(&elsewhere, &refusals).to_string();
            assert!(said.contains("was interrupted"), "{said}");
            assert!(said.contains("connection reset"), "{said}");
            assert!(!said.contains("Import a pack"), "{said}");
        }

        // Gone, or not shared any more: only the sender can fix it.
        for gone in [403u16, 404, 410] {
            let said = refused(&elsewhere, &[Refusal::Status(gone)]).to_string();
            assert!(said.contains("no longer has that pack"), "{gone}: {said}");
            assert!(said.contains("files.example.com"), "{gone}: {said}");
        }

        // Anything else says what the host answered rather than guessing.
        let said = refused(&elsewhere, &[Refusal::Status(500)]).to_string();
        assert!(said.contains("answered 500"), "{said}");
        // A page loses to anything more definite: a status says something
        // about the link, where a page only says what the host chose to render.
        let said = refused(&elsewhere, &[Refusal::Page, Refusal::Status(404)]).to_string();
        assert!(said.contains("no longer has that pack"), "{said}");
    }

    /// The same, end to end: what `attempt` saw is what the message is built
    /// from. A pack whose link is still handed round after the file has gone
    /// is the likeliest failure of all.
    #[tokio::test]
    async fn a_pack_that_is_not_there_any_more_says_to_ask_for_a_fresh_link() {
        // Nothing is served at `/one`, so the fake answers 404.
        let serving = serving(vec![]).await;
        let scratch = Scratch::new("fetch-gone");
        let dest = scratch.paths.root.join("one.geodepack");

        let said = pack(
            &one_url(&serving.base),
            &hash_of(b"nothing"),
            &dest,
            &Progress::silent("pack-link"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(said.contains("no longer has that pack"), "{said}");
        assert!(!dest.exists());
    }

    /// A body that is not an archive is refused on its first four bytes, and
    /// the hash in the link is given to it on purpose: whoever wrote the link
    /// chose the hash too, so a matching hash must not be enough to make a web
    /// page into a pack.
    #[tokio::test]
    async fn a_body_that_is_not_an_archive_is_refused_even_when_its_hash_matches() {
        let not_a_pack = b"<!doctype html><title>Sign in</title>".to_vec();
        let mut answer = Answer::zip(not_a_pack.clone());
        // A content type that says nothing, so only the magic can refuse it.
        answer.content_type = Some("application/octet-stream");
        let serving = serving(vec![("/one", answer)]).await;
        let scratch = Scratch::new("fetch-magic");
        let dest = scratch.paths.root.join("one.geodepack");

        let failed = pack(
            &one_url(&serving.base),
            &hash_of(&not_a_pack),
            &dest,
            &Progress::silent("p"),
        )
        .await;
        assert!(failed.is_err(), "a web page was imported as a pack");
        assert!(!dest.exists(), "{} was created", dest.display());
        assert!(!dest.with_extension("part").exists());
    }

    /// The content type alone is enough, before any of the body is read. Given
    /// a body that would otherwise pass every other check, so the check being
    /// tested is the only thing that can refuse it.
    #[tokio::test]
    async fn an_html_content_type_is_refused_whatever_the_body_turns_out_to_be() {
        let bytes = archive(600);
        let mut answer = Answer::zip(bytes.clone());
        answer.content_type = Some("text/html; charset=utf-8");
        let serving = serving(vec![("/one", answer)]).await;
        let scratch = Scratch::new("fetch-html-type");
        let dest = scratch.paths.root.join("one.geodepack");

        let failed = pack(
            &one_url(&serving.base),
            &hash_of(&bytes),
            &dest,
            &Progress::silent("p"),
        )
        .await;
        assert!(
            failed.is_err(),
            "an answer calling itself a web page was taken for a pack"
        );
        assert!(!dest.exists());
    }

    /// A body can arrive in pieces smaller than the four bytes that prove it
    /// is an archive. Those bytes are still the pack's, and still the hash's:
    /// dropping them would have the recipient told that whoever sent the link
    /// got the pack wrong.
    #[tokio::test]
    async fn a_pack_that_arrives_in_pieces_keeps_the_bytes_it_was_judged_on() {
        let bytes = archive(97);
        let scratch = Scratch::new("fetch-pieces");

        for piece in [1, 2, 3, 4, 5] {
            let serving = in_pieces(bytes.clone(), piece).await;
            let dest = scratch.paths.root.join(format!("{piece}.geodepack"));
            pack(
                &one_url(&serving),
                &hash_of(&bytes),
                &dest,
                &Progress::silent("p"),
            )
            .await
            .unwrap_or_else(|e| panic!("{piece}-byte pieces: {e}"));
            assert_eq!(tokio::fs::read(&dest).await.unwrap(), bytes, "{piece}");
        }
    }

    #[tokio::test]
    async fn a_hash_that_does_not_match_leaves_nothing_behind() {
        let bytes = archive(2048);
        let serving = serving(vec![("/one", Answer::zip(bytes))]).await;
        let scratch = Scratch::new("fetch-hash");
        let dest = scratch.paths.root.join("one.geodepack");

        let said = pack(
            &one_url(&serving.base),
            &hash_of(&archive(2047)),
            &dest,
            &Progress::silent("p"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(said.contains("not the pack the link names"), "{said}");
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
    }

    /// A reply written by hand, with no content length at all: the body
    /// arrives chunked, so only counting it as it comes can refuse it.
    async fn chunked(body: Vec<u8>) -> String {
        let whole = body.len();
        in_pieces(body, whole).await
    }

    /// The same, framed as chunks of `piece` bytes, so the body is read in
    /// pieces instead of all at once.
    async fn in_pieces(body: Vec<u8>, piece: usize) -> String {
        let mut framed = Vec::new();
        for part in body.chunks(piece) {
            framed.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
            framed.extend_from_slice(part);
            framed.extend_from_slice(b"\r\n");
        }
        framed.extend_from_slice(b"0\r\n\r\n");
        raw(
            "Content-Type: application/zip\r\nTransfer-Encoding: chunked".into(),
            framed,
        )
        .await
    }

    /// A reply written by hand, so it can claim a content length its body does
    /// not have. Hyper refuses to send one — which is the whole reason the
    /// header is only a claim and the count is the fact.
    async fn claiming(length: u64, body: Vec<u8>) -> String {
        raw(
            format!("Content-Type: application/zip\r\nContent-Length: {length}"),
            body,
        )
        .await
    }

    /// The bytes of one reply, exactly as written.
    async fn raw(headers: String, body: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let (headers, body) = (headers.clone(), body.clone());
                tokio::spawn(async move {
                    let (mut seen, mut buf) = (Vec::new(), [0u8; 1024]);
                    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tcp.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                    let mut reply = format!("HTTP/1.1 200 OK\r\n{headers}\r\n\r\n").into_bytes();
                    reply.extend_from_slice(&body);
                    let _ = tcp.write_all(&reply).await;
                    let _ = tcp.flush().await;
                });
            }
        });
        base
    }

    /// The header is a claim; the count is the fact. Both are checked, in that
    /// order, so a lie costs nothing and the truth is still bounded.
    #[tokio::test]
    async fn a_pack_over_the_ceiling_is_refused_by_the_header_and_by_the_count() {
        let scratch = Scratch::new("fetch-ceiling");
        let dest = scratch.paths.root.join("one.geodepack");

        let lies = claiming(MAX_PACK_BYTES + 1, archive(64)).await;
        let said = pack(
            &one_url(&lies),
            &hash_of(&archive(64)),
            &dest,
            &Progress::silent("p"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(said.contains("too large"), "{said}");
        assert!(!dest.exists());

        // And the count, which is the fact. The body arrives chunked, so
        // there is no length to read and only counting it can refuse it — and
        // the ceiling is brought down to what a test can push through a socket.
        let big = chunked(archive(4096)).await;
        let said = pack_within(
            &one_url(&big),
            &hash_of(&archive(4096)),
            &dest,
            &Progress::silent("p"),
            1024,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(said.contains("too large"), "{said}");
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
    }

    #[tokio::test]
    async fn a_cancel_stops_the_download_and_removes_what_arrived() {
        let bytes = archive(1 << 20);
        let serving = serving(vec![("/one", Answer::zip(bytes.clone()))]).await;
        let scratch = Scratch::new("fetch-cancel");
        let dest = scratch.paths.root.join("one.geodepack");

        let progress = Progress::silent("pack-link");
        progress
            .cancel_flag()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let stopped = pack(&one_url(&serving.base), &hash_of(&bytes), &dest, &progress).await;
        assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
    }

    /// The sharer finds out before the recipient does.
    #[tokio::test]
    async fn the_self_check_tells_a_served_pack_from_a_web_page() {
        let mut unmarked = Answer::zip(b"<!doctype html><title>Sign in</title>".to_vec());
        // A 200 that says nothing about itself: only the first four bytes can
        // tell this from the pack.
        unmarked.content_type = Some("application/octet-stream");
        let serving = serving(vec![
            ("/pack", Answer::zip(archive(4096))),
            ("/page", Answer::html()),
            ("/unmarked", unmarked),
        ])
        .await;
        assert_eq!(
            probe(&format!("{}/pack", serving.base)).await,
            PublicCheck::Served
        );
        assert_eq!(
            probe(&format!("{}/page", serving.base)).await,
            PublicCheck::WebPage
        );
        assert_eq!(
            probe(&format!("{}/unmarked", serving.base)).await,
            PublicCheck::WebPage
        );
        // A 404 is not a web page: it says nothing about what a recipient
        // gets, so the sharer is not told their pack is too large.
        assert_eq!(
            probe(&format!("{}/missing", serving.base)).await,
            PublicCheck::Unknown
        );
        // And a check that could not be made says so rather than guessing.
        assert_eq!(probe("http://127.0.0.1:1/pack").await, PublicCheck::Unknown);
    }
}

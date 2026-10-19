//! Connecting a Google account: the loopback redirect flow with PKCE.
//!
//! Google's own guidance for "macOS, Linux, and Windows desktop apps" is the
//! loopback redirect, and that is what this is. The out-of-band copy-and-paste
//! redirect it replaced is gone — "no longer supported" — so nothing here may
//! fall back to it.
//!
//! The device flow [`crate::auth`] uses for Minecraft would also work:
//! `drive.file` is on the list of scopes Google's limited-input-device page
//! allows. It is still the wrong choice here. That flow exists for devices
//! with no browser and no keyboard, and it makes the user read a code off one
//! screen and type it into another for no gain on a desktop.
//!
//! The steps:
//!
//! 1. Bind an HTTP listener on `127.0.0.1` on whatever port is free, and make
//!    `http://127.0.0.1:<port>` the redirect — the form Google's own sample
//!    uses. IPv4 loopback only: nothing off this machine can reach it.
//! 2. Open Google's authorization page with a PKCE challenge and a `state`.
//! 3. Wait for the browser to come back to the listener with the code.
//! 4. Exchange the code for tokens, sending the verifier the challenge was
//!    made from.
//!
//! PKCE is not optional here even though Google marks the parameters
//! "Recommended": a desktop app is a public client that "cannot keep secrets
//! confidential", so the verifier is the only thing that ties the code to the
//! process that asked for it. `S256` is always sent explicitly, because the
//! documented default for a missing `code_challenge_method` is `plain`.
//!
//! The listener is open on loopback for as long as the user is in their
//! browser. Any process on this machine can connect to it, which is why the
//! `state` is compared without leaking where it differs, a mismatch does not
//! end the wait, and the page served echoes nothing from the request. A local
//! process that could read the code could equally read `google.json`, so this
//! does not widen what a local attacker already has.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{Error, Result};
use crate::net::Progress;

use super::SCOPE;

/// The characters a verifier and a `state` are drawn from.
///
/// RFC 7636 allows `A–Z a–z 0–9 - . _ ~` for a verifier. This is that set
/// without `.` and `~`, which leaves exactly 64 — so one random byte modulo
/// the alphabet is uniform, and there is no modulo bias to reason about. A
/// subset of what is allowed is still allowed.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// 64 characters, inside RFC 7636's 43–128.
const VERIFIER_LEN: usize = 64;

/// 32 characters: this only has to be unguessable for the minute the browser
/// is away, and it is compared, never stored.
const STATE_LEN: usize = 32;

/// How long the user has in their browser before the listener gives up.
pub const REDIRECT_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// How often a wait on the browser looks to see whether it was cancelled, and
/// whether the deadline has passed. The same reason [`crate::share::join`]
/// polls: nothing arrives to wake the wait while the user is reading Google's
/// consent screen.
const POLL: Duration = Duration::from_millis(100);

/// How long one connection may take to send its request line and headers.
/// A browser sends them at once; anything else is a probe holding a socket.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// What the browser is served once it comes back. Nothing from the request is
/// echoed into it.
const DONE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Geode</title></head><body style=\"font:16px system-ui;padding:3rem\">\
<h1>Geode is connected to Google Drive.</h1><p>You can close this tab.</p>\
</body></html>";

/// One connect attempt: the listener, the verifier and the `state`.
pub struct Authorization {
    listener: TcpListener,
    verifier: String,
    state: String,
    redirect_uri: String,
}

/// Tokens Google granted, with the expiry already turned into a timestamp.
#[derive(Debug, Clone)]
pub struct Granted {
    pub access_token: String,
    /// Always present for an installed application, by Google's own words.
    /// Absent is a case worth refusing rather than storing.
    pub refresh_token: Option<String>,
    pub expires_at: i64,
}

/// Why a refresh failed, because the two have different answers.
#[derive(Debug)]
pub enum RefreshError {
    /// Google will not accept this refresh token again: it was revoked, went
    /// six months unused, was pushed out by the hundredth token issued to this
    /// client, or fell foul of a session-control policy. All of them mean the
    /// user has to connect again.
    Expired,
    /// A server or transport failure. The refresh token is still good.
    Transient(Error),
}

impl Authorization {
    /// Bind the loopback listener and draw a fresh verifier and `state`.
    pub async fn begin() -> Result<Self> {
        // Port 0 means "whatever is free". Google says to "query your platform
        // for the relevant loopback IP address and start an HTTP listener on a
        // random available port", and its own sample redirect is a bare
        // `http://127.0.0.1:9004` — no path, no trailing slash.
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| {
            Error::msg(format!(
                "Geode could not open a port on this computer to finish signing in to Google: {e}"
            ))
        })?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            verifier: random_text(VERIFIER_LEN),
            state: random_text(STATE_LEN),
            redirect_uri: format!("http://127.0.0.1:{port}"),
        })
    }

    /// The redirect, byte for byte as it goes into both the authorization URL
    /// and the token exchange. Google requires them to be identical.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The page to open in the user's browser.
    ///
    /// Carries exactly the parameters Google's installed-app table lists, and
    /// no others: `access_type` and `prompt` are not in it and are not sent.
    pub fn url(&self, auth_endpoint: &str, client_id: &str) -> Result<String> {
        let mut url = url::Url::parse(auth_endpoint)
            .map_err(|_| Error::msg("Google's sign-in address is not a valid URL"))?;
        url.query_pairs_mut()
            .clear()
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPE)
            .append_pair("code_challenge", &challenge(&self.verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &self.state);
        Ok(url.to_string())
    }

    /// Wait for the browser to come back, and hand over the code it brought.
    ///
    /// A request that is not the user's redirect — a browser asking for
    /// `/favicon.ico`, a stray probe, a `state` that does not match — is
    /// answered and the wait goes on. Only the redirect, a refusal from
    /// Google, the deadline or a cancel ends it.
    pub async fn finish(&self, progress: &Progress, deadline: Duration) -> Result<String> {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<String>>(4);
        let expect = Arc::new(self.state.clone());
        let until = Instant::now() + deadline;
        let mut poll = tokio::time::interval(POLL);

        loop {
            progress.check_cancelled()?;
            tokio::select! {
                accepted = self.listener.accept() => {
                    // A failed accept is not a reason to stop waiting: the
                    // browser may well connect again.
                    if let Ok((tcp, _)) = accepted {
                        tokio::spawn(answer(tcp, expect.clone(), tx.clone()));
                    }
                }
                arrived = rx.recv() => {
                    // The sender is held by this function, so `None` cannot
                    // happen; treat it as nothing having arrived.
                    if let Some(outcome) = arrived {
                        return outcome;
                    }
                }
                _ = poll.tick() => {
                    progress.check_cancelled()?;
                    if Instant::now() >= until {
                        return Err(Error::msg(
                            "Google did not come back within five minutes — press Connect to start over",
                        ));
                    }
                }
            }
        }
    }

    pub fn verifier(&self) -> &str {
        &self.verifier
    }
}

/// Serve one connection, and report an outcome only once the browser has its
/// answer — so the tab shows the page rather than a dropped connection.
async fn answer(
    tcp: TcpStream,
    expect: Arc<String>,
    tx: tokio::sync::mpsc::Sender<Result<String>>,
) {
    let found: Arc<Mutex<Option<Result<String>>>> = Arc::new(Mutex::new(None));
    let service = hyper::service::service_fn(|request: Request<Incoming>| {
        let (expect, found) = (expect.clone(), found.clone());
        async move {
            let (outcome, response) = redirect(&request, &expect);
            if let Some(outcome) = outcome {
                // Keep the first: a connection carries one request, and a
                // second would be a reload of the same redirect.
                let mut slot = found.lock().unwrap_or_else(|p| p.into_inner());
                if slot.is_none() {
                    *slot = Some(outcome);
                }
            }
            Ok::<_, Infallible>(response)
        }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT)
        .keep_alive(false)
        .serve_connection(TokioIo::new(tcp), service)
        .await;

    let outcome = found.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(outcome) = outcome {
        let _ = tx.send(outcome).await;
    }
}

/// Read one request as the redirect, if that is what it is.
///
/// Returns what to report, if anything, and what to answer. `None` means the
/// wait goes on.
fn redirect(
    request: &Request<Incoming>,
    expect: &str,
) -> (Option<Result<String>>, Response<Full<Bytes>>) {
    if request.method() != Method::GET || request.uri().path() != "/" {
        return (None, page(StatusCode::NOT_FOUND, "not found"));
    }

    let (mut code, mut state, mut failed) = (None, None, None);
    for (key, value) in url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
    {
        match key.as_ref() {
            "code" => code = Some(value.to_string()),
            "state" => state = Some(value.to_string()),
            "error" => failed = Some(value.to_string()),
            // Google also sends `scope` and `authuser`; anything unasked-for
            // is simply not read.
            _ => {}
        }
    }

    // A request from some other process on this machine must not end the
    // user's wait, so a mismatch is answered and forgotten.
    if !state.as_deref().is_some_and(|given| same(given, expect)) {
        return (None, page(StatusCode::BAD_REQUEST, "not for this app"));
    }

    if let Some(failed) = failed {
        let said = if failed == "access_denied" {
            "you declined Geode's request in your browser".to_string()
        } else {
            format!("Google refused the request ({failed})")
        };
        return (
            Some(Err(Error::msg(said))),
            page(
                StatusCode::OK,
                "Geode heard Google's answer. You can close this tab.",
            ),
        );
    }

    match code {
        Some(code) if !code.is_empty() => (
            Some(Ok(code)),
            Response::builder()
                .status(StatusCode::OK)
                .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
                .header(hyper::header::CACHE_CONTROL, "no-store")
                .body(Full::new(Bytes::from(DONE_PAGE)))
                .expect("a response built from fixed, valid parts"),
        ),
        // The right `state` and nothing to do with it. Not an answer, so not
        // the end of the wait.
        _ => (None, page(StatusCode::BAD_REQUEST, "nothing to do")),
    }
}

fn page(status: StatusCode, text: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(hyper::header::CACHE_CONTROL, "no-store")
        .body(Full::new(Bytes::from(text.to_string())))
        .expect("a response built from fixed, valid parts")
}

/// Whether two strings match, without leaking where they first differ.
///
/// The same fold [`crate::share::code::Code::matches`] uses, and for the same
/// reason: this one is checked against whatever any process on this machine
/// chooses to send.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// PKCE
// ---------------------------------------------------------------------------

/// `len` characters from [`ALPHABET`], from the operating system's randomness.
fn random_text(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).expect("the OS always has randomness");
    bytes
        .iter()
        .map(|b| ALPHABET[(*b % 64) as usize] as char)
        .collect()
}

/// BASE64URL-no-pad(SHA-256(verifier)), which is what `S256` means.
fn challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    base64url_nopad(&hasher.finalize())
}

/// Base64 with the URL alphabet and no `=` padding, as RFC 7636 requires.
///
/// Twelve lines rather than a dependency, in keeping with the hand-rolled MD5
/// in [`crate::auth`] next door, and pinned by the RFC's own test vector.
fn base64url_nopad(bytes: &[u8]) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let three = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let packed = (u32::from(three[0]) << 16) | (u32::from(three[1]) << 8) | u32::from(three[2]);
        // One character per six bits, and one fewer for each byte the chunk is
        // short of three: 3 bytes give 4 characters, 2 give 3, 1 gives 2.
        for i in 0..=chunk.len() {
            out.push(B64[((packed >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The token endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Trade the code for tokens.
///
/// `client_secret` is sent only when there is one. Google's parameter table
/// marks it Optional for an installed app, and a desktop client's "secret" is
/// by Google's own words not confidential — so Geode asks for one only as a
/// contingency and sends it only if the user supplied it.
pub async fn exchange(
    client: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Granted> {
    let mut form = vec![
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }

    let response = client.post(token_endpoint).form(&form).send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(Error::msg(format!(
            "{} {}",
            refused(status, &body),
            crate::auth::describe_oauth_error(&body)
        )));
    }

    let granted = read_tokens(&body)?;
    // "Note that refresh tokens are always returned for installed
    // applications." If one is missing anyway, a connection is stored that
    // stops working within the hour, which looks like a bug rather than a
    // setting — so it is refused with the one thing that fixes it.
    if granted.refresh_token.is_none() {
        return Err(Error::msg(
            "Google did not return a refresh token — remove Geode at myaccount.google.com \
             under Third-party access, then connect again",
        ));
    }
    Ok(granted)
}

/// A new access token from the refresh token.
pub async fn refresh(
    client: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
) -> std::result::Result<Granted, RefreshError> {
    let mut form = vec![
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }

    let response = client
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| RefreshError::Transient(e.into()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| RefreshError::Transient(e.into()))?;

    if status.is_success() {
        return read_tokens(&body).map_err(RefreshError::Transient);
    }
    // `invalid_grant` is the one permanent answer: the token is gone and no
    // amount of retrying brings it back. Everything else — a 500, a 503, a
    // rate limit — is worth trying again with the token still in hand.
    if crate::auth::oauth_error_code(&body).as_deref() == Some("invalid_grant") {
        return Err(RefreshError::Expired);
    }
    Err(RefreshError::Transient(Error::msg(format!(
        "Google could not refresh the Drive connection ({status}). {}",
        crate::auth::describe_oauth_error(&body)
    ))))
}

/// Ask Google to forget the connection. The token rides in `token`.
pub async fn revoke(client: &reqwest::Client, revoke_endpoint: &str, token: &str) -> Result<()> {
    let response = client
        .post(revoke_endpoint)
        .form(&[("token", token)])
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(Error::msg(format!(
            "Google did not accept the request to disconnect ({status})"
        )));
    }
    Ok(())
}

fn read_tokens(body: &str) -> Result<Granted> {
    let parsed: TokenResponse =
        serde_json::from_str(body).map_err(|e| Error::json("Google's token response", e))?;
    Ok(Granted {
        expires_at: chrono::Utc::now().timestamp() + parsed.expires_in.unwrap_or(3600),
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
    })
}

/// The sentence a refused exchange starts with, naming the cause when the
/// status says what it is.
fn refused(status: reqwest::StatusCode, body: &str) -> String {
    match crate::auth::oauth_error_code(body).as_deref() {
        // The one failure whose cause is a setting the user can see.
        Some("redirect_uri_mismatch") => {
            "Google refused the redirect. Create the client as a Desktop app, \
             not a Web application."
                .to_string()
        }
        Some("invalid_client") => {
            "Google did not recognise this client. Check the Client ID, and if it is right, \
             paste the client secret Google showed beside it into Settings."
                .to_string()
        }
        _ => format!("Google refused the sign-in ({status})."),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Call, Fake, Reply};
    use super::*;

    /// A client that talks plain HTTP to the fake, with no retries of its own.
    fn plain() -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent(crate::net::USER_AGENT)
            .build()
            .unwrap()
    }

    /// RFC 7636's own appendix B vector. If the base64, the hash or the
    /// padding is wrong, every real connect fails with `invalid_grant` and
    /// nothing says why.
    #[test]
    fn the_challenge_matches_rfc_7636s_test_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    /// Every chunk length, so the "one character per six bits" loop is pinned
    /// at a one-byte and a two-byte tail as well as a whole block.
    #[test]
    fn base64url_has_no_padding_and_the_url_alphabet() {
        assert_eq!(base64url_nopad(b""), "");
        assert_eq!(base64url_nopad(b"f"), "Zg");
        assert_eq!(base64url_nopad(b"fo"), "Zm8");
        assert_eq!(base64url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64url_nopad(b"foob"), "Zm9vYg");
        assert_eq!(base64url_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url_nopad(b"foobar"), "Zm9vYmFy");
        // The two characters base64url replaces: `+/` become `-_`.
        assert_eq!(base64url_nopad(&[0xfb, 0xff, 0xbf]), "-_-_");
    }

    #[test]
    fn a_verifier_is_64_unreserved_characters_and_never_the_same_twice() {
        let one = random_text(VERIFIER_LEN);
        let two = random_text(VERIFIER_LEN);
        assert_eq!(one.chars().count(), 64);
        assert_ne!(one, two);
        for text in [&one, &two] {
            assert!(
                text.bytes().all(|b| ALPHABET.contains(&b)),
                "{text} is outside the unreserved set"
            );
        }
    }

    #[tokio::test]
    async fn the_authorization_url_carries_exactly_the_documented_parameters() {
        let auth = Authorization::begin().await.unwrap();
        let url = auth
            .url("https://accounts.google.com/o/oauth2/v2/auth", "client-1")
            .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        assert_eq!(parsed.path(), "/o/oauth2/v2/auth");

        let pairs: std::collections::HashMap<String, String> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let mut keys: Vec<&str> = pairs.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "client_id",
                "code_challenge",
                "code_challenge_method",
                "redirect_uri",
                "response_type",
                "scope",
                "state",
            ]
        );

        assert_eq!(pairs["client_id"], "client-1");
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["scope"], "https://www.googleapis.com/auth/drive.file");
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["code_challenge"], challenge(auth.verifier()));
        assert_eq!(pairs["state"].len(), STATE_LEN);
        // The redirect is the port actually bound, and the exact form Google's
        // own sample uses: no path, no trailing slash.
        let port = auth.listener.local_addr().unwrap().port();
        assert_eq!(pairs["redirect_uri"], format!("http://127.0.0.1:{port}"));
        assert_eq!(auth.redirect_uri(), pairs["redirect_uri"]);
    }

    /// Visiting the redirect the way a browser does.
    async fn visit(redirect_uri: &str, query: &str) -> (reqwest::StatusCode, String) {
        let response = plain()
            .get(format!("{redirect_uri}/{query}"))
            .send()
            .await
            .expect("the loopback listener answered");
        (response.status(), response.text().await.unwrap())
    }

    #[tokio::test]
    async fn the_redirect_hands_over_the_code_and_answers_with_a_page() {
        let auth = Authorization::begin().await.unwrap();
        let state = auth.state.clone();
        let redirect = auth.redirect_uri().to_string();

        let waiting = tokio::spawn(async move {
            let got = auth
                .finish(&Progress::silent("connect"), Duration::from_secs(10))
                .await;
            (got, auth)
        });
        // Give the wait a moment to reach its accept, then come back as the
        // browser would.
        let (status, body) = visit(&redirect, &format!("?state={state}&code=auth-code-7")).await;

        assert_eq!(status, 200);
        assert!(body.contains("connected to Google Drive"), "served: {body}");
        assert!(!body.contains("auth-code-7"), "the page echoed the code");

        let (got, auth) = waiting.await.unwrap();
        assert_eq!(got.unwrap(), "auth-code-7");
        // And it stopped listening: the port is let go when the listener drops.
        drop(auth);
    }

    #[tokio::test]
    async fn a_wrong_state_and_a_stray_path_are_answered_and_the_wait_goes_on() {
        let auth = Authorization::begin().await.unwrap();
        let state = auth.state.clone();
        let redirect = auth.redirect_uri().to_string();

        let waiting = tokio::spawn(async move {
            auth.finish(&Progress::silent("connect"), Duration::from_secs(10))
                .await
        });

        // Another process on this machine, guessing.
        let (status, _) = visit(&redirect, "?state=not-the-one&code=stolen").await;
        assert_eq!(status, 400);
        // A browser fetching its icon.
        let (status, _) = visit(&redirect, "favicon.ico").await;
        assert_eq!(status, 404);
        // The right state and nothing to do with it.
        let (status, _) = visit(&redirect, &format!("?state={state}")).await;
        assert_eq!(status, 400);

        // Still waiting, and the real redirect still works.
        let (status, _) = visit(&redirect, &format!("?state={state}&code=real")).await;
        assert_eq!(status, 200);
        assert_eq!(waiting.await.unwrap().unwrap(), "real");
    }

    #[tokio::test]
    async fn a_refusal_from_google_ends_the_wait_in_words_a_user_can_read() {
        for (error, expect) in [
            ("access_denied", "declined"),
            ("admin_policy_enforced", "admin_policy_enforced"),
        ] {
            let auth = Authorization::begin().await.unwrap();
            let state = auth.state.clone();
            let redirect = auth.redirect_uri().to_string();
            let waiting = tokio::spawn(async move {
                auth.finish(&Progress::silent("connect"), Duration::from_secs(10))
                    .await
            });

            let (status, _) = visit(&redirect, &format!("?state={state}&error={error}")).await;
            assert_eq!(status, 200);
            let said = waiting.await.unwrap().unwrap_err().to_string();
            assert!(said.contains(expect), "{error} came back as {said:?}");
        }
    }

    #[tokio::test]
    async fn a_cancel_ends_the_wait_promptly_and_so_does_the_deadline() {
        let auth = Authorization::begin().await.unwrap();
        let progress = Progress::silent("connect");
        let flag = progress.cancel_flag();
        let waiting =
            tokio::spawn(async move { auth.finish(&progress, Duration::from_secs(600)).await });
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let ended = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("a cancelled wait should end promptly")
            .unwrap();
        assert!(matches!(ended, Err(Error::Cancelled)), "{ended:?}");

        let auth = Authorization::begin().await.unwrap();
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            auth.finish(&Progress::silent("connect"), Duration::from_millis(150)),
        )
        .await
        .expect("a wait past its deadline should end promptly")
        .unwrap_err()
        .to_string();
        assert!(ended.contains("start over"), "{ended}");
    }

    #[tokio::test]
    async fn the_code_exchange_sends_the_verifier_and_a_secret_only_when_there_is_one() {
        let google = Fake::start(|_| {
            |_call: &Call| {
                Reply::json(
                    200,
                    serde_json::json!({
                        "access_token": "access-1",
                        "refresh_token": "refresh-1",
                        "expires_in": 3599,
                        "scope": SCOPE,
                        "token_type": "Bearer",
                    }),
                )
            }
        })
        .await;
        let client = plain();

        let granted = exchange(
            &client,
            &google.endpoints.token,
            "client-1",
            None,
            "code-7",
            "verifier-7",
            "http://127.0.0.1:4242",
        )
        .await
        .unwrap();
        assert_eq!(granted.access_token, "access-1");
        assert_eq!(granted.refresh_token.as_deref(), Some("refresh-1"));
        // Within a second or two of an hour from now.
        assert!((granted.expires_at - chrono::Utc::now().timestamp() - 3599).abs() <= 2);

        let form = google.to_path("/token")[0].form();
        assert_eq!(form.get("client_id").map(String::as_str), Some("client-1"));
        assert_eq!(form.get("code").map(String::as_str), Some("code-7"));
        assert_eq!(
            form.get("code_verifier").map(String::as_str),
            Some("verifier-7")
        );
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("authorization_code")
        );
        assert_eq!(
            form.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:4242")
        );
        assert_eq!(
            form.get("client_secret"),
            None,
            "a secret nobody configured was sent"
        );

        exchange(
            &client,
            &google.endpoints.token,
            "client-1",
            Some("not-really-secret"),
            "code-7",
            "verifier-7",
            "http://127.0.0.1:4242",
        )
        .await
        .unwrap();
        let form = google.to_path("/token")[1].form();
        assert_eq!(
            form.get("client_secret").map(String::as_str),
            Some("not-really-secret")
        );
    }

    /// "Note that refresh tokens are always returned for installed
    /// applications." A connection without one stops working inside the hour
    /// and looks like a bug, so it is refused with the thing that fixes it.
    #[tokio::test]
    async fn a_grant_with_no_refresh_token_is_refused_rather_than_stored() {
        let google = Fake::start(|_| {
            |_call: &Call| {
                Reply::json(
                    200,
                    serde_json::json!({ "access_token": "access-1", "expires_in": 3599 }),
                )
            }
        })
        .await;
        let said = exchange(
            &plain(),
            &google.endpoints.token,
            "client-1",
            None,
            "code-7",
            "verifier-7",
            "http://127.0.0.1:4242",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(said.contains("myaccount.google.com"), "{said}");
    }

    /// The two refusals whose cause is a setting the user can see, which is
    /// the only reason they are told apart from any other failure.
    #[tokio::test]
    async fn a_refused_exchange_names_the_setting_that_caused_it() {
        for (error, expect) in [
            ("redirect_uri_mismatch", "Desktop app"),
            ("invalid_client", "client secret"),
            ("invalid_request", "refused the sign-in"),
        ] {
            let google = Fake::start(move |_| {
                move |_call: &Call| {
                    Reply::json(
                        400,
                        serde_json::json!({ "error": error, "error_description": "no" }),
                    )
                }
            })
            .await;
            let said = exchange(
                &plain(),
                &google.endpoints.token,
                "client-1",
                None,
                "code-7",
                "verifier-7",
                "http://127.0.0.1:4242",
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(said.contains(expect), "{error} came back as {said:?}");
        }
    }
}

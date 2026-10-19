//! Sharing a pack through the user's own Google Drive.
//!
//! ## Why Drive at all
//!
//! A server is a machine that is already up and already has an address, so
//! serving its own pack costs nothing and [`crate::share::host`] does exactly
//! that. An instance lives on a laptop, and "here, try my pack" is
//! asynchronous: the person you sent it to installs it tomorrow, when the
//! laptop is shut. Drive holds the bytes so neither side has to be online at
//! the same time.
//!
//! [`crate::share`] still says a personal pack is a file and nothing more, and
//! that is still true — exporting a `.geodepack` and handing it over works
//! with none of this. This is the other half of the same choice:
//! `docs/sharing-and-hosting.md` recorded that any durable hosting means an
//! account per sharer, and then observed that asking somebody to provision a
//! bucket is a worse product than giving them the file. A Google account is
//! the one such account most people already have, with no bucket to provision.
//!
//! ## Why the user brings their own client id
//!
//! Geode ships no API keys. It already asks for the user's own Azure client id
//! for Minecraft sign-in and their own CurseForge key, for the same kind of
//! reason, and this follows that pattern rather than inventing a second one:
//! `google_client_id` in [`crate::config::Config`], seeded from
//! `GEODE_GOOGLE_CLIENT_ID` at build time by [`default_google_client_id`], the
//! way `client_id` is seeded by [`crate::auth::default_client_id`].
//!
//! A Google "Desktop app" client is a public client — Google's own words are
//! that desktop apps "cannot keep secrets confidential" — and its parameter
//! tables mark `client_secret` optional. So Geode asks only for the id. There
//! is a second, optional secret field purely as a contingency: if Google
//! answers `invalid_client`, the user pastes the string shown beside the id,
//! which by Google's own description is not confidential for this client type.
//!
//! ## What the scope reaches
//!
//! `https://www.googleapis.com/auth/drive.file` and nothing else. Google
//! classes it as non-sensitive and says to prefer it "whenever possible" since
//! it grants per-file access. It reaches only files Geode itself created:
//! Geode cannot see the rest of the user's Drive, and that is a fact about the
//! scope rather than a promise about this code. It is also why no verification
//! review is needed to publish the client.
//!
//! The visible cost of that narrowness is in [`api`]: a folder is found by the
//! id Geode stored, never by searching, and a folder Geode no longer has
//! access to is simply replaced.
//!
//! ## Why the loopback flow and not the device flow
//!
//! Google's limited-input-device page does list `drive.file`, so the device
//! flow [`crate::auth`] uses for Minecraft would be legal here. It is still
//! the wrong choice: it exists for devices with no browser, it makes the user
//! retype a code, and Google's guidance for "macOS, Linux, and Windows desktop
//! apps" is the loopback redirect. See [`oauth`].
//!
//! ## What a share actually does
//!
//! [`upload_pack`]: find or make the folder, upload the staged pack, check
//! Drive's own SHA-256 against Geode's, give that one file a public reader
//! permission, and then **fetch the public link from this machine with no
//! credentials at all** to find out what a stranger will get. Anyone with the
//! link can download the file; that is a property of the transport, and it is
//! said where the link is made.

pub mod api;
#[cfg(test)]
mod fake;
pub mod oauth;
pub mod tokens;

use std::path::PathBuf;

use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::net::Progress;
use crate::paths::Paths;
use crate::share::fetch::{self, PublicCheck};
use crate::share::packlink::{PackHash, PackLink, PackSource};

/// The only scope Geode ever asks for.
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";

/// What the folder is called when Geode makes it. The user may rename it; the
/// stored id is what Geode goes back to.
pub const FOLDER_NAME: &str = "Geode Packs";

/// A Google client id compiled in via `GEODE_GOOGLE_CLIENT_ID`, if there was
/// one. The same shape, and the same reasoning, as
/// [`crate::auth::default_client_id`].
pub fn default_google_client_id() -> Option<String> {
    option_env!("GEODE_GOOGLE_CLIENT_ID")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Every Google address this module talks to.
///
/// Injectable for one reason: there is no Google account here to test against
/// and there must never be one, so every test points these at a loopback fake.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub revoke: String,
    pub drive: String,
    pub upload: String,
    /// The URL the post-upload self-check fetches, when it should not be the
    /// one a recipient would derive for itself.
    ///
    /// `None` in production, where the check asks for exactly what
    /// [`PackSource::download_urls`] gives a recipient — so the two cannot
    /// drift. A test points it at its own listener, because the whole point of
    /// the check is one real unauthenticated request and there is no Google
    /// here to make it against.
    pub public: Option<String>,
}

impl Endpoints {
    pub fn google() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            drive: "https://www.googleapis.com/drive/v3".into(),
            upload: "https://www.googleapis.com/upload/drive/v3".into(),
            public: None,
        }
    }
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::google()
    }
}

/// The Drive connection as the frontend sees it. No token field, ever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveStatus {
    /// A client id is set, so connecting is possible at all.
    pub configured: bool,
    pub connected: bool,
    /// Google refused the connection for good — most often an OAuth client
    /// still in Testing, which expires every authorization after seven days.
    pub needs_reconnect: bool,
    pub account: Option<String>,
    pub folder_name: Option<String>,
    pub folder_url: Option<String>,
}

/// A connection that has been forgotten, and whether Google said it had let
/// go of it too.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Disconnected {
    pub status: DriveStatus,
    /// False when Google could not be reached, or refused: the grant is then
    /// still listed under the user's Third-party access, and only they can
    /// remove it.
    pub revoked: bool,
}

/// A pack that is now in Drive, and the message to hand out.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveShare {
    /// The `geode://pack` link.
    pub link: String,
    /// Drive's own `webViewLink` — the plain link for somebody with no Geode.
    /// Returned by Drive, never assembled here.
    pub file_url: String,
    pub size: u64,
    /// What this machine got when it fetched the public link itself.
    pub public_check: PublicCheck,
}

/// A pack exported and waiting to be uploaded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StagedPack {
    /// The staging id, which is what the upload command takes. Not a path: a
    /// command that took a path would let the webview ask Geode to upload any
    /// file on the disk.
    pub stage: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    /// What the export did with the instance's content, from the export
    /// itself — so the dialog cannot promise what the export did not do.
    pub report: crate::instance::modpack::ExportReport,
}

/// Where a connect is up to, for Settings.
pub async fn status(paths: &Paths, config: &Config) -> Result<DriveStatus> {
    let connection = tokens::Connection::load(paths).await?;
    let configured = client_id(config).is_some();
    Ok(match connection {
        None => DriveStatus {
            configured,
            connected: false,
            needs_reconnect: false,
            account: None,
            folder_name: None,
            folder_url: None,
        },
        Some(connection) => DriveStatus {
            configured,
            connected: !connection.needs_reconnect(),
            needs_reconnect: connection.needs_reconnect(),
            account: Some(connection.account).filter(|who| !who.is_empty()),
            folder_name: Some(connection.folder_name).filter(|name| !name.is_empty()),
            folder_url: Some(connection.folder_url).filter(|url| !url.is_empty()),
        },
    })
}

/// The client id to use: whatever the user saved, else whatever was compiled
/// in. The same layering as the CurseForge key, for the same reason.
pub fn client_id(config: &Config) -> Option<String> {
    config
        .google_client_id
        .as_ref()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .or_else(default_google_client_id)
}

pub fn client_secret(config: &Config) -> Option<String> {
    config
        .google_client_secret
        .as_ref()
        .map(|secret| secret.trim())
        .filter(|secret| !secret.is_empty())
        .map(str::to_string)
}

/// Open the loopback listener and say where to send the user's browser.
pub async fn begin(
    endpoints: &Endpoints,
    client_id: &str,
) -> Result<(oauth::Authorization, String)> {
    let authorization = oauth::Authorization::begin().await?;
    let url = authorization.url(&endpoints.auth, client_id)?;
    Ok((authorization, url))
}

/// Wait for the browser, take the tokens, and make the folder.
///
/// The folder is made now rather than at the first share so the user can see
/// straight away that Geode can actually write to their Drive — which is the
/// half of this that a client id alone does not prove.
pub async fn finish(
    client: &reqwest::Client,
    paths: &Paths,
    endpoints: &Endpoints,
    client_id: &str,
    client_secret: Option<&str>,
    authorization: oauth::Authorization,
    progress: &Progress,
) -> Result<DriveStatus> {
    let code = authorization
        .finish(progress, oauth::REDIRECT_DEADLINE)
        .await?;
    let granted = oauth::exchange(
        client,
        &endpoints.token,
        client_id,
        client_secret,
        &code,
        authorization.verifier(),
        authorization.redirect_uri(),
    )
    .await?;
    let refresh_token = granted
        .refresh_token
        .clone()
        .ok_or_else(|| Error::msg("Google did not return a refresh token"))?;

    // A reconnect should land back in the folder the user already has, so
    // whatever was known before is the starting point.
    let mut connection = tokens::Connection::load(paths).await?.unwrap_or_default();
    connection.refresh_token = refresh_token;
    connection.access_token = granted.access_token.clone();
    connection.expires_at = granted.expires_at;

    let bearer = tokens::Bearer::fixed(granted.access_token);
    let drive = api::Drive::new(client, endpoints, &bearer);
    connection.account = drive.about().await.unwrap_or_default();
    let folder = drive
        .folder(Some(&connection.folder_id), FOLDER_NAME)
        .await?;
    connection.folder_id = folder.id;
    connection.folder_name = folder.name;
    connection.folder_url = folder.url;
    connection.save(paths).await?;

    let config = Config {
        google_client_id: Some(client_id.to_string()),
        ..Config::default()
    };
    status(paths, &config).await
}

/// Forget the connection, and ask Google to forget it too.
///
/// The file goes whatever Google answers: a revoke that failed must not leave
/// the user unable to disconnect. But whether Google accepted it is reported
/// rather than swallowed — somebody who disconnects *because* they want the
/// grant gone has to be told when it is still there, or they are left
/// believing a thing this cannot promise.
pub async fn disconnect(
    client: &reqwest::Client,
    paths: &Paths,
    endpoints: &Endpoints,
    config: &Config,
) -> Result<Disconnected> {
    let mut revoked = true;
    if let Some(connection) = tokens::Connection::load(paths).await? {
        if !connection.refresh_token.is_empty() {
            revoked = oauth::revoke(client, &endpoints.revoke, &connection.refresh_token)
                .await
                .is_ok();
        }
    }
    tokens::Connection::forget(paths).await;
    Ok(Disconnected {
        status: status(paths, config).await?,
        revoked,
    })
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// Where a pack waits between being exported and being uploaded.
///
/// Its own directory, not [`crate::share::host`]'s `outgoing/`: that one holds
/// server packs being served right now, and the listener clears it whole when
/// it starts.
fn staged_dir(paths: &Paths) -> PathBuf {
    paths.share_dir().join("staged")
}

/// Clear the staging directory and name the file the next export writes.
///
/// One pack is staged at a time. A previous staging that was never uploaded —
/// the user closed the dialog — is just a file taking up room.
pub async fn stage(paths: &Paths) -> Result<(String, PathBuf)> {
    let dir = staged_dir(paths);
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir).await?;
    let stage = uuid::Uuid::new_v4().to_string();
    Ok((stage.clone(), dir.join(format!("{stage}.geodepack"))))
}

/// The staged file one id names.
///
/// The id has to be a uuid, which is what stops a path arriving from the
/// webview: a command that took a path would let anything in the window ask
/// Geode to upload any file on the disk.
pub fn staged_path(paths: &Paths, stage: &str) -> Result<PathBuf> {
    let stage = uuid::Uuid::parse_str(stage)
        .map_err(|_| Error::msg("that is not a pack Geode staged for upload"))?;
    Ok(staged_dir(paths).join(format!("{stage}.geodepack")))
}

/// Remove anything a staging left behind. Called at startup, where the join
/// directory is cleared for the same reason.
pub async fn clear_staged(paths: &Paths) {
    let _ = tokio::fs::remove_dir_all(staged_dir(paths)).await;
}

// ---------------------------------------------------------------------------
// The share itself
// ---------------------------------------------------------------------------

/// Upload a staged pack, make it readable by anyone with the link, and come
/// back with the message to hand out.
///
/// Sharing the same instance again uploads a new file and mints a new link.
/// The old link keeps working until the user deletes that file in Drive, which
/// is deliberate: replacing the bytes under a file id would break every link
/// already handed out, because a link carries the pack's hash. It is the same
/// reasoning [`crate::share::pack`] applies to a pack's revision.
pub async fn upload_pack(
    client: &reqwest::Client,
    paths: &Paths,
    endpoints: &Endpoints,
    config: &Config,
    stage: &str,
    name: &str,
    progress: &Progress,
) -> Result<DriveShare> {
    let path = staged_path(paths, stage)?;
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Err(Error::msg(
            "that pack is no longer staged — prepare it again",
        ));
    }
    let client_id = client_id(config).ok_or_else(|| {
        Error::msg("Google Drive needs your own Google client ID. Add one in Settings.")
    })?;
    let client_secret = client_secret(config);

    let ours = crate::net::sha256_file(&path).await?;
    let hash = PackHash::from_hex(&ours)
        .ok_or_else(|| Error::msg("the staged pack could not be hashed"))?;

    let token = tokens::access_token(
        client,
        paths,
        endpoints,
        &client_id,
        client_secret.as_deref(),
    )
    .await?;
    let bearer = tokens::Bearer::renewable(
        token,
        client.clone(),
        paths.clone(),
        endpoints.clone(),
        client_id,
        client_secret,
    );
    let drive = api::Drive::new(client, endpoints, &bearer);

    let mut connection = tokens::Connection::load(paths)
        .await?
        .ok_or_else(|| Error::msg("no Google account is connected — connect one in Settings"))?;
    let folder = drive
        .folder(Some(&connection.folder_id), FOLDER_NAME)
        .await?;
    if folder.id != connection.folder_id
        || folder.name != connection.folder_name
        || folder.url != connection.folder_url
    {
        connection.folder_id = folder.id.clone();
        connection.folder_name = folder.name.clone();
        connection.folder_url = folder.url.clone();
        connection.save(paths).await?;
    }

    let uploaded = drive
        .upload(&folder.id, &file_name(name), &path, progress)
        .await?;

    // Drive's own checksum, when it gave one. A disagreement means the bytes
    // in Drive are not the bytes Geode hashed, and the link carries Geode's
    // hash — so the file is worse than useless and goes.
    if let Some(theirs) = &uploaded.sha256 {
        if !theirs.eq_ignore_ascii_case(&ours) {
            let _ = drive.delete(&uploaded.id).await;
            return Err(Error::msg(
                "Google Drive stored different bytes than Geode sent — nothing was shared",
            ));
        }
    }

    // A pack in somebody's Drive that nobody can read is worse than no pack.
    if let Err(e) = drive.share_anyone(&uploaded.id).await {
        let _ = drive.delete(&uploaded.id).await;
        return Err(e);
    }

    let link = PackLink {
        source: PackSource::Drive {
            file_id: uploaded.id.clone(),
        },
        hash,
    };
    // The sharer finds out before the recipient does: one more request, from
    // this machine, with no credentials, against the very URL a recipient
    // will use.
    let candidate = endpoints
        .public
        .clone()
        .or_else(|| link.source.download_urls().into_iter().next());
    let public_check = match candidate {
        Some(url) => fetch::probe(&url).await,
        None => PublicCheck::Unknown,
    };
    let _ = tokio::fs::remove_file(&path).await;

    Ok(DriveShare {
        link: link.to_url(),
        file_url: uploaded.web_view_link,
        size: uploaded.size,
        public_check,
    })
}

/// What the pack is called in Drive.
///
/// An instance name is whatever the user typed, and it becomes a file name in
/// somebody else's Drive listing, so separators and control characters come
/// out and the length is bounded.
fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
        .take(120)
        .collect();
    let base = cleaned.trim();
    if base.is_empty() {
        "pack.geodepack".to_string()
    } else {
        format!("{base}.geodepack")
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Call, Fake, Reply};
    use super::*;
    use crate::paths::Scratch;

    fn configured() -> Config {
        Config {
            google_client_id: Some("client-1".into()),
            ..Config::default()
        }
    }

    /// Nothing about a connection may cross the IPC boundary but this, and a
    /// token must never be in it. The same rule, and the same kind of test, as
    /// `redacted_never_leaks_the_key`.
    #[tokio::test]
    async fn a_status_carries_no_token() {
        let scratch = Scratch::new("drive-status");
        let paths = &scratch.paths;
        tokens::Connection {
            refresh_token: "refresh-secret".into(),
            access_token: "access-secret".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            account: "someone@example.com".into(),
            folder_id: "folder-1".into(),
            folder_name: FOLDER_NAME.into(),
            folder_url: "https://drive.google.com/drive/folders/folder-1".into(),
        }
        .save(paths)
        .await
        .unwrap();

        let connected = status(paths, &configured()).await.unwrap();
        let text = serde_json::to_string(&connected).unwrap();
        assert!(
            !text.contains("refresh-secret"),
            "a refresh token leaked: {text}"
        );
        assert!(
            !text.contains("access-secret"),
            "an access token leaked: {text}"
        );
        assert_eq!(
            connected,
            DriveStatus {
                configured: true,
                connected: true,
                needs_reconnect: false,
                account: Some("someone@example.com".into()),
                folder_name: Some(FOLDER_NAME.into()),
                folder_url: Some("https://drive.google.com/drive/folders/folder-1".into()),
            }
        );

        // A client id with no connection, and a connection Google refused for
        // good, are different problems with different answers.
        tokens::Connection::forget(paths).await;
        let fresh = status(paths, &configured()).await.unwrap();
        assert!(fresh.configured && !fresh.connected && !fresh.needs_reconnect);
        let nothing = status(paths, &Config::default()).await.unwrap();
        assert!(!nothing.configured);

        tokens::Connection {
            folder_id: "folder-1".into(),
            ..Default::default()
        }
        .save(paths)
        .await
        .unwrap();
        let stale = status(paths, &configured()).await.unwrap();
        assert!(stale.needs_reconnect && !stale.connected);
    }

    /// A whole connect, from the code the browser brings back to the folder
    /// being written down.
    #[tokio::test]
    async fn connecting_takes_the_tokens_asks_who_it_is_and_makes_the_folder() {
        let scratch = Scratch::new("drive-connect");
        let paths = &scratch.paths;
        let google = Fake::start(|_| {
            |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/token") => Reply::json(
                    200,
                    serde_json::json!({
                        "access_token": "access-1",
                        "refresh_token": "refresh-1",
                        "expires_in": 3599,
                        "scope": SCOPE,
                        "token_type": "Bearer",
                    }),
                ),
                ("GET", "/drive/v3/about") => Reply::json(
                    200,
                    serde_json::json!({ "user": { "emailAddress": "someone@example.com" } }),
                ),
                ("POST", "/drive/v3/files") => Reply::json(
                    200,
                    serde_json::json!({
                        "id": "folder-9",
                        "name": FOLDER_NAME,
                        "webViewLink": "https://drive.google.com/drive/folders/folder-9",
                    }),
                ),
                _ => Reply::status(500),
            }
        })
        .await;

        let client = crate::net::client();
        let (authorization, url) = begin(&google.endpoints, "client-1").await.unwrap();
        assert!(url.starts_with(&google.endpoints.auth), "{url}");
        let redirect = authorization.redirect_uri().to_string();
        let state = url::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .unwrap();

        let endpoints = google.endpoints.clone();
        let paths_for_task = paths.clone();
        let connecting = tokio::spawn(async move {
            finish(
                &client,
                &paths_for_task,
                &endpoints,
                "client-1",
                None,
                authorization,
                &Progress::silent("drive:connect"),
            )
            .await
        });
        // The browser comes back.
        let visited = crate::net::client()
            .get(format!("{redirect}/?state={state}&code=code-7"))
            .send()
            .await
            .unwrap();
        assert_eq!(visited.status(), 200);

        let status = connecting.await.unwrap().unwrap();
        assert_eq!(status.account.as_deref(), Some("someone@example.com"));
        assert_eq!(status.folder_name.as_deref(), Some(FOLDER_NAME));
        assert!(status.connected && !status.needs_reconnect);

        let saved = tokens::Connection::load(paths).await.unwrap().unwrap();
        assert_eq!(saved.refresh_token, "refresh-1");
        assert_eq!(saved.folder_id, "folder-9");

        // The exchange sent the verifier the challenge was made from, and no
        // secret nobody configured.
        let form = google.to_path("/token")[0].form();
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("authorization_code")
        );
        assert_eq!(form.get("code").map(String::as_str), Some("code-7"));
        assert_eq!(
            form.get("redirect_uri").map(String::as_str),
            Some(redirect.as_str())
        );
        assert!(form.contains_key("code_verifier"));
        assert_eq!(form.get("client_secret"), None);
        // `fields` is required on about.get, so it is always sent.
        assert!(google.to_path("/drive/v3/about")[0]
            .param("fields")
            .is_some_and(|fields| fields.contains("emailAddress")));
    }

    /// The file goes either way — a revoke that failed must not leave the user
    /// unable to disconnect — but what Google answered is reported, because
    /// somebody disconnecting to get the grant gone must not be told it is
    /// gone when it is not.
    #[tokio::test]
    async fn disconnecting_revokes_the_token_and_forgets_the_file_either_way() {
        for revoke_status in [200u16, 400] {
            let scratch = Scratch::new("drive-disconnect");
            let paths = &scratch.paths;
            let google = Fake::start(move |_| {
                move |call: &Call| match call.path.as_str() {
                    "/revoke" => Reply::status(revoke_status),
                    _ => Reply::status(500),
                }
            })
            .await;
            tokens::Connection {
                refresh_token: "refresh-1".into(),
                ..Default::default()
            }
            .save(paths)
            .await
            .unwrap();

            let gone = disconnect(
                &crate::net::client(),
                paths,
                &google.endpoints,
                &configured(),
            )
            .await
            .unwrap();
            assert!(!gone.status.connected && !gone.status.needs_reconnect);
            assert_eq!(
                gone.revoked,
                revoke_status == 200,
                "a revoke answering {revoke_status} was reported as {}",
                gone.revoked
            );
            assert!(
                tokens::Connection::load(paths).await.unwrap().is_none(),
                "a revoke answering {revoke_status} left the user still connected"
            );
            let form = google.to_path("/revoke")[0].form();
            assert_eq!(form.get("token").map(String::as_str), Some("refresh-1"));
        }
    }

    /// A pack staged for upload is named by an id, and the id has to be one
    /// Geode wrote — the alternative is a command that uploads any file on the
    /// disk the webview asks for.
    #[tokio::test]
    async fn only_a_staged_uuid_names_a_file_to_upload() {
        let scratch = Scratch::new("drive-stage");
        let paths = &scratch.paths;
        let (id, dest) = stage(paths).await.unwrap();
        assert_eq!(staged_path(paths, &id).unwrap(), dest);
        assert!(dest.starts_with(paths.share_dir().join("staged")));

        for junk in ["../../../etc/passwd", "/etc/passwd", "not-a-uuid", "", ".."] {
            assert!(
                staged_path(paths, junk).is_err(),
                "{junk:?} was taken for a staged pack"
            );
        }

        // Staging again clears what the last one left.
        tokio::fs::write(&dest, b"PK\x03\x04").await.unwrap();
        let (_, second) = stage(paths).await.unwrap();
        assert!(!dest.exists(), "an abandoned staging was left behind");
        assert!(second.parent().unwrap().exists());

        clear_staged(paths).await;
        assert!(!paths.share_dir().join("staged").exists());
    }

    /// An instance name becomes a file name in somebody else's Drive.
    #[test]
    fn a_pack_is_named_after_the_instance_without_its_separators() {
        assert_eq!(
            file_name("Survival with Create"),
            "Survival with Create.geodepack"
        );
        assert_eq!(file_name("a/b\\c"), "abc.geodepack");
        assert_eq!(file_name("  spaced  "), "spaced.geodepack");
        assert_eq!(file_name(""), "pack.geodepack");
        assert_eq!(file_name("   "), "pack.geodepack");
        assert_eq!(file_name("line\nbreak"), "linebreak.geodepack");
        assert!(file_name(&"x".repeat(400)).len() <= 120 + ".geodepack".len());
    }

    /// A whole share against a fake: the folder, the upload, the permission,
    /// the link, and the self-check that fetches the public URL with nothing
    /// of the user in it.
    #[tokio::test]
    async fn sharing_uploads_the_pack_makes_it_readable_and_hands_back_a_link() {
        let scratch = Scratch::new("drive-share");
        let paths = &scratch.paths;
        let (stage, bytes) = staged(paths).await;
        let ours = crate::net::sha256_file(&staged_path(paths, &stage).unwrap())
            .await
            .unwrap();
        let google = drive_fake(Some(ours.clone()), 200, bytes).await;
        connected(paths).await;

        let share = upload_pack(
            &crate::net::client(),
            paths,
            &google.endpoints,
            &configured(),
            &stage,
            "Survival with Create",
            &Progress::silent("drive-upload:one"),
        )
        .await
        .unwrap();

        // The link is a pack link, naming the file Drive gave and the hash
        // Geode computed over the very bytes it sent.
        let parsed = PackLink::parse(&share.link).unwrap();
        assert_eq!(
            parsed.source,
            PackSource::Drive {
                file_id: "file-1".into()
            }
        );
        assert_eq!(parsed.hash.to_hex(), ours);
        // The plain link is Drive's own webViewLink, not a URL Geode built.
        assert_eq!(
            share.file_url,
            "https://drive.google.com/file/d/file-1/view"
        );
        assert_eq!(share.public_check, PublicCheck::Served);

        // Named after the instance, inside the folder, and shared as one file.
        let start = &google.to_path("/upload/drive/v3/files")[0];
        assert_eq!(
            start.json(),
            serde_json::json!({
                "name": "Survival with Create.geodepack",
                "parents": ["folder-1"],
            })
        );
        assert_eq!(
            google.to_path("/drive/v3/files/file-1/permissions").len(),
            1
        );
        // And the staged copy is gone: it was only ever waiting to go up.
        assert!(!staged_path(paths, &stage).unwrap().exists());
        // The self-check carried no credentials — that is the point of it.
        let probe = &google.to_path("/public")[0];
        assert_eq!(probe.header("authorization"), None);
    }

    /// Drive's own checksum against Geode's. A disagreement means the link
    /// would name bytes that are not there, so the file goes.
    #[tokio::test]
    async fn a_checksum_drive_disagrees_with_deletes_the_file_and_shares_nothing() {
        let scratch = Scratch::new("drive-checksum");
        let paths = &scratch.paths;
        let (stage, bytes) = staged(paths).await;
        let google = drive_fake(Some("00".repeat(32)), 200, bytes).await;
        connected(paths).await;

        let said = upload_pack(
            &crate::net::client(),
            paths,
            &google.endpoints,
            &configured(),
            &stage,
            "Survival",
            &Progress::silent("drive-upload:one"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(said.contains("different bytes"), "{said}");
        assert_eq!(
            google
                .calls()
                .iter()
                .filter(|call| call.method == "DELETE")
                .count(),
            1,
            "a pack Drive stored wrong was left in the user's Drive"
        );
        assert!(
            google
                .to_path("/drive/v3/files/file-1/permissions")
                .is_empty(),
            "bytes nobody can verify were published anyway"
        );
    }

    /// Documented as "if available", so an absent one costs a cross-check and
    /// nothing else.
    #[tokio::test]
    async fn a_checksum_drive_did_not_give_is_skipped_rather_than_failed() {
        let scratch = Scratch::new("drive-no-checksum");
        let paths = &scratch.paths;
        let (stage, bytes) = staged(paths).await;
        let google = drive_fake(None, 200, bytes).await;
        connected(paths).await;

        let share = upload_pack(
            &crate::net::client(),
            paths,
            &google.endpoints,
            &configured(),
            &stage,
            "Survival",
            &Progress::silent("drive-upload:one"),
        )
        .await
        .unwrap();
        assert!(share.link.starts_with("geode://pack?drive=file-1"));
    }

    /// A pack in somebody's Drive that nobody can read is worse than no pack.
    #[tokio::test]
    async fn a_permission_that_could_not_be_set_deletes_the_file() {
        let scratch = Scratch::new("drive-permission");
        let paths = &scratch.paths;
        let (stage, bytes) = staged(paths).await;
        let google = drive_fake(None, 403, bytes).await;
        connected(paths).await;

        let failed = upload_pack(
            &crate::net::client(),
            paths,
            &google.endpoints,
            &configured(),
            &stage,
            "Survival",
            &Progress::silent("drive-upload:one"),
        )
        .await;
        assert!(failed.is_err(), "a pack nobody can read was called a share");
        assert_eq!(
            google
                .calls()
                .iter()
                .filter(|call| call.method == "DELETE")
                .count(),
            1
        );
    }

    /// A pack staged for upload, and the bytes a fake should serve back.
    async fn staged(paths: &Paths) -> (String, Vec<u8>) {
        let (stage, dest) = stage(paths).await.unwrap();
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend((0..4000u32).map(|i| (i % 251) as u8));
        tokio::fs::write(&dest, &bytes).await.unwrap();
        (stage, bytes)
    }

    async fn connected(paths: &Paths) {
        tokens::Connection {
            refresh_token: "refresh-1".into(),
            access_token: "access-1".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            account: "someone@example.com".into(),
            folder_id: "folder-1".into(),
            folder_name: FOLDER_NAME.into(),
            folder_url: "https://drive.google.com/drive/folders/folder-1".into(),
        }
        .save(paths)
        .await
        .unwrap();
    }

    /// A Drive that accepts one upload and then answers the public link with
    /// the pack itself.
    async fn drive_fake(sha256: Option<String>, permission: u16, served: Vec<u8>) -> Fake {
        Fake::start(move |base| {
            move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("GET", "/drive/v3/files/folder-1") => Reply::json(
                    200,
                    serde_json::json!({
                        "id": "folder-1",
                        "name": FOLDER_NAME,
                        "mimeType": "application/vnd.google-apps.folder",
                        "trashed": false,
                        "webViewLink": "https://drive.google.com/drive/folders/folder-1",
                        "capabilities": { "canAddChildren": true },
                    }),
                ),
                ("POST", "/upload/drive/v3/files") => {
                    Reply::status(200).with("location", format!("{base}/upload/session/1"))
                }
                ("PUT", "/upload/session/1") => {
                    let mut resource = serde_json::json!({
                        "id": "file-1",
                        "name": "pack.geodepack",
                        "size": served.len().to_string(),
                        "webViewLink": "https://drive.google.com/file/d/file-1/view",
                        "webContentLink": "https://drive.google.com/uc?id=file-1&export=download",
                    });
                    if let Some(sha256) = &sha256 {
                        resource["sha256Checksum"] = serde_json::json!(sha256);
                    }
                    Reply::json(200, resource)
                }
                ("POST", "/drive/v3/files/file-1/permissions") => {
                    if permission == 200 {
                        Reply::json(200, serde_json::json!({ "id": "perm-1" }))
                    } else {
                        Reply::error(permission, "insufficientFilePermissions", "No permission.")
                    }
                }
                ("DELETE", "/drive/v3/files/file-1") => Reply::status(204),
                ("GET", "/public") => {
                    Reply::bytes(200, served.clone()).with("content-type", "application/zip")
                }
                _ => Reply::status(500),
            }
        })
        .await
    }
}

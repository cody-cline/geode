//! Where a Google Drive connection is kept, and how it stays usable.
//!
//! `google.json` beside `accounts.json`, written with owner-only permissions
//! through the same helper. That is the same posture as the official launcher
//! and Prism, and it carries the same honest caveat: it is not protection
//! against local malware, because anything running as this user could equally
//! read a keyring-unlocked secret.
//!
//! Nothing in this module logs a token. The only thing that crosses the IPC
//! boundary is [`super::DriveStatus`], which has no token field at all.
//!
//! A refresh token can die several documented ways — the user revoked access,
//! six months went by unused, the hundredth token issued to this client pushed
//! the oldest out, a session-control policy expired it, or the OAuth client is
//! still in **Testing** and Google expires every authorization after seven
//! days. Every one of them has the same answer, so every one of them has to
//! read as "reconnect your Google account", never as a crash.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

use super::oauth::{self, RefreshError};
use super::Endpoints;

/// Refresh this long before the access token expires, so one cannot lapse
/// between the check and the request it was fetched for. The same margin, for
/// the same reason, as [`crate::auth::store::Account::token_expired`].
const MARGIN: i64 = 300;

/// The sentence a dead connection always comes back as.
pub fn reconnect_error() -> Error {
    Error::msg(
        "your Google Drive connection has expired — reconnect your Google account in Settings",
    )
}

/// What `google.json` holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Connection {
    /// Empty once Google has refused it for good, which is what
    /// [`Connection::needs_reconnect`] reads.
    pub refresh_token: String,
    pub access_token: String,
    pub expires_at: i64,
    /// Who Google said this is, for the "Connected as …" line. May be empty.
    pub account: String,
    pub folder_id: String,
    pub folder_name: String,
    pub folder_url: String,
}

impl Connection {
    pub async fn load(paths: &Paths) -> Result<Option<Self>> {
        match tokio::fs::read(paths.google_file()).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| Error::json("google.json", e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn save(&self, paths: &Paths) -> Result<()> {
        tokio::fs::create_dir_all(&paths.root).await?;
        crate::auth::store::write_private(&paths.google_file(), &serde_json::to_vec_pretty(self)?)
            .await
    }

    /// Remove the file, however it goes. Disconnecting must always work.
    pub async fn forget(paths: &Paths) {
        let _ = tokio::fs::remove_file(paths.google_file()).await;
    }

    /// A connection Google will not honour again: kept only so Settings can
    /// say "reconnect" rather than "not configured", which are different
    /// problems with different answers.
    pub fn needs_reconnect(&self) -> bool {
        self.refresh_token.is_empty()
    }

    fn access_still_good(&self) -> bool {
        !self.access_token.is_empty() && self.expires_at - MARGIN > chrono::Utc::now().timestamp()
    }
}

/// A bearer token that can be asked for again.
///
/// A pack carrying jars is routinely hundreds of megabytes, and a few hundred
/// megabytes up a home connection can take longer than an access token lives.
/// So a request that comes back 401 part way through an upload asks for a new
/// token rather than throwing the whole upload away.
pub struct Bearer {
    current: tokio::sync::Mutex<String>,
    renew: Option<Renewal>,
}

/// How to get another access token. Owns what it needs, so it can be held for
/// as long as an upload runs.
type Renewal = Box<dyn Fn() -> futures::future::BoxFuture<'static, Result<String>> + Send + Sync>;

impl Bearer {
    /// A token and no way to get another. What a test uses.
    pub fn fixed(token: impl Into<String>) -> Self {
        Self {
            current: tokio::sync::Mutex::new(token.into()),
            renew: None,
        }
    }

    /// The real thing: a token now, and the refresh flow to get the next one.
    pub fn renewable(
        token: String,
        client: reqwest::Client,
        paths: Paths,
        endpoints: Endpoints,
        client_id: String,
        client_secret: Option<String>,
    ) -> Self {
        let renew: Renewal = Box::new(move || {
            let (client, paths, endpoints, client_id, client_secret) = (
                client.clone(),
                paths.clone(),
                endpoints.clone(),
                client_id.clone(),
                client_secret.clone(),
            );
            Box::pin(async move {
                force_refresh(
                    &client,
                    &paths,
                    &endpoints,
                    &client_id,
                    client_secret.as_deref(),
                )
                .await
            })
        });
        Self {
            current: tokio::sync::Mutex::new(token),
            renew: Some(renew),
        }
    }

    pub async fn get(&self) -> String {
        self.current.lock().await.clone()
    }

    /// Ask for a fresh token. `false` when there is nothing to ask.
    pub async fn renew(&self) -> Result<bool> {
        let Some(renew) = &self.renew else {
            return Ok(false);
        };
        let fresh = renew().await?;
        *self.current.lock().await = fresh;
        Ok(true)
    }
}

/// A usable access token, refreshing first if the one on file is spent.
pub async fn access_token(
    client: &reqwest::Client,
    paths: &Paths,
    endpoints: &Endpoints,
    client_id: &str,
    client_secret: Option<&str>,
) -> Result<String> {
    let connection = Connection::load(paths)
        .await?
        .ok_or_else(|| Error::msg("no Google account is connected — connect one in Settings"))?;
    if connection.needs_reconnect() {
        return Err(reconnect_error());
    }
    if connection.access_still_good() {
        return Ok(connection.access_token);
    }
    exchange_refresh(
        client,
        paths,
        endpoints,
        client_id,
        client_secret,
        connection,
    )
    .await
}

/// Refresh whatever is on file, whether or not it looked spent. What a 401
/// mid-upload asks for.
async fn force_refresh(
    client: &reqwest::Client,
    paths: &Paths,
    endpoints: &Endpoints,
    client_id: &str,
    client_secret: Option<&str>,
) -> Result<String> {
    let connection = Connection::load(paths).await?.ok_or_else(reconnect_error)?;
    if connection.needs_reconnect() {
        return Err(reconnect_error());
    }
    exchange_refresh(
        client,
        paths,
        endpoints,
        client_id,
        client_secret,
        connection,
    )
    .await
}

async fn exchange_refresh(
    client: &reqwest::Client,
    paths: &Paths,
    endpoints: &Endpoints,
    client_id: &str,
    client_secret: Option<&str>,
    mut connection: Connection,
) -> Result<String> {
    let granted = match oauth::refresh(
        client,
        &endpoints.token,
        client_id,
        client_secret,
        &connection.refresh_token,
    )
    .await
    {
        Ok(granted) => granted,
        // Permanent. Keeping the refresh token would mean retrying something
        // that can only fail, so it goes — and what is left says "reconnect"
        // rather than "not configured".
        Err(RefreshError::Expired) => {
            connection.refresh_token.clear();
            connection.access_token.clear();
            connection.expires_at = 0;
            connection.save(paths).await?;
            return Err(reconnect_error());
        }
        // A 500, a 503, a dropped connection. The refresh token is still
        // good, so nothing is thrown away.
        Err(RefreshError::Transient(e)) => return Err(e),
    };

    connection.access_token = granted.access_token.clone();
    connection.expires_at = granted.expires_at;
    // Google may hand out a new refresh token; when it does not, the old one
    // stays valid and must not be overwritten with nothing.
    if let Some(fresh) = granted.refresh_token {
        connection.refresh_token = fresh;
    }
    connection.save(paths).await?;
    Ok(granted.access_token)
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Fake, Reply};
    use super::*;
    use crate::paths::Scratch;

    fn stored(expires_in: i64) -> Connection {
        Connection {
            refresh_token: "refresh-1".into(),
            access_token: "access-1".into(),
            expires_at: chrono::Utc::now().timestamp() + expires_in,
            account: "someone@example.com".into(),
            folder_id: "folder-1".into(),
            folder_name: "Geode Packs".into(),
            folder_url: "https://drive.google.com/drive/folders/folder-1".into(),
        }
    }

    /// A Google that grants one fresh access token and counts how often it is
    /// asked.
    async fn granting() -> Fake {
        Fake::start(|_| {
            |_call: &super::super::fake::Call| {
                Reply::json(
                    200,
                    serde_json::json!({ "access_token": "access-2", "expires_in": 3599 }),
                )
            }
        })
        .await
    }

    #[tokio::test]
    async fn the_file_round_trips_and_only_its_owner_can_read_it() {
        let scratch = Scratch::new("drive-tokens");
        let paths = &scratch.paths;
        let connection = stored(3600);
        connection.save(paths).await.unwrap();

        let read = Connection::load(paths).await.unwrap().unwrap();
        assert_eq!(read.refresh_token, "refresh-1");
        assert_eq!(read.folder_id, "folder-1");
        assert_eq!(read.folder_url, connection.folder_url);

        // The keys are the camelCase the frontend and a human both read.
        let text = tokio::fs::read_to_string(paths.google_file())
            .await
            .unwrap();
        assert!(text.contains("\"refreshToken\""), "{text}");
        assert!(text.contains("\"folderId\""), "{text}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = tokio::fs::metadata(paths.google_file())
                .await
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "google.json is readable by others");
        }

        Connection::forget(paths).await;
        assert!(Connection::load(paths).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_token_inside_its_margin_is_reused_and_one_past_it_is_refreshed_once() {
        let scratch = Scratch::new("drive-margin");
        let paths = &scratch.paths;
        let google = granting().await;
        let client = crate::net::client();

        // Comfortably inside the five-minute margin.
        stored(3600).save(paths).await.unwrap();
        let token = access_token(&client, paths, &google.endpoints, "id", None)
            .await
            .unwrap();
        assert_eq!(token, "access-1");
        assert!(
            google.calls().is_empty(),
            "a good token was refreshed anyway"
        );

        // Not yet expired, but inside the five-minute margin — the case the
        // margin exists for. Written out rather than taken from MARGIN, so a
        // margin of nothing moves the code and not the test with it.
        stored(290).save(paths).await.unwrap();
        let token = access_token(&client, paths, &google.endpoints, "id", None)
            .await
            .unwrap();
        assert_eq!(token, "access-2");
        assert_eq!(
            google.to_path("/token").len(),
            1,
            "refreshed more than once"
        );

        // And the new one was written down, so the next call does not refresh.
        let saved = Connection::load(paths).await.unwrap().unwrap();
        assert_eq!(saved.access_token, "access-2");
        assert_eq!(
            saved.refresh_token, "refresh-1",
            "the refresh token was replaced with nothing"
        );
        let token = access_token(&client, paths, &google.endpoints, "id", None)
            .await
            .unwrap();
        assert_eq!(token, "access-2");
        assert_eq!(google.to_path("/token").len(), 1);
    }

    #[tokio::test]
    async fn the_refresh_sends_the_client_id_and_a_secret_only_when_there_is_one() {
        let scratch = Scratch::new("drive-refresh-form");
        let paths = &scratch.paths;
        let google = granting().await;
        let client = crate::net::client();

        stored(0).save(paths).await.unwrap();
        access_token(&client, paths, &google.endpoints, "id-1", None)
            .await
            .unwrap();
        let form = google.to_path("/token")[0].form();
        assert_eq!(form.get("client_id").map(String::as_str), Some("id-1"));
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("refresh_token")
        );
        assert_eq!(
            form.get("refresh_token").map(String::as_str),
            Some("refresh-1")
        );
        assert_eq!(
            form.get("client_secret"),
            None,
            "a secret nobody configured was sent"
        );

        stored(0).save(paths).await.unwrap();
        access_token(&client, paths, &google.endpoints, "id-1", Some("shh"))
            .await
            .unwrap();
        let form = google.to_path("/token")[1].form();
        assert_eq!(form.get("client_secret").map(String::as_str), Some("shh"));
    }

    /// The difference that matters: one of these is the user's to fix and the
    /// other will fix itself.
    #[tokio::test]
    async fn invalid_grant_asks_for_a_reconnect_and_a_503_keeps_the_connection() {
        let scratch = Scratch::new("drive-refresh-fail");
        let paths = &scratch.paths;
        let client = crate::net::client();

        let google = Fake::start(|_| {
            |_call: &super::super::fake::Call| {
                Reply::json(400, serde_json::json!({ "error": "invalid_grant" }))
            }
        })
        .await;
        stored(0).save(paths).await.unwrap();
        let said = access_token(&client, paths, &google.endpoints, "id", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(said.contains("reconnect your Google account"), "{said}");
        let left = Connection::load(paths).await.unwrap().unwrap();
        assert!(left.needs_reconnect(), "a dead refresh token was kept");
        assert!(left.access_token.is_empty());
        // The folder is still known, so reconnecting lands back in the same one.
        assert_eq!(left.folder_id, "folder-1");

        let google = Fake::start(|_| |_call: &super::super::fake::Call| Reply::status(503)).await;
        stored(0).save(paths).await.unwrap();
        let said = access_token(&client, paths, &google.endpoints, "id", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            !said.contains("reconnect"),
            "a server failure read as a dead connection: {said}"
        );
        let left = Connection::load(paths).await.unwrap().unwrap();
        assert_eq!(
            left.refresh_token, "refresh-1",
            "a good refresh token was thrown away"
        );
    }

    #[tokio::test]
    async fn a_bearer_with_nothing_to_ask_says_so_and_a_renewable_one_asks() {
        let scratch = Scratch::new("drive-bearer");
        let paths = &scratch.paths;
        let google = granting().await;

        let fixed = Bearer::fixed("access-1");
        assert_eq!(fixed.get().await, "access-1");
        assert!(
            !fixed.renew().await.unwrap(),
            "a fixed bearer claimed to renew"
        );

        stored(3600).save(paths).await.unwrap();
        let renewable = Bearer::renewable(
            "access-1".into(),
            crate::net::client(),
            paths.clone(),
            google.endpoints.clone(),
            "id".into(),
            None,
        );
        assert!(renewable.renew().await.unwrap());
        // Refreshed although the token on file still looked good: a 401 is
        // Google saying otherwise.
        assert_eq!(renewable.get().await, "access-2");
    }
}

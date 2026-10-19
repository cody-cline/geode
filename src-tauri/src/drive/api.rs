//! The Drive calls a share is made of, and nothing more.
//!
//! Five things happen: find or make the folder, upload the pack, give that one
//! file a public reader permission, delete a file when something went wrong,
//! and ask once who is connected. Every call carries
//! `Authorization: Bearer <access token>` and goes through
//! [`super::Endpoints`], so every test in this module runs against a loopback
//! fake rather than Google.
//!
//! Three decisions worth knowing before changing anything here:
//!
//! **The folder is found by its stored id, never by searching.** Drive's
//! search syntax would express it, but nothing in Google's documentation says
//! that `files.list` under `drive.file` is limited to the app's own files — so
//! a design that searched would rest on undocumented behaviour. Storing the id
//! needs no such assumption and survives the user renaming or moving the
//! folder.
//!
//! **The folder itself is never given a permission.** Sharing the folder would
//! publish every pack in it, including the ones not shared yet. One file, one
//! permission.
//!
//! **Every upload is resumable, even a three-megabyte one.** Google's guide
//! puts simple and multipart uploads at "5 MB or less" and says resumable "is
//! a good choice for most applications because it also works for smaller
//! files at the cost of one additional HTTP request". A pack that embeds jars
//! is routinely hundreds of megabytes, and one code path is cheaper than two.
//! A session URI expires a week after it is created, so nothing caches one.

use std::path::Path;
use std::time::Duration;

use hyper::body::Bytes;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::net::Progress;

use super::tokens::{reconnect_error, Bearer};
use super::Endpoints;

/// What Drive calls a folder.
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// 8 MiB: 32 × 256 KiB. Google requires every chunk but the last to be a
/// multiple of 256 KiB and says to use the largest size that is practical.
const CHUNK: u64 = 8 * 1024 * 1024;

/// The fields a finished upload comes back with. `webViewLink` is the plain
/// Drive link the share message carries; `sha256Checksum` is Drive's own
/// answer to "did you store what I sent".
const FILE_FIELDS: &str = "id,name,size,sha256Checksum,webViewLink,webContentLink";

/// How patient a retry is. Google's own rule is truncated exponential backoff:
/// `min((2^n + random milliseconds), 32 s)`, five tries.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub first: Duration,
    pub cap: Duration,
    pub tries: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(1),
            cap: Duration::from_secs(32),
            tries: 5,
        }
    }
}

impl Backoff {
    /// The wait before attempt `n`, counting from one.
    fn wait(&self, n: u32) -> Duration {
        let doubled = self.first.saturating_mul(1u32 << (n - 1).min(16));
        let mut jitter = [0u8; 2];
        getrandom::fill(&mut jitter).expect("the OS always has randomness");
        let spread = Duration::from_millis(u64::from(u16::from_le_bytes(jitter)) % 1000);
        (doubled + spread).min(self.cap)
    }
}

/// A folder in the user's Drive that Geode may put packs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub url: String,
}

/// A pack that is now in the user's Drive.
#[derive(Debug, Clone)]
pub struct Uploaded {
    pub id: String,
    pub name: String,
    pub size: u64,
    /// Drive's own SHA-256, when it gave one. Documented as "if available".
    pub sha256: Option<String>,
    pub web_view_link: String,
    pub web_content_link: Option<String>,
}

/// The Drive API, as one object so a caller does not thread four arguments
/// through every call.
pub struct Drive<'a> {
    pub client: &'a reqwest::Client,
    pub endpoints: &'a Endpoints,
    pub bearer: &'a Bearer,
    pub backoff: Backoff,
}

impl<'a> Drive<'a> {
    pub fn new(client: &'a reqwest::Client, endpoints: &'a Endpoints, bearer: &'a Bearer) -> Self {
        Self {
            client,
            endpoints,
            bearer,
            backoff: Backoff::default(),
        }
    }

    /// Who Google says is connected, for the "Connected as …" line.
    ///
    /// `fields` is required on this method. Nothing depends on the answer: an
    /// empty one leaves the UI saying "Connected" and the folder name.
    pub async fn about(&self) -> Option<String> {
        let url = format!(
            "{}/about?fields=user(displayName,emailAddress)",
            self.endpoints.drive
        );
        let about: About = self
            .read(self.client.get(&url))
            .await
            .ok()
            .and_then(|body| serde_json::from_str(&body).ok())?;
        let user = about.user?;
        user.email_address
            .or(user.display_name)
            .filter(|who| !who.is_empty())
    }

    /// The folder to put packs in: the stored one if it is still usable, else
    /// a new one.
    ///
    /// A stored id stops being usable in exactly the ways the per-file scope
    /// produces: the user deleted or trashed the folder, or they disconnected
    /// and reconnected, after which Geode is no longer authorised for a file
    /// it created under the old grant. All of them mean "make a new one", and
    /// none of them is worth telling the user about.
    pub async fn folder(&self, stored: Option<&str>, name: &str) -> Result<Folder> {
        if let Some(id) = stored.filter(|id| !id.is_empty()) {
            if let Some(folder) = self.usable_folder(id).await? {
                return Ok(folder);
            }
        }
        let url = format!("{}/files?fields=id,name,webViewLink", self.endpoints.drive);
        let body = serde_json::json!({ "name": name, "mimeType": FOLDER_MIME });
        let created: FileResource =
            serde_json::from_str(&self.read(self.client.post(&url).json(&body)).await?)
                .map_err(|e| Error::json("the folder Google Drive made", e))?;
        Ok(Folder {
            id: created.id,
            name: created.name,
            url: created.web_view_link.unwrap_or_default(),
        })
    }

    /// `None` when the folder is gone, trashed, not a folder, or not ours any
    /// more. An error only when Drive failed in a way worth reporting.
    async fn usable_folder(&self, id: &str) -> Result<Option<Folder>> {
        let url = format!(
            "{}/files/{id}?fields=id,name,trashed,mimeType,webViewLink,capabilities(canAddChildren)",
            self.endpoints.drive
        );
        let response = self.send(|| self.client.get(&url)).await?;
        let status = response.status();
        let body = response.text().await?;

        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status == reqwest::StatusCode::FORBIDDEN
            && reason(&body).as_deref() == Some("appNotAuthorizedToFile")
        {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(describe(status, &body));
        }

        let found: FileResource =
            serde_json::from_str(&body).map_err(|e| Error::json("a folder in Google Drive", e))?;
        let usable = !found.trashed
            && found.mime_type == FOLDER_MIME
            && found
                .capabilities
                .and_then(|c| c.can_add_children)
                .unwrap_or(false);
        Ok(usable.then(|| Folder {
            id: found.id,
            name: found.name,
            url: found.web_view_link.unwrap_or_default(),
        }))
    }

    /// Upload one file into `folder_id`, resumably.
    pub async fn upload(
        &self,
        folder_id: &str,
        name: &str,
        path: &Path,
        progress: &Progress,
    ) -> Result<Uploaded> {
        let total = tokio::fs::metadata(path).await?.len();
        progress.stage("Uploading to Google Drive", total).await;

        let mut file = tokio::fs::File::open(path).await?;
        let mut session = self.initiate(folder_id, name, total).await?;
        let mut sessions = 1u32;
        let mut offset = 0u64;
        let mut attempts = 0u32;
        // Set when the last answer said "ask me where you are" rather than
        // "send the next part".
        let mut ask_where = total == 0;

        loop {
            progress.check_cancelled()?;
            let step = if ask_where || offset >= total {
                ask_where = false;
                self.ask_where(&session, total).await?
            } else {
                let chunk = read_chunk(&mut file, offset, total).await?;
                self.send_chunk(&session, offset, chunk, total).await?
            };

            match step {
                Step::Done(resource) => return Ok(finished(resource, total)),
                Step::At(next) if next > offset => {
                    progress.advance(next - offset, name).await;
                    offset = next;
                    attempts = 0;
                }
                // Nothing new was acknowledged. Resend rather than spin.
                Step::At(_) => {
                    attempts += 1;
                    if attempts >= self.backoff.tries {
                        return Err(Error::msg(
                            "Google Drive stopped accepting this upload part way through — try sharing again",
                        ));
                    }
                    tokio::time::sleep(self.backoff.wait(attempts)).await;
                }
                Step::SessionGone => {
                    // Once. A session that keeps vanishing is not a hiccup.
                    if sessions >= 2 {
                        return Err(Error::msg(
                            "Google Drive lost this upload twice — try sharing again",
                        ));
                    }
                    session = self.initiate(folder_id, name, total).await?;
                    sessions += 1;
                    offset = 0;
                    attempts = 0;
                    ask_where = total == 0;
                    progress.stage("Uploading to Google Drive", total).await;
                }
                Step::Wait(e) => {
                    attempts += 1;
                    if attempts >= self.backoff.tries {
                        return Err(e);
                    }
                    tokio::time::sleep(self.backoff.wait(attempts)).await;
                    // "Don't assume the server received everything you sent."
                    ask_where = true;
                }
            }
        }
    }

    /// Start a resumable session, and take its URI from `Location`.
    async fn initiate(&self, folder_id: &str, name: &str, total: u64) -> Result<String> {
        let url = format!(
            "{}/files?uploadType=resumable&fields={FILE_FIELDS}",
            self.endpoints.upload
        );
        let body = serde_json::json!({ "name": name, "parents": [folder_id] });
        let response = self
            .send(|| {
                self.client
                    .post(&url)
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/json; charset=UTF-8",
                    )
                    .header("X-Upload-Content-Type", "application/zip")
                    .header("X-Upload-Content-Length", total.to_string())
                    .body(serde_json::to_vec(&body).unwrap_or_default())
            })
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            return Err(describe(status, &body));
        }
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| {
                Error::msg("Google Drive did not say where to upload the pack — try again")
            })
    }

    /// Send one chunk, and read what Drive made of it.
    async fn send_chunk(
        &self,
        session: &str,
        offset: u64,
        chunk: Bytes,
        total: u64,
    ) -> Result<Step> {
        let last = offset + chunk.len() as u64 - 1;
        let range = format!("bytes {offset}-{last}/{total}");
        let response = self
            .send(|| {
                self.client
                    .put(session)
                    .header(reqwest::header::CONTENT_RANGE, &range)
                    .body(chunk.clone())
            })
            .await;
        self.read_step(response, offset).await
    }

    /// Ask the session how much of the file it has. An empty body and
    /// `bytes */total`, which is also how a zero-length file is uploaded.
    async fn ask_where(&self, session: &str, total: u64) -> Result<Step> {
        let range = format!("bytes */{total}");
        let response = self
            .send(|| {
                self.client
                    .put(session)
                    .header(reqwest::header::CONTENT_RANGE, &range)
                    .header(reqwest::header::CONTENT_LENGTH, "0")
            })
            .await;
        self.read_step(response, 0).await
    }

    async fn read_step(&self, response: Result<reqwest::Response>, offset: u64) -> Result<Step> {
        let response = match response {
            Ok(response) => response,
            // No answer at all. The guide's own advice is to ask the session
            // where it is rather than assume nothing arrived.
            Err(Error::Http(e)) => {
                return Ok(Step::Wait(Error::msg(format!(
                    "the upload to Google Drive was interrupted: {e}"
                ))))
            }
            Err(e) => return Err(e),
        };

        let status = response.status();
        // 308 Resume Incomplete: `Range` says how much landed, and it may be
        // less than was sent. This is the bug that only shows up on a bad
        // connection, so it is read rather than assumed.
        if status.as_u16() == 308 {
            let range = response
                .headers()
                .get(reqwest::header::RANGE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            return Ok(Step::At(resume_from(range.as_deref())));
        }
        if status.is_success() {
            let body = response.text().await?;
            let resource: FileResource = serde_json::from_str(&body)
                .map_err(|e| Error::json("the pack Google Drive stored", e))?;
            return Ok(Step::Done(resource));
        }
        // A session Drive has forgotten. Starting a new one is the documented
        // answer, and the only one that does not lose the whole upload.
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(Step::SessionGone);
        }

        let body = response.text().await?;
        if transient(status, &body) {
            return Ok(Step::Wait(describe(status, &body)));
        }
        let _ = offset;
        Err(describe(status, &body))
    }

    /// Let anyone with the link read this one file.
    ///
    /// `allowFileDiscovery` is sent as false explicitly so the file is
    /// link-only rather than searchable, even if the default ever changes.
    /// `sendNotificationEmail` is never sent: Drive allows it only for a user
    /// or a group.
    pub async fn share_anyone(&self, file_id: &str) -> Result<()> {
        let url = format!(
            "{}/files/{file_id}/permissions?fields=id",
            self.endpoints.drive
        );
        let body = serde_json::json!({
            "role": "reader",
            "type": "anyone",
            "allowFileDiscovery": false,
        });
        self.read(self.client.post(&url).json(&body)).await?;
        Ok(())
    }

    /// Remove a file Geode put there. Used to undo a share that could not be
    /// finished — a pack in somebody's Drive that nobody can read is worse
    /// than no pack at all.
    pub async fn delete(&self, file_id: &str) -> Result<()> {
        let url = format!("{}/files/{file_id}", self.endpoints.drive);
        self.read(self.client.delete(&url)).await?;
        Ok(())
    }

    /// One request, its body read, with a failure turned into a sentence.
    ///
    /// Tried again on the answers Google's own guidance says to retry, on the
    /// same schedule the upload loop uses. A share is a chain of small calls
    /// around one long upload, and the calls after it are the expensive ones
    /// to lose: the permission is set *after* the bytes are in Drive and its
    /// caller deletes the file when it fails, so a 503 or a rate limit — which
    /// uploading 150 chunks is itself the way to earn — threw away a finished
    /// upload that trying again would have published.
    async fn read(&self, request: reqwest::RequestBuilder) -> Result<String> {
        let request = request
            .try_clone()
            .ok_or_else(|| Error::msg("a Google Drive request could not be prepared"))?;
        let mut attempts = 0u32;
        loop {
            attempts += 1;
            let response = self
                .send(|| {
                    request
                        .try_clone()
                        .expect("a request with a plain body clones")
                })
                .await?;
            let status = response.status();
            let body = response.text().await?;
            if status.is_success() {
                return Ok(body);
            }
            if !transient(status, &body) || attempts >= self.backoff.tries {
                return Err(describe(status, &body));
            }
            tokio::time::sleep(self.backoff.wait(attempts)).await;
        }
    }

    /// Send one request with the current token, and once more with a fresh one
    /// if Google says the token is no good.
    ///
    /// A few hundred megabytes up a home connection can take longer than an
    /// access token lives, so a 401 half way through an upload has to be
    /// answerable without throwing the upload away.
    async fn send<F>(&self, build: F) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let token = self.bearer.get().await;
        let response = build().bearer_auth(&token).send().await?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        if !self.bearer.renew().await? {
            return Err(reconnect_error());
        }
        let token = self.bearer.get().await;
        Ok(build().bearer_auth(&token).send().await?)
    }
}

/// What one chunk's answer means.
enum Step {
    Done(FileResource),
    /// Drive has this many bytes.
    At(u64),
    SessionGone,
    /// Worth trying again; carries what to say if it never works.
    Wait(Error),
}

async fn read_chunk(file: &mut tokio::fs::File, offset: u64, total: u64) -> Result<Bytes> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let want = CHUNK.min(total - offset) as usize;
    let mut buf = vec![0u8; want];
    file.read_exact(&mut buf).await?;
    Ok(Bytes::from(buf))
}

/// `bytes=0-262143` means Drive holds the first 262144 bytes.
///
/// No `Range` header at all means nothing landed, which the guide says
/// explicitly — so it must not be read as "carry on from where you were".
fn resume_from(range: Option<&str>) -> u64 {
    range
        .and_then(|range| range.rsplit_once('-'))
        .and_then(|(_, last)| last.trim().parse::<u64>().ok())
        .map_or(0, |last| last + 1)
}

fn finished(resource: FileResource, total: u64) -> Uploaded {
    Uploaded {
        id: resource.id,
        name: resource.name,
        // Drive returns an int64 as a string. Our own count of the staged file
        // is the fallback, and the only number the UI ever showed before.
        size: resource
            .size
            .and_then(|size| size.parse::<u64>().ok())
            .unwrap_or(total),
        sha256: resource.sha256_checksum.filter(|hash| !hash.is_empty()),
        web_view_link: resource.web_view_link.unwrap_or_default(),
        web_content_link: resource.web_content_link,
    }
}

// ---------------------------------------------------------------------------
// What Drive says when it refuses
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: Option<ErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<ErrorItem>,
}

#[derive(Debug, Deserialize)]
struct ErrorItem {
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct About {
    #[serde(default)]
    user: Option<AboutUser>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AboutUser {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    email_address: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileResource {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    trashed: bool,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    sha256_checksum: Option<String>,
    #[serde(default)]
    web_view_link: Option<String>,
    #[serde(default)]
    web_content_link: Option<String>,
    #[serde(default)]
    capabilities: Option<Capabilities>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    #[serde(default)]
    can_add_children: Option<bool>,
}

/// The `reason` Drive gave, which is the part worth switching on.
fn reason(body: &str) -> Option<String> {
    serde_json::from_str::<ErrorBody>(body)
        .ok()?
        .error?
        .errors
        .into_iter()
        .map(|item| item.reason)
        .find(|reason| !reason.is_empty())
}

fn said(body: &str) -> String {
    serde_json::from_str::<ErrorBody>(body)
        .ok()
        .and_then(|parsed| parsed.error)
        .map(|detail| detail.message)
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/// Whether trying again could work.
pub(super) fn transient(status: reqwest::StatusCode, body: &str) -> bool {
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return true;
    }
    status == reqwest::StatusCode::FORBIDDEN
        && matches!(
            reason(body).as_deref(),
            Some("rateLimitExceeded" | "userRateLimitExceeded")
        )
}

/// What a refusal should read as. Each of these is a different thing for the
/// user to do, which is the only reason they are told apart.
pub(super) fn describe(status: reqwest::StatusCode, body: &str) -> Error {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return reconnect_error();
    }
    match (status.as_u16(), reason(body).as_deref()) {
        (403, Some("storageQuotaExceeded")) => {
            Error::msg("your Google Drive is full — free some space and share again")
        }
        (403 | 429, Some("rateLimitExceeded" | "userRateLimitExceeded")) => {
            Error::msg("Google Drive is limiting this account — try again in a minute")
        }
        (403, Some("appNotAuthorizedToFile")) => Error::msg(
            "Geode can only touch files it created in your Drive. Reconnect your Google account.",
        ),
        (404, _) => Error::msg("Google Drive could not find that file"),
        // Geode built the request wrong, so it should read like a bug report
        // rather than like the user's fault.
        (400, _) => Error::msg(format!(
            "Google Drive refused a request Geode built: {}",
            said(body)
        )),
        _ => Error::msg(format!(
            "Google Drive refused the request ({status}): {}",
            said(body)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Call, Fake, Reply};
    use super::*;
    use crate::paths::Scratch;

    const FOLDER: &str = "Geode Packs";

    /// A backoff a test does not have to sit through. The schedule itself is
    /// Google's and is not what these tests are about.
    fn quick() -> Backoff {
        Backoff {
            first: Duration::from_millis(1),
            cap: Duration::from_millis(5),
            tries: 5,
        }
    }

    fn folder_resource(id: &str, can_add: bool, trashed: bool) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": FOLDER,
            "mimeType": FOLDER_MIME,
            "trashed": trashed,
            "webViewLink": format!("https://drive.google.com/drive/folders/{id}"),
            "capabilities": { "canAddChildren": can_add },
        })
    }

    #[tokio::test]
    async fn a_stored_folder_that_is_still_usable_is_reused_and_nothing_is_created() {
        let google = Fake::start(|_| {
            |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("GET", "/drive/v3/files/folder-1") => {
                    Reply::json(200, folder_resource("folder-1", true, false))
                }
                _ => Reply::status(500),
            }
        })
        .await;
        let client = crate::net::client();
        let bearer = Bearer::fixed("tok");
        let drive = Drive::new(&client, &google.endpoints, &bearer);

        let folder = drive.folder(Some("folder-1"), FOLDER).await.unwrap();
        assert_eq!(folder.id, "folder-1");
        assert_eq!(folder.name, FOLDER);
        assert_eq!(
            folder.url,
            "https://drive.google.com/drive/folders/folder-1"
        );
        assert!(
            google.to_path("/drive/v3/files").is_empty(),
            "a usable folder was replaced anyway"
        );
        // And the token travelled where Google expects it.
        assert_eq!(
            google.calls()[0].header("authorization"),
            Some("Bearer tok")
        );
    }

    /// Four ways a stored id stops being usable, all of which mean "make a new
    /// one" and none of which is worth bothering the user about.
    #[tokio::test]
    async fn a_folder_that_is_gone_trashed_or_no_longer_ours_is_replaced_silently() {
        for answer in [
            Reply::error(404, "notFound", "File not found: folder-1."),
            Reply::error(
                403,
                "appNotAuthorizedToFile",
                "The user has not granted the app access.",
            ),
            Reply::json(200, folder_resource("folder-1", true, true)),
            Reply::json(200, folder_resource("folder-1", false, false)),
        ] {
            let google = Fake::start(move |_| {
                let answer = answer.clone();
                move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                    ("GET", "/drive/v3/files/folder-1") => answer.clone(),
                    ("POST", "/drive/v3/files") => {
                        Reply::json(200, folder_resource("folder-2", true, false))
                    }
                    _ => Reply::status(500),
                }
            })
            .await;
            let client = crate::net::client();
            let bearer = Bearer::fixed("tok");
            let drive = Drive::new(&client, &google.endpoints, &bearer);

            let folder = drive.folder(Some("folder-1"), FOLDER).await.unwrap();
            assert_eq!(folder.id, "folder-2");

            // The body Geode sends to make one, and no permission for it ever.
            let made = &google.to_path("/drive/v3/files")[0];
            assert_eq!(
                made.json(),
                serde_json::json!({ "name": FOLDER, "mimeType": FOLDER_MIME })
            );
            assert_eq!(made.param("fields").as_deref(), Some("id,name,webViewLink"));
            assert!(
                google
                    .calls()
                    .iter()
                    .all(|call| !call.path.contains("permissions")),
                "a folder was given a permission, which would publish every pack in it"
            );
        }
    }

    /// A pack staged on disk, big enough to need three chunks if the chunk
    /// size were a quarter of a megabyte — which is what the resume test
    /// leans on.
    async fn staged(scratch: &Scratch, len: usize) -> std::path::PathBuf {
        tokio::fs::create_dir_all(&scratch.paths.root)
            .await
            .unwrap();
        let path = scratch.paths.root.join("survival.geodepack");
        let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&path, bytes).await.unwrap();
        path
    }

    fn uploaded_resource(sha256: Option<&str>, size: u64) -> serde_json::Value {
        let mut value = serde_json::json!({
            "id": "file-1",
            "name": "survival.geodepack",
            "size": size.to_string(),
            "webViewLink": "https://drive.google.com/file/d/file-1/view",
            "webContentLink": "https://drive.google.com/uc?id=file-1&export=download",
        });
        if let Some(sha256) = sha256 {
            value["sha256Checksum"] = serde_json::json!(sha256);
        }
        value
    }

    #[tokio::test]
    async fn the_initiation_says_what_is_coming_and_the_session_uri_is_taken_from_location() {
        let scratch = Scratch::new("drive-initiate");
        let path = staged(&scratch, 1024).await;
        let google = Fake::start(|base| {
            move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/upload/drive/v3/files") => Reply::status(200)
                    .with("location", format!("{base}/upload/session/7"))
                    .with("content-length", "0"),
                ("PUT", "/upload/session/7") => Reply::json(200, uploaded_resource(None, 1024)),
                _ => Reply::status(500),
            }
        })
        .await;
        let client = crate::net::client();
        let bearer = Bearer::fixed("tok");
        let drive = Drive::new(&client, &google.endpoints, &bearer);

        let done = drive
            .upload(
                "folder-1",
                "survival.geodepack",
                &path,
                &Progress::silent("up"),
            )
            .await
            .unwrap();
        assert_eq!(done.id, "file-1");
        assert_eq!(done.size, 1024);
        assert_eq!(
            done.web_view_link,
            "https://drive.google.com/file/d/file-1/view"
        );
        assert_eq!(done.sha256, None);

        let start = &google.to_path("/upload/drive/v3/files")[0];
        assert_eq!(start.param("uploadType").as_deref(), Some("resumable"));
        assert_eq!(start.param("fields").as_deref(), Some(FILE_FIELDS));
        assert_eq!(start.header("x-upload-content-length"), Some("1024"));
        assert_eq!(
            start.header("x-upload-content-type"),
            Some("application/zip")
        );
        assert_eq!(
            start.json(),
            serde_json::json!({ "name": "survival.geodepack", "parents": ["folder-1"] })
        );
        // And the one chunk said where it sat in the whole.
        assert_eq!(
            google.to_path("/upload/session/7")[0].header("content-range"),
            Some("bytes 0-1023/1024")
        );
    }

    /// "Don't assume the server received everything you sent." A 308 that
    /// acknowledges half a chunk has to be read, not rounded up — this is the
    /// bug that only appears on a bad connection.
    #[tokio::test]
    async fn a_308_that_acknowledges_less_than_was_sent_resumes_from_that_byte() {
        let scratch = Scratch::new("drive-resume");
        let total = 1000usize;
        let path = staged(&scratch, total).await;
        let google = Fake::start(|base| {
            let seen = std::sync::Mutex::new(0u32);
            move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/upload/drive/v3/files") => {
                    Reply::status(200).with("location", format!("{base}/upload/session/7"))
                }
                ("PUT", "/upload/session/7") => {
                    let mut seen = seen.lock().unwrap();
                    *seen += 1;
                    if *seen == 1 {
                        // Took 400 of the 1000 bytes it was sent.
                        Reply::status(308).with("range", "bytes=0-399")
                    } else {
                        Reply::json(200, uploaded_resource(None, total as u64))
                    }
                }
                _ => Reply::status(500),
            }
        })
        .await;
        let client = crate::net::client();
        let bearer = Bearer::fixed("tok");
        let drive = Drive::new(&client, &google.endpoints, &bearer);

        drive
            .upload(
                "folder-1",
                "survival.geodepack",
                &path,
                &Progress::silent("up"),
            )
            .await
            .unwrap();

        let puts = google.to_path("/upload/session/7");
        assert_eq!(puts.len(), 2);
        assert_eq!(puts[0].header("content-range"), Some("bytes 0-999/1000"));
        // From byte 400, not from 1000.
        assert_eq!(puts[1].header("content-range"), Some("bytes 400-999/1000"));
        assert_eq!(puts[1].body.len(), 600);
    }

    #[tokio::test]
    async fn a_lost_session_is_started_over_once_and_a_second_loss_gives_up() {
        for (losses, should_finish) in [(1u32, true), (2, false)] {
            let scratch = Scratch::new("drive-lost-session");
            let path = staged(&scratch, 512).await;
            let google = Fake::start(move |base| {
                let sessions = std::sync::Mutex::new(0u32);
                move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                    ("POST", "/upload/drive/v3/files") => {
                        let mut sessions = sessions.lock().unwrap();
                        *sessions += 1;
                        Reply::status(200)
                            .with("location", format!("{base}/upload/session/{sessions}"))
                    }
                    ("PUT", path) => {
                        let which: u32 = path.rsplit('/').next().unwrap().parse().unwrap();
                        if which <= losses {
                            Reply::error(404, "notFound", "Upload session not found.")
                        } else {
                            Reply::json(200, uploaded_resource(None, 512))
                        }
                    }
                    _ => Reply::status(500),
                }
            })
            .await;
            let client = crate::net::client();
            let bearer = Bearer::fixed("tok");
            let mut drive = Drive::new(&client, &google.endpoints, &bearer);
            drive.backoff = quick();

            let done = drive
                .upload(
                    "folder-1",
                    "survival.geodepack",
                    &path,
                    &Progress::silent("up"),
                )
                .await;
            assert_eq!(
                done.is_ok(),
                should_finish,
                "with {losses} lost session(s): {done:?}"
            );
            assert_eq!(
                google.to_path("/upload/drive/v3/files").len(),
                2,
                "a lost session should be started over exactly once, either way"
            );
        }
    }

    #[tokio::test]
    async fn a_503_is_re_probed_with_bytes_star_and_resumed_from_what_landed() {
        let scratch = Scratch::new("drive-503");
        let path = staged(&scratch, 800).await;
        let google = Fake::start(|base| {
            let seen = std::sync::Mutex::new(0u32);
            move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/upload/drive/v3/files") => {
                    Reply::status(200).with("location", format!("{base}/upload/session/7"))
                }
                ("PUT", "/upload/session/7") => {
                    let mut seen = seen.lock().unwrap();
                    *seen += 1;
                    match *seen {
                        1 => Reply::status(503),
                        2 => Reply::status(308).with("range", "bytes=0-299"),
                        _ => Reply::json(200, uploaded_resource(None, 800)),
                    }
                }
                _ => Reply::status(500),
            }
        })
        .await;
        let client = crate::net::client();
        let bearer = Bearer::fixed("tok");
        let mut drive = Drive::new(&client, &google.endpoints, &bearer);
        drive.backoff = quick();

        drive
            .upload(
                "folder-1",
                "survival.geodepack",
                &path,
                &Progress::silent("up"),
            )
            .await
            .unwrap();

        let puts = google.to_path("/upload/session/7");
        assert_eq!(puts.len(), 3);
        // The probe: no body, and `*` for "I do not know how much you have".
        assert_eq!(puts[1].header("content-range"), Some("bytes */800"));
        assert!(puts[1].body.is_empty());
        assert_eq!(puts[2].header("content-range"), Some("bytes 300-799/800"));
    }

    #[tokio::test]
    async fn the_permission_is_anyone_reader_and_not_discoverable() {
        let google = Fake::start(|_| {
            |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/drive/v3/files/file-1/permissions") => {
                    Reply::json(200, serde_json::json!({ "id": "perm-1" }))
                }
                _ => Reply::status(500),
            }
        })
        .await;
        let client = crate::net::client();
        let bearer = Bearer::fixed("tok");
        let drive = Drive::new(&client, &google.endpoints, &bearer);

        drive.share_anyone("file-1").await.unwrap();
        let call = &google.to_path("/drive/v3/files/file-1/permissions")[0];
        assert_eq!(
            call.json(),
            serde_json::json!({
                "role": "reader",
                "type": "anyone",
                "allowFileDiscovery": false,
            })
        );
        assert_eq!(call.param("fields").as_deref(), Some("id"));
    }

    /// The permission is set after the pack is in Drive, and its caller
    /// deletes the file when this fails — so an answer that trying again would
    /// fix must not cost the whole upload.
    #[tokio::test]
    async fn a_permission_call_rides_out_the_answers_worth_retrying() {
        let flaky = Fake::start(|_| {
            let seen = std::sync::Mutex::new(0u32);
            move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/drive/v3/files/file-1/permissions") => {
                    let mut seen = seen.lock().unwrap();
                    *seen += 1;
                    match *seen {
                        1 => Reply::status(503),
                        // The one an account earns by uploading 150 chunks.
                        2 => Reply::error(403, "userRateLimitExceeded", "Rate Limit Exceeded"),
                        _ => Reply::json(200, serde_json::json!({ "id": "perm-1" })),
                    }
                }
                _ => Reply::status(500),
            }
        })
        .await;
        let client = crate::net::client();
        let bearer = Bearer::fixed("tok");
        let mut drive = Drive::new(&client, &flaky.endpoints, &bearer);
        drive.backoff = quick();

        drive.share_anyone("file-1").await.unwrap();
        assert_eq!(flaky.to_path("/drive/v3/files/file-1/permissions").len(), 3);

        // And a refusal no amount of waiting would change is not waited on.
        let refusing = Fake::start(|_| {
            |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/drive/v3/files/file-1/permissions") => {
                    Reply::error(403, "insufficientFilePermissions", "No permission.")
                }
                _ => Reply::status(500),
            }
        })
        .await;
        let mut drive = Drive::new(&client, &refusing.endpoints, &bearer);
        drive.backoff = quick();
        assert!(drive.share_anyone("file-1").await.is_err());
        assert_eq!(
            refusing.to_path("/drive/v3/files/file-1/permissions").len(),
            1
        );
    }

    /// Each of these is a different thing for the user to do, which is the
    /// only reason they are told apart at all.
    #[test]
    fn every_refusal_reads_as_something_a_person_can_act_on() {
        let cases = [
            (
                401,
                "authError",
                "Invalid Credentials",
                "reconnect your Google account",
            ),
            (
                403,
                "storageQuotaExceeded",
                "The user has exceeded their Drive storage quota.",
                "Drive is full",
            ),
            (
                403,
                "userRateLimitExceeded",
                "Rate Limit Exceeded",
                "limiting this account",
            ),
            (
                429,
                "rateLimitExceeded",
                "Rate Limit Exceeded",
                "limiting this account",
            ),
            (
                403,
                "appNotAuthorizedToFile",
                "The user has not granted the app access.",
                "files it created",
            ),
            (
                404,
                "notFound",
                "File not found.",
                "could not find that file",
            ),
            (
                400,
                "badRequest",
                "The 'parents' field is not writable.",
                "parents",
            ),
        ];
        for (status, reason, message, expect) in cases {
            let body = serde_json::json!({
                "error": { "code": status, "message": message, "errors": [{ "reason": reason }] }
            })
            .to_string();
            let said = describe(reqwest::StatusCode::from_u16(status).unwrap(), &body).to_string();
            assert!(
                said.contains(expect),
                "{status} {reason} came back as {said:?}"
            );
        }
        // A 400 should read like a bug report, naming what Geode got wrong.
        let body = serde_json::json!({
            "error": { "code": 400, "message": "Bad Request", "errors": [{ "reason": "badRequest" }] }
        })
        .to_string();
        assert!(describe(reqwest::StatusCode::BAD_REQUEST, &body)
            .to_string()
            .contains("a request Geode built"));
    }

    #[test]
    fn only_a_failure_worth_retrying_is_called_transient() {
        let rate =
            serde_json::json!({ "error": { "errors": [{ "reason": "userRateLimitExceeded" }] } })
                .to_string();
        let quota =
            serde_json::json!({ "error": { "errors": [{ "reason": "storageQuotaExceeded" }] } })
                .to_string();
        for (status, body, expect) in [
            (500, "", true),
            (503, "", true),
            (429, "", true),
            (403, rate.as_str(), true),
            (403, quota.as_str(), false),
            (401, "", false),
            (400, "", false),
        ] {
            assert_eq!(
                transient(reqwest::StatusCode::from_u16(status).unwrap(), body),
                expect,
                "{status} {body}"
            );
        }
    }

    /// No `Range` means nothing landed. Reading it as "carry on where you
    /// were" would leave a hole in the middle of the file.
    #[test]
    fn a_resume_point_is_read_off_the_range_header_and_nothing_is_assumed() {
        assert_eq!(resume_from(Some("bytes=0-262143")), 262144);
        assert_eq!(resume_from(Some("bytes=0-0")), 1);
        assert_eq!(resume_from(None), 0);
        assert_eq!(resume_from(Some("")), 0);
        assert_eq!(resume_from(Some("bytes=*")), 0);
    }

    #[tokio::test]
    async fn a_401_part_way_through_asks_for_a_new_token_and_carries_on() {
        let scratch = Scratch::new("drive-401");
        let paths = &scratch.paths;
        let path = staged(&scratch, 256).await;
        let google = Fake::start(|base| {
            let puts = std::sync::Mutex::new(0u32);
            move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/token") => Reply::json(
                    200,
                    serde_json::json!({ "access_token": "access-2", "expires_in": 3599 }),
                ),
                ("POST", "/upload/drive/v3/files") => {
                    Reply::status(200).with("location", format!("{base}/upload/session/7"))
                }
                ("PUT", "/upload/session/7") => {
                    let mut puts = puts.lock().unwrap();
                    *puts += 1;
                    if call.header("authorization") == Some("Bearer access-1") {
                        Reply::error(401, "authError", "Invalid Credentials")
                    } else {
                        Reply::json(200, uploaded_resource(None, 256))
                    }
                }
                _ => Reply::status(500),
            }
        })
        .await;

        super::super::tokens::Connection {
            refresh_token: "refresh-1".into(),
            access_token: "access-1".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            ..Default::default()
        }
        .save(paths)
        .await
        .unwrap();

        let client = crate::net::client();
        let bearer = Bearer::renewable(
            "access-1".into(),
            crate::net::client(),
            paths.clone(),
            google.endpoints.clone(),
            "id".into(),
            None,
        );
        let drive = Drive::new(&client, &google.endpoints, &bearer);
        let done = drive
            .upload(
                "folder-1",
                "survival.geodepack",
                &path,
                &Progress::silent("up"),
            )
            .await
            .unwrap();
        assert_eq!(done.id, "file-1");
        assert_eq!(
            google.to_path("/token").len(),
            1,
            "the token was refreshed more than once"
        );
    }

    #[tokio::test]
    async fn a_cancel_between_chunks_stops_the_upload() {
        let scratch = Scratch::new("drive-cancel");
        // Two chunks' worth, so there is a between.
        let path = staged(&scratch, (CHUNK + 16) as usize).await;
        let google = Fake::start(|base| {
            move |call: &Call| match (call.method.as_str(), call.path.as_str()) {
                ("POST", "/upload/drive/v3/files") => {
                    Reply::status(200).with("location", format!("{base}/upload/session/7"))
                }
                ("PUT", "/upload/session/7") => {
                    Reply::status(308).with("range", format!("bytes=0-{}", CHUNK - 1))
                }
                _ => Reply::status(500),
            }
        })
        .await;
        let client = crate::net::client();
        let bearer = Bearer::fixed("tok");
        let drive = Drive::new(&client, &google.endpoints, &bearer);

        let progress = Progress::silent("up");
        let flag = progress.cancel_flag();
        // Cancelled before the second chunk is read, which is where the check
        // sits — nothing is sent after it.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let stopped = drive
            .upload("folder-1", "survival.geodepack", &path, &progress)
            .await;
        assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
    }
}

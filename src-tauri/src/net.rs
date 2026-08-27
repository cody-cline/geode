use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use sha1::{Digest, Sha1};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::{Error, Result};

pub const USER_AGENT: &str = concat!(
    "geode/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/littlesquirt1/geode)"
);

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(20))
        .pool_max_idle_per_host(16)
        .build()
        .expect("http client")
}

// ---------------------------------------------------------------------------
// Progress reporting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub id: String,
    pub stage: String,
    pub detail: String,
    /// Bytes (or items) completed and expected. `total == 0` means indeterminate.
    pub current: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// A throttled progress channel tied to one long-running job (an install, an
/// import, a Java download). The frontend keys off `id`.
///
/// The app handle is optional so the install pipeline can run headlessly —
/// from a test or a CLI — without a window attached.
#[derive(Clone)]
pub struct Progress {
    app: Option<AppHandle>,
    id: String,
    stage: Arc<Mutex<String>>,
    current: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    last_emit: Arc<Mutex<Instant>>,
    cancelled: Arc<AtomicBool>,
}

impl Progress {
    pub fn new(app: AppHandle, id: impl Into<String>) -> Self {
        Self::with_sink(Some(app), id)
    }

    /// A progress handle that reports nowhere. For headless callers.
    pub fn silent(id: impl Into<String>) -> Self {
        Self::with_sink(None, id)
    }

    fn with_sink(app: Option<AppHandle>, id: impl Into<String>) -> Self {
        Self {
            app,
            id: id.into(),
            stage: Arc::new(Mutex::new(String::new())),
            current: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
            last_emit: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1))),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    pub fn check_cancelled(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Begin a new stage, resetting the counters.
    pub async fn stage(&self, name: &str, total: u64) {
        *self.stage.lock().await = name.to_string();
        self.current.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.emit(name.to_string(), false, None).await;
    }

    pub async fn advance(&self, by: u64, detail: &str) {
        self.current.fetch_add(by, Ordering::Relaxed);
        // Throttle to ~20 fps; a 3000-file asset download would otherwise
        // flood the IPC bridge and stall the webview.
        let mut last = self.last_emit.lock().await;
        if last.elapsed() >= Duration::from_millis(50) {
            *last = Instant::now();
            drop(last);
            self.emit(detail.to_string(), false, None).await;
        }
    }

    pub async fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    pub async fn finish(&self) {
        self.current
            .store(self.total.load(Ordering::Relaxed), Ordering::Relaxed);
        self.emit("Done".into(), true, None).await;
    }

    pub async fn fail(&self, err: &str) {
        self.emit("Failed".into(), true, Some(err.to_string())).await;
    }

    async fn emit(&self, detail: String, done: bool, error: Option<String>) {
        let Some(app) = &self.app else {
            return;
        };
        let ev = ProgressEvent {
            id: self.id.clone(),
            stage: self.stage.lock().await.clone(),
            detail,
            current: self.current.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            done,
            error,
        };
        let _ = app.emit("geode://progress", ev);
    }
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Download {
    pub url: String,
    pub dest: PathBuf,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    /// Human-readable name shown in the progress detail line.
    pub label: String,
}

impl Download {
    pub fn new(url: impl Into<String>, dest: PathBuf) -> Self {
        let url = url.into();
        let label = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| url.clone());
        Self {
            url,
            dest,
            sha1: None,
            size: None,
            label,
        }
    }

    pub fn sha1(mut self, hash: Option<String>) -> Self {
        self.sha1 = hash;
        self
    }

    pub fn size(mut self, size: Option<u64>) -> Self {
        self.size = size;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

pub async fn sha1_file(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let mut h = Sha1::new();
    h.update(&bytes);
    Ok(hex::encode(h.finalize()))
}

/// True when the file already on disk matches what we were going to download.
/// When we have a hash we trust only the hash; otherwise we fall back to size,
/// and with neither we accept any existing file.
async fn already_valid(d: &Download) -> bool {
    let Ok(meta) = tokio::fs::metadata(&d.dest).await else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    if let Some(expect) = &d.sha1 {
        return matches!(sha1_file(&d.dest).await, Ok(actual) if actual.eq_ignore_ascii_case(expect));
    }
    if let Some(size) = d.size {
        return meta.len() == size;
    }
    meta.len() > 0
}

/// Download one file, verifying its hash. Writes to a `.part` sibling and
/// renames on success so an interrupted run never leaves a truncated jar that
/// looks complete on the next launch.
pub async fn fetch_one(client: &reqwest::Client, d: &Download) -> Result<()> {
    if already_valid(d).await {
        return Ok(());
    }
    if let Some(parent) = d.dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut last_err: Option<Error> = None;
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(400 * (1 << attempt))).await;
        }
        match try_fetch(client, d).await {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| Error::msg("download failed")))
}

async fn try_fetch(client: &reqwest::Client, d: &Download) -> Result<()> {
    let resp = client.get(&d.url).send().await?;
    if !resp.status().is_success() {
        return Err(Error::msg(format!(
            "{} returned HTTP {}",
            d.label,
            resp.status()
        )));
    }

    let tmp = d.dest.with_extension(format!(
        "{}.part",
        d.dest.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut hasher = Sha1::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    if let Some(expect) = &d.sha1 {
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(expect) {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(Error::Checksum {
                file: d.label.clone(),
                expected: expect.clone(),
                actual,
            });
        }
    }

    tokio::fs::rename(&tmp, &d.dest).await?;
    Ok(())
}

/// Run a batch of downloads with bounded concurrency, reporting per-file
/// progress. Counts files rather than bytes: asset objects are tiny and
/// numerous, so a file count is both cheaper to track and more legible.
pub async fn fetch_all(
    client: &reqwest::Client,
    downloads: Vec<Download>,
    progress: &Progress,
    concurrency: usize,
) -> Result<()> {
    let total = downloads.len() as u64;
    progress.set_total(total).await;
    if downloads.is_empty() {
        return Ok(());
    }

    let mut iter = downloads.into_iter();
    let mut inflight = FuturesUnordered::new();

    for _ in 0..concurrency.min(total as usize) {
        if let Some(d) = iter.next() {
            inflight.push(spawn_one(client.clone(), d));
        }
    }

    while let Some(res) = inflight.next().await {
        let label = res?;
        progress.advance(1, &label).await;
        progress.check_cancelled()?;
        if let Some(d) = iter.next() {
            inflight.push(spawn_one(client.clone(), d));
        }
    }
    Ok(())
}

async fn spawn_one(client: reqwest::Client, d: Download) -> Result<String> {
    fetch_one(&client, &d).await?;
    Ok(d.label)
}

pub async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(Error::msg(format!("{url} returned HTTP {status}")));
    }
    serde_json::from_str(&text).map_err(|e| Error::json(url, e))
}

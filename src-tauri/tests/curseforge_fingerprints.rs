//! What Geode asks CurseForge, and what it makes of the answer.
//!
//! Both halves of this file work the same way: a listener on localhost, the
//! real request builder pointed at it, and the raw bytes read off the socket.
//! The key is fake, the answers are hand-written, and nothing leaves the
//! machine.
//!
//! **Fingerprint lookups.** The request form is the one claim in that code
//! with no primary source behind it: CurseForge's published schema types the
//! fingerprint list `integer[]`, while Prism — which works against the live
//! API — sends decimal strings, and the same schema types `downloadUrl` as
//! non-nullable when it is null for every author-blocked file. So the form is
//! pinned here rather than trusted by eye.
//!
//! **Pack imports.** A CurseForge pack is nothing but ids: what the import
//! does is decided entirely by what it asks and what it reads back, so these
//! run whole imports against a CurseForge of our own — one that holds the
//! files the manifest names, answers both batch endpoints from them, and
//! serves the bytes at the URL it gave. That is the only way to see the
//! request count, the folder each file lands in and what happens to a file
//! nobody is allowed to fetch.
//!
//! The same stub answers as Modrinth, because a file whose CurseForge author
//! switched third-party downloads off is looked for among that author's own
//! Modrinth releases by the sha1 CurseForge published for it. Whether it is
//! found there is the difference between installing and refusing, so both
//! answers are served here and neither is guessed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

const FAKE_KEY: &str = "$2a$10$Abc./123Abc./123Abc./123Abc./123Abc./123Abc./123Abc./";

/// What one request looked like on the way in.
struct Seen {
    start_line: String,
    headers: Vec<String>,
    body: String,
}

/// Answer the next request with this JSON, and hand back what was asked.
///
/// Returns the base URL to point a call at, in the `.../v1` shape the real
/// `API` constant has, so the path the call appends is the path under test.
fn serve(answer: impl Into<String>) -> (String, std::thread::JoinHandle<Seen>) {
    let answer = answer.into();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(&stream);

        let mut start_line = String::new();
        reader.read_line(&mut start_line).expect("start line");

        let mut headers = Vec::new();
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            let line = line.trim_end().to_string();
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap_or(0);
            }
            headers.push(line);
        }

        // Exactly as many bytes as the request said, so nothing blocks waiting
        // for an end that never comes.
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).expect("body");

        let mut stream = stream;
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{answer}",
                answer.len()
            )
            .as_bytes(),
        );

        Seen {
            start_line: start_line.trim_end().to_string(),
            headers,
            body: String::from_utf8(body).expect("utf-8 body"),
        }
    });

    (format!("http://{addr}/v1"), handle)
}

/// One `exactMatches` entry, as CurseForge shapes it.
fn a_match(fingerprint: &str, mod_id: u32, file_id: u32, download_url: &str) -> String {
    format!(
        r#"{{"file":{{"id":{file_id},"modId":{mod_id},"displayName":"Mod {file_id}",
           "fileName":"mod-{file_id}.jar","fileFingerprint":{fingerprint},
           "downloadUrl":{download_url},"gameVersions":["1.21.1","Fabric"]}}}}"#
    )
}

/// The path and the body, which are the two halves of the claim with no
/// primary source behind them.
#[tokio::test]
async fn a_fingerprint_request_sends_decimal_strings_to_the_game_scoped_path() {
    let (base, listener) = serve(r#"{"data":{"exactMatches":[]}}"#);
    let client = geode_lib::net::client();

    let found = geode_lib::providers::curseforge::match_fingerprints_at(
        &client,
        &base,
        &[3623028036, 1540447798],
        FAKE_KEY,
    )
    .await
    .expect("the stub answers 200");
    assert!(found.is_empty());

    let seen = listener.join().expect("listener thread");

    assert_eq!(
        seen.start_line, "POST /v1/fingerprints/432 HTTP/1.1",
        "game-scoped path, so a file from another CurseForge game cannot match"
    );
    // Quoted decimals, in the order asked. A bare integer list is what the
    // published schema says and what a real key would answer 400 to if the
    // schema were right about this one.
    assert_eq!(
        seen.body, r#"{"fingerprints":["3623028036","1540447798"]}"#,
        "the fingerprint body changed shape"
    );

    let key = seen
        .headers
        .iter()
        .find(|h| h.to_ascii_lowercase().starts_with("x-api-key:"))
        .unwrap_or_else(|| panic!("no x-api-key header; got {:#?}", seen.headers));
    assert_eq!(
        key.split_once(':').map(|(_, v)| v.trim()),
        Some(FAKE_KEY),
        "the key was altered before reaching the socket"
    );
}

/// The failure this closes is silent: a response is neither ordered by the
/// request nor keyed to it, so correlating by position attaches one mod's ids
/// to another's jar and produces a pack that installs the wrong mods and looks
/// perfectly fine.
#[tokio::test]
async fn matches_are_correlated_by_fingerprint_not_by_position() {
    // Listed in an order the request did not ask for.
    let (base, listener) = serve(format!(
        r#"{{"data":{{"exactMatches":[{},{},{}]}}}}"#,
        a_match("300", 30, 31, r#""https://edge.forgecdn.net/c.jar""#),
        a_match("100", 10, 11, r#""https://edge.forgecdn.net/a.jar""#),
        a_match("200", 20, 21, r#""https://edge.forgecdn.net/b.jar""#),
    ));
    let client = geode_lib::net::client();

    let found = geode_lib::providers::curseforge::match_fingerprints_at(
        &client,
        &base,
        &[100, 200, 300],
        FAKE_KEY,
    )
    .await
    .expect("the stub answers 200");
    listener.join().expect("listener thread");

    assert_eq!(found.len(), 3);
    for (fingerprint, project, file) in [(100, "10", "11"), (200, "20", "21"), (300, "30", "31")] {
        let version = found
            .get(&fingerprint)
            .unwrap_or_else(|| panic!("no match for {fingerprint}"));
        assert_eq!(version.project_id, project, "fingerprint {fingerprint}");
        assert_eq!(version.id, file, "fingerprint {fingerprint}");
    }
}

/// A withdrawn file is not a name for anybody's bytes. Distinct from one whose
/// author turned third-party downloads off, which is a real match that simply
/// cannot be fetched — the test below.
#[tokio::test]
async fn an_unavailable_file_is_not_a_match() {
    let (base, listener) = serve(
        r#"{"data":{"exactMatches":[{"file":{"id":11,"modId":10,"displayName":"Gone",
           "fileName":"gone.jar","fileFingerprint":100,"isAvailable":false,
           "downloadUrl":"https://edge.forgecdn.net/gone.jar"}}]}}"#,
    );
    let client = geode_lib::net::client();

    let found =
        geode_lib::providers::curseforge::match_fingerprints_at(&client, &base, &[100], FAKE_KEY)
            .await
            .expect("the stub answers 200");
    listener.join().expect("listener thread");

    assert!(found.is_empty(), "a withdrawn file was offered as a match");
}

/// `downloadUrl: null` is CurseForge's way of saying the author has turned
/// third-party downloads off. It is still a match — that is how a caller knows
/// why it cannot be referenced — and the reason comes back in words.
///
/// The empty `files` is the point and not an accident: a reader that calls
/// `primary_file()` without looking at `blocked_reason` first must come away
/// with nothing, so there is no entry with an empty URL for it to try to
/// fetch. Keeping the sha1 CurseForge published for a blocked file — which is
/// what the owner's substitution rule needs, see `to_version` in
/// `providers/curseforge.rs` — therefore belongs beside `blocked_reason` and
/// not in here, and this assertion is not in that change's way.
#[tokio::test]
async fn an_author_blocked_file_is_matched_but_carries_its_reason() {
    let (base, listener) = serve(format!(
        r#"{{"data":{{"exactMatches":[{}]}}}}"#,
        a_match("100", 10, 11, "null")
    ));
    let client = geode_lib::net::client();

    let found =
        geode_lib::providers::curseforge::match_fingerprints_at(&client, &base, &[100], FAKE_KEY)
            .await
            .expect("the stub answers 200");
    listener.join().expect("listener thread");

    let version = found.get(&100).expect("still a match");
    assert!(
        version.blocked_reason.is_some(),
        "a file with no download URL has to say why"
    );
    assert!(
        version.files.is_empty(),
        "a file nobody can fetch was handed back with somewhere to fetch it"
    );
}

/// A fingerprint nothing matched comes back in `unmatchedFingerprints`, which
/// is the same fact as its absence from the map. Not an error, and not a 404.
#[tokio::test]
async fn an_unmatched_fingerprint_is_a_miss_not_an_error() {
    let (base, listener) = serve(
        r#"{"data":{"isCacheBuilt":true,"exactMatches":[],"exactFingerprints":[],
           "unmatchedFingerprints":[100]}}"#,
    );
    let client = geode_lib::net::client();

    let found =
        geode_lib::providers::curseforge::match_fingerprints_at(&client, &base, &[100], FAKE_KEY)
            .await
            .expect("a miss is not a failure");
    listener.join().expect("listener thread");

    assert!(found.is_empty());
}

// ---------------------------------------------------------------------------
// Importing a CurseForge pack
// ---------------------------------------------------------------------------

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use geode_lib::instance::content::ContentKind;
use geode_lib::instance::Instance;
use geode_lib::meta::Loader;
use geode_lib::net::Progress;
use geode_lib::paths::Paths;

/// One request, as the stub saw it.
#[derive(Clone)]
struct Asked {
    path: String,
    body: String,
}

/// A stand-in CurseForge, and the CDN its answers point at.
///
/// One listener answers everything an import does — both batch endpoints and
/// every file download — and keeps what it was asked, which is how a test can
/// tell two batched requests from three hundred serial ones.
struct Api {
    base: String,
    asked: Arc<Mutex<Vec<Asked>>>,
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
}

impl Api {
    /// `make_route` is handed the stub's own origin, because what CurseForge
    /// answers has to point its downloads back here.
    fn new<F, R>(make_route: F) -> Self
    where
        F: FnOnce(String) -> R,
        R: Fn(&Asked) -> (u16, Vec<u8>) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let asked: Arc<Mutex<Vec<Asked>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let route = Arc::new(make_route(format!("http://{addr}")));

        let (log, flag) = (asked.clone(), stop.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { break };
                let (log, route) = (log.clone(), route.clone());
                std::thread::spawn(move || answer(stream, &log, route.as_ref()));
            }
        });

        Self {
            base: format!("http://{addr}/v1"),
            asked,
            stop,
            addr,
        }
    }

    /// The same stub, in the `.../v2` shape Modrinth's own `API` constant
    /// has. One listener because one import talks to both.
    fn modrinth(&self) -> String {
        format!("http://{}/v2", self.addr)
    }

    fn paths(&self) -> Vec<String> {
        self.asked
            .lock()
            .expect("lock")
            .iter()
            .map(|a| a.path.clone())
            .collect()
    }

    fn bodies_for(&self, path: &str) -> Vec<String> {
        self.asked
            .lock()
            .expect("lock")
            .iter()
            .filter(|a| a.path == path)
            .map(|a| a.body.clone())
            .collect()
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the accept so the thread sees the flag, rather than holding the
        // port for the rest of the run.
        let _ = TcpStream::connect(self.addr);
    }
}

/// Answer every request on one connection until the client hangs up.
///
/// Kept alive deliberately: reqwest reuses a connection, and a handler that
/// answered once and closed would turn the second batch request into a
/// transport error rather than the thing under test.
fn answer(
    stream: TcpStream,
    log: &Mutex<Vec<Asked>>,
    route: &(dyn Fn(&Asked) -> (u16, Vec<u8>) + Send + Sync),
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut out = stream;
    loop {
        let mut start_line = String::new();
        if reader.read_line(&mut start_line).unwrap_or(0) == 0 {
            return;
        }

        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }

        let request = Asked {
            path: start_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string(),
            body: String::from_utf8_lossy(&body).to_string(),
        };
        log.lock().expect("lock").push(request.clone());

        let (status, answer) = route(&request);
        let head = format!(
            "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            if status == 200 { "OK" } else { "Refused" },
            answer.len()
        );
        if out.write_all(head.as_bytes()).is_err() || out.write_all(&answer).is_err() {
            return;
        }
    }
}

/// A file the stub CurseForge holds, and the project it belongs to.
#[derive(Clone)]
struct Held {
    project_id: u32,
    file_id: u32,
    /// The class CurseForge files the *project* under. A manifest states no
    /// path, so this is the only thing that says which folder the file belongs
    /// in.
    class_id: u32,
    file_name: String,
    /// The bytes served for this file, where the test needs real ones — a
    /// world has to be a zip the import can open and unpack. `None` is the
    /// shared `JAR` body every other file gets.
    bytes: Option<Vec<u8>>,
    /// False for a file whose author turned third-party downloads off, which
    /// CurseForge answers for with a null `downloadUrl`.
    downloadable: bool,
    /// Whether that same author published the identical bytes on Modrinth,
    /// where there is no third-party opt-out to express. The sha1 is the only
    /// thing that can prove it, and it comes from CurseForge's own `hashes`.
    on_modrinth: bool,
}

impl Held {
    /// A mod (class 6), fetchable, named after its file id.
    fn a_mod(project_id: u32, file_id: u32) -> Self {
        Self {
            project_id,
            file_id,
            class_id: 6,
            file_name: format!("mod-{file_id}.jar"),
            bytes: None,
            downloadable: true,
            on_modrinth: false,
        }
    }

    /// The sha1 CurseForge publishes for this file in its `hashes` array, and
    /// the only thing the Modrinth lookup has to go on.
    ///
    /// The real sha1 of the bytes this stub serves for the file, because both
    /// platforms' answers are checked against the bytes that arrive: a made-up
    /// hash would fail the download rather than the thing under test. The
    /// shared `JAR` body means one hash for most files, which these tests
    /// never need to tell apart — that a hit is tied to the hash that asked
    /// for it is pinned where the hashes differ, in `providers::modrinth` and
    /// `install::blocked`.
    fn sha1(&self) -> String {
        match &self.bytes {
            Some(bytes) => {
                use sha1::Digest;
                hex::encode(sha1::Sha1::digest(bytes))
            }
            None => "a7a941dea93a9fd0d1a0a8980df11d40b2cf576c".to_string(),
        }
    }

    /// What the CDN hands over for this file.
    fn body(&self) -> Vec<u8> {
        self.bytes.clone().unwrap_or_else(|| b"JAR".to_vec())
    }

    fn named(mut self, file_name: &str) -> Self {
        self.file_name = file_name.to_string();
        self
    }

    /// Served as these exact bytes, for a file whose content is the thing
    /// under test rather than something to be moved about.
    fn holding(mut self, bytes: Vec<u8>) -> Self {
        self.bytes = Some(bytes);
        self
    }

    fn classed(mut self, class_id: u32) -> Self {
        self.class_id = class_id;
        self
    }

    /// Its author turned third-party downloads off.
    fn blocked(mut self) -> Self {
        self.downloadable = false;
        self
    }

    /// Its author also published these exact bytes on Modrinth.
    fn also_on_modrinth(mut self) -> Self {
        self.on_modrinth = true;
        self
    }

    /// What Modrinth calls its own upload of the same bytes. Deliberately not
    /// CurseForge's name for the file: the file that installs is Modrinth's,
    /// under the name Modrinth has for it.
    fn modrinth_file_name(&self) -> String {
        format!("mr-{}", self.file_name)
    }
}

/// A CurseForge that holds exactly these files, answers both batch endpoints
/// out of them, and serves a file's bytes at the URL it handed out.
///
/// Anything else is a 404, so an import that went back to asking per file —
/// `/v1/mods/{project}/files/{file}` — fails here rather than passing quietly.
fn curseforge_holding(held: Vec<Held>) -> Api {
    Api::new(move |origin| {
        move |asked: &Asked| match asked.path.as_str() {
            "/v1/mods/files" => (200, files_answer(&held, &asked.body, &origin)),
            "/v1/mods" => (200, projects_answer(&held, &asked.body)),
            "/v2/version_files" => (200, hashes_answer(&held, &asked.body, &origin)),
            path if path.starts_with("/cdn/") => (200, cdn_body(&held, path)),
            path if path.starts_with("/mr/") => (200, b"JAR".to_vec()),
            _ => (404, b"{}".to_vec()),
        }
    })
}

/// What the stub's CDN serves at `/cdn/<file name>`: that file's own bytes
/// where it has any of its own, and the shared `JAR` body otherwise.
fn cdn_body(held: &[Held], path: &str) -> Vec<u8> {
    let name = path.trim_start_matches("/cdn/");
    held.iter()
        .find(|h| h.file_name == name)
        .map(|h| h.body())
        .unwrap_or_else(|| b"JAR".to_vec())
}

/// A zip of these entries, in memory: the shape CurseForge publishes a world
/// in, and the only kind of file whose content an import reads.
fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file(*name, options).expect("entry");
        zip.write_all(bytes).expect("write");
    }
    zip.finish().expect("finish").into_inner()
}

/// The ids a batch request asked about, read out of its own body so the answer
/// corresponds to the question.
fn ids_in(body: &str, key: &str) -> Vec<u32> {
    let json: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
    json[key]
        .as_array()
        .unwrap_or_else(|| panic!("no {key} in {body}"))
        .iter()
        .map(|v| v.as_u64().expect("an id") as u32)
        .collect()
}

fn files_answer(held: &[Held], body: &str, origin: &str) -> Vec<u8> {
    let wanted = ids_in(body, "fileIds");
    let files: Vec<String> = held
        .iter()
        .filter(|h| wanted.contains(&h.file_id))
        .map(|h| {
            let url = if h.downloadable {
                format!("\"{origin}/cdn/{}\"", h.file_name)
            } else {
                "null".to_string()
            };
            format!(
                r#"{{"id":{},"modId":{},"displayName":"{}","fileName":"{}","downloadUrl":{url},
                   "hashes":[{{"value":"{}","algo":1}},{{"value":"deadbeef","algo":2}}],
                   "gameVersions":["1.21.1","Fabric"]}}"#,
                h.file_id,
                h.project_id,
                h.file_name,
                h.file_name,
                h.sha1()
            )
        })
        .collect();
    format!(r#"{{"data":[{}]}}"#, files.join(",")).into_bytes()
}

/// Modrinth's bulk hash endpoint, answering out of the same held files: a hit
/// for each one whose author published the identical bytes there.
///
/// The matching jar is listed second and not marked primary, so an import that
/// took the version's primary file would install a sources jar instead of the
/// bytes CurseForge named.
fn hashes_answer(held: &[Held], body: &str, origin: &str) -> Vec<u8> {
    let json: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
    assert_eq!(
        json["algorithm"], "sha1",
        "the hash CurseForge publishes is a sha1, so that is what may be asked about"
    );
    let wanted: Vec<String> = json["hashes"]
        .as_array()
        .expect("hashes")
        .iter()
        .map(|v| v.as_str().expect("a hash").to_string())
        .collect();

    let hits: Vec<String> = held
        .iter()
        .filter(|h| h.on_modrinth && wanted.contains(&h.sha1()))
        .map(|h| {
            format!(
                r#""{}":{{"id":"MR{}","project_id":"MRP{}","name":"{} on Modrinth",
                   "version_number":"1.0.0","version_type":"release",
                   "game_versions":["1.21.1"],"loaders":["fabric"],"downloads":9,"files":[
                     {{"url":"{origin}/mr/sources.jar","filename":"sources.jar",
                       "hashes":{{"sha1":"0000000000000000000000000000000000000000"}},
                       "size":1,"primary":true}},
                     {{"url":"{origin}/mr/{}","filename":"{}",
                       "hashes":{{"sha1":"{}","sha512":"ff"}},"size":3,"primary":false}}
                   ]}}"#,
                h.sha1(),
                h.file_id,
                h.project_id,
                h.file_name,
                h.modrinth_file_name(),
                h.modrinth_file_name(),
                h.sha1(),
            )
        })
        .collect();
    format!("{{{}}}", hits.join(",")).into_bytes()
}

fn projects_answer(held: &[Held], body: &str) -> Vec<u8> {
    let wanted = ids_in(body, "modIds");
    let mut seen: Vec<u32> = Vec::new();
    let mut projects: Vec<String> = Vec::new();
    for h in held.iter().filter(|h| wanted.contains(&h.project_id)) {
        if seen.contains(&h.project_id) {
            continue;
        }
        seen.push(h.project_id);
        projects.push(format!(
            r#"{{"id":{},"name":"Project {}","slug":"p{}","classId":{}}}"#,
            h.project_id, h.project_id, h.project_id, h.class_id
        ));
    }
    format!(r#"{{"data":[{}]}}"#, projects.join(",")).into_bytes()
}

/// A throwaway data directory and an instance to import into.
fn importing(tag: &str) -> (Paths, Instance, PathBuf) {
    let root = std::env::temp_dir().join(format!("geode-cfimport-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch");
    let paths = Paths { root };
    let mut instance = Instance::new(
        "Imported".into(),
        "1.21.1".into(),
        Loader::Fabric,
        "0.16.0".into(),
    );
    instance.id = "imported".into();
    let game = paths.game_dir(&instance.id);
    (paths, instance, game)
}

/// The manifest of a pack holding these `(projectID, fileID, required)`
/// entries.
fn manifest(files: &[(u32, u32, bool)]) -> String {
    let entries: Vec<String> = files
        .iter()
        .map(|(project, file, required)| {
            format!(r#"{{"projectID":{project},"fileID":{file},"required":{required}}}"#)
        })
        .collect();
    format!(
        r#"{{"manifestType":"minecraftModpack","manifestVersion":1,"name":"Test Pack",
           "version":"1.0","minecraft":{{"version":"1.21.1",
           "modLoaders":[{{"id":"fabric-0.16.0","primary":true}}]}},"files":[{}]}}"#,
        entries.join(",")
    )
}

/// A pack zip holding just a manifest, which is all these tests read: an
/// `overrides/` tree is a separate mechanism with its own tests.
fn pack_with(dir: &Path, manifest: &str) -> PathBuf {
    let path = dir.join("pack.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).expect("create"));
    let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
    zip.start_file("manifest.json", options).expect("entry");
    zip.write_all(manifest.as_bytes()).expect("write");
    zip.finish().expect("finish");
    path
}

/// Importing returns what the pack named and the import did not install — a
/// list beside the instance, not instead of it.
async fn import(
    api: &Api,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
) -> geode_lib::error::Result<Vec<String>> {
    geode_lib::instance::modpack::import_into_at(
        &geode_lib::net::client(),
        &api.base,
        &api.modrinth(),
        paths,
        instance,
        archive,
        Some(FAKE_KEY),
        // A pack the user chose themselves syncs nothing: every override file
        // written whole, and a write that fails fails the import.
        &geode_lib::instance::modpack::Overrides::replace(),
        &Progress::silent("import"),
    )
    .await
    .map(|imported| imported.left_out)
}

/// The same, and what the run told the user along the way: a fact that is
/// neither progress nor a failure leaves through `geode://notice`, which a
/// silent handle would drop. The progress the same handle records is not what
/// any of this is about, so only the notices come back.
async fn import_telling(
    api: &Api,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
) -> (geode_lib::error::Result<Vec<String>>, Vec<serde_json::Value>) {
    let told = Arc::new(geode_lib::events::Recorder::default());
    let outcome = geode_lib::instance::modpack::import_into_at(
        &geode_lib::net::client(),
        &api.base,
        &api.modrinth(),
        paths,
        instance,
        archive,
        Some(FAKE_KEY),
        // A pack the user chose themselves syncs nothing: every override file
        // written whole, and a write that fails fails the import.
        &geode_lib::instance::modpack::Overrides::replace(),
        &Progress::new(told.clone(), "import"),
    )
    .await
    .map(|imported| imported.left_out);
    let notices = told
        .events()
        .into_iter()
        .filter(|(topic, _)| topic == "geode://notice")
        .map(|(_, payload)| payload)
        .collect();
    (outcome, notices)
}

/// A pack of three hundred mods used to be three hundred round trips, one per
/// `{projectID, fileID}` pair. It is two now, and the chunk boundary is where
/// the batch endpoint's is.
#[tokio::test]
async fn a_pack_is_resolved_in_one_batch_not_one_call_per_mod() {
    let held: Vec<Held> = (0..300).map(|n| Held::a_mod(1000 + n, 2000 + n)).collect();
    let entries: Vec<(u32, u32, bool)> = (0..300).map(|n| (1000 + n, 2000 + n, true)).collect();

    let api = curseforge_holding(held);
    let (paths, mut instance, game) = importing("batch");
    let archive = pack_with(&paths.root, &manifest(&entries));

    import(&api, &paths, &mut instance, &archive)
        .await
        .expect("the pack installs");

    let bodies = api.bodies_for("/v1/mods/files");
    assert_eq!(
        bodies.len(),
        2,
        "three hundred files took {} requests; two chunks of two hundred is the whole point",
        bodies.len()
    );
    let chunk = |ids: std::ops::Range<u32>| {
        let list: Vec<String> = ids.map(|n| n.to_string()).collect();
        format!(r#"{{"fileIds":[{}]}}"#, list.join(","))
    };
    assert_eq!(bodies[0], chunk(2000..2200), "the first chunk's body");
    assert_eq!(bodies[1], chunk(2200..2300), "the second chunk's body");

    // Nothing asked about one file on its own, which is the shape of the loop
    // this replaced.
    let per_file: Vec<String> = api
        .paths()
        .into_iter()
        .filter(|p| p.starts_with("/v1/mods/") && p != "/v1/mods/files")
        .collect();
    assert!(
        per_file.is_empty(),
        "the import still asks file by file: {per_file:?}"
    );

    let installed = std::fs::read_dir(game.join("mods")).expect("mods").count();
    assert_eq!(installed, 300, "every file in the pack should have arrived");
}

/// The failure this closes was silent: a request that failed was swallowed
/// into a progress note and the import carried on, so a key that stopped
/// working part way through produced an instance quietly short of mods.
#[tokio::test]
async fn a_batch_failure_fails_the_import() {
    let api = Api::new(|_origin| {
        |asked: &Asked| match asked.path.as_str() {
            "/v1/mods/files" => (500, br#"{"error":"nope"}"#.to_vec()),
            _ => (200, br#"{"data":[]}"#.to_vec()),
        }
    });
    let (paths, mut instance, game) = importing("failure");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true), (1001, 2001, true)]));

    let err = import(&api, &paths, &mut instance, &archive)
        .await
        .expect_err("a refused batch must end the import");

    assert!(
        err.to_string().contains("500"),
        "the import has to say CurseForge refused it, not invent a reason: {err}"
    );
    assert!(
        !err.to_string().contains("did not return"),
        "a refused request was read as 'CurseForge does not have these files': {err}"
    );
    assert!(
        instance.content.is_empty() && instance.pack.is_none(),
        "the import claimed a pack it never resolved"
    );
    assert!(
        !game.join("mods").exists(),
        "an instance was left part way through a pack"
    );
}

/// A file the answer left out is a different thing from a refused request, and
/// the user can act on it — so it is named rather than dropped.
///
/// Named *beside* the instance, not instead of it: a two-year-old pack of two
/// hundred mods with one withdrawn file installs the other 199, and an import
/// that came back as a failure for it would invite a retry that makes a second
/// instance. `commands::import_modpack` states that rule.
#[tokio::test]
async fn a_file_the_batch_did_not_return_is_named() {
    // CurseForge holds the first file and knows nothing of the second.
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000).named("present.jar")]);
    let (paths, mut instance, game) = importing("missing");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true), (1001, 2001, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("one withdrawn file is a problem to report, not a failed import");

    assert_eq!(problems.len(), 1, "{problems:?}");
    for named in ["1001", "2001"] {
        assert!(
            problems[0].contains(named),
            "the missing file is only identified by its ids, so both have to be in there: \
             {problems:?}"
        );
    }
    assert!(
        game.join("mods/present.jar").exists(),
        "the rest of the pack should still install"
    );
    assert!(
        instance.pack.is_some() && instance.content.len() == 1,
        "the import that reported a problem still has to have happened: {:?}",
        instance.content
    );
}

/// The live bug: a manifest states no path, and every file was installed as a
/// mod into `mods/`, where a resource pack does nothing at all.
#[tokio::test]
async fn a_resource_pack_from_a_curseforge_pack_lands_in_resourcepacks() {
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000)
        .named("pretty.zip")
        .classed(12)]);
    let (paths, mut instance, game) = importing("resourcepack");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true)]));

    import(&api, &paths, &mut instance, &archive)
        .await
        .expect("the pack installs");

    assert!(
        game.join("resourcepacks/pretty.zip").exists(),
        "a resource pack landed somewhere else"
    );
    assert!(
        !game.join("mods/pretty.zip").exists(),
        "a resource pack was installed as a mod"
    );
    let record = instance
        .content
        .iter()
        .find(|r| r.file_name == "pretty.zip")
        .expect("the pack's file should be recorded");
    assert_eq!(record.kind, ContentKind::ResourcePack);
}

/// Class 17 is a world, which `kind_from_class` did not know and so read as a
/// mod. A world is not content Geode tracks: it goes where a world lives, and
/// nothing calls it a mod.
///
/// And arriving there is not the end of it. The game reads a world as a
/// directory holding `level.dat`, which is what Geode's own writer produces,
/// and CurseForge publishes one as a zip of that directory. The import that
/// only moved the bytes was reporting a success the player could not see — an
/// empty singleplayer list and a file to find and unzip by hand — so the zip
/// is unpacked into the folder the game reads, under the name its author gave
/// it, and the player is told the folder is there.
#[tokio::test]
async fn a_bundled_world_arrives_as_a_folder_the_game_reads() {
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000)
        .named("spawn.zip")
        .classed(17)
        .holding(zip_of(&[
            ("Spawn Town/level.dat", b"NBT"),
            ("Spawn Town/region/r.0.0.mca", b"REGION"),
        ]))]);
    let (paths, mut instance, game) = importing("world");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true)]));

    let (outcome, told) = import_telling(&api, &paths, &mut instance, &archive).await;
    let problems = outcome.expect("the pack installs");

    assert!(
        !game.join("mods/spawn.zip").exists(),
        "a world was installed into the mods folder"
    );
    assert!(
        !instance
            .content
            .iter()
            .any(|r| r.kind == ContentKind::Mod && r.file_name == "spawn.zip"),
        "a world was recorded as a mod: {:?}",
        instance.content
    );

    assert_eq!(
        std::fs::read(game.join("saves/Spawn Town/level.dat")).expect("the world's level.dat"),
        b"NBT",
        "the world is not in the singleplayer list unless its level.dat is"
    );
    assert!(
        game.join("saves/Spawn Town/region/r.0.0.mca").exists(),
        "the world arrived without the rest of itself"
    );
    assert!(
        !game.join("saves/spawn.zip").exists(),
        "the zip was left in saves/ beside the world it became"
    );
    assert!(
        problems.is_empty(),
        "a world that arrived playable was reported as something the player must deal with: \
         {problems:?}"
    );

    // A new save folder appearing in somebody's instance is worth a sentence,
    // and the import's own report is for what it could not do.
    assert_eq!(told.len(), 1, "{told:?}");
    let body = told[0]["body"].as_str().expect("a body");
    assert!(
        body.contains("Spawn Town") && body.contains("saves"),
        "the player has to be told which folder arrived and where: {body}"
    );
}

/// A map published in chapters is several worlds side by side in one archive.
/// The zip is deleted once it has been unpacked, so a world taken out of it
/// and left behind is one the player cannot get back without downloading the
/// pack again — and the notice is the only place they are told what arrived.
#[tokio::test]
async fn every_world_in_one_archive_arrives() {
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000)
        .named("chapters.zip")
        .classed(17)
        .holding(zip_of(&[
            ("Chapter 1/level.dat", b"ONE"),
            ("Chapter 1/region/r.0.0.mca", b"REGION ONE"),
            ("Chapter 2/level.dat", b"TWO"),
            ("Chapter 2/region/r.0.0.mca", b"REGION TWO"),
        ]))]);
    let (paths, mut instance, game) = importing("chapters");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true)]));

    let (outcome, told) = import_telling(&api, &paths, &mut instance, &archive).await;
    let problems = outcome.expect("the pack installs");

    for (folder, region) in [
        ("Chapter 1", &b"REGION ONE"[..]),
        ("Chapter 2", &b"REGION TWO"[..]),
    ] {
        assert_eq!(
            std::fs::read(game.join(format!("saves/{folder}/region/r.0.0.mca")))
                .unwrap_or_else(|e| panic!("{folder}: {e}")),
            region,
            "{folder} is not in the singleplayer list unless its own files are"
        );
    }
    assert!(
        !game.join("saves/chapters.zip").exists(),
        "the zip was left in saves/ beside the worlds it became"
    );
    assert!(problems.is_empty(), "{problems:?}");

    assert_eq!(told.len(), 1, "{told:?}");
    let body = told[0]["body"].as_str().expect("a body");
    assert!(
        body.contains("Chapter 1") && body.contains("Chapter 2"),
        "a world the player was not told about is one they cannot be told about later: {body}"
    );
}

/// A manifest may list the same `{projectID, fileID}` twice, which the import
/// deliberately tolerates: two entries, one file on disk. Unpacking that file
/// twice does not work — the first unpack deletes the zip — and the second
/// attempt's failure used to be turned into advice about a file that was no
/// longer there, which the command then reports as a failed import.
#[tokio::test]
async fn a_world_listed_twice_is_unpacked_once_and_reported_as_nothing() {
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000)
        .named("spawn.zip")
        .classed(17)
        .holding(zip_of(&[("Spawn Town/level.dat", b"NBT")]))]);
    let (paths, mut instance, game) = importing("world-twice");
    let archive = pack_with(
        &paths.root,
        &manifest(&[(1000, 2000, true), (1000, 2000, true)]),
    );

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("the pack installs");

    assert!(game.join("saves/Spawn Town/level.dat").exists());
    assert!(
        problems.is_empty(),
        "a world that arrived playable was reported as something the player must deal with: \
         {problems:?}"
    );
}

/// The same class, on a zip that holds no `level.dat`. That is not a world,
/// whatever CurseForge files it under, and a save folder invented out of a
/// guess would show up in the singleplayer list with nothing in it that the
/// game can open. So it stays exactly as it arrived and the report says so.
#[tokio::test]
async fn a_world_zip_with_no_world_in_it_is_left_as_it_arrived() {
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000)
        .named("spawn.zip")
        .classed(17)
        .holding(zip_of(&[("pack.mcmeta", b"{}")]))]);
    let (paths, mut instance, game) = importing("worldless");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("the pack installs");

    assert!(
        game.join("saves/spawn.zip").exists(),
        "the file did not arrive anywhere"
    );
    assert!(
        !game.join("saves/spawn").exists(),
        "a save folder was invented for a zip holding no world"
    );

    // A world in no content record and no content tab is in nothing at all
    // unless the report names it.
    assert_eq!(
        problems.len(),
        1,
        "a file the import could do nothing with was reported as nothing: {problems:?}"
    );
    assert!(
        problems[0].contains("spawn.zip"),
        "the report has to name the file the player must unzip: {problems:?}"
    );
    assert!(
        problems[0].contains("saves") && problems[0].contains("unzip"),
        "and say where it is and what to do with it: {problems:?}"
    );
}

/// `required: false` is the format saying a file is optional, and `.disabled`
/// is what that means on disk. It arrived switched on before, which is a
/// visible change for anyone importing a pack they already have.
#[tokio::test]
async fn an_unrequired_file_arrives_turned_off() {
    let api = curseforge_holding(vec![
        Held::a_mod(1000, 2000).named("needed.jar"),
        Held::a_mod(1001, 2001).named("optional.jar"),
    ]);
    let (paths, mut instance, game) = importing("optional");
    let archive = pack_with(
        &paths.root,
        &manifest(&[(1000, 2000, true), (1001, 2001, false)]),
    );

    import(&api, &paths, &mut instance, &archive)
        .await
        .expect("the pack installs");

    assert!(
        game.join("mods/optional.jar.disabled").exists(),
        "an optional file arrived switched on"
    );
    assert!(
        !game.join("mods/optional.jar").exists(),
        "it arrived twice, on and off"
    );
    assert!(
        game.join("mods/needed.jar").exists(),
        "a required file was switched off"
    );

    // Records are keyed on the enabled name, which is how provenance survives
    // a toggle.
    let record = instance
        .content
        .iter()
        .find(|r| r.file_name == "optional.jar")
        .unwrap_or_else(|| panic!("keyed on the disabled name: {:?}", instance.content));
    assert!(
        record.dependency,
        "`required: false` still means the pack did not ask for this itself"
    );
}

/// Two fields say a `manifest.json` is a modpack's, and Geode read neither —
/// so any JSON with a `minecraft` object got as far as installing.
#[tokio::test]
async fn a_manifest_that_is_not_a_modpack_is_refused() {
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000)]);
    let (paths, mut instance, _game) = importing("notapack");
    let archive = pack_with(
        &paths.root,
        r#"{"minecraft":{"version":"1.21.1","modLoaders":[]},"name":"Not a pack","files":[]}"#,
    );

    let err = import(&api, &paths, &mut instance, &archive)
        .await
        .expect_err("a manifest that does not say it is a modpack must be refused");
    assert!(
        err.to_string().contains("manifestType"),
        "the refusal should say which field is missing: {err}"
    );
    assert!(
        api.paths().is_empty(),
        "CurseForge was asked about a file in something that is not a pack: {:?}",
        api.paths()
    );

    // The same door, so the preview refuses it too rather than offering an
    // import that cannot work.
    let err = geode_lib::instance::modpack::preview(archive)
        .await
        .expect_err("the preview reads the same manifest");
    assert!(err.to_string().contains("manifestType"), "{err}");
}

/// A file whose author turned third-party downloads off and who published it
/// nowhere else is still refused: the import ends naming it, and the only
/// place that was looked is Modrinth's hash lookup.
#[tokio::test]
async fn an_author_blocked_file_nobody_else_published_is_still_reported() {
    let api = curseforge_holding(vec![
        Held::a_mod(1000, 2000).named("fetchable.jar"),
        Held::a_mod(1001, 2001).named("locked.jar").blocked(),
    ]);
    let (paths, mut instance, game) = importing("blocked");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true), (1001, 2001, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("one blocked file is a problem to report, not a failed import");

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("locked.jar"),
        "the file has to be named so it can be fetched by hand: {problems:?}"
    );
    assert!(
        problems[0].contains("third-party downloads"),
        "and the reason has to be the author's, not a vague failure: {problems:?}"
    );
    assert!(
        problems[0].contains("no file with the same bytes"),
        "and why there was nothing of the author's own to install instead: {problems:?}"
    );
    assert!(
        !game.join("mods/locked.jar").exists(),
        "the blocked file was fetched anyway"
    );
    assert!(
        game.join("mods/fetchable.jar").exists(),
        "the rest of the pack should still install"
    );

    // The two batch endpoints, one download for the one file that may be
    // downloaded, and one hash lookup — the author's own Modrinth releases
    // and nowhere else. No mirror, no guessed CDN address.
    let mut asked = api.paths();
    asked.sort();
    assert_eq!(
        asked,
        vec![
            "/cdn/fetchable.jar",
            "/v1/mods",
            "/v1/mods/files",
            "/v2/version_files"
        ],
        "a second source other than the author's own Modrinth release was tried"
    );
}

/// The owner's decision: a CurseForge file the author will not let anyone else
/// serve may be taken from Modrinth when the hashes prove it is the same file.
/// A hash match means that author published those exact bytes there
/// themselves, where there is no third-party opt-out — so this is the author's
/// own release, not a mirror.
#[tokio::test]
async fn a_blocked_file_the_author_also_published_on_modrinth_comes_from_there() {
    let held = Held::a_mod(1001, 2001)
        .named("locked.jar")
        .blocked()
        .also_on_modrinth();
    let api = curseforge_holding(vec![held.clone()]);
    let (paths, mut instance, game) = importing("substituted");
    let archive = pack_with(&paths.root, &manifest(&[(1001, 2001, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("the pack installs");

    assert!(
        problems.is_empty(),
        "the file installed, so there is nothing left for the user to do: {problems:?}"
    );
    // Modrinth's own upload, under Modrinth's own name for it, and the file
    // whose hash matched rather than the version's primary one.
    assert!(
        game.join("mods").join(held.modrinth_file_name()).is_file(),
        "the author's own Modrinth release did not arrive"
    );
    assert!(
        !game.join("mods/sources.jar").exists(),
        "the version's primary file was installed instead of the one that matched"
    );

    // Modrinth, because that is where the bytes came from.
    let record = instance
        .content
        .iter()
        .find(|r| r.file_name == held.modrinth_file_name())
        .unwrap_or_else(|| panic!("nothing was recorded: {:?}", instance.content));
    assert_eq!(
        record.provider, "modrinth",
        "the provenance has to say where the bytes came from: {record:?}"
    );
    assert_eq!(record.project_id, "MRP1001");
    assert_eq!(record.version_id, "MR2001");
    assert_eq!(record.kind, ContentKind::Mod);

    // Asked by the sha1 CurseForge published for its own file, and fetched
    // from Modrinth's URL. Nothing reached CurseForge's CDN.
    let bodies = api.bodies_for("/v2/version_files");
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    assert!(
        bodies[0].contains(&held.sha1()),
        "the hash asked about has to be the one CurseForge gave: {bodies:?}"
    );
    let fetched: Vec<String> = api
        .paths()
        .into_iter()
        .filter(|p| p.starts_with("/cdn/") || p.starts_with("/mr/"))
        .collect();
    assert_eq!(
        fetched,
        vec![format!("/mr/{}", held.modrinth_file_name())],
        "the bytes came from somewhere other than the author's Modrinth release"
    );
}

/// A rate limit is not the author's decision. Read as "they never published it
/// there" it would refuse a file that installs perfectly well a minute later,
/// in words that blame somebody who did nothing.
#[tokio::test]
async fn a_modrinth_lookup_that_fails_is_reported_as_one() {
    let api = Api::new(|origin| {
        let held = vec![Held::a_mod(1001, 2001)
            .named("locked.jar")
            .blocked()
            .also_on_modrinth()];
        move |asked: &Asked| match asked.path.as_str() {
            "/v1/mods/files" => (200, files_answer(&held, &asked.body, &origin)),
            "/v1/mods" => (200, projects_answer(&held, &asked.body)),
            "/v2/version_files" => (429, br#"{"error":"slow down"}"#.to_vec()),
            _ => (404, b"{}".to_vec()),
        }
    });
    let (paths, mut instance, game) = importing("ratelimited");
    let archive = pack_with(&paths.root, &manifest(&[(1001, 2001, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("a lookup nobody answered is one file's problem, not the import's");

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("could not be asked"),
        "a refused lookup has to read as one: {problems:?}"
    );
    assert!(
        !problems[0].contains("no file with the same bytes"),
        "a refused lookup was reported as Modrinth not having the file: {problems:?}"
    );
    assert!(
        !game.join("mods").join("mr-locked.jar").exists(),
        "something was installed on the strength of an answer nobody gave"
    );
}

/// The folder is Geode's to choose and the file name is CurseForge's, so what
/// they join into is held to the same rule as a path out of somebody else's
/// archive. A name that climbs out of the instance installs nothing, and is
/// refused before anything is fetched.
#[tokio::test]
async fn a_file_name_that_climbs_out_of_the_instance_installs_nothing() {
    // This one's stub serves bytes at *any* address it is asked for, unlike
    // the shared one: a climbing name normalises to an address outside
    // `/cdn/`, and a 404 there would end the import for the wrong reason and
    // pass this test without the check it is about.
    let api = Api::new(|origin| {
        let held = vec![Held::a_mod(1000, 2000).named("../../escaped.jar")];
        move |asked: &Asked| match asked.path.as_str() {
            "/v1/mods/files" => (200, files_answer(&held, &asked.body, &origin)),
            "/v1/mods" => (200, projects_answer(&held, &asked.body)),
            _ => (200, b"JAR".to_vec()),
        }
    });
    let (paths, mut instance, game) = importing("escape");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("a name Geode will not write is one file's problem, not the import's");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("escaped.jar"),
        "the refusal should name the file: {problems:?}"
    );

    let climbed = game.parent().expect("instance dir").join("escaped.jar");
    assert!(!climbed.exists(), "a pack wrote outside the game directory");

    // Only the two batch endpoints were reached: nothing was fetched before
    // the name was looked at.
    let fetched: Vec<String> = api
        .paths()
        .into_iter()
        .filter(|p| !p.starts_with("/v1/"))
        .collect();
    assert!(
        fetched.is_empty(),
        "the file was requested before its name was looked at: {fetched:?}"
    );
}

/// The whole point of reading the class is that a file goes where something
/// reads it — and the one message whose job is to tell the user what to do by
/// hand said `mods/` whatever the file was. Author opt-outs are common on
/// texture and shader projects, so a blocked resource pack is not a corner
/// case; it is this commit's own bug moved into the instructions.
#[tokio::test]
async fn an_author_blocked_file_is_reported_against_its_own_folder() {
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000)
        .named("prettier.zip")
        .classed(12)
        .blocked()]);
    let (paths, mut instance, _game) = importing("blockedclass");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("one blocked file is a problem to report, not a failed import");

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("resourcepacks"),
        "the advice names a folder, and the class already said which one: {problems:?}"
    );
    assert!(
        !problems[0].contains("mods folder"),
        "a resource pack dropped into mods/ is not read, and may fail a Forge mod scan: \
         {problems:?}"
    );
}

/// `plugins/` is a server's folder. No Minecraft client reads it, the instance
/// content tab never asks for that kind, and both `providers/mod.rs` and
/// `instance/content.rs` say so in as many words — so a class-5 file routed
/// there is a file the user cannot find, toggle or delete from inside Geode,
/// with nothing reported. Class 5 says nothing useful about a *client*, which
/// is the `mods/` case: visible, and what Prism leaves such a file as.
#[tokio::test]
async fn a_server_plugin_in_a_pack_stays_where_it_can_be_seen() {
    let api = curseforge_holding(vec![Held::a_mod(1000, 2000)
        .named("gamemodes.jar")
        .classed(5)]);
    let (paths, mut instance, game) = importing("plugin");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("the pack installs");
    assert!(problems.is_empty(), "{problems:?}");

    assert!(
        !game.join("plugins").exists(),
        "a pack made an instance a plugins folder, which nothing in Geode or the game reads"
    );
    assert!(
        game.join("mods/gamemodes.jar").exists(),
        "the file went somewhere else entirely"
    );
    let record = instance
        .content
        .iter()
        .find(|r| r.file_name == "gamemodes.jar")
        .unwrap_or_else(|| panic!("the file should be recorded: {:?}", instance.content));
    assert_eq!(
        record.kind,
        ContentKind::Mod,
        "a record the content tab never asks for is a record nothing can show"
    );
}

/// A manifest is somebody else's JSON, and its `{projectID, fileID}` pair can
/// be wrong — hand-assembled, or stale because the project that owns the file
/// was transferred or merged upstream. The files answer says who owns each
/// file in `modId`, which is already what the ContentRecord stores, so asking
/// about the manifest's claim instead placed the file by one project's class
/// while recording another's. The per-file endpoint this replaced validated
/// the pair server-side; the batch one cannot.
#[tokio::test]
async fn the_folder_comes_from_the_project_the_answer_names() {
    // CurseForge says file 2000 belongs to project 1234, which is a resource
    // pack. The manifest claims project 1000, which it does not hold at all.
    let api = curseforge_holding(vec![Held::a_mod(1234, 2000)
        .named("pretty.zip")
        .classed(12)]);
    let (paths, mut instance, game) = importing("staleid");
    let archive = pack_with(&paths.root, &manifest(&[(1000, 2000, true)]));

    let problems = import(&api, &paths, &mut instance, &archive)
        .await
        .expect("the pack installs");
    assert!(problems.is_empty(), "{problems:?}");

    let asked = api.bodies_for("/v1/mods");
    assert_eq!(
        asked,
        vec![r#"{"modIds":[1234]}"#],
        "the class was looked up by the id the manifest claimed, not the one that owns the file"
    );
    assert!(
        game.join("resourcepacks/pretty.zip").exists(),
        "the file was placed by a project that does not own it"
    );
    assert!(
        !game.join("mods/pretty.zip").exists(),
        "a resource pack was installed as a mod"
    );
    let record = instance
        .content
        .iter()
        .find(|r| r.file_name == "pretty.zip")
        .unwrap_or_else(|| panic!("the file should be recorded: {:?}", instance.content));
    assert_eq!(
        record.project_id, "1234",
        "the record already stored the answer's id, which is how folder and \
         provenance came to describe two different projects"
    );
    assert_eq!(record.kind, ContentKind::ResourcePack);
}

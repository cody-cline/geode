//! Proves what a CurseForge fingerprint lookup puts on the wire, and what it
//! makes of the answer.
//!
//! The request form is the one claim in this code with no primary source
//! behind it: CurseForge's published schema types the fingerprint list
//! `integer[]`, while Prism — which works against the live API — sends decimal
//! strings, and the same schema types `downloadUrl` as non-nullable when it is
//! null for every author-blocked file. So the form is pinned here rather than
//! trusted by eye. These tests bind a listener on localhost, point the real
//! request builder at it and read the raw bytes off the socket. The key is
//! fake, the answers are hand-written, and nothing leaves the machine.

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

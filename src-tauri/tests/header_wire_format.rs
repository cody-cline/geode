//! Proves the CurseForge key reaches the wire byte-for-byte.
//!
//! A bcrypt key is full of `$`, and every layer between the config file and the
//! socket is a chance to mangle it. This binds a listener on localhost, points
//! the real request builder at it, and reads the raw bytes off the socket. The
//! key here is fake and nothing leaves the machine.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

const FAKE_KEY: &str = "$2a$10$Abc./123Abc./123Abc./123Abc./123Abc./123Abc./123Abc./";

#[tokio::test]
async fn api_key_header_survives_dollar_signs() {
    assert_eq!(FAKE_KEY.len(), 60, "test fixture should match a real key's shape");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let captured = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(&stream);
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line == "\r\n" {
                break;
            }
            headers.push(line.trim_end().to_string());
        }
        let mut stream = stream;
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 22\r\n\r\n{\"data\":[],\"pagination\":{}}",
        );
        headers
    });

    let client = geode_lib::net::client();
    let base = format!("http://{addr}/v1");
    let result =
        geode_lib::providers::curseforge::check_key_at(&client, &base, FAKE_KEY, "test").await;

    let headers = captured.join().expect("listener thread");

    let api_key_header = headers
        .iter()
        .find(|h| h.to_ascii_lowercase().starts_with("x-api-key:"))
        .unwrap_or_else(|| panic!("no x-api-key header sent; got {headers:#?}"));

    let sent = api_key_header
        .split_once(':')
        .map(|(_, value)| value.trim())
        .expect("header value");

    assert_eq!(sent, FAKE_KEY, "the key was altered before reaching the socket");

    assert!(
        headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case("accept: application/json")),
        "missing Accept header; got {headers:#?}"
    );

    // A 200 from the stub must be reported as success, so a real 403 is
    // genuinely CurseForge's verdict and not our own parsing.
    assert!(result.ok, "stubbed 200 should read as success: {result:?}");
    assert_eq!(result.status, 200);

    // The fingerprint must never carry the payload.
    assert!(!result.fingerprint.contains("Abc./123"), "{}", result.fingerprint);
}

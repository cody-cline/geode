//! `geode://join` links: everything a player needs to fetch a server's pack.
//!
//! A link carries three things, and nothing else:
//!
//! - **where** — the admin's address: `host:port` straight to their machine,
//!   or an `https://` URL they put in front of it (a domain, a tunnel);
//! - **which** — the share code, which is also the secret that lets you in;
//! - **who** — for a `host:port`, the SHA-256 fingerprint of the certificate
//!   that machine serves, so the player's Geode talks only to that machine.
//!   An `https://` address is verified the ordinary way instead, by its
//!   certificate authority, and carries no fingerprint.
//!
//! ```text
//! geode://join?addr=203.0.113.7:25590&code=7K2QF9XWMP&pin=<64 hex>
//! geode://join?addr=https://pack.example.com&code=7K2QF9XWMP
//! ```
//!
//! A link arrives from a web page, a chat message, or another process on the
//! machine, so it is hostile until parsed. [`Link::parse`] is strict: the
//! `geode` scheme, the `join` host, the three known parameters at most once
//! each, a code that passes [`Code::parse`], and no character that could make
//! a platform split it into two arguments on the way in.

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::code::Code;

/// The scheme links are registered under.
pub const SCHEME: &str = "geode";

/// The longest link or address read, in bytes.
///
/// A link Geode writes is a few hundred bytes. The cap is there so that a
/// pasted or forwarded megabyte is refused before any of it is looked at.
const MAX_LEN: usize = 2048;

/// Where a server's pack is served from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Address {
    /// Straight to the admin's machine. Always verified by [`Pin`].
    Direct { host: String, port: u16 },
    /// Through something the admin put in front of it, verified by its
    /// certificate authority.
    Https { url: String },
}

/// The SHA-256 fingerprint of a sharing machine's certificate.
///
/// Public — anyone who connects is shown the certificate — so it proves who a
/// server is, not who may download from it. That is what the code is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pin(pub [u8; 32]);

/// A parsed, validated link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub address: Address,
    pub code: Code,
    /// Required for [`Address::Direct`] when the link came from an admin;
    /// `None` only for an address somebody typed, which is trusted on first
    /// use and pinned from then on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<Pin>,
}

impl Pin {
    /// Lowercase hex, 64 characters.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Exactly 64 hex characters, either case. Anything else is `None`.
    pub fn from_hex(input: &str) -> Option<Self> {
        let mut bytes = [0u8; 32];
        // Refuses any length but 64 and any byte that is not a hex digit, so
        // there is no prefix, separator or padding it quietly steps over.
        hex::decode_to_slice(input, &mut bytes).ok()?;
        Some(Self(bytes))
    }
}

impl Serialize for Pin {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Pin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Pin::from_hex(&text).ok_or_else(|| serde::de::Error::custom("not a certificate fingerprint"))
    }
}

impl Address {
    /// What an admin typed as their public address, or a player typed as the
    /// server's: `host`, `host:port`, `[v6]:port`, or `https://…`.
    ///
    /// A bare host takes `default_port`. Rejects whitespace, control
    /// characters, `|`, userinfo in a URL, any scheme other than `https`, and
    /// a port of 0.
    ///
    /// Also rejects anything outside printable ASCII: nothing Geode writes
    /// contains it, and it is where the invisible and right-to-left characters
    /// that disguise an address live. An IPv6 address must be bracketed even
    /// without a port, because `2001:db8::1:25590` may or may not end in one
    /// and guessing wrong connects somewhere else. A host name must be a DNS
    /// name that a URL parser reads the same way, which rules out `1.2.3` —
    /// shown as a name, connected to as `1.2.0.3`.
    ///
    /// What comes back is normalised, so one server is always written one way:
    /// host names in lower case, IP addresses in their canonical form, and an
    /// `https://` URL as the URL parser writes it, without a trailing slash.
    pub fn parse(input: &str, default_port: u16) -> Result<Self> {
        parse_address(input, Some(default_port))
    }

    /// The base URL requests are made against: `https://host:port` or the
    /// `https://` URL itself without a trailing slash.
    pub fn base_url(&self) -> String {
        match self {
            Address::Direct { .. } => format!("https://{}", self.display()),
            Address::Https { url } => url.trim_end_matches('/').to_string(),
        }
    }

    /// As a person should see it: `203.0.113.7:25590`, or the URL.
    pub fn display(&self) -> String {
        match self {
            // An IPv6 address is full of colons, so the brackets are what keep
            // the port apart from it.
            Address::Direct { host, port } if host.contains(':') => format!("[{host}]:{port}"),
            Address::Direct { host, port } => format!("{host}:{port}"),
            Address::Https { url } => url.clone(),
        }
    }
}

impl Link {
    /// Parse a `geode://join?…` link. See the module docs for the rules.
    ///
    /// A [`Address::Direct`] link without a `pin` is rejected — a link an admin
    /// copied always has one, so a missing one is a link that was tampered
    /// with or hand-built. An [`Address::Https`] link with a `pin` is rejected
    /// too, rather than silently ignoring half of what it says.
    ///
    /// For the same reason a direct address in a link must carry its port:
    /// the link does not get to lean on whatever port this machine would
    /// default to.
    pub fn parse(input: &str) -> Result<Self> {
        if input.len() > MAX_LEN {
            return Err(Error::msg("this link is too long to be a Geode link"));
        }
        if !is_plain(input) {
            return Err(Error::msg("this link contains characters a Geode link never has"));
        }

        // The front is compared as text rather than read back from a parsed
        // URL, because a URL parser forgives things a link Geode wrote never
        // has — `geode://@join`, `geode://join:` and `geode://join/.` all come
        // back as plain `join` — and each thing forgiven is one more way to
        // dress a link up as something else.
        let (front, query) = input.split_once('?').unwrap_or((input, ""));
        let front = front.strip_suffix('/').unwrap_or(front);
        if !front.eq_ignore_ascii_case(&format!("{SCHEME}://join")) {
            return Err(Error::msg("this is not a Geode join link"));
        }

        // No separate check for a `#`: it can only land in a key, which is
        // then unknown, or in a value, which its own rules refuse.
        let (mut addr, mut code, mut pin) = (None, None, None);
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            // Decoded values answer to the same rule as the raw link, or `%20`
            // and `%7C` would carry in exactly what the rule keeps out.
            if !is_plain(&value) {
                return Err(Error::msg("this link contains characters a Geode link never has"));
            }
            let slot = match key.as_ref() {
                "addr" => &mut addr,
                "code" => &mut code,
                "pin" => &mut pin,
                _ => return Err(Error::msg("this link has a part a Geode link never has")),
            };
            // Refused rather than resolved: whichever copy this keeps, the
            // next thing to read the link might keep the other.
            if slot.replace(value).is_some() {
                return Err(Error::msg(format!("this link gives its {key} more than once")));
            }
        }

        let addr = addr.ok_or_else(|| Error::msg("this link has no server address"))?;
        let code = code.ok_or_else(|| Error::msg("this link has no password"))?;
        let address = parse_address(&addr, None)
            .map_err(|e| Error::msg(format!("this link's server address is not valid: {e}")))?;
        let code = Code::parse(&code).ok_or_else(|| Error::msg("this link's password is not valid"))?;
        let pin = pin
            .map(|hex| {
                Pin::from_hex(&hex).ok_or_else(|| Error::msg("this link's certificate fingerprint is not valid"))
            })
            .transpose()?;

        match (&address, &pin) {
            (Address::Direct { .. }, None) => Err(Error::msg(
                "this link is missing the server's certificate fingerprint, so there is no telling \
                 it leads to the right machine — ask for the link again",
            )),
            (Address::Https { .. }, Some(_)) => Err(Error::msg(
                "this link has both an https:// address and a certificate fingerprint, \
                 which a link Geode writes never does",
            )),
            _ => Ok(Link { address, code, pin }),
        }
    }

    /// What a person typed: an address and a code, and no fingerprint.
    pub fn from_parts(address: &str, code: &str, default_port: u16) -> Result<Self> {
        let address = Address::parse(address, default_port)?;
        let code = Code::parse(code).ok_or_else(|| Error::msg("that is not a valid password — it looks like GEODE-XXXXX-XXXXX"))?;
        Ok(Link { address, code, pin: None })
    }

    /// The `geode://join?…` form. Round-trips through [`Link::parse`].
    ///
    /// Only for a link [`Link::parse`] would accept: a direct address with its
    /// pin, or an `https://` one without. A typed address that has not been
    /// pinned yet writes out a link that parse refuses, on purpose.
    pub fn to_url(&self) -> String {
        let mut url = format!(
            "{SCHEME}://join?addr={}&code={}",
            encode_value(&self.address.display()),
            encode_value(self.code.raw()),
        );
        if let Some(pin) = &self.pin {
            url.push_str("&pin=");
            url.push_str(&pin.to_hex());
        }
        url
    }
}

/// [`Address::parse`], where `default_port: None` means the port is required.
fn parse_address(input: &str, default_port: Option<u16>) -> Result<Address> {
    if input.is_empty() {
        return Err(Error::msg("the address is empty"));
    }
    if input.len() > MAX_LEN {
        return Err(Error::msg("the address is too long"));
    }
    if !is_plain(input) {
        return Err(Error::msg("the address contains spaces or characters an address never has"));
    }

    if let Some((scheme, rest)) = input.split_once("://") {
        if !scheme.eq_ignore_ascii_case("https") {
            return Err(Error::msg(
                "only an https:// address can be used — or give the address alone, as host:port",
            ));
        }
        return parse_https(input, rest);
    }

    let (host, port) = split_host_port(input)?;
    match port.or(default_port) {
        Some(0) => Err(bad_port()),
        Some(port) => Ok(Address::Direct { host, port }),
        None => Err(Error::msg("the address has no port")),
    }
}

/// An `https://` address, `rest` being everything after the `://`.
fn parse_https(input: &str, rest: &str) -> Result<Address> {
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    // Checked on the text, not the parsed URL: the parser drops an empty `@`
    // without a trace. And an address that shows a familiar name before an
    // `@` while connecting to whatever follows it is a well-worn disguise.
    if authority.contains('@') {
        return Err(Error::msg("the address must not contain a user name or password"));
    }
    // The same host and port rules as a direct address, so a URL cannot carry
    // a host past them that `host:port` could not.
    split_host_port(authority)?;

    let url = url::Url::parse(input).map_err(|_| Error::msg("that is not a valid https:// address"))?;
    // Requests are made by appending a path to this, which would land inside
    // a query or fragment.
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::msg("an https:// address cannot have a ? or # part"));
    }
    Ok(Address::Https {
        url: url.as_str().trim_end_matches('/').to_string(),
    })
}

/// `host`, `host:port`, `[v6]` or `[v6]:port`, with the host normalised and
/// the port checked.
fn split_host_port(input: &str) -> Result<(String, Option<u16>)> {
    let (host, port) = match input.strip_prefix('[') {
        Some(rest) => {
            let (inside, after) = rest.split_once(']').ok_or_else(bad_host)?;
            let ip: Ipv6Addr = inside.parse().map_err(|_| bad_host())?;
            let port = match after {
                "" => None,
                _ => Some(after.strip_prefix(':').ok_or_else(bad_host)?),
            };
            (ip.to_string(), port)
        }
        None => {
            if input.matches(':').count() > 1 {
                return Err(Error::msg("an IPv6 address needs brackets, like [2001:db8::1]:25590"));
            }
            let (host, port) = match input.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (input, None),
            };
            (normalise_host(host)?, port)
        }
    };
    Ok((host, port.map(parse_port).transpose()?))
}

/// An IPv4 address or a DNS name, in the one form it is always written.
fn normalise_host(host: &str) -> Result<String> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(ip.to_string());
    }
    let name = host.to_ascii_lowercase();
    let is_dns_name = name.len() <= 253
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        });
    // Requests go out through a URL parser, which reads a name ending in a
    // number as an IPv4 address: `1.2.3` becomes 1.2.0.3 and `0x7f.1` becomes
    // 127.0.0.1. A name it would read as anything but itself is shown as one
    // machine and connected to as another, so it is refused.
    let read_back_the_same = url::Url::parse(&format!("https://{name}/"))
        .is_ok_and(|url| url.host() == Some(url::Host::Domain(name.as_str())));
    if is_dns_name && read_back_the_same {
        Ok(name)
    } else {
        Err(bad_host())
    }
}

/// Digits only, 1 to 65535. `u16::from_str` alone would take `+80`.
fn parse_port(text: &str) -> Result<u16> {
    Some(text)
        .filter(|text| text.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|text| text.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(bad_port)
}

fn bad_host() -> Error {
    Error::msg("that is not a valid host name or IP address")
}

fn bad_port() -> Error {
    Error::msg("the port must be a number from 1 to 65535")
}

/// Printable ASCII, without `|`.
///
/// That leaves out whitespace and control characters, which are how one
/// argument becomes two on the way into a process; `|`, which the Windows
/// single-instance plugin splits forwarded arguments on; and everything past
/// ASCII, which nothing Geode writes contains.
fn is_plain(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_graphic() && b != b'|')
}

/// Percent-encode a query value, leaving as they are only the characters that
/// cannot change what a query means: letters, digits, `-._~`, and the `:` and
/// `/` an address is made of. So `addr=203.0.113.7:25590` and
/// `addr=https://pack.example.com` stay readable, and a `&`, `=`, `+`, `%` or
/// `#` in a URL's path cannot end the value early or turn into something else
/// on the way back.
fn encode_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~:/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "7K2QF9XWMP";

    /// SHA-256 of nothing: a real fingerprint, and one whose bytes all differ
    /// enough that a hex routine which reversed or shuffled them would show.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn pin() -> Pin {
        Pin::from_hex(EMPTY_SHA256).unwrap()
    }

    fn code() -> Code {
        Code::parse(CODE).unwrap()
    }

    fn direct(host: &str, port: u16) -> Address {
        Address::Direct { host: host.into(), port }
    }

    fn https(url: &str) -> Address {
        Address::Https { url: url.into() }
    }

    /// A well-formed direct link with `addr` spliced in as written.
    fn direct_link(addr: &str) -> String {
        format!("geode://join?addr={addr}&code={CODE}&pin={EMPTY_SHA256}")
    }

    fn assert_address_refused(input: &str, default_port: u16) {
        let parsed = Address::parse(input, default_port);
        assert!(parsed.is_err(), "{input:?} should be refused, got {parsed:?}");
    }

    fn assert_link_refused(input: &str) {
        let parsed = Link::parse(input);
        assert!(parsed.is_err(), "{input:?} should be refused, got {parsed:?}");
    }

    #[test]
    fn a_pin_is_written_as_lowercase_hex_and_read_back_in_either_case() {
        let pin = pin();
        assert_eq!(pin.0[..4], [0xe3, 0xb0, 0xc4, 0x42]);
        assert_eq!(pin.to_hex(), EMPTY_SHA256);
        assert_eq!(Pin::from_hex(&EMPTY_SHA256.to_uppercase()), Some(pin));
    }

    #[test]
    fn anything_but_exactly_64_hex_characters_is_not_a_pin() {
        let separated = EMPTY_SHA256
            .as_bytes()
            .chunks(2)
            .map(|pair| std::str::from_utf8(pair).unwrap())
            .collect::<Vec<_>>()
            .join(":");
        for junk in [
            String::new(),
            EMPTY_SHA256[..63].to_string(),
            format!("{EMPTY_SHA256}0"),
            format!("0x{}", &EMPTY_SHA256[..62]),
            format!("{}g", &EMPTY_SHA256[..63]),
            format!(" {}", &EMPTY_SHA256[..63]),
            // 64 bytes, but not 64 hex digits.
            format!("{}é", &EMPTY_SHA256[..62]),
            separated,
        ] {
            assert_eq!(Pin::from_hex(&junk), None, "{junk:?} should not be a pin");
        }
    }

    #[test]
    fn a_pin_serialises_as_its_hex_and_refuses_anything_else() {
        let json = serde_json::to_string(&pin()).unwrap();
        assert_eq!(json, format!("\"{EMPTY_SHA256}\""));
        assert_eq!(serde_json::from_str::<Pin>(&json).unwrap(), pin());
        assert!(serde_json::from_str::<Pin>("\"e3b0\"").is_err());
        assert!(serde_json::from_str::<Pin>("[227, 176]").is_err());
    }

    #[test]
    fn addresses_people_type_are_read_and_normalised() {
        for (input, expected) in [
            ("203.0.113.7", direct("203.0.113.7", 4242)),
            ("203.0.113.7:25590", direct("203.0.113.7", 25590)),
            ("localhost", direct("localhost", 4242)),
            ("Play.Example.COM:30000", direct("play.example.com", 30000)),
            ("a-1.b2.example:1", direct("a-1.b2.example", 1)),
            ("mc.example.com:65535", direct("mc.example.com", 65535)),
            ("[2001:DB8:0:0::1]:25590", direct("2001:db8::1", 25590)),
            ("[::1]", direct("::1", 4242)),
            ("https://pack.example.com", https("https://pack.example.com")),
            ("HTTPS://Pack.Example.com/", https("https://pack.example.com")),
            ("https://pack.example.com:443", https("https://pack.example.com")),
            ("https://pack.example.com:8443/geode/", https("https://pack.example.com:8443/geode")),
            ("https://[2001:db8::1]:8443", https("https://[2001:db8::1]:8443")),
            ("https://203.0.113.7", https("https://203.0.113.7")),
        ] {
            assert_eq!(Address::parse(input, 4242).unwrap(), expected, "reading {input:?}");
        }
    }

    /// A normalised address is a fixed point: reading what it displays gives
    /// it back, so a stored address and a retyped one compare equal.
    #[test]
    fn a_normalised_address_reads_back_as_itself() {
        for input in [
            "Play.Example.COM:30000",
            "[2001:DB8:0:0::1]:25590",
            "[::FFFF:1.2.3.4]:9",
            "HTTPS://Pack.Example.com:8443/a/b//",
            "https://[::ffff:1.2.3.4]:9",
        ] {
            let once = Address::parse(input, 4242).unwrap();
            assert_eq!(Address::parse(&once.display(), 1).unwrap(), once, "re-reading {input:?}");
        }
    }

    #[test]
    fn a_bare_ipv6_address_is_refused_rather_than_guessed_at() {
        for input in [
            "::1",
            "2001:db8::1",
            // A whole address, or 2001:db8::1 on port 25590? No telling.
            "2001:db8::1:25590",
            "[fe80::1%eth0]:25590",
            "[::1",
            "[::1]25590",
            "[::1]:",
            "[example.com]:25590",
            "[203.0.113.7]:25590",
        ] {
            assert_address_refused(input, 4242);
        }
        // Refused with how to write it instead, since the address was real.
        let error = Address::parse("2001:db8::1", 4242).unwrap_err().to_string();
        assert!(error.contains("brackets"), "unhelpful refusal: {error}");
    }

    #[test]
    fn a_port_is_a_number_from_1_to_65535_and_never_0() {
        for input in [
            "203.0.113.7:0",
            "203.0.113.7:00",
            "203.0.113.7:65536",
            "203.0.113.7:99999999999",
            "203.0.113.7:+80",
            "203.0.113.7:-1",
            "203.0.113.7:",
            "203.0.113.7:8o",
            "[::1]:0",
            "https://pack.example.com:0",
            "https://pack.example.com:+443",
        ] {
            assert_address_refused(input, 4242);
        }
        // A default of 0 is no port either, however it came to be configured.
        assert_address_refused("203.0.113.7", 0);
        assert_eq!(Address::parse("203.0.113.7:1", 0).unwrap(), direct("203.0.113.7", 1));
    }

    #[test]
    fn a_host_name_must_be_a_dns_name() {
        let label = "a".repeat(63);
        let longest = format!("{label}.{label}.{label}.{}", "a".repeat(61));
        assert_eq!(longest.len(), 253);
        assert_eq!(Address::parse(&longest, 1).unwrap(), direct(&longest, 1));
        assert_eq!(
            Address::parse(&format!("{label}.com"), 1).unwrap(),
            direct(&format!("{label}.com"), 1)
        );

        for input in [
            String::new(),
            ":25590".into(),
            format!("{longest}a"),
            format!("{label}a.com"),
            "under_score.example.com".into(),
            "-leading.example.com".into(),
            "trailing-.example.com".into(),
            "two..dots".into(),
            ".leading.dot".into(),
            "trailing.dot.".into(),
            "dollar$.example.com".into(),
            "percent%2e.example.com".into(),
            "bücher.example".into(),
        ] {
            assert_address_refused(&input, 4242);
            assert_address_refused(&format!("https://{input}"), 4242);
        }
        // A path is fine after https://, but a direct address has nowhere to put one.
        assert_address_refused("pack.example.com/geode", 4242);
    }

    /// Requests go out through a URL parser that reads names ending in a
    /// number as IPv4 addresses. Accepting one would show a player one machine
    /// and connect them to another.
    #[test]
    fn a_host_name_a_url_parser_would_read_as_an_address_is_refused() {
        for input in ["1.2.3", "0x7f.1", "12345", "01.2.3.4", "1.2.3.256", "example.0x10"] {
            assert_address_refused(input, 4242);
            assert_address_refused(&format!("{input}:25590"), 4242);
            assert_address_refused(&format!("https://{input}"), 4242);
        }
    }

    #[test]
    fn only_https_urls_are_accepted_and_never_with_userinfo_a_query_or_a_fragment() {
        for input in [
            "http://pack.example.com",
            "HTTP://pack.example.com",
            "ftp://pack.example.com",
            "geode://join",
            "://pack.example.com",
            "https://user:pass@pack.example.com",
            "https://user@pack.example.com",
            // The URL parser forgets an empty userinfo entirely, which is
            // exactly why it is caught before the parser sees it.
            "https://@pack.example.com",
            "https://:@pack.example.com",
            "https://pack.example.com\\@evil.example",
            "https://pack.example.com/?",
            "https://pack.example.com/?code=7K2QF9XWMP",
            "https://pack.example.com/#",
            "https://pack.example.com#top",
            "https://",
            "https:///pack.example.com",
        ] {
            assert_address_refused(input, 4242);
        }
        // Said plainly, since whoever typed it may think the login is needed.
        for input in ["https://@pack.example.com", "https://user:pass@pack.example.com"] {
            let error = Address::parse(input, 4242).unwrap_err().to_string();
            assert!(error.contains("user name or password"), "unhelpful refusal for {input:?}: {error}");
        }
    }

    #[test]
    fn characters_that_never_belong_in_an_address_are_refused() {
        for input in [
            " 203.0.113.7",
            "203.0.113.7 ",
            "203.0.113.7\t",
            "203.0.113.7\n",
            "pack.example.com|evil",
            "https://pack.example.com/a|b",
            "https://pack.example.com/a b",
            "\0",
            "pack.example.com\u{7f}",
            // Right-to-left override and a no-break space: invisible, or
            // nearly, and both a way to make one address look like another.
            "\u{202e}moc.elpmaxe.kcap",
            "pack\u{a0}example.com",
            "https://pack.example.com/p\u{e2}ck",
        ] {
            assert_address_refused(input, 4242);
        }
    }

    #[test]
    fn an_address_is_capped_in_length() {
        let prefix = "https://pack.example.com/";
        let at_cap = format!("{prefix}{}", "a".repeat(MAX_LEN - prefix.len()));
        assert_eq!(at_cap.len(), MAX_LEN);
        assert_eq!(Address::parse(&at_cap, 1).unwrap(), https(&at_cap));
        assert_address_refused(&format!("{at_cap}a"), 1);
    }

    #[test]
    fn an_address_is_shown_and_requested_with_its_port_kept_apart() {
        let v4 = direct("203.0.113.7", 25590);
        assert_eq!(v4.display(), "203.0.113.7:25590");
        assert_eq!(v4.base_url(), "https://203.0.113.7:25590");

        let v6 = direct("2001:db8::1", 25590);
        assert_eq!(v6.display(), "[2001:db8::1]:25590");
        assert_eq!(v6.base_url(), "https://[2001:db8::1]:25590");

        let name = direct("play.example.com", 30000);
        assert_eq!(name.base_url(), "https://play.example.com:30000");

        let url = https("https://pack.example.com/geode");
        assert_eq!(url.display(), "https://pack.example.com/geode");
        assert_eq!(url.base_url(), "https://pack.example.com/geode");
        // A URL that reached here some way other than parsing still yields
        // a base that a path can be appended to.
        assert_eq!(https("https://pack.example.com/").base_url(), "https://pack.example.com");
    }

    /// The two forms in the module docs, written exactly as documented.
    #[test]
    fn a_link_is_written_the_way_the_module_docs_show() {
        let direct = Link { address: direct("203.0.113.7", 25590), code: code(), pin: Some(pin()) };
        assert_eq!(
            direct.to_url(),
            format!("geode://join?addr=203.0.113.7:25590&code=7K2QF9XWMP&pin={EMPTY_SHA256}")
        );

        let domain = Link { address: https("https://pack.example.com"), code: code(), pin: None };
        assert_eq!(domain.to_url(), "geode://join?addr=https://pack.example.com&code=7K2QF9XWMP");
    }

    #[test]
    fn links_round_trip_through_their_own_url() {
        for (address, pin) in [
            ("203.0.113.7:25590", Some(pin())),
            ("play.example.com:30000", Some(pin())),
            ("[2001:db8::1]:25590", Some(pin())),
            ("[::ffff:203.0.113.7]:25590", Some(pin())),
            ("https://pack.example.com", None),
            ("https://pack.example.com:8443/geode", None),
            // A path holding everything that means something in a query: if
            // any of it went out unencoded, it would end or change the value.
            ("https://pack.example.com/a&code=0000000000=1+2%25;x", None),
        ] {
            let link = Link { address: Address::parse(address, 1).unwrap(), code: code(), pin };
            let url = link.to_url();
            assert_eq!(Link::parse(&url).unwrap(), link, "{address:?} went out as {url:?}");
        }
    }

    #[test]
    fn a_link_reads_the_same_however_its_values_are_encoded() {
        let expected = Link { address: https("https://pack.example.com"), code: code(), pin: None };
        for input in [
            "geode://join?addr=https%3A%2F%2Fpack.example.com&code=7K2QF9XWMP",
            "geode://join?code=7K2QF9XWMP&addr=https://pack.example.com/",
            "geode://join/?addr=https://pack.example.com&code=GEODE-7K2QF-9XWMP",
            "geode://join?addr=https://pack.example.com&code=%37K2QF9XWMP&",
        ] {
            assert_eq!(Link::parse(input).unwrap(), expected, "reading {input:?}");
        }
    }

    #[test]
    fn the_scheme_and_host_are_matched_in_any_case() {
        let expected = Link { address: direct("203.0.113.7", 25590), code: code(), pin: Some(pin()) };
        for front in ["GEODE://JOIN", "Geode://Join/", "geode://jOiN"] {
            let input = format!("{front}?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}");
            assert_eq!(Link::parse(&input).unwrap(), expected, "reading {input:?}");
        }
    }

    #[test]
    fn anything_but_a_geode_join_link_is_refused() {
        let query = format!("?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}");
        for front in [
            "https://join",
            "geodex://join",
            "geod://join",
            "geode://joins",
            "geode://jo%69n",
            "geode:join",
            "geode:/join",
            "geode:///join",
            "geode://@join",
            "geode://user@join",
            "geode://join:",
            "geode://join:80",
            "geode://join//",
            "geode://join/.",
            "geode://join/x",
            "geode://share/join",
            "",
        ] {
            assert_link_refused(&format!("{front}{query}"));
        }
        assert_link_refused("geode://join");
    }

    #[test]
    fn a_link_has_only_the_known_parts_and_each_at_most_once() {
        let addr = "addr=203.0.113.7:25590";
        let code = format!("code={CODE}");
        let pin = format!("pin={EMPTY_SHA256}");
        // The well-formed baseline, so every refusal below is down to its change.
        assert!(Link::parse(&format!("geode://join?{addr}&{code}&{pin}")).is_ok());

        for query in [
            format!("{addr}&{code}&{pin}&name=x"),
            format!("{addr}&{code}&{pin}&=x"),
            format!("{addr}&{code}&{pin}&x"),
            format!("ADDR=203.0.113.7:25590&{code}&{pin}"),
            format!("{addr}&{addr}&{code}&{pin}"),
            format!("{addr}&addr=198.51.100.9:25590&{code}&{pin}"),
            format!("{addr}&{code}&code=0000000000&{pin}"),
            format!("{addr}&{code}&{pin}&{pin}"),
            format!("{code}&{pin}"),
            format!("{addr}&{pin}"),
            format!("addr=&{code}&{pin}"),
            format!("{addr}&code=&{pin}"),
            format!("{addr}&{code}&pin="),
        ] {
            assert_link_refused(&format!("geode://join?{query}"));
        }
        // Players are told about a password, whatever the link calls it.
        let error = Link::parse(&format!("geode://join?{addr}&{pin}"))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "this link has no password");
    }

    #[test]
    fn a_bad_code_or_fingerprint_is_refused() {
        for (code, pin) in [
            ("K3VB0", EMPTY_SHA256.to_string()),
            ("https://example.com", EMPTY_SHA256.to_string()),
            (CODE, EMPTY_SHA256[..63].to_string()),
            (CODE, format!("{}zz", &EMPTY_SHA256[..62])),
        ] {
            assert_link_refused(&format!("geode://join?addr=203.0.113.7:25590&code={code}&pin={pin}"));
        }
        let error = Link::parse(&format!(
            "geode://join?addr=203.0.113.7:25590&code=K3VB0&pin={EMPTY_SHA256}"
        ))
        .unwrap_err()
        .to_string();
        assert_eq!(error, "this link's password is not valid");
    }

    /// Windows' single-instance plugin splits forwarded arguments on `|`, and
    /// whitespace and control characters split them everywhere else. Encoded
    /// forms are refused too, or they would carry the same thing in.
    #[test]
    fn characters_that_could_split_a_link_are_refused_raw_or_encoded() {
        for input in [
            format!("geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}|x"),
            format!("geode://join?addr=203.0.113.7:25590&code=7K2QF|9XWMP&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590&code=7K2QF 9XWMP&pin={EMPTY_SHA256}"),
            format!(" geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}\n"),
            format!("geode://join?addr=203.0.113.7:25590\t&code={CODE}&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590&code={CODE}\r&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}\0"),
            format!("geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}\u{2028}"),
            // Code::parse takes spaces, hyphens and underscores as separators,
            // so an encoded space would sail through it if the link let it by.
            format!("geode://join?addr=203.0.113.7:25590&code=7K2QF%209XWMP&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590&code=7K2QF+9XWMP&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590&code=7K2QF%7C9XWMP&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590&code=7K2QF%0A9XWMP&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590%09&code={CODE}&pin={EMPTY_SHA256}"),
            format!("geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}%C2%A0"),
            format!("geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}#frag"),
            format!("geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}&x=%FF"),
            // A URL path would keep a `|`, so here nothing else would stop it.
            format!("geode://join?addr=https://pack.example.com/a|b&code={CODE}"),
        ] {
            assert_link_refused(&input);
        }
        // Refused for what is actually wrong, not for whatever the stray
        // character happened to break further along.
        for input in [
            format!(" geode://join?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}"),
            format!("geode://join\n?addr=203.0.113.7:25590&code={CODE}&pin={EMPTY_SHA256}"),
        ] {
            let error = Link::parse(&input).unwrap_err().to_string();
            assert!(error.contains("characters"), "{input:?} refused for the wrong reason: {error}");
        }
        // The same link without the damage, so the refusals are not about
        // something else in it.
        assert!(Link::parse(&format!(
            "geode://join?addr=203.0.113.7:25590&code=7K2QF-9XWMP&pin={EMPTY_SHA256}"
        ))
        .is_ok());
    }

    #[test]
    fn a_link_is_capped_in_length() {
        let prefix = "geode://join?addr=https://pack.example.com/";
        let suffix = format!("&code={CODE}");
        let path_len = MAX_LEN - prefix.len() - suffix.len();
        let at_cap = format!("{prefix}{}{suffix}", "a".repeat(path_len));
        assert_eq!(at_cap.len(), MAX_LEN);
        assert!(Link::parse(&at_cap).is_ok());

        let over = format!("{prefix}{}{suffix}", "a".repeat(path_len + 1));
        assert_link_refused(&over);
    }

    #[test]
    fn a_direct_link_needs_its_port() {
        assert_link_refused(&direct_link("203.0.113.7"));
        assert_link_refused(&direct_link("play.example.com"));
        assert_link_refused(&direct_link("%5B2001:db8::1%5D"));
        assert!(Link::parse(&direct_link("play.example.com:25590")).is_ok());
    }

    #[test]
    fn a_link_address_answers_to_the_address_rules() {
        for addr in ["203.0.113.7:0", "2001:db8::1:25590", "1.2.3:25590", "under_score.example:25590"] {
            assert_link_refused(&direct_link(addr));
        }
        // Without a pin, so the https address itself is the only thing wrong.
        for addr in [
            "http://pack.example.com",
            "https%3A%2F%2F%40pack.example.com",
            "https://pack.example.com/%3Fx",
            "https://pack.example.com/%23x",
        ] {
            assert_link_refused(&format!("geode://join?addr={addr}&code={CODE}"));
        }
    }

    /// A direct link without a fingerprint could lead anywhere; an https link
    /// with one says two contradictory things. Only a person typing an
    /// address by hand gets to leave the fingerprint out.
    #[test]
    fn a_link_pins_a_direct_address_and_only_a_direct_address() {
        assert_link_refused(&format!("geode://join?addr=203.0.113.7:25590&code={CODE}"));
        assert_link_refused(&format!(
            "geode://join?addr=https://pack.example.com&code={CODE}&pin={EMPTY_SHA256}"
        ));

        let typed = Link::from_parts("203.0.113.7", "geode-7k2qf-9xwmp", 25590).unwrap();
        assert_eq!(typed, Link { address: direct("203.0.113.7", 25590), code: code(), pin: None });
    }

    #[test]
    fn from_parts_reads_what_a_person_typed_and_refuses_what_is_not_an_address_or_a_code() {
        assert_eq!(
            Link::from_parts("HTTPS://Pack.Example.com/", CODE, 25590).unwrap(),
            Link { address: https("https://pack.example.com"), code: code(), pin: None }
        );
        assert_eq!(
            Link::from_parts("[2001:db8::1]:30000", "7K2QF 9XWMP", 25590).unwrap(),
            Link { address: direct("2001:db8::1", 30000), code: code(), pin: None }
        );
        assert!(Link::from_parts("http://pack.example.com", CODE, 25590).is_err());
        assert!(Link::from_parts("203.0.113.7:0", CODE, 25590).is_err());
        let error = Link::from_parts("203.0.113.7", "not a code", 25590)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a valid password"), "{error}");
        assert!(error.contains("GEODE-XXXXX-XXXXX"), "{error}");
        assert!(Link::from_parts("203.0.113.7", "", 25590).is_err());
    }

    /// The shape `JoinLink` in the frontend's types expects, since the webview
    /// takes a link from a preview and hands it straight back.
    #[test]
    fn a_link_survives_serde_in_the_shape_the_frontend_expects() {
        let pinned = Link { address: direct("2001:db8::1", 25590), code: code(), pin: Some(pin()) };
        let json = serde_json::to_value(&pinned).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "address": { "kind": "direct", "host": "2001:db8::1", "port": 25590 },
                "code": CODE,
                "pin": EMPTY_SHA256,
            })
        );
        assert_eq!(serde_json::from_value::<Link>(json).unwrap(), pinned);

        let domain = Link { address: https("https://pack.example.com"), code: code(), pin: None };
        let json = serde_json::to_value(&domain).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "address": { "kind": "https", "url": "https://pack.example.com" },
                "code": CODE,
            })
        );
        assert_eq!(serde_json::from_value::<Link>(json).unwrap(), domain);
    }
}

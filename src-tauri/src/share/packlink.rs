//! `geode://pack` links: a pack somebody put somewhere and sent you.
//!
//! A join link points at a machine that is up right now. A pack link points at
//! bytes sitting somewhere, because handing somebody a pack is asynchronous —
//! they install it tomorrow, when the laptop it came from is shut.
//!
//! ```text
//! geode://pack?url=https://files.example.com/survival.geodepack&h=<64 hex>
//! ```
//!
//! Two things, and nothing else: **where** the bytes are, and **what they
//! should hash to**.
//!
//! The URL is wherever the sender already puts files — a chat attachment, a
//! release asset, their own web space. Geode uploads nothing and holds no
//! account anywhere, so there is no host it depends on and none it can be cut
//! off from.
//!
//! The hash is required, and it is the load-bearing part. It is what makes the
//! open-ended `url=` form safe to support at all: it catches a truncated
//! download, an HTML interstitial served in place of the archive, a file
//! replaced since the link was written, and a host that decided to serve
//! something else — all before the importer sees a byte. It is deliberately not [`super::link::Pin`]: a
//! pin is a certificate fingerprint, and overloading it would blur two
//! different claims.
//!
//! Nothing else goes in. No name, no size, no author: a name in a link is
//! attacker-controlled text that would be rendered in a dialog before anything
//! had been verified, so the dialog shows the source host — which parsing
//! proved — and then the real preview read out of the downloaded archive. No
//! secret either: anyone holding the message can fetch the pack, which is a
//! property of the transport and is said where the link is made.
//!
//! Parsed exactly as strictly as a join link, by the same helpers, for the
//! same reasons — see [`super::link`].

use crate::error::{Error, Result};

use super::link::{encode_value, is_plain, MAX_LEN, SCHEME};

/// The host part of a pack link, compared as text.
const HOST: &str = "pack";

/// Where the bytes of a pack are: an `https://` URL serving the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSource {
    pub url: String,
}

/// The SHA-256 a downloaded pack must hash to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackHash(pub [u8; 32]);

/// A parsed, validated pack link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackLink {
    pub source: PackSource,
    pub hash: PackHash,
}

impl PackHash {
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

impl PackSource {
    /// The URLs to try, in order, to get the archive itself.
    ///
    /// One, used exactly as it was written — [`PackLink::parse`] proved it is
    /// an `https://` URL, and nothing is appended to it. A list rather than a
    /// single value because [`super::fetch`] judges each attempt by what came
    /// back, and that shape outlived the second candidate that once needed it.
    pub fn download_urls(&self) -> Vec<String> {
        vec![self.url.clone()]
    }

    /// The host a dialog names, so what the player is told came out of parsing
    /// rather than out of the link's own words.
    pub fn host(&self) -> String {
        // Parsing proved there is one.
        url::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default()
    }
}

impl PackLink {
    /// Parse a `geode://pack?…` link. See the module docs for the rules.
    pub fn parse(input: &str) -> Result<Self> {
        if input.len() > MAX_LEN {
            return Err(Error::msg("this link is too long to be a Geode link"));
        }
        if !is_plain(input) {
            return Err(Error::msg(
                "this link contains characters a Geode link never has",
            ));
        }

        // The front is compared as text rather than read back from a parsed
        // URL, for the reason `link.rs` gives: a URL parser forgives
        // `geode://@pack`, `geode://pack:` and `geode://pack/.` alike, and
        // each thing forgiven is one more way to dress a link up as something
        // else.
        let (front, query) = input.split_once('?').unwrap_or((input, ""));
        let front = front.strip_suffix('/').unwrap_or(front);
        if !front.eq_ignore_ascii_case(&format!("{SCHEME}://{HOST}")) {
            return Err(Error::msg("this is not a Geode pack link"));
        }

        let (mut url, mut hash) = (None, None);
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            // Decoded values answer to the same rule as the raw link, or `%20`
            // and `%7C` would carry in exactly what the rule keeps out.
            if !is_plain(&value) {
                return Err(Error::msg(
                    "this link contains characters a Geode link never has",
                ));
            }
            let slot = match key.as_ref() {
                "url" => &mut url,
                "h" => &mut hash,
                _ => return Err(Error::msg("this link has a part a Geode link never has")),
            };
            // Refused rather than resolved: whichever copy this keeps, the
            // next thing to read the link might keep the other.
            if slot.replace(value).is_some() {
                return Err(Error::msg(format!(
                    "this link gives its {key} more than once"
                )));
            }
        }

        let source = match url {
            Some(url) => parse_url(&url)?,
            None => return Err(Error::msg("this link does not say where the pack is")),
        };
        // Required. A pack link with no hash names bytes nobody can check,
        // and checking them is the whole of what makes this safe.
        let hash = hash.ok_or_else(|| Error::msg("this link has no pack fingerprint"))?;
        let hash = PackHash::from_hex(&hash)
            .ok_or_else(|| Error::msg("this link's pack fingerprint is not valid"))?;

        Ok(PackLink { source, hash })
    }

    /// The `geode://pack?…` form. Round-trips through [`PackLink::parse`].
    pub fn to_url(&self) -> String {
        let where_it_is = format!("url={}", encode_value(&self.source.url));
        format!("{SCHEME}://{HOST}?{where_it_is}&h={}", self.hash.to_hex())
    }
}

/// An `https://` URL that a download may be made against.
///
/// Deliberately not [`super::link::Address::parse`]: that one forbids a query,
/// because a request is built by appending a path to it, and exists to stop
/// address spoofing for a pinned certificate. This is a whole URL to fetch
/// once, and a download URL needs its `?export=download`.
fn parse_url(raw: &str) -> Result<PackSource> {
    // Read off the text first, as `link.rs` reads an address: the parser
    // forgives an authority that is not there at all — `https:///a.example`
    // comes back with `a.example` as its host, so the path is shown as the
    // machine and the machine is connected to as the path — and it drops an
    // empty `@` without a trace.
    let authority = raw
        .split_once("://")
        .map(|(_, rest)| &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())])
        .unwrap_or_default();
    if authority.is_empty() {
        return Err(Error::msg("this link's pack address names no host"));
    }
    // An address that shows a familiar name before an `@` while connecting to
    // whatever follows it is a well-worn disguise.
    if authority.contains('@') {
        return Err(Error::msg(
            "this link's pack address must not contain a user name or password",
        ));
    }

    let url =
        url::Url::parse(raw).map_err(|_| Error::msg("this link's pack address is not a URL"))?;
    if url.scheme() != "https" {
        return Err(Error::msg("a pack link can only name an https:// address"));
    }
    match url.host_str() {
        Some(host) if !host.is_empty() => {}
        _ => return Err(Error::msg("this link's pack address names no host")),
    }
    // Nothing fetches a fragment, so one in a link is a sign it was built by
    // something other than Geode.
    if url.fragment().is_some() {
        return Err(Error::msg("this link's pack address cannot have a # part"));
    }
    Ok(PackSource {
        url: url.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of nothing: a real hash, and one whose bytes differ enough that
    /// a hex routine which reversed or shuffled them would show.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const PACK_URL: &str = "https://files.example.com/survival.geodepack";

    fn hash() -> PackHash {
        PackHash::from_hex(EMPTY_SHA256).unwrap()
    }

    fn url_link(url: &str) -> String {
        format!("geode://pack?url={url}&h={EMPTY_SHA256}")
    }

    fn assert_refused(input: &str) {
        let parsed = PackLink::parse(input);
        assert!(
            parsed.is_err(),
            "{input:?} should be refused, got {parsed:?}"
        );
    }

    #[test]
    fn a_url_link_round_trips_through_parse() {
        for link in [
            PackLink {
                source: PackSource {
                    url: "https://files.example.com/survival.geodepack".into(),
                },
                hash: hash(),
            },
            PackLink {
                source: PackSource {
                    url: "https://files.example.com/a%20b/survival.geodepack?export=download"
                        .into(),
                },
                hash: hash(),
            },
        ] {
            let url = link.to_url();
            assert_eq!(PackLink::parse(&url).unwrap(), link, "re-reading {url}");
            // And the written form is itself a fixed point.
            assert_eq!(PackLink::parse(&url).unwrap().to_url(), url);
        }
    }

    #[test]
    fn a_hash_is_required_and_is_exactly_64_hex_characters() {
        assert_refused("geode://pack?drive=abc");
        for bad in [
            String::new(),
            EMPTY_SHA256[..63].to_string(),
            format!("{EMPTY_SHA256}0"),
            format!("0x{}", &EMPTY_SHA256[..62]),
            format!("{}g", &EMPTY_SHA256[..63]),
        ] {
            assert_refused(&format!("geode://pack?url={PACK_URL}&h={bad}"));
        }
        // Either case, as the hex in a join link's pin is.
        let upper = format!(
            "geode://pack?url={PACK_URL}&h={}",
            EMPTY_SHA256.to_uppercase()
        );
        assert_eq!(PackLink::parse(&upper).unwrap().hash, hash());
    }

    #[test]
    fn a_link_names_exactly_one_place_to_get_the_pack() {
        assert_refused(&format!("geode://pack?h={EMPTY_SHA256}"));
    }

    #[test]
    fn an_unknown_key_and_a_repeated_key_are_both_refused() {
        assert_refused(&format!(
            "geode://pack?url=https://files.example.com/a.geodepack&h={EMPTY_SHA256}&name=Survival"
        ));
        assert_refused(&format!(
            "geode://pack?url=https://a.example.com/x.geodepack&url=https://b.example.com/y.geodepack&h={EMPTY_SHA256}"
        ));
        assert_refused(&format!(
            "geode://pack?url={PACK_URL}&h={EMPTY_SHA256}&h={EMPTY_SHA256}"
        ));
        assert_refused(&format!(
            "geode://pack?addr=203.0.113.7:25590&h={EMPTY_SHA256}"
        ));
    }


    #[test]
    fn a_url_must_be_https_with_a_host_no_userinfo_and_no_fragment() {
        for bad in [
            "http://files.example.com/a.geodepack",
            "ftp://files.example.com/a.geodepack",
            "https://user:pw@files.example.com/a.geodepack",
            "https://nice.example.com@evil.example.com/a.geodepack",
            "https:///a.geodepack",
            "https://files.example.com/a.geodepack%23frag",
            "not-a-url",
        ] {
            assert_refused(&url_link(bad));
        }
        // A query is allowed: a download URL needs one.
        let with_query = url_link("https://files.example.com/get%3Fid%3D7%26export%3Ddownload");
        assert_eq!(
            PackLink::parse(&with_query).unwrap().source,
            PackSource {
                url: "https://files.example.com/get?id=7&export=download".into()
            }
        );
    }

    /// The same rule the raw link answers to, applied again to every decoded
    /// value — `%20` and `%7C` would otherwise carry in what it keeps out.
    #[test]
    fn whitespace_pipes_and_a_pasted_novel_are_refused_encoded_or_not() {
        assert_refused(&format!("geode://pack?url={PACK_URL} &h={EMPTY_SHA256}"));
        assert_refused(&format!("geode://pack?url={PACK_URL}|x&h={EMPTY_SHA256}"));
        assert_refused(&url_link("https://files.example.com/a%20b.geodepack"));
        assert_refused(&url_link("https://files.example.com/a%7Cb.geodepack"));
        assert_refused(&format!(
            "geode://pack?url=https://files.example.com/{}&h={EMPTY_SHA256}",
            "a".repeat(MAX_LEN)
        ));
    }

    /// The front is text, not a parsed URL, so none of these is forgiven.
    #[test]
    fn a_front_that_only_looks_like_a_pack_link_is_refused() {
        for front in [
            "geode://@pack",
            "geode://pack:",
            "geode://pack/.",
            "geode://packs",
            "geode://pack/x",
            "geode:/pack",
            "https://pack",
            "geode://join",
        ] {
            assert_refused(&format!("{front}?url={PACK_URL}&h={EMPTY_SHA256}"));
        }
        // Case and one trailing slash are the only forgiveness there is.
        for front in ["geode://pack", "GEODE://PACK", "geode://pack/"] {
            let link = format!("{front}?url={PACK_URL}&h={EMPTY_SHA256}");
            assert!(PackLink::parse(&link).is_ok(), "{link} should be accepted");
        }
    }

    #[test]
    fn a_source_offers_the_url_it_was_given_and_names_its_host() {
        let url = PackSource {
            url: "https://files.example.com/a.geodepack?x=1".into(),
        };
        assert_eq!(
            url.download_urls(),
            vec!["https://files.example.com/a.geodepack?x=1".to_string()]
        );
        assert_eq!(url.host(), "files.example.com");
    }
}

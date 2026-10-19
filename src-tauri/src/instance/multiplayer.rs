//! The player's multiplayer list — `servers.dat` — added to without losing it.
//!
//! This is the file behind the Multiplayer screen, and it is the player's, not
//! ours: their own servers are in it, in the order they put them, with keys
//! written by their client and by their mods. Geode has exactly one thing to
//! say to it — here is the server you just joined — and everything else in it
//! has to come back out unchanged.
//!
//! So nothing here is decoded that does not have to be. The file is walked by
//! tag type, each row of the list is kept as the slice of bytes it arrived as,
//! and a rewrite is our row plus every other row's original bytes, verbatim.
//! Editing our own row rewrites its `name` and `ip` and copies its other keys
//! through in place. Unknown keys, unknown tag types, key order and Java's own
//! string encoding all survive because none of them is ever interpreted. It is
//! `server/properties.rs`'s rule in binary: the file is somebody else's, so
//! anything we do not understand is copied through untouched.
//!
//! The format, from the format documentation and from twelve real files on
//! this machine, every one of which agreed:
//!
//! ```text
//! 0a 00 00                      TAG_Compound, and its name: empty
//!   09 00 07 "servers"          TAG_List
//!     0a                          of TAG_Compound
//!     00 00 00 02                 two of them
//!     …row… …row…                 each a compound of keys, ended by TAG_End
//!   00                          end of the root compound
//! ```
//!
//! Two things about it are worth saying out loud, because guessing either
//! wrong loses the file:
//!
//! - **It is not compressed.** That is not the same answer as for `level.dat`,
//!   which is gzip (`1f 8b`) — checked, on this machine, against both. A
//!   `servers.dat` starts straight in on `0a`.
//! - **A row holds keys we will never know.** The documented set is `name`,
//!   `ip`, `icon`, `hidden`, `acceptTextures` and `acceptedCodeOfConduct`, and
//!   `hideAddress` is one the game wrote for five versions and then stopped.
//!   But two of the real files here carry `preventsChatReports`, which is No
//!   Chat Reports', not Mojang's. Rebuilding a row from a known-key list would
//!   quietly throw a mod's settings away, so no row is ever rebuilt from one.
//!
//! The game rewrites this file whole, from memory, whenever the player changes
//! their list — so a row written underneath a running game is erased when it
//! exits. Callers check for that; this module only ever writes the file it was
//! asked to.

use std::path::Path;

use crate::error::{Error, Result};

/// Minecraft's own port, filled in where an address names none, so
/// `play.example.com` and `play.example.com:25565` are one server rather than
/// two rows.
use crate::share::link::GAME_PORT as DEFAULT_PORT;

const TAG_END: u8 = 0;
const TAG_BYTE: u8 = 1;
const TAG_SHORT: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_LONG: u8 = 4;
const TAG_FLOAT: u8 = 5;
const TAG_DOUBLE: u8 = 6;
const TAG_BYTE_ARRAY: u8 = 7;
const TAG_STRING: u8 = 8;
const TAG_LIST: u8 = 9;
const TAG_COMPOUND: u8 = 10;
const TAG_INT_ARRAY: u8 = 11;
const TAG_LONG_ARRAY: u8 = 12;

/// What [`ensure`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrote {
    /// There was no row for this server, so one was added at the end of the
    /// player's list.
    Added,
    /// Our row was there and now says something else — the server was renamed,
    /// or moved.
    Updated,
    /// The player already had a row at this address. That is the row the
    /// server is reached by from now on, and its name is left as they wrote
    /// it.
    ///
    /// It stays theirs, which is why a caller has to keep this answer: a
    /// claimed row handed back as `previous` would be taken for one of ours
    /// and renamed to the admin's name on the next call, which is the one
    /// thing this promises not to do.
    Claimed,
    /// The list already says exactly this. Nothing was written.
    Unchanged,
}

/// `servers.dat`, inside an instance's game directory.
pub fn file(game_dir: &Path) -> std::path::PathBuf {
    game_dir.join("servers.dat")
}

/// Put `name` at `address` in this instance's multiplayer list.
///
/// `previous` is the address this instance's row was last written at, which is
/// how a rename or a move edits that row instead of adding a second one beside
/// it. See [`merge_rules`](self) — the matching order is spelled out there.
/// Only a row Geode wrote belongs there: a row the player wrote, which came
/// back [`Wrote::Claimed`], is passed as `None` and found again by its address
/// like any other row of theirs, so it keeps their name every time.
///
/// Nothing is written when the list already says this. A list that does not
/// parse whole is an error and is left exactly as it was: a file we cannot
/// read is never one we overwrite.
pub async fn ensure(
    game_dir: &Path,
    name: &str,
    address: &str,
    previous: Option<&str>,
) -> Result<Wrote> {
    let path = file(game_dir);
    let existing = match tokio::fs::read(&path).await {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };

    let Some(bytes) = existing else {
        // No list yet: one with a single row, in the shape the game writes.
        let made = empty_list_with(&[new_row(name, address)]);
        check_our_own_work(&made, 1)?;
        tokio::fs::create_dir_all(game_dir).await?;
        crate::server::write_atomic(&path, &made).await?;
        return Ok(Wrote::Added);
    };

    let list = parse(&bytes)?;
    let outcome = decide(&list, name, address, previous)?;
    let (rows, wrote) = match outcome {
        Decision::Keep(wrote) => return Ok(wrote),
        Decision::Replace { at, row } => {
            let mut rows: Vec<&[u8]> = list.rows.iter().map(|r| r.bytes).collect();
            rows[at] = &row;
            (assemble(&list, &rows), Wrote::Updated)
        }
        Decision::Append(row) => {
            let mut rows: Vec<&[u8]> = list.rows.iter().map(|r| r.bytes).collect();
            rows.push(&row);
            (assemble(&list, &rows), Wrote::Added)
        }
    };

    let expected = match wrote {
        Wrote::Added => list.rows.len() + 1,
        _ => list.rows.len(),
    };
    check_our_own_work(&rows, expected)?;
    crate::server::write_atomic(&path, &rows).await?;
    Ok(wrote)
}

/// What to do about the row for this server, worked out before anything is
/// written.
enum Decision {
    /// Nothing to write. The list already says what it should.
    Keep(Wrote),
    /// Row `at` becomes `row`.
    Replace { at: usize, row: Vec<u8> },
    /// A row the list does not have yet, for the end of it.
    Append(Vec<u8>),
}

/// Which row is this server's, and what has to change about it.
///
/// In order:
///
/// 1. The row at the address we last wrote for this instance. That is what
///    makes a rename or a move edit the row rather than add a second.
/// 2. Else the row at the address the server names *now* — claimed, name and
///    all. This is the instance joined before Geode wrote lists at all, and
///    the player who had already added the server by hand.
/// 3. Else a new row at the end.
///
/// Addresses are compared as addresses, not as strings: `play.example.com`,
/// `PLAY.example.com` and `play.example.com:25565` are one server. Without
/// that, case 2 misses and the player gets a duplicate of a row they already
/// had.
///
/// A row found by case 2 keeps the name the player gave it. Renaming their
/// entry to the admin's would be editing something of theirs, and a row
/// already pointing at the server already does what was asked for.
///
/// A move is the one place case 1 gives way to case 2: if the address the
/// server has moved *to* already has a row, that row is the server's from now
/// on, because moving ours onto it as well would leave the player two rows for
/// one server and nothing here ever takes a row away. Ours is left where it
/// is, at an address that no longer answers, for them to keep or delete.
fn decide(list: &List<'_>, name: &str, address: &str, previous: Option<&str>) -> Result<Decision> {
    if let Some(at) = previous.and_then(|previous| find(list, previous)) {
        let row = &list.rows[at];
        let moving = !same_address(&row.ip, address);
        if !moving && row.name.as_deref() == Some(name) {
            return Ok(Decision::Keep(Wrote::Unchanged));
        }
        if moving && another_row_at(list, address, at) {
            return Ok(Decision::Keep(Wrote::Claimed));
        }
        return Ok(Decision::Replace {
            at,
            row: rewritten(row.bytes, name, address)?,
        });
    }
    match find(list, address) {
        // A row already pointing at the server, whoever made it. It does what
        // was asked for already, so it is recorded as this server's and left
        // alone — renaming it would be editing something of the player's.
        Some(_) => Ok(Decision::Keep(Wrote::Claimed)),
        None => Ok(Decision::Append(new_row(name, address))),
    }
}

/// The first row at `address`, if the list has one.
fn find(list: &List<'_>, address: &str) -> Option<usize> {
    list.rows
        .iter()
        .position(|row| same_address(&row.ip, address))
}

/// Whether some row other than `ours` is at `address` too.
fn another_row_at(list: &List<'_>, address: &str, ours: usize) -> bool {
    list.rows
        .iter()
        .enumerate()
        .any(|(at, row)| at != ours && same_address(&row.ip, address))
}

/// Whether two addresses name the same server.
fn same_address(a: &str, b: &str) -> bool {
    split(a) == split(b)
}

/// An address as host and port, for comparing one against another.
///
/// Lower case, no IPv6 brackets, and Minecraft's own port filled in where the
/// address names none. Anything that is not a port is left as part of the
/// host, because that is what the game will try to resolve.
fn split(address: &str) -> (String, u16) {
    let text = address.trim().to_lowercase();
    if let Some((inside, after)) = text.strip_prefix('[').and_then(|r| r.split_once(']')) {
        let port = after
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        return (inside.to_string(), port);
    }
    // An unbracketed IPv6 address is all colons, so none of them is a port.
    if text.matches(':').count() > 1 {
        return (text, DEFAULT_PORT);
    }
    match text.split_once(':') {
        Some((host, port)) => match port.parse() {
            Ok(port) => (host.to_string(), port),
            Err(_) => (text, DEFAULT_PORT),
        },
        None => (text, DEFAULT_PORT),
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Where the `servers` list sits in a file, and the rows in it.
///
/// `head` and `tail` are the file either side of the list's own payload, kept
/// as bytes so everything outside the rows — the root's name, any other key it
/// might hold, the end markers — comes back exactly as it went in.
struct List<'a> {
    head: &'a [u8],
    tail: &'a [u8],
    rows: Vec<Row<'a>>,
}

/// One server in the list, as the file holds it.
struct Row<'a> {
    /// The whole compound, end marker included. Written back untouched unless
    /// this is the row the server owns.
    bytes: &'a [u8],
    /// Its `ip`, which is the only thing in a row Geode ever reads.
    ip: String,
    /// Its `name`, read only to tell "already says this" from "needs saying".
    name: Option<String>,
}

fn unreadable(why: &str) -> Error {
    Error::msg(format!(
        "this instance's multiplayer list could not be read, so it was left alone: {why}"
    ))
}

/// How deep one value may nest before the file is called unreadable.
///
/// The walk below is recursive, and a Rust stack overflow is an abort rather
/// than an error — so without a ceiling a file nested deeply enough takes the
/// launcher down with it instead of reaching `unreadable`, which is the one
/// thing this module promises. Real rows nest three deep.
const MAX_DEPTH: usize = 64;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0, depth: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or_else(|| unreadable("it ends partway through"))?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| unreadable("it ends partway through"))?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn i32(&mut self) -> Result<i32> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A length-prefixed string: a tag's name, and a string tag's payload too,
    /// which have the same shape.
    fn text(&mut self) -> Result<&'a [u8]> {
        let len = self.u16()? as usize;
        self.take(len)
    }

    /// Step over one tag's payload without decoding any of it.
    ///
    /// Every size here is the format's, and getting one wrong is how a walk
    /// ends up reading a length out of the middle of somebody's server icon —
    /// which is why the round-trip test breaks this function first.
    fn skip(&mut self, tag: u8) -> Result<()> {
        match tag {
            TAG_BYTE => self.take(1).map(|_| ()),
            TAG_SHORT => self.take(2).map(|_| ()),
            TAG_INT | TAG_FLOAT => self.take(4).map(|_| ()),
            TAG_LONG | TAG_DOUBLE => self.take(8).map(|_| ()),
            TAG_BYTE_ARRAY => {
                let n = self.count()?;
                self.take(n).map(|_| ())
            }
            TAG_STRING => self.text().map(|_| ()),
            TAG_LIST => {
                let element = self.u8()?;
                let n = self.i32()?;
                for _ in 0..n.max(0) {
                    self.nested(element)?;
                }
                Ok(())
            }
            TAG_COMPOUND => loop {
                let tag = self.u8()?;
                if tag == TAG_END {
                    return Ok(());
                }
                self.text()?;
                self.nested(tag)?;
            },
            TAG_INT_ARRAY => {
                let n = self.count()?;
                self.take(n * 4).map(|_| ())
            }
            TAG_LONG_ARRAY => {
                let n = self.count()?;
                self.take(n * 8).map(|_| ())
            }
            // TAG_End has no payload, so it cannot be a value either.
            other => Err(unreadable(&format!(
                "it holds a kind of data this reader does not know (NBT tag {other})"
            ))),
        }
    }

    /// Step over a value held inside another one, a level further in.
    fn nested(&mut self, tag: u8) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(unreadable("its values nest deeper than this reader goes"));
        }
        self.depth += 1;
        let stepped = self.skip(tag);
        self.depth -= 1;
        stepped
    }

    /// An array's length. Negative means a writer spelled "none" that way.
    fn count(&mut self) -> Result<usize> {
        Ok(self.i32()?.max(0) as usize)
    }
}

/// Read a whole file, finding the `servers` list and the rows in it.
///
/// Whole is the point: a file that stops early, or has bytes left over, or is
/// not NBT at all comes back as an error and is never written to. Half a
/// parse is how a player loses their list.
fn parse(bytes: &[u8]) -> Result<List<'_>> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        // Worth its own sentence: `level.dat` beside it *is* gzip, so this is
        // the mistake a reader of either file makes.
        return Err(unreadable("it is compressed, and a multiplayer list is not"));
    }
    let mut r = Reader::new(bytes);
    if r.u8()? != TAG_COMPOUND {
        return Err(unreadable("it does not start with an NBT compound"));
    }
    r.text()?; // the root's own name, which the game leaves empty

    let mut found = None;
    loop {
        let tag = r.u8()?;
        if tag == TAG_END {
            break;
        }
        let key = r.text()?;
        // Only the first `servers` list is read. A second would be invalid
        // NBT, and acting on one would be acting on a file we cannot explain.
        if key == b"servers" && tag == TAG_LIST && found.is_none() {
            let head = &bytes[..r.at];
            let rows = read_rows(&mut r)?;
            found = Some((head, rows, r.at));
        } else {
            r.skip(tag)?;
        }
    }
    if r.at != bytes.len() {
        return Err(unreadable("it has bytes after the end of its contents"));
    }

    let Some((head, rows, end)) = found else {
        return Err(unreadable("it has no list of servers in it"));
    };
    Ok(List {
        head,
        tail: &bytes[end..],
        rows,
    })
}

fn read_rows<'a>(r: &mut Reader<'a>) -> Result<Vec<Row<'a>>> {
    let element = r.u8()?;
    let count = r.i32()?;
    // An empty list has no element type to speak of; the game writes TAG_End
    // for it, and a reader has to take that rather than refuse the file.
    if count <= 0 {
        return Ok(Vec::new());
    }
    if element != TAG_COMPOUND {
        return Err(unreadable("its list of servers does not hold servers"));
    }
    // Not sized from `count`: that number comes out of the file, and a corrupt
    // one would ask for gigabytes. Every row costs at least a byte, so the
    // reads themselves are what stop this at the end of the file.
    let mut rows = Vec::new();
    for _ in 0..count {
        let start = r.at;
        let (ip, name) = read_row(r)?;
        rows.push(Row {
            bytes: &r.bytes[start..r.at],
            ip,
            name,
        });
    }
    Ok(rows)
}

/// One row's `ip` and `name`, stepping over everything else it holds.
fn read_row(r: &mut Reader<'_>) -> Result<(String, Option<String>)> {
    let (mut ip, mut name) = (String::new(), None);
    loop {
        let tag = r.u8()?;
        if tag == TAG_END {
            return Ok((ip, name));
        }
        let key = r.text()?;
        if (key == b"ip" || key == b"name") && tag == TAG_STRING {
            // Read as it falls out rather than decoded from Java's own UTF-8.
            // The two agree over printable ASCII, which is all an address can
            // be, and a name is only ever compared with one we wrote
            // ourselves — so a name the two would spell differently is one
            // this row does not hold and gets written again. Harmless, and it
            // keeps a decoder out of the read path entirely.
            let text = String::from_utf8_lossy(r.text()?).into_owned();
            if key == b"ip" {
                ip = text;
            } else {
                name = Some(text);
            }
        } else {
            r.skip(tag)?;
        }
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// The file again, with `rows` in place of the ones it held.
fn assemble(list: &List<'_>, rows: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(list.head.len() + list.tail.len() + 256);
    out.extend_from_slice(list.head);
    // Written rather than copied, because a list that was empty carries
    // TAG_End here and is about to hold a row.
    out.push(TAG_COMPOUND);
    out.extend_from_slice(&(rows.len() as i32).to_be_bytes());
    for row in rows {
        out.extend_from_slice(row);
    }
    out.extend_from_slice(list.tail);
    out
}

/// A file with one `servers` list in it, in the shape the game writes.
fn empty_list_with(rows: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![TAG_COMPOUND, 0, 0];
    out.push(TAG_LIST);
    put_text(&mut out, b"servers");
    out.push(TAG_COMPOUND);
    out.extend_from_slice(&(rows.len() as i32).to_be_bytes());
    for row in rows {
        out.extend_from_slice(row);
    }
    out.push(TAG_END);
    out
}

/// This row with its `name` and `ip` replaced, and every other key copied
/// through exactly as it was, in the order it was.
///
/// This is the whole reason nothing here goes through a value model: an icon,
/// a resource-pack answer, an accepted code of conduct and a mod's own key all
/// survive because none of them is looked at.
fn rewritten(row: &[u8], name: &str, address: &str) -> Result<Vec<u8>> {
    let mut r = Reader::new(row);
    let mut out = Vec::with_capacity(row.len() + 64);
    let (mut named, mut addressed) = (false, false);

    loop {
        let tag = r.u8()?;
        if tag == TAG_END {
            break;
        }
        let key = r.text()?;
        let from = r.at;
        r.skip(tag)?;

        let ours = key == b"name" || key == b"ip";
        if ours && tag != TAG_STRING {
            return Err(unreadable(
                "one of its servers has a name or an address that is not text",
            ));
        }
        out.push(tag);
        put_text(&mut out, key);
        match key {
            b"name" => {
                put_text(&mut out, &java_utf8(name));
                named = true;
            }
            b"ip" => {
                put_text(&mut out, &java_utf8(address));
                addressed = true;
            }
            _ => out.extend_from_slice(&row[from..r.at]),
        }
    }

    if !named {
        put_string(&mut out, b"name", name);
    }
    if !addressed {
        put_string(&mut out, b"ip", address);
    }
    out.push(TAG_END);
    Ok(out)
}

/// A row for a server the list does not have yet: what it is called, where it
/// is, and shown rather than hidden — the three keys every real row carries.
///
/// Nothing else. An absent `acceptTextures` is the game's own "not asked yet",
/// which is what this is, and an icon is the game's to fill in from the
/// server's status ping.
fn new_row(name: &str, address: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_string(&mut out, b"name", name);
    put_string(&mut out, b"ip", address);
    out.push(TAG_BYTE);
    put_text(&mut out, b"hidden");
    out.push(0);
    out.push(TAG_END);
    out
}

/// A length-prefixed string's bytes, as NBT holds them.
fn put_text(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// A whole `TAG_String` entry: its tag, its key, and its text.
fn put_string(out: &mut Vec<u8>, key: &[u8], text: &str) {
    out.push(TAG_STRING);
    put_text(out, key);
    put_text(out, &java_utf8(text));
}

/// Java's own UTF-8, which is what an NBT string holds.
///
/// It differs from UTF-8 in two places: U+0000 is two bytes rather than one,
/// and anything above U+FFFF is the two three-byte halves of its UTF-16
/// surrogate pair rather than one four-byte sequence. The second is not a
/// curiosity — a server name with an emoji in it is the ordinary case, and
/// written as plain UTF-8 the game reads the name as mojibake from that
/// character on.
fn java_utf8(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let c = ch as u32;
        match c {
            0 => out.extend_from_slice(&[0xC0, 0x80]),
            0x1..=0x7F => out.push(c as u8),
            0x80..=0x7FF => out.extend_from_slice(&[0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8]),
            0x800..=0xFFFF => out.extend_from_slice(&three(c)),
            _ => {
                let rest = c - 0x1_0000;
                out.extend_from_slice(&three(0xD800 | (rest >> 10)));
                out.extend_from_slice(&three(0xDC00 | (rest & 0x3FF)));
            }
        }
    }
    out
}

/// One code unit in the three-byte form, which is how a surrogate half is
/// written as well as an ordinary character in that range.
fn three(c: u32) -> [u8; 3] {
    [
        0xE0 | (c >> 12) as u8,
        0x80 | ((c >> 6) & 0x3F) as u8,
        0x80 | (c & 0x3F) as u8,
    ]
}

/// Read back what we are about to write, before the rename makes it the
/// player's list.
///
/// The cost of a bug in the walk above is somebody's server list, and this is
/// the cheapest check that would catch one: bytes that do not parse, or that
/// parse to the wrong number of rows, never reach the file.
fn check_our_own_work(bytes: &[u8], rows: usize) -> Result<()> {
    let read = parse(bytes).map_err(|e| {
        Error::msg(format!(
            "Geode built a multiplayer list it could not read back, so yours was left alone: {e}"
        ))
    })?;
    if read.rows.len() != rows {
        return Err(Error::msg(format!(
            "Geode built a multiplayer list with {} servers in it instead of {rows}, so yours was left alone",
            read.rows.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list in the layout the real files are in, so the fixtures under test
    /// are the thing the game writes rather than a shape of our own.
    fn list(rows: Vec<Vec<u8>>) -> Vec<u8> {
        empty_list_with(&rows)
    }

    /// A row written the way the 1.21.4 files write one: `hidden`, `ip`,
    /// `name`, `icon` — a different key order from the 1.20.4 files, which is
    /// itself the reason nothing here may depend on the order.
    fn row(name: &str, ip: &str, icon: Option<&str>) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(TAG_BYTE);
        put_text(&mut out, b"hidden");
        out.push(0);
        put_string(&mut out, b"ip", ip);
        put_string(&mut out, b"name", name);
        if let Some(icon) = icon {
            put_string(&mut out, b"icon", icon);
        }
        out.push(TAG_END);
        out
    }

    fn names_and_ips(bytes: &[u8]) -> Vec<(String, String)> {
        parse(bytes)
            .unwrap()
            .rows
            .iter()
            .map(|r| (r.name.clone().unwrap_or_default(), r.ip.clone()))
            .collect()
    }

    fn dir(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("geode-mp-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// Every byte of a list Geode did not write comes back, because a rewrite
    /// is the rows it arrived as and nothing more.
    ///
    /// Break the TAG_List arm of `skip` to a fixed stride and this fails:
    /// the walk reads a length out of the middle of a row and the parse falls
    /// over, or worse, lands somewhere that parses.
    #[test]
    fn a_list_geode_did_not_write_round_trips_byte_for_byte() {
        let icon = "iVBORw0KGgo=".repeat(40);
        let original = list(vec![
            row("Home", "play.example.com", Some(&icon)),
            row("Friend's", "203.0.113.9:25566", None),
            row("§aColoured", "[2001:db8::5]:25565", Some("abc")),
            // A row holding lists of its own, which is where a walk with a
            // fixed stride instead of a per-element skip goes wrong: an empty
            // list costs nothing to step over by accident, and these do not.
            with_nested_lists("Modded", "modded.example.com"),
        ]);

        let read = parse(&original).unwrap();
        let rows: Vec<&[u8]> = read.rows.iter().map(|r| r.bytes).collect();
        assert_eq!(assemble(&read, &rows), original);
        assert_eq!(read.rows.len(), 4);
        assert_eq!(read.rows[2].ip, "[2001:db8::5]:25565");
        assert_eq!(read.rows[3].name.as_deref(), Some("Modded"));

        // And renaming that row leaves its lists where they were.
        let renamed = rewritten(read.rows[3].bytes, "Renamed", "moved.example.com").unwrap();
        let mut rows: Vec<&[u8]> = read.rows.iter().map(|r| r.bytes).collect();
        rows[3] = &renamed;
        let out = assemble(&read, &rows);
        let back = parse(&out).unwrap();
        assert_eq!(back.rows.len(), 4);
        assert_eq!(back.rows[3].ip, "moved.example.com");
        for key in ["tags", "waypoints", "numbers"] {
            assert!(
                renamed.windows(key.len()).any(|w| w == key.as_bytes()),
                "{key} was lost"
            );
        }
    }

    /// A row with a list of strings, a list of compounds and a list of ints in
    /// it. Nothing Mojang writes, and exactly the sort of thing a mod does.
    fn with_nested_lists(name: &str, ip: &str) -> Vec<u8> {
        let mut out = Vec::new();
        put_string(&mut out, b"name", name);
        put_string(&mut out, b"ip", ip);

        out.push(TAG_LIST);
        put_text(&mut out, b"tags");
        out.push(TAG_STRING);
        out.extend_from_slice(&2i32.to_be_bytes());
        put_text(&mut out, b"survival");
        put_text(&mut out, b"whitelisted");

        out.push(TAG_LIST);
        put_text(&mut out, b"waypoints");
        out.push(TAG_COMPOUND);
        out.extend_from_slice(&2i32.to_be_bytes());
        for n in [1i32, 2] {
            out.push(TAG_INT);
            put_text(&mut out, b"x");
            out.extend_from_slice(&n.to_be_bytes());
            out.push(TAG_END);
        }

        out.push(TAG_LIST);
        put_text(&mut out, b"numbers");
        out.push(TAG_INT);
        out.extend_from_slice(&3i32.to_be_bytes());
        for n in [7i32, 8, 9] {
            out.extend_from_slice(&n.to_be_bytes());
        }

        out.push(TAG_END);
        out
    }

    /// The rule the whole module exists for. A row holds keys Geode will never
    /// know — one of the real files on this machine carries No Chat Reports'
    /// own — and a rename must not be where they go missing.
    ///
    /// Break it by rebuilding the row from a known-key list and all five go.
    #[test]
    fn every_key_geode_does_not_understand_survives_a_rename() {
        let mut odd = Vec::new();
        put_string(&mut odd, b"ip", "play.example.com");
        put_string(&mut odd, b"name", "Old");
        odd.push(TAG_BYTE);
        put_text(&mut odd, b"acceptTextures");
        odd.push(1);
        odd.push(TAG_BYTE);
        put_text(&mut odd, b"preventsChatReports");
        odd.push(1);
        odd.push(TAG_INT);
        put_text(&mut odd, b"acceptedCodeOfConduct");
        odd.extend_from_slice(&7i32.to_be_bytes());
        // A nested compound with an int array in it, and an empty list: the
        // two shapes a walk gets wrong first.
        odd.push(TAG_COMPOUND);
        put_text(&mut odd, b"mystery");
        odd.push(TAG_INT_ARRAY);
        put_text(&mut odd, b"numbers");
        odd.extend_from_slice(&2i32.to_be_bytes());
        odd.extend_from_slice(&1i32.to_be_bytes());
        odd.extend_from_slice(&2i32.to_be_bytes());
        odd.push(TAG_END);
        odd.push(TAG_LIST);
        put_text(&mut odd, b"nothing");
        odd.push(TAG_END);
        odd.extend_from_slice(&0i32.to_be_bytes());
        odd.push(TAG_END);

        let renamed = rewritten(&odd, "New", "play.example.com:25566").unwrap();
        assert_eq!(
            names_and_ips(&list(vec![renamed.clone()])),
            vec![("New".to_string(), "play.example.com:25566".to_string())]
        );
        // Each key still there, and the ones we do not touch byte for byte.
        for key in [
            "acceptTextures",
            "preventsChatReports",
            "acceptedCodeOfConduct",
            "mystery",
            "numbers",
            "nothing",
        ] {
            assert!(
                renamed.windows(key.len()).any(|w| w == key.as_bytes()),
                "{key} was lost by a rename"
            );
        }
        // And the row still reads, which a lost end marker would break.
        assert_eq!(parse(&list(vec![renamed])).unwrap().rows.len(), 1);
    }

    /// Added once, then edited in place — not added again — when the admin
    /// renames the server or moves it.
    ///
    /// Break it by matching on the name instead of the address and the move
    /// leaves two rows.
    #[tokio::test]
    async fn the_row_is_added_once_then_updated_in_place() {
        let dir = dir("once");
        assert_eq!(
            ensure(&dir, "Survival", "play.example.com", None).await.unwrap(),
            Wrote::Added
        );

        let read = |dir: &std::path::Path| names_and_ips(&std::fs::read(file(dir)).unwrap());
        assert_eq!(
            read(&dir),
            vec![("Survival".to_string(), "play.example.com".to_string())]
        );

        // Renamed and moved at once, quoting where it was.
        assert_eq!(
            ensure(&dir, "Survival 2", "203.0.113.7:25566", Some("play.example.com"))
                .await
                .unwrap(),
            Wrote::Updated
        );
        assert_eq!(
            read(&dir),
            vec![("Survival 2".to_string(), "203.0.113.7:25566".to_string())]
        );

        // The same pair again writes nothing at all.
        let before = std::fs::read(file(&dir)).unwrap();
        assert_eq!(
            ensure(&dir, "Survival 2", "203.0.113.7:25566", Some("203.0.113.7:25566"))
                .await
                .unwrap(),
            Wrote::Unchanged
        );
        assert_eq!(std::fs::read(file(&dir)).unwrap(), before);
    }

    /// A player who had already added the server by hand keeps their own row
    /// and their own name for it, and gets no second one.
    ///
    /// Break it by comparing the two `ip` strings literally — `25565` is the
    /// port the game fills in, and the duplicate appears.
    #[tokio::test]
    async fn a_row_the_player_already_has_at_that_address_is_claimed_not_duplicated() {
        let dir = dir("claim");
        std::fs::write(
            file(&dir),
            list(vec![row("My Server", "play.example.com", None)]),
        )
        .unwrap();
        let before = std::fs::read(file(&dir)).unwrap();

        assert_eq!(
            ensure(&dir, "Survival", "PLAY.example.com:25565", None).await.unwrap(),
            Wrote::Claimed
        );
        assert_eq!(
            std::fs::read(file(&dir)).unwrap(),
            before,
            "claiming a row the player made rewrote their list"
        );
    }

    /// A server that moves onto an address the player's list already has a row
    /// for gets that row, rather than a second one saying the same thing.
    ///
    /// Break it by replacing ours anyway and the Multiplayer screen shows the
    /// same server twice, with nothing here that would ever take either away.
    #[tokio::test]
    async fn a_move_onto_an_address_the_list_already_has_adds_no_second_row() {
        let dir = dir("occupied");
        std::fs::write(
            file(&dir),
            list(vec![
                row("Dan's box", "play.example.com", None),
                row("Survival", "203.0.113.7", None),
            ]),
        )
        .unwrap();
        let before = std::fs::read(file(&dir)).unwrap();

        // The admin got a domain, so the server moved onto the address the
        // player's own row was already pointing at.
        assert_eq!(
            ensure(&dir, "Survival", "play.example.com", Some("203.0.113.7"))
                .await
                .unwrap(),
            Wrote::Claimed
        );
        assert_eq!(
            std::fs::read(file(&dir)).unwrap(),
            before,
            "the player was left two rows for one server"
        );
    }

    /// A file nested deeper than the reader goes is refused like any other it
    /// cannot read — not recursed into.
    ///
    /// A Rust stack overflow is an abort, not an error, so without the ceiling
    /// this file never reaches "left alone": it takes the launcher with it, and
    /// pressing Play does nothing but kill the process. Lift `MAX_DEPTH` and
    /// this test ends the whole run with "fatal runtime error: stack overflow".
    #[tokio::test]
    async fn a_list_nested_deeper_than_the_reader_goes_is_left_alone() {
        const DEEP: usize = 50_000;
        let mut deep = Vec::new();
        put_string(&mut deep, b"name", "Deep");
        put_string(&mut deep, b"ip", "play.example.com");
        // One key whose value is a chain of compounds, closed off again, so
        // the file is whole NBT and its depth is the only thing wrong with it.
        for _ in 0..DEEP {
            deep.push(TAG_COMPOUND);
            put_text(&mut deep, b"deeper");
        }
        // One end marker for each of them, and one for the row itself.
        deep.extend(std::iter::repeat_n(TAG_END, DEEP + 1));

        let dir = dir("deep");
        let bytes = list(vec![deep]);
        std::fs::write(file(&dir), &bytes).unwrap();
        let err = ensure(&dir, "Survival", "play.example.com", None)
            .await
            .expect_err("a file nested past the ceiling was read anyway");
        assert!(err.to_string().contains("nest deeper"), "{err}");
        assert_eq!(std::fs::read(file(&dir)).unwrap(), bytes, "it was written over");
    }

    /// Our row goes at the end, and the player's own come back where they
    /// were, as they were.
    #[tokio::test]
    async fn the_players_own_rows_are_never_moved_or_rewritten() {
        let dir = dir("order");
        let theirs = vec![
            row("First", "one.example.com", Some("icon-one")),
            row("Second", "two.example.com", None),
            row("Third", "three.example.com:25566", Some("icon-three")),
        ];
        std::fs::write(file(&dir), list(theirs.clone())).unwrap();

        ensure(&dir, "Survival", "play.example.com", None).await.unwrap();
        let after = std::fs::read(file(&dir)).unwrap();
        let read = parse(&after).unwrap();
        assert_eq!(read.rows.len(), 4);
        for (i, original) in theirs.iter().enumerate() {
            assert_eq!(read.rows[i].bytes, &original[..], "row {i} was rewritten");
        }
        assert_eq!(read.rows[3].name.as_deref(), Some("Survival"));
    }

    /// A list Geode cannot read is a list Geode does not write. Each of these
    /// would be a different kind of bug to overwrite.
    ///
    /// Break it by writing anyway when the read failed and every one of them
    /// costs the player their list.
    #[tokio::test]
    async fn a_list_that_cannot_be_read_is_left_exactly_as_it_was() {
        let good = list(vec![row("Home", "play.example.com", None)]);
        let mut gzipped = vec![0x1f, 0x8b, 0x08, 0x00];
        gzipped.extend_from_slice(&good);
        let mut trailing = good.clone();
        trailing.extend_from_slice(b"extra");
        let mut wrong_root = good.clone();
        wrong_root[0] = TAG_STRING;

        for (what, bytes) in [
            ("truncated", good[..good.len() - 4].to_vec()),
            ("gzipped", gzipped),
            ("trailing bytes", trailing),
            ("wrong root tag", wrong_root),
            ("not NBT at all", b"hello there".to_vec()),
            ("no servers list", vec![TAG_COMPOUND, 0, 0, TAG_END]),
        ] {
            let dir = dir("unreadable");
            std::fs::write(file(&dir), &bytes).unwrap();
            let err = ensure(&dir, "Survival", "play.example.com", None)
                .await
                .expect_err(&format!("{what} was accepted"));
            assert!(err.to_string().contains("left alone"), "{what}: {err}");
            assert_eq!(std::fs::read(file(&dir)).unwrap(), bytes, "{what} was written over");
        }
    }

    /// A name past the basic plane is written the way the game reads it: two
    /// three-byte surrogate halves, not UTF-8's four bytes.
    ///
    /// Break it with `str::as_bytes()` and a name with an emoji in it arrives
    /// as mojibake.
    #[test]
    fn a_name_outside_the_basic_plane_is_written_the_way_the_game_reads_it() {
        assert_eq!(java_utf8("A"), b"A");
        // U+00A7, the section sign colour codes use.
        assert_eq!(java_utf8("\u{a7}"), vec![0xC2, 0xA7]);
        // U+1F600, which plain UTF-8 writes as f0 9f 98 80.
        assert_eq!(
            java_utf8("\u{1f600}"),
            vec![0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]
        );
        assert_eq!("\u{1f600}".len(), 4, "plain UTF-8 writes it in four bytes");
        // U+0000, which Java never writes as one zero byte.
        assert_eq!(java_utf8("\0"), vec![0xC0, 0x80]);
    }

    /// Written by rename, so a crash leaves the old list rather than half a
    /// new one — and nothing is left beside it afterwards.
    #[tokio::test]
    async fn nothing_is_left_beside_the_file_it_wrote() {
        let dir = dir("atomic");
        ensure(&dir, "Survival", "play.example.com", None).await.unwrap();
        ensure(&dir, "Survival", "play.example.com:25566", Some("play.example.com"))
            .await
            .unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["servers.dat"]);
    }

    /// No list yet makes one, in the shape the game writes: a root compound
    /// with an empty name, one `servers` list, and no bytes left over.
    #[tokio::test]
    async fn no_file_yet_makes_one_row() {
        let dir = dir("fresh");
        assert_eq!(
            ensure(&dir, "Survival", "play.example.com", None).await.unwrap(),
            Wrote::Added
        );
        let bytes = std::fs::read(file(&dir)).unwrap();
        assert_eq!(&bytes[..6], &[TAG_COMPOUND, 0, 0, TAG_LIST, 0, 7]);
        assert_eq!(&bytes[6..13], b"servers");
        assert_eq!(
            names_and_ips(&bytes),
            vec![("Survival".to_string(), "play.example.com".to_string())]
        );
    }

    /// An address is compared as an address, which is what keeps one server
    /// from becoming two rows.
    #[test]
    fn one_server_written_two_ways_is_one_address() {
        assert!(same_address("play.example.com", "PLAY.example.com:25565"));
        assert!(same_address("[2001:db8::5]", "[2001:DB8::5]:25565"));
        assert!(same_address("2001:db8::5", "2001:db8::5"));
        assert!(!same_address("play.example.com", "play.example.com:25566"));
        assert!(!same_address("play.example.com", "other.example.com"));
        // Not a port, so part of the host — the same thing the game resolves.
        assert!(same_address("play.example.com:draft", "play.example.com:draft"));
    }

    /// An empty list carries TAG_End as its element type, and a reader that
    /// refuses that refuses a file the game wrote.
    #[tokio::test]
    async fn an_empty_list_takes_its_first_row() {
        let dir = dir("empty");
        let empty = vec![
            TAG_COMPOUND, 0, 0, //
            TAG_LIST, 0, 7, b's', b'e', b'r', b'v', b'e', b'r', b's', //
            TAG_END, 0, 0, 0, 0, //
            TAG_END,
        ];
        std::fs::write(file(&dir), &empty).unwrap();
        assert_eq!(
            ensure(&dir, "Survival", "play.example.com", None).await.unwrap(),
            Wrote::Added
        );
        assert_eq!(
            names_and_ips(&std::fs::read(file(&dir)).unwrap()),
            vec![("Survival".to_string(), "play.example.com".to_string())]
        );
    }
}

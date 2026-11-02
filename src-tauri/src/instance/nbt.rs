//! The byte-level NBT walk, shared by the two of the player's files Geode
//! edits.
//!
//! This was [`super::multiplayer`]'s, written for `servers.dat` and then needed
//! again verbatim for `level.dat`. It is here rather than copied because the
//! cost of a bug in it is somebody's server list or somebody's world, and two
//! copies of that is two places to get it wrong.
//!
//! It is deliberately not an NBT library. There is no value model and no
//! decoder: a file is walked by tag type, the bytes of anything we are not
//! rewriting are kept as the slice they arrived as, and a rewrite is our one
//! key plus everything else verbatim. That is what makes unknown keys, key
//! order and Java's own string encoding all survive — none of them is ever
//! interpreted, only measured and copied.
//!
//! A tag *type* this walk does not know is the one thing it cannot carry
//! through, and it says so rather than guessing. Stepping over a value means
//! knowing how many bytes it is, and an unknown tag has no length anything here
//! could work out — so [`Reader::skip`] answers `unreadable`, the caller
//! reports it, and the edit does not happen: a world whose `level.dat` holds
//! one cannot be renamed, rather than being rewritten around a guess at where
//! the next key starts. That is the same rule as the copying, not an exception
//! to it — the file is somebody else's, so what is not understood is either
//! copied untouched or left alone entirely.
//!
//! Every error here says what is wrong with the *bytes*; the caller names the
//! file, because "this instance's multiplayer list" and "this world's
//! level.dat" are the two sentences a person reads.

use crate::error::Error;
use crate::error::Result;

pub const TAG_END: u8 = 0;
pub const TAG_BYTE: u8 = 1;
pub const TAG_SHORT: u8 = 2;
pub const TAG_INT: u8 = 3;
pub const TAG_LONG: u8 = 4;
pub const TAG_FLOAT: u8 = 5;
pub const TAG_DOUBLE: u8 = 6;
pub const TAG_BYTE_ARRAY: u8 = 7;
pub const TAG_STRING: u8 = 8;
pub const TAG_LIST: u8 = 9;
pub const TAG_COMPOUND: u8 = 10;
pub const TAG_INT_ARRAY: u8 = 11;
pub const TAG_LONG_ARRAY: u8 = 12;

/// A file we will not write because we could not read it.
///
/// `what` is the caller's name for it, so the sentence reads as copy rather
/// than as a parser's complaint.
pub fn unreadable(what: &str, why: &str) -> Error {
    Error::msg(format!(
        "{what} could not be read, so it was left alone: {why}"
    ))
}

/// How deep one value may nest before the file is called unreadable.
///
/// The walk below is recursive, and a Rust stack overflow is an abort rather
/// than an error — so without a ceiling a file nested deeply enough takes the
/// launcher down with it instead of reaching [`unreadable`], which is the one
/// thing this promises. Real files nest three or four deep.
pub const MAX_DEPTH: usize = 64;

pub struct Reader<'a> {
    pub bytes: &'a [u8],
    pub at: usize,
    depth: usize,
    /// What the caller calls this file, for the error text.
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], what: &'static str) -> Self {
        Self {
            bytes,
            at: 0,
            depth: 0,
            what,
        }
    }

    /// This file's name, for a caller building an error of its own.
    pub fn unreadable(&self, why: &str) -> Error {
        unreadable(self.what, why)
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| self.unreadable("it ends partway through"))?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| self.unreadable("it ends partway through"))?;
        self.at = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn i32(&mut self) -> Result<i32> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i64(&mut self) -> Result<i64> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// A length-prefixed string: a tag's name, and a string tag's payload too,
    /// which have the same shape.
    pub fn text(&mut self) -> Result<&'a [u8]> {
        let len = self.u16()? as usize;
        self.take(len)
    }

    /// Step over one tag's payload without decoding any of it.
    ///
    /// Every size here is the format's, and getting one wrong is how a walk
    /// ends up reading a length out of the middle of somebody's server icon —
    /// which is why the round-trip tests break this function first.
    pub fn skip(&mut self, tag: u8) -> Result<()> {
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
            other => Err(self.unreadable(&format!(
                "it holds a kind of data this reader does not know (NBT tag {other})"
            ))),
        }
    }

    /// Step over a value held inside another one, a level further in.
    pub fn nested(&mut self, tag: u8) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(self.unreadable("its values nest deeper than this reader goes"));
        }
        self.depth += 1;
        let stepped = self.skip(tag);
        self.depth -= 1;
        stepped
    }

    /// An array's length. Negative means a writer spelled "none" that way.
    pub fn count(&mut self) -> Result<usize> {
        Ok(self.i32()?.max(0) as usize)
    }
}

/// A length-prefixed string's bytes, as NBT holds them.
pub fn put_text(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// A whole `TAG_String` entry: its tag, its key, and its text.
pub fn put_string(out: &mut Vec<u8>, key: &[u8], text: &str) {
    out.push(TAG_STRING);
    put_text(out, key);
    put_text(out, &java_utf8(text));
}

/// Java's own UTF-8, which is what an NBT string holds.
///
/// It differs from UTF-8 in two places: U+0000 is two bytes rather than one,
/// and anything above U+FFFF is the two three-byte halves of its UTF-16
/// surrogate pair rather than one four-byte sequence. The second is not a
/// curiosity — a server name or a world name with an emoji in it is the
/// ordinary case, and written as plain UTF-8 the game reads it as mojibake from
/// that character on.
pub fn java_utf8(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let c = ch as u32;
        match c {
            0 => out.extend_from_slice(&[0xC0, 0x80]),
            0x1..=0x7F => out.push(c as u8),
            0x80..=0x7FF => {
                out.extend_from_slice(&[0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8])
            }
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

/// Text out of Java's own UTF-8 — [`java_utf8`] the other way round.
///
/// An NBT string is not UTF-8, so `String::from_utf8_lossy` is wrong for one:
/// a world called "Home 😀" holds that emoji as two three-byte surrogate halves,
/// and read as plain UTF-8 the name comes back with replacement characters in
/// it. `servers.dat` could live with that — an address is ASCII, and a name
/// there is only ever compared against one Geode wrote — but a world's name is
/// shown to the player and compared with what they typed, so it has to be
/// decoded properly.
///
/// Anything that is not valid Java UTF-8 becomes U+FFFD and the walk moves on
/// one byte. A name is display text: a single bad byte in one should not make
/// the world unreadable.
pub fn java_text(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // A plain ASCII byte. Java never writes 0x00 this way, so it is not
        // special-cased here — it simply is not a continuation of anything.
        if b < 0x80 {
            out.push(b as char);
            i += 1;
            continue;
        }
        if let Some((unit, width)) = code_unit(bytes, i) {
            // A high surrogate followed by its low half is one character,
            // which is the whole reason this cannot be `from_utf8`.
            if (0xD800..0xDC00).contains(&unit) {
                if let Some((low, low_width)) = code_unit(bytes, i + width) {
                    if (0xDC00..0xE000).contains(&low) {
                        let c = 0x1_0000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                        out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
                        i += width + low_width;
                        continue;
                    }
                }
            }
            out.push(char::from_u32(unit).unwrap_or('\u{fffd}'));
            i += width;
            continue;
        }
        out.push('\u{fffd}');
        i += 1;
    }
    out
}

/// One UTF-16 code unit at `at`, and how many bytes it took.
///
/// Java UTF-8 encodes every unit in one, two or three bytes and never in four,
/// so a unit is all this ever has to read — a character above U+FFFF arrives as
/// two of them.
fn code_unit(bytes: &[u8], at: usize) -> Option<(u32, usize)> {
    let b = *bytes.get(at)?;
    if b < 0x80 {
        return Some((b as u32, 1));
    }
    let cont = |n: usize| -> Option<u32> {
        let c = *bytes.get(at + n)?;
        (c & 0xC0 == 0x80).then_some((c & 0x3F) as u32)
    };
    if b & 0xE0 == 0xC0 {
        return Some(((((b & 0x1F) as u32) << 6) | cont(1)?, 2));
    }
    if b & 0xF0 == 0xE0 {
        return Some((
            (((b & 0x0F) as u32) << 12) | (cont(1)? << 6) | cont(2)?,
            3,
        ));
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// And read back again, which is what a world's name needs: it is shown to
    /// the player and compared with what they typed.
    ///
    /// Break it with `String::from_utf8_lossy` and every name with an emoji in
    /// it reads back as replacement characters — and a rename of one fails its
    /// own read-back check, because the name it wrote is not the name it reads.
    #[test]
    fn a_name_read_back_is_the_name_that_was_written() {
        for text in [
            "Home",
            "",
            "My World (1)",
            // The section sign, two bytes.
            "\u{a7}aColoured",
            // Above U+FFFF: two three-byte surrogate halves, not four bytes.
            "Home \u{1f600}",
            "\u{1f600}\u{1f3f0}",
            // Han, three bytes and no surrogate.
            "\u{4e16}\u{754c}",
            "mixed \u{a7}e \u{1f600} \u{4e16}",
        ] {
            assert_eq!(java_text(&java_utf8(text)), text, "{text:?} did not survive");
        }
        // A lone high surrogate is not a character; it reads as a replacement
        // rather than taking the string with it.
        assert_eq!(java_text(&[0xED, 0xA0, 0xBD]), "\u{fffd}");
        // Truncated, and trailing rubbish: a bad byte costs one character.
        assert_eq!(java_text(&[b'o', b'k', 0xE0]), "ok\u{fffd}");
        assert_eq!(java_text(&[0xFF, b'o', b'k']), "\u{fffd}ok");
    }

    /// The error says which file, because it is read by a person.
    #[test]
    fn an_unreadable_file_is_named_in_the_sentence() {
        let said = unreadable("this world's level.dat", "it ends partway through").to_string();
        assert!(said.starts_with("this world's level.dat could not be read"), "{said}");
        assert!(said.ends_with("it ends partway through"), "{said}");
    }
}

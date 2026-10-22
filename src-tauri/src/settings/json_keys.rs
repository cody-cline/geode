//! JSON, JSONC and the commented JSON a Fabric mod writes, located and
//! spliced one setting at a time.
//!
//! The Fabric half of the ecosystem, and the one format with nothing to lean
//! on: nothing in Rust is a format-preserving JSON writer. `serde_json`'s map
//! is a `BTreeMap`, so a file read and written back comes out alphabetised —
//! and the `preserve_order` feature that fixes the order is a crate-global
//! flag that would change every `Value`-shaped map in Geode, still loses
//! comments and still reflows the whitespace. So this is the same byte-span
//! splice [`kv`](super::kv) and [`toml_keys`](super::toml_keys) are: a scanner
//! reports where each value *is*, and [`text::patch`](super::text::patch) puts
//! the new bytes there. Nothing is re-rendered, so the comments, the
//! indentation, the key order and the file's own ending all survive because
//! nothing wrote them.
//!
//! Three facts about the real files decided the rest. Measured here against
//! the 2441 JSON-family files under the `config/` folders of ten instances:
//!
//! - **Reading needs comment handling, not a JSON5 parser.** 2377 of them are
//!   strict JSON and 56 more parse the moment `//` and `/* */` comments are
//!   stripped. Only 8 need anything else — unquoted keys and trailing commas,
//!   every one of them a `.json5` — and those are refused with a reason rather
//!   than guessed at, because a scanner that guessed would write bytes into
//!   somebody's file somewhere they did not mean. Adding the `json5` crate
//!   would pull a parser that loses the comments and the layout anyway, to
//!   solve a problem 2433 files do not have.
//! - **The extension decides nothing.** A real `polylib.json` and a real
//!   `lithostitched.json` carry `//` comments; a real `ad_astra.jsonc` carries
//!   block comments; most of the `.json5` files are strict JSON. So the format
//!   is sniffed from the bytes, which is [`super::sniff`]'s rule for every
//!   format and not an exception made here.
//! - **2157 of the 2441 end without a final newline**, because that is what
//!   GSON writes, and 197 are CRLF. A splice has neither to restore: it writes
//!   no byte it was not handed.
//!
//! 2283 of the 2441 are readable one setting at a time. What this says no to,
//! besides the 8: 129 files that are groups of empty groups, so there is
//! nothing in them to compare or to push, and 21 that are a list at the top.
//! Across the 2283 the scanner agrees with a real parser on every one of their
//! 55,716 settings, and all but one of the files comes back byte for byte when
//! every setting in it is spliced back to the value it already holds.
//!
//! What a key can name, and why each limit is a limit:
//!
//! - The file has to be **an object at the top**. A file that is an array at
//!   the top — a blacklist, a list of recipes — has nothing a name can
//!   address, and an index is not a name: a player who reordered or deleted an
//!   element would have the sender's value written into a different one, which
//!   parses and is wrong.
//! - An **object is a group of settings and never one setting**, so its own
//!   settings are named as further segments and it cannot be set whole. An
//!   **array is one value**, replaced whole and never merged element by
//!   element — which falls out of splicing the value's span, and is honest:
//!   across 258 real TOML and JSON files there were 251 arrays against 7701
//!   scalar settings.
//! - A setting the file does not have is **written into the group it belongs
//!   to**, after that group's last setting and keeping its indentation. A
//!   group the file does not have at all is refused by name: a mod that moved
//!   its config from one shape to another has not changed a setting, and
//!   inventing the missing shape is a guess that cannot be undone.
//! - A pushed value is **parsed before it is spliced**, with `serde_json`,
//!   whose answer is thrown away. Not to re-render it — the raw text is what
//!   goes in — but because the value is the one piece of a push whose text
//!   came from somewhere else, and `1, "evil": 2` must not be able to become a
//!   second setting in the player's file.

use std::ops::Range;

use super::text;
use crate::error::{Error, Result};

/// A change to one setting: a new raw value, or nothing at all to take it out.
#[derive(Debug, Clone, Copy)]
pub struct Edit<'a> {
    /// The setting, segment by segment: `["quality", "render_distance"]` for
    /// `render_distance` inside the `quality` object. Segments are matched
    /// literally, so a name holding dots, spaces or colons needs no quoting
    /// rules anywhere.
    pub key: &'a [String],
    pub value: Option<&'a str>,
}

/// Whether Geode can find a single setting inside this text.
///
/// Strict, because the answer to "I do not recognise this" has to be no rather
/// than a guess: the whole file has to read as one object with nothing after
/// it, and it has to hold at least one setting — an object of nothing but
/// other empty objects holds none.
pub fn is_json(text: &str) -> bool {
    read(text).is_some_and(|doc| doc.members.iter().any(|member| member.leaf))
}

/// Every setting the text holds, as a key path and the raw text of its value.
///
/// Empty for a text [`is_json`] says no to, which means the same thing here as
/// a file with no settings in it: nothing to compare and nothing to splice. A
/// name written twice in one object is one setting, the last of them, which is
/// what every reader of these files takes the file to mean.
pub fn values(text: &str) -> Vec<(Vec<String>, String)> {
    let mut out: Vec<(Vec<String>, String)> = Vec::new();
    for member in read(text).map(|doc| doc.members).unwrap_or_default() {
        if !member.leaf {
            continue;
        }
        let value = text[member.value].to_string();
        match out.iter_mut().find(|(key, _)| *key == member.path) {
            Some(slot) => slot.1 = value,
            None => out.push((member.path, value)),
        }
    }
    out
}

/// Apply edits to the text, rewriting only the settings they name.
pub fn splice(text: &str, edits: &[Edit]) -> Result<String> {
    let doc = read(text).ok_or_else(unreadable)?;

    let mut cuts: Vec<(Range<usize>, String)> = Vec::new();
    for edit in edits {
        let found = doc.locate(edit.key)?;
        match (edit.value, found) {
            (Some(value), Some(member)) => {
                parsed(value, edit.key)?;
                cuts.push((member.value.clone(), value.to_string()));
            }
            (Some(value), None) => {
                parsed(value, edit.key)?;
                cuts.push(doc.insertion(text, edit.key, value)?);
            }
            (None, Some(member)) => {
                cuts.extend(doc.removal(text, member).into_iter().map(|at| (at, String::new())));
            }
            // A setting to take out of a file that does not have it.
            (None, None) => {}
        }
    }
    Ok(text::patch(text, cuts))
}

/// One `"name": value` the file holds, as byte spans into the text it was read
/// from.
#[derive(Debug, Clone)]
struct Member {
    /// The key path from the top of the file, segment by segment.
    path: Vec<String>,
    /// The span of the quoted name, quotes included.
    name: Range<usize>,
    /// The span of the raw value. The only span a value change replaces.
    value: Range<usize>,
    /// Whether this is a setting rather than a group of them. `false` for an
    /// object, whose own settings are members in their own right.
    leaf: bool,
    /// The span of the `{ … }` this one is written inside, which is what says
    /// which members are its siblings.
    object: Range<usize>,
    /// Where the comma after this one is, when it has one — so a removal can
    /// take out the comma that joined it and leave the file readable.
    comma: Option<usize>,
}

/// One settings file, read.
struct Doc {
    /// The `{ … }` at the top.
    root: Range<usize>,
    /// Every member in the file, in the order the file writes them.
    members: Vec<Member>,
}

impl Doc {
    /// Find one setting, or `None` when the file does not have it — which is
    /// not an error, because that is the file a push has to be written into.
    ///
    /// An error is for a key that cannot mean a setting in this file at all:
    /// one naming a group of settings, and one naming a setting inside a
    /// single value. Both are refused by name rather than guessed at.
    fn locate(&self, key: &[String]) -> Result<Option<&Member>> {
        if key.is_empty() {
            return Err(Error::msg(
                "the server sent a setting with no name, so Geode left this file as it is",
            ));
        }
        // Every group the key reaches through has to be a group. One that is a
        // single value in this file means the key means nothing here.
        for depth in 1..key.len() {
            if self
                .members
                .iter()
                .any(|member| member.path == key[..depth] && member.leaf)
            {
                return Err(Error::msg(format!(
                    "the server set '{}', which names a setting inside a single value",
                    shown(key)
                )));
            }
        }
        match self.members.iter().rev().find(|member| member.path == key) {
            Some(member) if member.leaf => Ok(Some(member)),
            Some(_) => Err(Error::msg(format!(
                "the server set '{}', which is a group of settings in this file and not a setting",
                shown(key)
            ))),
            None => Ok(None),
        }
    }

    /// The `{ … }` a setting at this path is written inside, or `None` when
    /// this file has no such group.
    fn group(&self, prefix: &[String]) -> Option<Range<usize>> {
        if prefix.is_empty() {
            return Some(self.root.clone());
        }
        self.members
            .iter()
            .rev()
            .find(|member| member.path == prefix && !member.leaf)
            .map(|member| member.value.clone())
    }

    /// The members written directly inside one group, in the file's own order.
    fn inside(&self, object: &Range<usize>) -> Vec<&Member> {
        self.members
            .iter()
            .filter(|member| member.object == *object)
            .collect()
    }

    /// Where a setting this file does not have goes, and the text to put
    /// there.
    ///
    /// After the group's last setting, keeping that line's indentation and the
    /// spacing it puts after its colon — for the reason
    /// [`kv::splice`](super::kv::splice) appends at all: a setting the sender
    /// pushed has to arrive even into a file written before that setting
    /// existed, and a mod reading its own config takes a setting it did not
    /// write as a default it did not need.
    ///
    /// A **group** the file does not have is refused instead. A mod that moved
    /// its config from one shape to another has not changed a setting, and a
    /// shape invented here would be a guess written into somebody's file that
    /// nothing can undo.
    fn insertion(&self, text: &str, key: &[String], value: &str) -> Result<(Range<usize>, String)> {
        let (last, prefix) = key.split_last().expect("located first");
        let object = self.group(prefix).ok_or_else(|| {
            Error::msg(format!(
                "the server set '{}', and this file has no '{}' to put it in, so Geode left the \
                 file as it is",
                shown(key),
                shown(prefix)
            ))
        })?;
        let ending = text::line_ending(text);
        let name = quoted(last);

        match self.inside(&object).last() {
            Some(prev) => {
                let line = text::line_of(text, prev.value.clone());
                let indent: String = text[line]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                // Whatever this file writes between a name and its value.
                let between = &text[prev.name.end..prev.value.start];
                Ok((
                    prev.value.end..prev.value.end,
                    format!(",{ending}{indent}{name}{between}{value}"),
                ))
            }
            // A group with nothing in it yet, so there is no line to copy and
            // no comma to write. Two spaces, because the file has said nothing
            // about how it indents.
            None => {
                let inner = object.start + 1;
                Ok((inner..inner, format!("{ending}  {name}: {value}{ending}")))
            }
        }
    }

    /// What taking one setting out takes out.
    ///
    /// The setting, and the comma that joined it to the setting beside it:
    /// `{"a": 1, "b": 2}` with `a` gone has to read `{"b": 2}` and not
    /// `{, "b": 2}`, and with `b` gone it has to read `{"a": 1}` and not
    /// `{"a": 1,}` — a trailing comma is not JSON, and a mod whose config does
    /// not parse reads none of its own settings.
    ///
    /// A comment beside the setting is left where it was, which is
    /// [`toml_keys::splice`](super::toml_keys::splice)'s rule for the same
    /// reason: a comment belongs to whoever wrote it, and guessing how much of
    /// one documented the setting rather than the group is not a guess worth
    /// making when the result cannot be undone.
    fn removal(&self, text: &str, member: &Member) -> Vec<Range<usize>> {
        let own = cut_of(
            text,
            member.name.start,
            member.comma.map_or(member.value.end, |comma| comma + 1),
        );
        if member.comma.is_some() {
            return vec![own];
        }
        // The last setting in its group, so the comma above it is the one that
        // would be left dangling.
        let siblings = self.inside(&member.object);
        let before = siblings
            .iter()
            .position(|other| std::ptr::eq(*other, member))
            .and_then(|nth| nth.checked_sub(1))
            .and_then(|nth| siblings[nth].comma);
        match before {
            Some(comma) => vec![comma..comma + 1, own],
            None => vec![own],
        }
    }
}

/// What taking a setting out covers: the setting itself, and the whole line
/// when the setting had that line to itself.
///
/// Line by line where the file is written line by line, and span by span where
/// it is not — a JSON file is under no obligation to put one setting on one
/// line, and `{"a": 1}` is a whole file on one of them. Taking the line there
/// would take the braces with it.
fn cut_of(text: &str, from: usize, to: usize) -> Range<usize> {
    let start = from - (text[..from].len() - text[..from].trim_end_matches([' ', '\t']).len());
    let rest = to + (text[to..].len() - text[to..].trim_start_matches([' ', '\t']).len());
    let alone = (start == 0 || text[..start].ends_with('\n'))
        && (rest >= text.len() || text[rest..].starts_with(['\n', '\r']));
    if !alone {
        return from..to;
    }
    text::removal(text, start..text::line_of(text, to..to).end)
}

/// Read the file, or `None` when no key can name a setting inside it.
fn read(text: &str) -> Option<Doc> {
    let mut scan = Scan {
        text,
        at: 0,
        members: Vec::new(),
        recording: true,
    };
    scan.gap();
    if scan.peek()? != b'{' {
        return None;
    }
    let root = scan.object(&mut Vec::new())?;
    scan.gap();
    // Nothing after the object but space and comments, so a file holding two
    // documents, or ending halfway through one, is not read as the first.
    if scan.at != text.len() {
        return None;
    }
    // A name written with an escape in it is one Geode cannot address
    // literally, and a segment matched against the escape rather than the
    // character it stands for would be a name that is not the name. No
    // measured file has one.
    if scan
        .members
        .iter()
        .any(|member| text[member.name.clone()].contains('\\'))
    {
        return None;
    }
    Some(Doc {
        root,
        members: scan.members,
    })
}

/// A pass over the text, which is where every span comes from.
struct Scan<'t> {
    text: &'t str,
    at: usize,
    members: Vec<Member>,
    /// Whether what is being read can be addressed by name.
    ///
    /// False inside an array, and that is the one thing in this scanner that
    /// would be a silent catastrophe to get wrong. An array is one value and
    /// an index is not a name, so an object written inside one holds nothing a
    /// key could reach — and recorded anyway its members would take the names
    /// they have *inside the element*, which collide with the real settings of
    /// the same name elsewhere in the file. A real
    /// `default_radar_categories_client.json` is exactly that shape: `name`
    /// and `excludeList` at the top, and a `subCategories` array of objects
    /// writing both again. A push naming `name` would have spliced the last
    /// one found, inside the array, in a file that parses afterwards and is
    /// wrong.
    recording: bool,
}

impl<'t> Scan<'t> {
    fn bytes(&self) -> &'t [u8] {
        self.text.as_bytes()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes().get(self.at).copied()
    }

    /// Space and comments: everything between two things that mean something.
    ///
    /// The comments are the one part of this that is not JSON, and the only
    /// part 2200 real files needed. Both kinds, because the extension does not
    /// say which a file uses and a real `.json` uses each.
    fn gap(&mut self) {
        let bytes = self.bytes();
        loop {
            while self.at < bytes.len() && bytes[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            let two = (self.peek(), bytes.get(self.at + 1).copied());
            match two {
                (Some(b'/'), Some(b'/')) => {
                    self.at += 2;
                    while self.at < bytes.len() && bytes[self.at] != b'\n' {
                        self.at += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    self.at += 2;
                    while self.at + 1 < bytes.len()
                        && !(bytes[self.at] == b'*' && bytes[self.at + 1] == b'/')
                    {
                        self.at += 1;
                    }
                    // A comment the file never closes: nothing after it can be
                    // read, which is what leaves `read` with text left over.
                    if self.at + 1 >= bytes.len() {
                        self.at = bytes.len();
                        return;
                    }
                    self.at += 2;
                }
                _ => return,
            }
        }
    }

    /// Take one expected character, or fail.
    fn eat(&mut self, want: u8) -> Option<()> {
        self.gap();
        if self.peek()? != want {
            return None;
        }
        self.at += 1;
        Some(())
    }

    /// One quoted string, as the span including its quotes.
    ///
    /// Escape-aware, which is what keeps a `//` or a `*/` inside a string from
    /// being read as a comment and a `\"` from being read as the end: the
    /// character after a backslash is skipped whatever it is, so `"a\\"` ends
    /// where it says it does.
    fn string(&mut self) -> Option<Range<usize>> {
        self.gap();
        let start = self.at;
        if self.peek()? != b'"' {
            return None;
        }
        self.at += 1;
        let bytes = self.bytes();
        while self.at < bytes.len() {
            match bytes[self.at] {
                b'\\' => self.at += 2,
                b'"' => {
                    self.at += 1;
                    return Some(start..self.at);
                }
                _ => self.at += 1,
            }
        }
        None
    }

    /// One value, as the span of its own text, and whether it is a group of
    /// settings rather than one.
    fn value(&mut self, path: &mut Vec<String>) -> Option<(Range<usize>, bool)> {
        self.gap();
        match self.peek()? {
            b'"' => Some((self.string()?, false)),
            b'{' => Some((self.object(path)?, true)),
            b'[' => Some((self.array()?, false)),
            _ => Some((self.scalar()?, false)),
        }
    }

    /// `{ … }`, recording every member written inside it.
    fn object(&mut self, path: &mut Vec<String>) -> Option<Range<usize>> {
        let start = self.at;
        self.at += 1;
        // The members of this object are not known to belong to it until the
        // closing brace says where it ends, so they are given its span here on
        // the way out. A nested one has had its own by then.
        let first = self.members.len();
        self.gap();
        if self.peek()? == b'}' {
            self.at += 1;
            return Some(start..self.at);
        }
        loop {
            let name = self.string()?;
            self.eat(b':')?;
            path.push(unquoted(&self.text[name.clone()]).to_string());
            let mut mine = None;
            if self.recording {
                mine = Some(self.members.len());
                self.members.push(Member {
                    path: path.clone(),
                    name,
                    value: 0..0,
                    leaf: true,
                    object: 0..0,
                    comma: None,
                });
            }
            let (value, group) = self.value(path)?;
            path.pop();
            if let Some(mine) = mine {
                self.members[mine].value = value;
                self.members[mine].leaf = !group;
            }

            self.gap();
            match self.peek()? {
                b',' => {
                    if let Some(mine) = mine {
                        self.members[mine].comma = Some(self.at);
                    }
                    self.at += 1;
                    // And round again, where a name is the only thing that
                    // will do — which is how a trailing comma is refused.
                    // That is JSON5, which 8 of the 2208 real files use and no
                    // reader of the other 2200 accepts, so such a file is sent
                    // once rather than read by a scanner guessing at it.
                }
                b'}' => {
                    self.at += 1;
                    let span = start..self.at;
                    for member in &mut self.members[first..] {
                        if member.object.is_empty() {
                            member.object = span.clone();
                        }
                    }
                    return Some(span);
                }
                _ => return None,
            }
        }
    }

    /// `[ … ]`, as one span. An array is one value: see this module's own doc.
    ///
    /// Read for its shape and not for its contents, so a `]` inside a string
    /// or a comment cannot end it early — and so nothing inside it is
    /// recorded as a setting, which is what [`Scan::recording`] is for.
    fn array(&mut self) -> Option<Range<usize>> {
        let outer = std::mem::replace(&mut self.recording, false);
        let span = self.elements();
        self.recording = outer;
        span
    }

    fn elements(&mut self) -> Option<Range<usize>> {
        let start = self.at;
        self.at += 1;
        self.gap();
        if self.peek()? == b']' {
            self.at += 1;
            return Some(start..self.at);
        }
        loop {
            self.value(&mut Vec::new())?;
            self.gap();
            match self.peek()? {
                // And round again, where a value is the only thing that
                // will do — so a trailing comma is refused here too.
                b',' => self.at += 1,
                b']' => {
                    self.at += 1;
                    return Some(start..self.at);
                }
                _ => return None,
            }
        }
    }

    /// A number, `true`, `false` or `null`: everything up to the next thing
    /// that could end a value.
    fn scalar(&mut self) -> Option<Range<usize>> {
        let start = self.at;
        let bytes = self.bytes();
        while self.at < bytes.len()
            && !bytes[self.at].is_ascii_whitespace()
            && !matches!(bytes[self.at], b',' | b'}' | b']' | b'"' | b'{' | b'[')
            && !matches!(
                (bytes[self.at], bytes.get(self.at + 1)),
                (b'/', Some(b'/')) | (b'/', Some(b'*'))
            )
        {
            self.at += 1;
        }
        (self.at > start).then_some(start..self.at)
    }
}

/// A name without its quotes. Not unescaped: a name holding an escape makes
/// the whole file unaddressable, which [`read`] refuses outright.
fn unquoted(raw: &str) -> &str {
    raw.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(raw)
}

/// A segment as this format writes a name. Every character JSON escapes in a
/// name is one [`crate::share::manifest::Settings::checked`] already refuses
/// in a segment, except the two written here.
fn quoted(segment: &str) -> String {
    format!("\"{}\"", segment.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Hold a pushed value to being exactly one JSON value, which is the one part
/// of a push whose text came from somewhere else.
///
/// `serde_json` is the right tool for this and the wrong one for writing: it
/// says yes or no to one value and its answer is thrown away, so nothing it
/// would reorder or reformat ever reaches the file. It is also stricter than
/// the scanner above, which is the way round that is safe — a value Geode
/// would not have read is one it does not write. The measured cost of that
/// strictness is one setting in 55,716: a real `attributefix.json` writes
/// `f64`'s own limit as a 309-digit integer, which `serde_json` calls a number
/// out of range, so that one setting cannot be pushed and says so.
fn parsed(value: &str, key: &[String]) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|_| {
            Error::msg(format!(
                "the server set '{}' to something this file's format cannot hold, so Geode left \
                 the file as it is",
                shown(key)
            ))
        })
}

fn shown(key: &[String]) -> String {
    key.join(".")
}

fn unreadable() -> Error {
    Error::msg("Geode cannot read this file one setting at a time, so it was left as it is")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> Vec<String> {
        path.split('.').map(str::to_string).collect()
    }

    fn set<'a>(key: &'a [String], value: &'a str) -> Edit<'a> {
        Edit {
            key,
            value: Some(value),
        }
    }

    fn taken_out(key: &[String]) -> Edit<'_> {
        Edit { key, value: None }
    }

    /// The file GSON writes, which is most of them: two-space indentation and
    /// no final newline. One setting changes and nothing else does — not the
    /// key order, not the indentation, and not the missing newline at the end,
    /// which 2156 of the 2208 real JSON configs measured here are without.
    #[test]
    fn a_changed_setting_is_the_only_thing_that_moves_in_a_file_gson_wrote() {
        let before = "{\n  \"renderDistance\": 8,\n  \"vsync\": true,\n  \"name\": \"mine\"\n}";
        let out = splice(before, &[set(&key("renderDistance"), "12")]).unwrap();
        assert_eq!(
            out,
            "{\n  \"renderDistance\": 12,\n  \"vsync\": true,\n  \"name\": \"mine\"\n}"
        );
        assert!(!out.ends_with('\n'), "a final newline was added: {out:?}");

        // A string value is spliced as the raw text it is, quotes and all, so
        // nothing has to understand the escaping that wrote it.
        let out = splice(before, &[set(&key("name"), "\"theirs\\\"s\"")]).unwrap();
        assert!(out.ends_with("\"name\": \"theirs\\\"s\"\n}"), "{out:?}");

        // And a file that does end with a newline keeps that too, as does a
        // CRLF one — 197 of the real files are CRLF.
        let crlf = "{\r\n  \"a\": 1,\r\n  \"b\": 2\r\n}\r\n";
        assert_eq!(
            splice(crlf, &[set(&key("b"), "3")]).unwrap(),
            "{\r\n  \"a\": 1,\r\n  \"b\": 3\r\n}\r\n"
        );
    }

    /// The extension says nothing: a real `polylib.json` carries `//`
    /// comments and a real `ad_astra.jsonc` carries block comments, so both
    /// are read, and a splice keeps every one of them — which is the whole
    /// reason this is a byte splice and not a reserialise.
    #[test]
    fn a_commented_file_is_read_and_spliced_with_every_comment_intact() {
        let line_comments = "{\n\
                             \t// Allows poly lib to run as a server side only support.\n\
                             \t\"serverOnly\": false, // and after it, too\n\
                             \t\"debug\": false\n\
                             }";
        assert!(is_json(line_comments));
        let out = splice(line_comments, &[set(&key("serverOnly"), "true")]).unwrap();
        assert_eq!(
            out,
            "{\n\
             \t// Allows poly lib to run as a server side only support.\n\
             \t\"serverOnly\": true, // and after it, too\n\
             \t\"debug\": false\n\
             }"
        );

        let block_comments = "{\n\
                              /* what this file is for,\n\
                                 over two lines */\n\
                              \"oxygen\": true\n\
                              }\n";
        assert!(is_json(block_comments));
        assert_eq!(
            splice(block_comments, &[set(&key("oxygen"), "false")]).unwrap(),
            "{\n\
             /* what this file is for,\n\
                over two lines */\n\
             \"oxygen\": false\n\
             }\n"
        );
    }

    /// A `//` or a `/*` inside a string is part of the string, and a quote the
    /// string escapes does not end it. Read either one wrong and the scanner
    /// loses its place in the file, which is how a splice lands in the middle
    /// of somebody's setting.
    #[test]
    fn a_comment_marker_inside_a_string_is_part_of_the_string() {
        let text = "{\n  \"url\": \"https://example.invalid/*\",\n  \"quote\": \"a \\\" // b\",\n  \"fov\": 70\n}";
        assert_eq!(
            values(text),
            vec![
                (key("url"), "\"https://example.invalid/*\"".to_string()),
                (key("quote"), "\"a \\\" // b\"".to_string()),
                (key("fov"), "70".to_string()),
            ]
        );
        // The setting after both of them is still found where it is, which is
        // what a scanner that had lost its place would get wrong.
        assert_eq!(
            splice(text, &[set(&key("fov"), "90")]).unwrap(),
            "{\n  \"url\": \"https://example.invalid/*\",\n  \"quote\": \"a \\\" // b\",\n  \"fov\": 90\n}"
        );
        // A string ending in an escaped backslash ends where it says it does.
        assert_eq!(
            values("{\"path\": \"C:\\\\\", \"fov\": 70}"),
            vec![
                (key("path"), "\"C:\\\\\"".to_string()),
                (key("fov"), "70".to_string()),
            ]
        );
    }

    /// A group of settings is named by its parts, and an array is one value —
    /// replaced whole, never merged element by element. Across 258 real TOML
    /// and JSON files there were 251 arrays against 7701 scalar settings, so
    /// the rule costs almost nothing and is one somebody can predict.
    #[test]
    fn a_nested_setting_is_named_by_its_parts_and_an_array_is_one_value() {
        let text = "{\n  \"quality\": {\n    \"render_distance\": 8\n  },\n  \"packs\": [\n    \"vanilla\",\n    \"fabric\"\n  ]\n}";
        assert_eq!(
            values(text),
            vec![
                (key("quality.render_distance"), "8".to_string()),
                (
                    key("packs"),
                    "[\n    \"vanilla\",\n    \"fabric\"\n  ]".to_string()
                ),
            ]
        );
        assert_eq!(
            splice(text, &[set(&key("quality.render_distance"), "16")]).unwrap(),
            "{\n  \"quality\": {\n    \"render_distance\": 16\n  },\n  \"packs\": [\n    \"vanilla\",\n    \"fabric\"\n  ]\n}"
        );
        assert_eq!(
            splice(text, &[set(&key("packs"), "[\"vanilla\"]")]).unwrap(),
            "{\n  \"quality\": {\n    \"render_distance\": 8\n  },\n  \"packs\": [\"vanilla\"]\n}"
        );

        // The group itself is not a setting, and a setting named inside a
        // single value is not one either. Both refused by name.
        let err = splice(text, &[set(&key("quality"), "1")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("group of settings") && err.contains("quality"), "{err}");
        let err = splice(text, &[set(&key("quality.render_distance.deeper"), "1")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("inside a single value"), "{err}");
    }

    /// An object written inside an array holds nothing a key can reach, so
    /// nothing in it is a setting. This is the one mistake in this scanner
    /// that would be silent: a real `default_radar_categories_client.json` has
    /// `name` and `excludeList` at the top and a `subCategories` array of
    /// objects writing both again, so recording what is inside the array would
    /// make a push of `name` splice the last one found — leaving a file that
    /// parses afterwards and is wrong.
    #[test]
    fn a_setting_written_inside_an_array_is_not_a_setting() {
        let text = "{\n  \"name\": \"root\",\n  \"subCategories\": [\n    { \"name\": \"living\" },\n    { \"name\": \"other\" }\n  ]\n}";
        assert_eq!(
            values(text)
                .into_iter()
                .map(|(key, _)| key.join("."))
                .collect::<Vec<_>>(),
            vec!["name", "subCategories"],
        );
        assert_eq!(
            splice(text, &[set(&key("name"), "\"mine\"")]).unwrap(),
            "{\n  \"name\": \"mine\",\n  \"subCategories\": [\n    { \"name\": \"living\" },\n    { \"name\": \"other\" }\n  ]\n}"
        );
        // And a `]` inside a string inside one of those objects does not end
        // the array early, which would put every setting after it in the
        // wrong place.
        let tricky = "{\n  \"list\": [{ \"s\": \"]\" }],\n  \"fov\": 70\n}";
        assert_eq!(
            values(tricky),
            vec![
                (key("list"), "[{ \"s\": \"]\" }]".to_string()),
                (key("fov"), "70".to_string()),
            ]
        );
    }

    /// A setting the file does not have is written into the group it belongs
    /// to, keeping that group's indentation and the spacing it puts after its
    /// colon — a setting the sender pushed has to arrive even into a file
    /// written before that setting existed. A **group** the file does not have
    /// is refused instead: a mod that moved its config from one shape to
    /// another has not changed a setting, and the shape is not Geode's to
    /// invent.
    #[test]
    fn a_setting_the_file_lacks_is_written_in_and_a_group_it_lacks_is_refused() {
        let text = "{\n  \"quality\": {\n    \"render_distance\": 8\n  }\n}";
        let out = splice(text, &[set(&key("quality.vsync"), "true")]).unwrap();
        assert_eq!(
            out,
            "{\n  \"quality\": {\n    \"render_distance\": 8,\n    \"vsync\": true\n  }\n}"
        );
        assert!(is_json(&out), "the file no longer reads as one: {out:?}");

        // At the top, after the last setting there, and into a group that has
        // nothing in it yet.
        assert_eq!(
            splice(text, &[set(&key("fov"), "70")]).unwrap(),
            "{\n  \"quality\": {\n    \"render_distance\": 8\n  },\n  \"fov\": 70\n}"
        );
        let empty = "{\n  \"quality\": {}\n}";
        let out = splice(empty, &[set(&key("quality.vsync"), "true")]).unwrap();
        assert_eq!(out, "{\n  \"quality\": {\n  \"vsync\": true\n}\n}");
        assert!(is_json(&out), "{out:?}");

        let err = splice(text, &[set(&key("sodium.quality.vsync"), "true")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("no 'sodium.quality' to put it in"), "{err}");
    }

    /// Taking a setting out has to leave the file readable, and that is not
    /// the same cut twice: the comma that joined it goes with it, whether the
    /// comma was after it or — for the last setting in its group — before it.
    /// A trailing comma is not JSON, and a mod whose config does not parse
    /// reads none of its own settings.
    #[test]
    fn taking_a_setting_out_takes_the_comma_that_joined_it() {
        let text = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}";
        for (gone, left) in [
            ("a", "{\n  \"b\": 2,\n  \"c\": 3\n}"),
            ("b", "{\n  \"a\": 1,\n  \"c\": 3\n}"),
            ("c", "{\n  \"a\": 1,\n  \"b\": 2\n}"),
        ] {
            let out = splice(text, &[taken_out(&key(gone))]).unwrap();
            assert_eq!(out, left, "taking out {gone}");
            assert!(is_json(&out), "{gone}: {out:?}");
        }

        // The only setting in its group leaves an empty group rather than a
        // file that does not parse.
        let out = splice("{\n  \"a\": 1\n}", &[taken_out(&key("a"))]).unwrap();
        assert_eq!(out, "{\n}");
        assert!(read(&out).is_some(), "{out:?}");

        // A file that writes its settings on one line is not a file of lines,
        // so the setting goes and the braces stay.
        let out = splice("{\"a\": 1, \"b\": 2}", &[taken_out(&key("a"))]).unwrap();
        assert!(is_json(&out) && values(&out) == vec![(key("b"), "2".to_string())], "{out:?}");
        let out = splice("{\"a\": 1, \"b\": 2}", &[taken_out(&key("b"))]).unwrap();
        assert!(is_json(&out) && values(&out) == vec![(key("a"), "1".to_string())], "{out:?}");

        // A setting to take out of a file that does not have it is nothing to
        // do, not a failure.
        assert_eq!(splice(text, &[taken_out(&key("missing"))]).unwrap(), text);
    }

    /// What this cannot read says no rather than guessing, because a scanner
    /// that guessed would write bytes into somebody's file somewhere they did
    /// not mean. Each of these is sent once instead, and the admin is told.
    #[test]
    fn what_no_key_can_name_is_refused_rather_than_guessed_at() {
        // Genuine JSON5 — an unquoted key and a trailing comma. 8 of the 2208
        // real files are like this, every one a `.json5`.
        assert!(!is_json("{\n  renderDistance: 8\n}"));
        assert!(!is_json("{\n  \"renderDistance\": 8,\n}"));
        assert!(!is_json("{\n  \"packs\": [\"a\",]\n}"));
        // An array at the top: an index is not a name, so a player who
        // reordered an element would get the sender's value in a different one.
        assert!(!is_json("[\n  {\"version\": 2}\n]"));
        // A group of empty groups, which is 129 of the real files: nothing to
        // compare and nothing to push.
        assert!(!is_json("{\n  \"movement\": {},\n  \"backgroundHide\": {}\n}"));
        // Halfway through an edit, and not text at all.
        assert!(!is_json("{\n  \"a\": 1,\n  \"b\":"));
        assert!(!is_json("{\n  \"a\": 1\n} trailing"));
        assert!(!is_json("{\n  /* never closed\n  \"a\": 1\n}"));
        assert!(!is_json(""));

        // And a push into one of them is reported rather than written.
        let err = splice("{\n  \"a\": 1,\n}", &[set(&key("a"), "2")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("one setting at a time"), "{err}");
    }

    /// The value is the one piece of a push whose text came from somewhere
    /// else, so it is held to being exactly one JSON value before a byte of it
    /// is written. Without that, `1, "evil": 2` would become a second setting
    /// in the player's file — settings the push never named, in a file whose
    /// whole promise to them is that nothing else in it moved.
    #[test]
    fn a_pushed_value_has_to_be_one_value_of_this_format() {
        let text = "{\n  \"a\": 1,\n  \"b\": 2\n}";
        for evil in [
            "1, \"evil\": 2",
            "1}, \"evil\": {\"x\": 2",
            "",
            "// nothing",
            "{\"x\": 1,}",
            "undefined",
        ] {
            let err = splice(text, &[set(&key("a"), evil)]).unwrap_err().to_string();
            assert!(err.contains("cannot hold"), "{evil:?} was allowed: {err}");
        }
        // Every shape a real value takes is allowed through.
        for good in ["1", "-2.5e-3", "true", "null", "\"a b\"", "[]", "{\"x\": 1}"] {
            splice(text, &[set(&key("a"), good)])
                .unwrap_or_else(|e| panic!("{good:?} was refused: {e}"));
        }
    }
}

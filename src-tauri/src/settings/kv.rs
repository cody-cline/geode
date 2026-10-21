//! Key-and-value settings files, located and spliced a line at a time.
//!
//! `.properties`, flat `.cfg` and `options.txt` are all the same shape: one
//! setting per line, a key, a separator, and the rest of the line. That makes
//! them the cheapest format to change one setting inside, and the one where
//! doing it badly is most obvious — these files are mostly a player's keybinds
//! and a mod's documented defaults, with a comment above each key that is the
//! only documentation the setting has.
//!
//! So nothing here rewrites a line it was not asked to. Every edit is a byte
//! span inside the text the file already holds: the value of one key is
//! replaced, or one line is taken out, and every other byte — the comments,
//! the blank lines, the order, each line's own spacing around its separator,
//! the file's line endings and whether it ends with a newline — is copied
//! through untouched. A writer that normalises any of those rewrites every
//! line of the file as far as a later diff or hash can tell.
//!
//! Three facts about these files decided the rules below, each measured
//! against real files:
//!
//! - Values are taken and put back as **raw text**, never parsed and never
//!   re-escaped. `level-type=minecraft\:normal` is in a live `server.properties`
//!   Geode itself wrote, and any re-escaper will differ from Java's somewhere.
//! - `options.txt` splits on the **first** separator only, because its values
//!   contain more of them: `key_iris.keybind.reload:key.keyboard.r:CONTROL`.
//! - Both spacings are in the wild — `RMBTweak=1` in MouseTweaks,
//!   `current_profile = default` in Xaero — so a changed line keeps its own.
//!
//! This is deliberately *not* `server::properties::apply`, which is the right
//! shape and the wrong behaviour for a player's file: it rewrites the changed
//! line as `{key}={value}`, dropping ` = `, and because it works through
//! `lines()` it turns CRLF into LF across the whole file and forces a final
//! newline. Seven tests pin that, and `server.properties` — Geode's own file,
//! on the admin's own machine — wants it. A file a player has made their own
//! does not, so this is a sibling rather than a change to a working function.

use std::ops::Range;

use super::text;

/// Which character this file puts between a key and its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    /// `.properties` and flat `.cfg`.
    Equals,
    /// `options.txt`, and Minecraft's own files generally.
    Colon,
}

impl Separator {
    pub fn ch(self) -> char {
        match self {
            Separator::Equals => '=',
            Separator::Colon => ':',
        }
    }
}

/// One `key<sep>value` line, as byte spans into the text it was read from.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The key, trimmed of the spaces around it.
    pub key: String,
    /// Where the separator itself is.
    sep_at: usize,
    /// The raw value: everything after the separator and the horizontal space
    /// following it, up to but not including the line's terminator. This is
    /// the only span a value change ever replaces.
    pub value: Range<usize>,
    /// The whole line, terminator and all. What a removal takes out.
    line: Range<usize>,
}

/// A change to one key: a new raw value, or nothing at all to take it out.
#[derive(Debug, Clone, Copy)]
pub struct Edit<'a> {
    pub key: &'a str,
    pub value: Option<&'a str>,
}

/// Every setting the text holds, in the order the file lists them.
///
/// A key named twice is two entries here, because the file has two lines; what
/// to do about that is [`splice`]'s business.
pub fn entries(text: &str, sep: Separator) -> Vec<Entry> {
    rows(text)
        .into_iter()
        .filter_map(|(line, body)| entry(text, sep, line, body))
        .collect()
}

/// Every key and its raw value, with the last of a repeated key winning —
/// which is what Java's own reader does, so it is what the file means.
pub fn values(text: &str, sep: Separator) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for found in entries(text, sep) {
        let value = text[found.value.clone()].to_string();
        match out.iter_mut().find(|(key, _)| *key == found.key) {
            Some(slot) => slot.1 = value,
            None => out.push((found.key, value)),
        }
    }
    out
}

/// Whether this text is one of these files at all: every line of it is blank,
/// a comment, or `key<sep>value`, and at least one is a setting.
///
/// Strict on purpose, because the answer to "I do not recognise this" has to
/// be no rather than a guess. A TOML file's `[table]` headers, an array
/// spilling over several lines and a JSON file's braces all fail it, and so a
/// file this says no to is one the admin cannot choose per-key syncing for —
/// it is sent once instead, which is a promise Geode can keep.
pub fn is_key_value(text: &str, sep: Separator) -> bool {
    let mut any = false;
    for (line, body) in rows(text) {
        if skipped(&text[body.clone()]) {
            continue;
        }
        if entry(text, sep, line, body).is_none() {
            return false;
        }
        any = true;
    }
    any
}

/// Apply edits to the text, rewriting only the lines whose keys they name.
///
/// A key the file does not have is appended, because a setting the server
/// pushed has to arrive even into a file written before that setting existed —
/// and a mod reading its own config takes a key it did not write as a default
/// it did not need. A key the file names twice is replaced at its last line,
/// the one that decides what the file means; a removal takes out every line
/// naming the key, since leaving one would not remove the setting.
///
/// What is appended keeps the file's own shape: the spacing the file's first
/// setting uses around its separator, the file's own line ending, and its
/// final-newline state either way.
pub fn splice(text: &str, sep: Separator, edits: &[Edit]) -> String {
    let found = entries(text, sep);

    // Every edit as a span of the original and what goes there: a value
    // replaced, or a line removed. Nothing else is written.
    let mut cuts: Vec<(Range<usize>, &str)> = Vec::new();
    let mut appended: Vec<&Edit> = Vec::new();
    for edit in edits {
        match edit.value {
            Some(value) => match found.iter().rev().find(|e| e.key == edit.key) {
                Some(e) => cuts.push((e.value.clone(), value)),
                None => appended.push(edit),
            },
            None => {
                for e in found.iter().filter(|e| e.key == edit.key) {
                    cuts.push((text::removal(text, e.line.clone()), ""));
                }
            }
        }
    }
    let mut out = text::patch(text, cuts);

    if !appended.is_empty() {
        let ending = text::line_ending(text);
        let final_newline = text.is_empty() || text.ends_with('\n');
        let (before, after) = spacing(text, &found);
        if !out.is_empty() && !final_newline {
            out.push_str(ending);
        }
        for (nth, edit) in appended.iter().enumerate() {
            if nth > 0 {
                out.push_str(ending);
            }
            out.push_str(edit.key);
            out.push_str(before);
            out.push(sep.ch());
            out.push_str(after);
            out.push_str(edit.value.unwrap_or_default());
        }
        if final_newline {
            out.push_str(ending);
        }
    }
    out
}

/// Each line of the text as `(the line with its terminator, the line without)`.
fn rows(text: &str) -> Vec<(Range<usize>, Range<usize>)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let body_end = if at > start && bytes[at - 1] == b'\r' {
            at - 1
        } else {
            at
        };
        out.push((start..at + 1, start..body_end));
        start = at + 1;
    }
    // A last line with no terminator of its own, which 2 of 34 real
    // `.properties` files and every `.json` GSON writes have.
    if start < bytes.len() {
        out.push((start..bytes.len(), start..bytes.len()));
    }
    out
}

/// Whether this line says nothing: blank, or a comment. `!` as well as `#`,
/// because the Java properties format takes both — and the timestamp
/// `Properties.store()` writes as line 2 is a comment, which is why a file a
/// mod rewrote with the same values has nothing here that moved.
fn skipped(body: &str) -> bool {
    let trimmed = body.trim_start();
    trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!')
}

fn entry(
    text: &str,
    sep: Separator,
    line: Range<usize>,
    body: Range<usize>,
) -> Option<Entry> {
    let whole = &text[body.clone()];
    if skipped(whole) {
        return None;
    }
    // The FIRST separator the line does not escape, and nothing else: a
    // keybind's value holds two more colons, and a server property's holds an
    // escaped one.
    let at = separator_at(whole, sep)?;
    let key = whole[..at].trim();
    if !named(key) {
        return None;
    }
    let sep_at = body.start + at;
    let after = sep_at + sep.ch().len_utf8();
    let spaces = text[after..body.end].len()
        - text[after..body.end].trim_start_matches([' ', '\t']).len();
    Some(Entry {
        key: key.to_string(),
        sep_at,
        value: (after + spaces)..body.end,
        line,
    })
}

/// Where the key ends: the first separator the line does not escape.
///
/// The escape matters because a Java `.properties` key escapes a separator it
/// holds, and the files that do it are not a curiosity — a real
/// `allowed_sounds.properties` is 2196 settings, 1476 of them written
/// `minecraft\:some.sound=true`. Reading one of those escapes as the
/// separator makes every key its own namespace prefix: the file reads as 36
/// settings, a push rewrites a line belonging to a different setting, and a
/// removal takes out all 1476 lines.
fn separator_at(body: &str, sep: Separator) -> Option<usize> {
    let mut escaped = false;
    for (at, ch) in body.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == sep.ch() {
            return Some(at);
        }
    }
    None
}

/// Whether this is a key one of these files could hold.
///
/// No space, so a sentence in a file of some other format is not read as a
/// setting; neither separator unescaped, so the same file cannot be read two
/// ways depending on which was tried first — `level-type=minecraft\:normal`
/// is a key-and-equals line and is not a key of `level-type=minecraft\`; and
/// none of the characters that start a structure in the formats this must say
/// no to, so a line of JSON or a quoted TOML table header is not a setting.
///
/// What a backslash escapes is part of the name, which is the only way a key
/// holds a colon at all. Raw text in and raw text out, as everywhere here:
/// the escape stays in the key Geode names the setting by, so the key it
/// matches on and the key it writes are the file's own bytes.
fn named(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let mut escaped = false;
    for c in key.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c.is_whitespace() || c.is_control() || "={}[]\",:".contains(c) {
            return false;
        }
    }
    true
}

/// The spacing to put around an appended key's separator: whatever the file's
/// first setting uses. One rule, so a file written `k = v` throughout does not
/// gain a line written `k=v`.
fn spacing<'t>(text: &'t str, found: &[Entry]) -> (&'t str, &'t str) {
    match found.first() {
        Some(e) => {
            let left = &text[..e.sep_at];
            let before = left.len() - left.trim_end_matches([' ', '\t']).len();
            (
                &text[e.sep_at - before..e.sep_at],
                &text[e.sep_at + 1..e.value.start],
            )
        }
        None => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set<'a>(key: &'a str, value: &'a str) -> Edit<'a> {
        Edit {
            key,
            value: Some(value),
        }
    }

    fn remove(key: &str) -> Edit<'_> {
        Edit { key, value: None }
    }

    /// The whole point: one setting changes, and the file is otherwise the
    /// bytes it was — the comment above the key, the key below it, the blank
    /// line, and the spacing of a line written another way.
    #[test]
    fn a_changed_setting_is_the_only_line_that_moves() {
        let before = "#Mon Oct 19 23:58:04 EDT 2026\n\
                      # Range: 0 ~ 32\nrenderDistance=8\n\nfov = 70\n";
        let after = splice(before, Separator::Equals, &[set("renderDistance", "12")]);
        assert_eq!(
            after,
            "#Mon Oct 19 23:58:04 EDT 2026\n\
             # Range: 0 ~ 32\nrenderDistance=12\n\nfov = 70\n"
        );
    }

    /// A value is raw text in and raw text out. Java's own escaping and a
    /// keybind's colons are both in real files, and a splice of a key beside
    /// them must not touch either — nor may the key they sit on be read as
    /// ending at the first colon of its value.
    #[test]
    fn an_escaped_value_and_one_full_of_colons_are_left_exactly_as_they_were() {
        let properties = "level-type=minecraft\\:normal\nmotd=hi\n";
        assert_eq!(
            splice(properties, Separator::Equals, &[set("motd", "hello")]),
            "level-type=minecraft\\:normal\nmotd=hello\n"
        );

        let options = "key_iris.keybind.reload:key.keyboard.r:CONTROL\nfov:70\n";
        assert_eq!(
            splice(options, Separator::Colon, &[set("fov", "90")]),
            "key_iris.keybind.reload:key.keyboard.r:CONTROL\nfov:90\n"
        );
        assert_eq!(
            values(options, Separator::Colon)[0],
            (
                "key_iris.keybind.reload".to_string(),
                "key.keyboard.r:CONTROL".to_string()
            )
        );
    }

    /// Each line keeps its own spacing: the one that changed, and the ones
    /// that did not.
    #[test]
    fn a_changed_line_keeps_the_spacing_the_file_gave_it() {
        let before = "current_profile = default\nRMBTweak=1\n";
        let after = splice(
            before,
            Separator::Equals,
            &[set("current_profile", "mine"), set("RMBTweak", "0")],
        );
        assert_eq!(after, "current_profile = mine\nRMBTweak=0\n");
    }

    /// CRLF stays CRLF and a file with no final newline gains none, appends
    /// included — a writer that normalises either rewrites every line of the
    /// file as far as any later diff can tell.
    #[test]
    fn the_files_line_endings_and_final_newline_survive_a_splice() {
        let crlf = "a=1\r\nb=2\r\n";
        assert_eq!(
            splice(crlf, Separator::Equals, &[set("b", "3")]),
            "a=1\r\nb=3\r\n"
        );
        assert_eq!(
            splice(crlf, Separator::Equals, &[set("c", "4")]),
            "a=1\r\nb=2\r\nc=4\r\n"
        );

        let bare = "a=1\nb=2";
        assert_eq!(
            splice(bare, Separator::Equals, &[set("b", "3")]),
            "a=1\nb=3"
        );
        assert_eq!(
            splice(bare, Separator::Equals, &[set("c", "4")]),
            "a=1\nb=2\nc=4"
        );
    }

    /// A key the file lacks is appended, in the file's own spacing; a removal
    /// takes out exactly that line.
    #[test]
    fn a_missing_key_is_appended_and_a_removed_one_takes_its_line_with_it() {
        let before = "a = 1\nb = 2\n";
        assert_eq!(
            splice(before, Separator::Equals, &[set("c", "3")]),
            "a = 1\nb = 2\nc = 3\n"
        );
        assert_eq!(
            splice(before, Separator::Equals, &[remove("a")]),
            "b = 2\n"
        );
        // The last line with no terminator of its own takes the one above it,
        // so the file still ends without a final newline.
        assert_eq!(splice("a=1\nb=2", Separator::Equals, &[remove("b")]), "a=1");
    }

    /// A key named twice means what its last line says, so that is the line a
    /// change rewrites — and a removal has to take both or the setting is
    /// still there.
    #[test]
    fn a_key_the_file_names_twice_is_read_and_written_at_its_last_line() {
        let before = "a=1\na=2\n";
        assert_eq!(
            values(before, Separator::Equals),
            vec![("a".to_string(), "2".to_string())]
        );
        assert_eq!(
            splice(before, Separator::Equals, &[set("a", "3")]),
            "a=1\na=3\n"
        );
        assert_eq!(splice(before, Separator::Equals, &[remove("a")]), "");
    }

    /// What this format is and is not. A properties file and an `options.txt`
    /// each read as themselves; TOML, JSON and prose do not read as either,
    /// because the answer to "I do not recognise this" has to be no.
    #[test]
    fn only_a_file_that_is_all_keys_and_values_reads_as_one() {
        assert!(is_key_value("# c\na=1\n\nb = 2\n", Separator::Equals));
        assert!(is_key_value("version:3955\nfov:70\n", Separator::Colon));
        // One key per line, and the last without a terminator.
        assert!(is_key_value("a=1\nb=2", Separator::Equals));

        // TOML: the table header is not a setting.
        assert!(!is_key_value("[general]\na = 1\n", Separator::Equals));
        // JSON: the braces are not, and `{"a"` is not a key.
        assert!(!is_key_value("{\n\"a\": 1\n}\n", Separator::Colon));
        // Prose, and nothing at all.
        assert!(!is_key_value("this is a note\n", Separator::Equals));
        assert!(!is_key_value("", Separator::Equals));
        assert!(!is_key_value("# only a comment\n", Separator::Equals));
        // A properties line is not an `options.txt` line read at its first
        // colon: the key would hold the equals, so this is not that format.
        assert!(!is_key_value("level-type=minecraft\\:normal\n", Separator::Colon));
    }

    /// A Java `.properties` key escapes a colon it holds, and a real
    /// `allowed_sounds.properties` is 2196 settings with 1476 of them written
    /// that way. Read at a colon the key escapes, every one of them collapses
    /// to its namespace prefix: the file is 36 settings instead of 2196, a
    /// push rewrites a line belonging to a different setting, and a removal
    /// takes out all 1476 lines.
    #[test]
    fn a_key_that_escapes_a_colon_is_read_at_the_separator_it_does_not_escape() {
        let before = "minecraft\\:block.anvil.land=true\n\
                      minecraft\\:weather.rain.above=false\n";
        // The equals is the separator, and the escaped colons are part of the
        // keys — so this is not a file of colon-separated settings at all.
        assert!(is_key_value(before, Separator::Equals));
        assert!(!is_key_value(before, Separator::Colon));
        assert_eq!(
            values(before, Separator::Equals),
            vec![
                (
                    "minecraft\\:block.anvil.land".to_string(),
                    "true".to_string()
                ),
                (
                    "minecraft\\:weather.rain.above".to_string(),
                    "false".to_string()
                ),
            ]
        );

        // The setting named is the only line that changes, and the one below
        // it — which shares its prefix — keeps its own value.
        assert_eq!(
            splice(
                before,
                Separator::Equals,
                &[set("minecraft\\:block.anvil.land", "false")]
            ),
            "minecraft\\:block.anvil.land=false\n\
             minecraft\\:weather.rain.above=false\n"
        );
        assert_eq!(
            splice(
                before,
                Separator::Equals,
                &[remove("minecraft\\:block.anvil.land")]
            ),
            "minecraft\\:weather.rain.above=false\n"
        );
    }
}

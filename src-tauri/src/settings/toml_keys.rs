//! Forge and NeoForge TOML, located and spliced one setting at a time.
//!
//! The single biggest group of real config files by far — 753 of the TOML
//! files under the `config/` folders measured on one machine, every Forge and
//! NeoForge mod — and the one where "change one thing" pays off most, because
//! the comments beside each setting are the only documentation it has:
//!
//! ```toml
//! [general]
//!     #Default: 2
//!     #Range: 0 ~ 4
//!     level = 2
//! ```
//!
//! So the rule is the same one [`kv`](super::kv) follows, reached a different
//! way: every edit is a **byte span** in the text the file already holds.
//! `toml_edit` is asked where each value *is* — it reports a span per value —
//! and [`text::patch`](super::text::patch) puts the new bytes there. Nothing
//! is ever re-rendered, so the comment block above the setting, the tab
//! indentation, the order, the file's CRLF and its final newline all survive
//! because nothing wrote them.
//!
//! That is worth saying twice, because the obvious route does not work. Edit a
//! value through `toml_edit`'s document and write the document back and the
//! file is mostly intact — but a key written with `insert()`, or by assigning
//! a fresh `Item`, **moves to the top of its table and loses the comment
//! block above it**: measured, 16 lines differing on a 24-line file instead of
//! one. And a round trip normalises CRLF to LF and adds a missing final
//! newline, which rewrites every line of the file as far as any later diff or
//! hash can tell — 544 of those 753 files are CRLF. Splicing spans has neither
//! problem to solve. The `toml` crate already in the tree is further still
//! from usable: parse one of these files to a map and print it and 1150 bytes
//! come back as 299, every comment gone, the keys alphabetised, the
//! indentation lost.
//!
//! Three rules, each measured against those files:
//!
//! - An **array or an inline table is one value**, replaced whole and never
//!   merged element by element. It falls out of splicing the value's span, and
//!   it is honest: across 258 real TOML and JSON files there were 251 arrays
//!   against 7701 scalar settings, so element-wise merging would buy 3.2% of
//!   settings at the price of a rule nobody could predict.
//! - A file holding a **list of sections** — `[[modules_right]]` — cannot be
//!   read per setting at all, so [`is_toml`] says no to it and it is sent once
//!   instead. An index is not a name: a player who reorders or deletes one of
//!   those sections would have the sender's value written into a different one,
//!   which parses and is wrong. 5 of the 753 files are like this.
//! - A pushed value is **parsed before it is spliced**. Not to re-render it —
//!   the raw text is what goes in — but because a value is the one piece of a
//!   push that comes from somewhere else, and `1\n[evil]\nx = 1` must not be
//!   able to become a section in the player's file. `toml_edit` refuses
//!   anything that is not exactly one value, trailing comment included.

use std::ops::Range;

use toml_edit::{Document, Item, Key, Table};

use super::text;
use crate::error::{Error, Result};

/// A change to one setting: a new raw value, or nothing at all to take it out.
#[derive(Debug, Clone, Copy)]
pub struct Edit<'a> {
    /// The setting, segment by segment: `["general", "level"]` for `level`
    /// under `[general]`. Segments are matched literally, so a table name
    /// holding dots, spaces or colons — `["Configs for Create: Tweaked
    /// Controllers"]` is real — needs no quoting rules anywhere.
    pub key: &'a [String],
    pub value: Option<&'a str>,
}

/// Whether Geode can find a single setting inside this text.
///
/// Strict, because the answer to "I do not recognise this" has to be no rather
/// than a guess: it has to parse as TOML, it has to hold at least one setting
/// — an empty file and a file of nothing but comments both parse — and it must
/// hold no list of sections.
pub fn is_toml(text: &str) -> bool {
    read(text).is_some_and(|found| !found.is_empty())
}

/// Every setting the text holds, as a key path and the raw text of its value.
///
/// Empty for a text [`is_toml`] says no to, which is the same answer as a file
/// with no settings in it and means the same thing here: nothing to compare
/// and nothing to splice.
pub fn values(text: &str) -> Vec<(Vec<String>, String)> {
    read(text)
        .unwrap_or_default()
        .into_iter()
        .map(|(key, span)| (key, text[span].to_string()))
        .collect()
}

/// Apply edits to the text, rewriting only the settings they name.
///
/// A setting the file does not have is written into the section it belongs to,
/// after that section's last setting and keeping its indentation and its
/// spacing around the `=`; a section the file does not have at all is written
/// at the end. That is for the same reason [`kv::splice`](super::kv::splice)
/// appends: a setting the sender pushed has to arrive even into a file written
/// before that setting existed, and a mod reading its own config takes a
/// setting it did not write as a default it did not need.
///
/// A removal takes out the setting's own line, or its own lines where its
/// value is written over several. The comment block above it is left alone: a
/// comment belongs to whoever wrote it, and guessing how much of it documented
/// the setting rather than the section is not a guess worth making when the
/// result cannot be undone.
pub fn splice(text: &str, edits: &[Edit]) -> Result<String> {
    let doc = Document::parse(text).map_err(|_| unreadable())?;
    let root = doc.as_table();

    let mut cuts: Vec<(Range<usize>, String)> = Vec::new();
    for edit in edits {
        let at = locate(root, edit.key)?;
        match (edit.value, at) {
            (Some(value), Some(at)) => {
                parsed(value, edit.key)?;
                cuts.push((at.value, value.to_string()));
            }
            (Some(value), None) => {
                parsed(value, edit.key)?;
                cuts.push(insertion(text, root, edit.key, value)?);
            }
            (None, Some(at)) => {
                let line = text::line_of(text, at.key.start..at.value.end);
                cuts.push((text::removal(text, line), String::new()));
            }
            // A setting to take out of a file that does not have it.
            (None, None) => {}
        }
    }
    Ok(text::patch(text, cuts))
}

/// Where one setting is: the span of its name, and the span of its raw value.
struct At {
    key: Range<usize>,
    value: Range<usize>,
}

/// Every setting in the document, as a key path and the span of its raw value.
///
/// `None` for a text no key can address: one that does not parse as TOML, and
/// one holding a list of sections.
fn read(text: &str) -> Option<Vec<(Vec<String>, Range<usize>)>> {
    let doc = Document::parse(text).ok()?;
    let mut found = Vec::new();
    collect(doc.as_table(), &mut Vec::new(), &mut found)?;
    Some(found)
}

fn collect(
    table: &Table,
    path: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, Range<usize>)>,
) -> Option<()> {
    for (name, item) in table.iter() {
        path.push(name.to_string());
        match item {
            Item::Value(value) => out.push((path.clone(), value.span()?)),
            Item::Table(inner) => collect(inner, path, out)?,
            // A list of sections, which no key can name a setting inside.
            Item::ArrayOfTables(_) => return None,
            Item::None => {}
        }
        path.pop();
    }
    Some(())
}

/// Find one setting, or `None` when the file does not have it — which is not
/// an error, because that is the file a push has to be written into.
///
/// An error is for a key that cannot mean a setting in this file at all: one
/// naming a setting inside a value, and one naming a whole section. Both are
/// refused by name rather than guessed at, because a guess here writes bytes
/// into somebody's file somewhere they did not mean.
fn locate(root: &Table, key: &[String]) -> Result<Option<At>> {
    let Some((last, prefix)) = key.split_last() else {
        return Err(Error::msg(
            "the server sent a setting with no name, so Geode left this file as it is",
        ));
    };
    let Some(table) = section(root, prefix, key)? else {
        return Ok(None);
    };
    match table.get(last) {
        Some(Item::Value(value)) => Ok(Some(At {
            key: table.key(last).and_then(|k| k.span()).unwrap_or_default(),
            value: value.span().unwrap_or_default(),
        })),
        Some(_) => Err(Error::msg(format!(
            "the server set '{}', which is a section in this file and not a setting",
            shown(key)
        ))),
        None => Ok(None),
    }
}

/// The section a setting lives in, or `None` when this file has no such
/// section.
fn section<'t>(root: &'t Table, prefix: &[String], key: &[String]) -> Result<Option<&'t Table>> {
    let mut table = root;
    for segment in prefix {
        match table.get(segment) {
            Some(Item::Table(inner)) => table = inner,
            Some(_) => {
                return Err(Error::msg(format!(
                    "the server set '{}', which names a setting inside a single value",
                    shown(key)
                )))
            }
            None => return Ok(None),
        }
    }
    Ok(Some(table))
}

/// Where a setting this file does not have goes, and the line to put there.
fn insertion(
    text: &str,
    root: &Table,
    key: &[String],
    value: &str,
) -> Result<(Range<usize>, String)> {
    let ending = text::line_ending(text);
    let (last, prefix) = key.split_last().expect("located first");
    let name = Key::new(last.as_str()).to_string();

    // A section this file writes no header for has no line to add to, and
    // that is two cases rather than one: a section the file does not have,
    // and a section only *implied* by a sub-section's header — `[a.b.c]`
    // written without `[a.b]`, which 129 of the 1078 real TOML files measured
    // do. Treating the second as a section to add to writes the setting at
    // the top of the file with no header above it at all, where it reads back
    // as a root key of its own name and reaches the mod as nothing.
    let Some(table) =
        section(root, prefix, key)?.filter(|table| prefix.is_empty() || !table.is_implicit())
    else {
        // So the section is written too, at the end and after a blank line,
        // the way a person adding one would. TOML lets a section be opened
        // after one of its own sub-sections, which is what makes the implied
        // case legal to write this way.
        let mut put = String::new();
        if !text.is_empty() && !text.ends_with('\n') {
            put.push_str(ending);
        }
        let header: Vec<String> = prefix.iter().map(|s| Key::new(s.as_str()).to_string()).collect();
        put.push_str(ending);
        put.push_str(&format!("[{}]{ending}{name} = {value}{ending}", header.join(".")));
        return Ok((text.len()..text.len(), put));
    };
    // `a.b = 1` writes its section into every one of its own lines, so a
    // setting added to one has to be written `a.c = 2` rather than under a
    // header — and writing the header instead would make the file unreadable,
    // since a section cannot be opened twice. No file in the 753 measured is
    // written this way, so it is refused by name instead of guessed at.
    if table.is_dotted() {
        return Err(Error::msg(format!(
            "the server set '{}', which this file writes a different way than Geode can add to",
            shown(key)
        )));
    }

    // After the section's last setting, which is also what says how this file
    // writes one: its indentation, and its spacing around the `=`.
    let last_setting = settings(table).into_iter().max_by_key(|(_, value)| value.end);
    let (line, indent, between) = match last_setting {
        Some((key_at, value_at)) => {
            let line = text::line_of(text, value_at.clone());
            let indent: String = text[line.clone()]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            (line, indent, text[key_at.end..value_at.start].to_string())
        }
        // A section with no settings of its own yet: after its header, or —
        // for the top level, which has no header — before everything.
        None => match table.span().filter(|_| !prefix.is_empty()) {
            Some(header) => (text::line_of(text, header), String::new(), " = ".to_string()),
            None => (0..0, String::new(), " = ".to_string()),
        },
    };

    let put = if line.is_empty() || text[line.clone()].ends_with('\n') {
        format!("{indent}{name}{between}{value}{ending}")
    } else {
        // The file's last line has no terminator of its own, and gains none.
        format!("{ending}{indent}{name}{between}{value}")
    };
    Ok((line.end..line.end, put))
}

/// The settings written directly in this section, as the spans of each one's
/// name and raw value. Not its sub-sections, whose lines come after this
/// section's own and belong inside them.
fn settings(table: &Table) -> Vec<(Range<usize>, Range<usize>)> {
    table
        .iter()
        .filter_map(|(name, item)| {
            Some((
                table.key(name)?.span()?,
                item.as_value().and_then(|value| value.span())?,
            ))
        })
        .collect()
}

/// Hold a pushed value to being exactly one TOML value, which is the one part
/// of a push whose text came from somewhere else.
fn parsed(value: &str, key: &[String]) -> Result<()> {
    value
        .parse::<toml_edit::Value>()
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

    fn set<'a>(key: &'a [String], value: &'a str) -> Edit<'a> {
        Edit {
            key,
            value: Some(value),
        }
    }

    fn key(path: &str) -> Vec<String> {
        path.split('.').map(str::to_string).collect()
    }

    /// The file a real Forge mod writes, and the whole point of the format
    /// being read per setting: one line differs, and the `#Default:` and
    /// `#Range:` comments that are the only documentation the setting has —
    /// along with the tab the body is indented with — are exactly where they
    /// were.
    #[test]
    fn a_changed_setting_is_the_only_line_that_moves_comments_and_indentation_intact() {
        let before = "#General settings\n\
                      [general]\n\
                      \t#Default: 2\n\
                      \t#Range: 0 ~ 4\n\
                      \tlevel = 2\n\
                      \t#Allowed Values: XBOX, NINTENDO, PLAYSTATION\n\
                      \tbrand = \"XBOX\"\n";
        let out = splice(before, &[set(&key("general.level"), "4")]).unwrap();
        assert_eq!(
            out,
            "#General settings\n\
             [general]\n\
             \t#Default: 2\n\
             \t#Range: 0 ~ 4\n\
             \tlevel = 4\n\
             \t#Allowed Values: XBOX, NINTENDO, PLAYSTATION\n\
             \tbrand = \"XBOX\"\n"
        );
        assert_eq!(
            before.lines().zip(out.lines()).filter(|(a, b)| a != b).count(),
            1
        );
    }

    /// A section named with spaces and a colon is real — `["Configs for
    /// Create: Tweaked Controllers"]` is a file on this machine — and is
    /// reached by naming it as a segment, with no quoting rules anywhere.
    #[test]
    fn a_setting_under_a_quoted_section_name_is_found_by_its_segments() {
        let before = "[\"Configs for Create: Tweaked Controllers\"]\n\
                      enabled = true\n";
        let path = vec![
            "Configs for Create: Tweaked Controllers".to_string(),
            "enabled".to_string(),
        ];
        assert_eq!(
            values(before),
            vec![(path.clone(), "true".to_string())]
        );
        assert_eq!(
            splice(before, &[set(&path, "false")]).unwrap(),
            "[\"Configs for Create: Tweaked Controllers\"]\nenabled = false\n"
        );
    }

    /// An array and an inline table are each one value: replaced whole, never
    /// merged element by element — including an array written over several
    /// lines, whose every line is the one value.
    #[test]
    fn an_array_and_an_inline_table_are_each_replaced_whole() {
        let before = "packs = [\n  \"vanilla\",\n  \"fabric\",\n]\n\
                      general = {disable_mod = false, size = 1}\n";
        assert_eq!(
            values(before),
            vec![
                (key("packs"), "[\n  \"vanilla\",\n  \"fabric\",\n]".to_string()),
                (
                    key("general"),
                    "{disable_mod = false, size = 1}".to_string()
                ),
            ]
        );
        let out = splice(
            before,
            &[
                set(&key("packs"), "[\"vanilla\"]"),
                set(&key("general"), "{disable_mod = true}"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "packs = [\"vanilla\"]\ngeneral = {disable_mod = true}\n"
        );
    }

    /// Both conventions a file can have survive, because nothing here writes a
    /// byte it was not handed: a CRLF file stays CRLF, a file with no final
    /// newline gains none, and one line differs either way.
    #[test]
    fn the_files_line_endings_and_final_newline_survive_a_splice() {
        let crlf = "[general]\r\n\tlevel = 2\r\n\tbrand = \"XBOX\"\r\n";
        let out = splice(crlf, &[set(&key("general.level"), "4")]).unwrap();
        assert_eq!(out, "[general]\r\n\tlevel = 4\r\n\tbrand = \"XBOX\"\r\n");

        let bare = "[general]\nlevel = 2";
        let out = splice(bare, &[set(&key("general.level"), "4")]).unwrap();
        assert_eq!(out, "[general]\nlevel = 4");

        // And an added setting takes the file's own ending and leaves its
        // final-newline state as it found it.
        let out = splice(crlf, &[set(&key("general.extra"), "true")]).unwrap();
        assert_eq!(
            out,
            "[general]\r\n\tlevel = 2\r\n\tbrand = \"XBOX\"\r\n\textra = true\r\n"
        );
        let out = splice(bare, &[set(&key("general.extra"), "true")]).unwrap();
        assert_eq!(out, "[general]\nlevel = 2\nextra = true");
    }

    /// A setting the file does not have is written into the section it belongs
    /// to — keeping that section's indentation and its spacing — and a section
    /// the file does not have is written at the end. Both still read back as
    /// the setting that was pushed, which is the whole requirement.
    #[test]
    fn a_setting_the_file_lacks_is_added_and_reads_back() {
        let before = "[general]\n\tlevel = 2\n\n[client]\n\tfancy = true\n";
        let out = splice(
            before,
            &[
                set(&key("general.added"), "\"yes\""),
                set(&key("server.ticks"), "20"),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "[general]\n\tlevel = 2\n\tadded = \"yes\"\n\n[client]\n\tfancy = true\n\n\
             [server]\nticks = 20\n"
        );
        assert_eq!(
            values(&out),
            vec![
                (key("general.level"), "2".to_string()),
                (key("general.added"), "\"yes\"".to_string()),
                (key("client.fancy"), "true".to_string()),
                (key("server.ticks"), "20".to_string()),
            ]
        );
    }

    /// A section a file only *implies* — `[a.b.c]` written without `[a.b]`,
    /// which 129 of the 1078 real TOML files measured do, a real
    /// `lambdynlights.toml` among them — has no line to add a setting to, so
    /// the header is written for it. Taken for a section to add to, the
    /// setting goes in at the very top of the file instead, above the file's
    /// own first comment and under no header at all, where it reads back as a
    /// root key of its own name and reaches the mod as nothing.
    #[test]
    fn a_setting_whose_section_is_only_implied_gets_a_header_of_its_own() {
        let before = "# LambDynamicLights configuration.\n\
                      [light_sources.settings.entities.minecraft]\n\
                      \tzombie = true\n";
        let out = splice(
            before,
            &[set(&key("light_sources.settings.enabled"), "true")],
        )
        .unwrap();
        assert!(
            out.starts_with(before),
            "the file's own lines moved: {out:?}"
        );
        let mut found = values(&out);
        found.sort();
        assert_eq!(
            found,
            vec![
                (key("light_sources.settings.enabled"), "true".to_string()),
                (
                    key("light_sources.settings.entities.minecraft.zombie"),
                    "true".to_string()
                ),
            ]
        );
    }

    /// A setting taken out loses its own line and nothing else — a value
    /// written over several lines included — and the comments above it stay
    /// where whoever wrote them put them.
    #[test]
    fn a_removed_setting_takes_its_own_lines_and_leaves_the_comments() {
        let before = "[general]\n\t# documented\n\tlevel = 2\n\tpacks = [\n\t\t1,\n\t]\n";
        let out = splice(
            before,
            &[
                Edit {
                    key: &key("general.level"),
                    value: None,
                },
                Edit {
                    key: &key("general.packs"),
                    value: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(out, "[general]\n\t# documented\n");
    }

    /// A value is the one part of a push whose text came from somewhere else,
    /// so it has to be exactly one TOML value. Anything that would turn into
    /// more of the file than its own value — a section, another setting, a
    /// comment — is refused and the file is left alone.
    #[test]
    fn a_value_that_is_not_one_value_is_refused_and_nothing_is_written() {
        for value in ["1\n[evil]\nx = 1", "1 # and a comment", "", "[1,2"] {
            let err = splice(
                "[general]\nlevel = 2\n",
                &[set(&key("general.level"), value)],
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("general.level"), "{value:?}: {err}");
        }
    }

    /// What Geode will not try to read one setting at a time, each for its own
    /// stated reason — and what it will.
    #[test]
    fn only_a_file_whose_settings_can_each_be_named_reads_as_one() {
        assert!(is_toml("[general]\nlevel = 2\n"));
        assert!(is_toml("level = 2\n"));
        // A list of sections: an index is not a name, so no setting inside one
        // can be addressed and the whole file is left to be sent once.
        assert!(!is_toml("[[modules_right]]\nname = \"fps\"\n"));
        assert!(!is_toml(
            "[general]\nlevel = 2\n[[modules]]\nname = \"fps\"\n"
        ));
        // Parses, holds no setting: an empty file, and a file of comments.
        assert!(!is_toml(""));
        assert!(!is_toml("#Allowed Values: XBOX\n"));
        // Not TOML at all.
        assert!(!is_toml("{\"renderDistance\": 8}"));
        assert!(!is_toml("general {\n\tB:enable=true\n}\n"));
        assert!(!is_toml("[Window][Debug##Default]\nPos=60,60\n"));
        assert!(!is_toml("level-type=minecraft\\:normal\n"));
    }

    /// A key that cannot mean a setting in this file is refused by name, so
    /// nothing is written anywhere unintended — and a setting elsewhere in the
    /// file keeps every byte it had, scientific notation and format markers
    /// included.
    #[test]
    fn a_key_that_cannot_name_a_setting_here_is_refused_by_name() {
        let before = "_version = 5\nrate = 3.051850947599719E-5\n[general]\nlevel = 2\n";
        for path in ["general", "rate.inner"] {
            let err = splice(before, &[set(&key(path), "1")])
                .unwrap_err()
                .to_string();
            assert!(err.contains(path), "{path}: {err}");
        }
        // And the splice of a different setting leaves both of those bytes
        // exactly as they were.
        let out = splice(before, &[set(&key("general.level"), "7")]).unwrap();
        assert_eq!(
            out,
            "_version = 5\nrate = 3.051850947599719E-5\n[general]\nlevel = 7\n"
        );
    }
}

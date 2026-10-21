//! Settings files, read and written one setting at a time.
//!
//! A server ships a config folder, players tune it, and then the admin changes
//! one setting and wants that one setting — and nothing else — to reach
//! everybody. The mechanism for that is in [`crate::share::manifest::Settings`]:
//! the admin's machine works out which **keys** moved, the pack carries those
//! keys and their raw values, and a player's Geode splices them into whatever
//! they have made of the file. No player stores a baseline, no player runs a
//! diff, and nothing can touch a key the admin did not name.
//!
//! This module is the part that has to know what a file is. It answers three
//! questions and nothing else:
//!
//! - [`sniff`] — what format is this, judged from its **content**. Never from
//!   its name: a real `polylib.json` holds `//` comments, a real
//!   `ad_astra.jsonc` holds block comments, and `.cfg` is `RMBTweak=1` in one
//!   mod and `current_profile = default` in the next. There is a defined
//!   answer for "I do not recognise this" — `None` — and a path whose files
//!   Geode cannot read per key cannot be kept in sync, which the admin is
//!   told rather than promised otherwise.
//! - [`diff`] — which keys moved between two copies of one file. Run once, on
//!   the admin's machine, in front of the one person who can tell whether it
//!   is right.
//! - [`apply`] — put those values where the old ones were, and leave every
//!   other byte alone.
//!
//! Today that is two families, each with a locator of its own: the
//! key-and-value one — `.properties`, flat `.cfg` and `options.txt`, every
//! file a player would call "settings" — and Forge and NeoForge TOML, which is
//! the biggest group of config files on disk by far. The JSON family is a
//! locator of its own and comes next; until it lands those files sniff as
//! unknown, which is the honest answer and not a wrong one.

pub mod kv;
pub mod push;
pub mod report;
pub mod text;
pub mod toml_keys;

use std::path::Path;

use crate::error::{Error, Result};
use crate::share::manifest::Override;

/// A format Geode can find a single setting inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One setting per line, split at the first separator.
    KeyValue(kv::Separator),
    /// Sections and settings, located by asking a TOML parser where each
    /// value is.
    Toml,
}

/// What moved between two copies of one settings file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub key: Vec<String>,
    /// What the published copy held, or `None` for a key that is new.
    pub was: Option<String>,
    /// What the file holds now, or `None` for a key that is gone.
    pub now: Option<String>,
}

/// One setting a splice put into a file, and what was there before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub key: Vec<String>,
    /// What this file held before the splice, raw. `None` when it had no such
    /// key and the setting was appended.
    pub had: Option<String>,
    /// What it holds now. `None` when the setting was removed.
    pub now: Option<String>,
    /// Whether what it held was the recipient's own change rather than the
    /// value the sender last published — which is the only thing here worth
    /// interrupting them about. See [`Override::was`].
    pub theirs: bool,
}

/// What this file is, worked out from its bytes.
///
/// `None` for anything not recognised, which includes a file in a format
/// Geode cannot yet read per key and a file somebody is halfway through
/// editing. Both get the same answer, because both mean the same thing: no
/// setting can be found inside it, so none can be spliced into it.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    let text = std::str::from_utf8(bytes).ok()?;
    // Equals first, which is the larger family; the two cannot both match,
    // because a key may hold neither separator (`kv::named`).
    for sep in [kv::Separator::Equals, kv::Separator::Colon] {
        if kv::is_key_value(text, sep) {
            return Some(Format::KeyValue(sep));
        }
    }
    // TOML last, and only for a file the line-at-a-time reading says no to. A
    // header-less TOML file of plain values is both at once, and the two
    // readings agree on it — the same keys, the same raw values, the same
    // spans — so the tie goes to the reading that was already proven rather
    // than to a parser.
    if toml_keys::is_toml(text) {
        return Some(Format::Toml);
    }
    None
}

/// Which settings moved from `published` to `now`, per key.
///
/// The two have to be the same format, which is the point of comparing per
/// key at all: a file a mod rewrote on boot has a new timestamp comment, new
/// defaults it did not have and reflowed comments, and none of that is a
/// setting that moved. A byte comparison says the whole file changed; this
/// says nothing changed, which is the truth the admin needs.
pub fn diff(published: &[u8], now: &[u8]) -> Result<Vec<Moved>> {
    let format = sniff(now).ok_or_else(|| {
        Error::msg(
            "Geode cannot read this file one setting at a time, so there is nothing to compare",
        )
    })?;
    if sniff(published) != Some(format) {
        return Err(Error::msg(
            "this file is not in the format it was in when its settings were last published, so \
             Geode cannot say which settings moved",
        ));
    }
    let text = std::str::from_utf8(now).map_err(not_text)?;
    let was_text = std::str::from_utf8(published).map_err(not_text)?;
    let (before, after) = match format {
        Format::KeyValue(sep) => (
            one_deep(kv::values(was_text, sep)),
            one_deep(kv::values(text, sep)),
        ),
        Format::Toml => (toml_keys::values(was_text), toml_keys::values(text)),
    };

    let mut moved = Vec::new();
    for (key, value) in &after {
        let was = before.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        if was.as_deref() != Some(value.as_str()) {
            moved.push(Moved {
                key: key.clone(),
                was,
                now: Some(value.clone()),
            });
        }
    }
    for (key, value) in &before {
        if !after.iter().any(|(k, _)| k == key) {
            moved.push(Moved {
                key: key.clone(),
                was: Some(value.clone()),
                now: None,
            });
        }
    }
    Ok(moved)
}

/// A key-and-value file's keys as the segment lists every other format
/// reports, since one of these files has exactly one segment per setting.
fn one_deep(values: Vec<(String, String)>) -> Vec<(Vec<String>, String)> {
    values
        .into_iter()
        .map(|(key, value)| (vec![key], value))
        .collect()
}

/// Splice settings into a file's own bytes, leaving every other byte alone.
///
/// The overrides must all be for this one file; the caller groups them. Order
/// does not matter, because the list is cumulative — one entry per key — so
/// two entries never name the same setting.
pub fn apply(bytes: &[u8], overrides: &[Override]) -> Result<(Vec<u8>, Vec<Applied>)> {
    let format = sniff(bytes).ok_or_else(|| {
        Error::msg("Geode cannot read this file one setting at a time, so it was left as it is")
    })?;
    let text = std::str::from_utf8(bytes).map_err(not_text)?;
    let held = match format {
        Format::KeyValue(sep) => one_deep(kv::values(text, sep)),
        Format::Toml => toml_keys::values(text),
    };

    let mut wanted: Vec<&Override> = Vec::new();
    let mut applied = Vec::new();
    for entry in overrides {
        // What the format cannot hold at all, refused by name first and
        // whether or not it would change a byte.
        if let Format::KeyValue(_) = format {
            one_line(entry)?;
        }
        let had = held
            .iter()
            .find(|(key, _)| *key == entry.key)
            .map(|(_, value)| value.clone());
        if had.is_none() && entry.value.is_none() {
            // Nothing to remove, so nothing happened and nothing is said.
            continue;
        }
        if had == entry.value {
            continue;
        }
        wanted.push(entry);
        applied.push(Applied {
            key: entry.key.clone(),
            theirs: had.is_some() && had != entry.was,
            had,
            now: entry.value.clone(),
        });
    }
    if wanted.is_empty() {
        return Ok((bytes.to_vec(), applied));
    }
    let next = match format {
        Format::KeyValue(sep) => {
            let mut edits = Vec::with_capacity(wanted.len());
            for entry in &wanted {
                edits.push(kv::Edit {
                    key: one_line(entry)?,
                    value: entry.value.as_deref(),
                });
            }
            kv::splice(text, sep, &edits)
        }
        Format::Toml => {
            let edits: Vec<toml_keys::Edit> = wanted
                .iter()
                .map(|entry| toml_keys::Edit {
                    key: &entry.key,
                    value: entry.value.as_deref(),
                })
                .collect();
            toml_keys::splice(text, &edits)?
        }
    };
    Ok((next.into_bytes(), applied))
}

/// The one segment a file of one setting per line names a setting by, or the
/// reason this entry cannot be one of its lines.
///
/// A deeper key belongs to a format with tables in it, and a pack naming one
/// here was not written by this Geode. A value holding a line break is worse:
/// the splice puts the value's bytes where the old value's were, so the lines
/// after the first would be settings the push never named — and "nothing else
/// in them" is what the player's notice promises. `Override`'s own doc rests
/// on segments and values having no syntax to abuse, and for this format a
/// line break is that syntax.
fn one_line(entry: &Override) -> Result<&str> {
    let [key] = entry.key.as_slice() else {
        return Err(Error::msg(format!(
            "the server set '{}', which names a setting inside another setting — that file \
             holds one setting per line",
            entry.key.join(".")
        )));
    };
    if entry
        .value
        .as_deref()
        .is_some_and(|value| value.contains(['\n', '\r']))
    {
        return Err(Error::msg(format!(
            "the server set '{key}' to a value of more than one line, and that file holds one \
             setting per line"
        )));
    }
    Ok(key)
}

fn not_text(_: std::str::Utf8Error) -> Error {
    Error::msg("this file is not text Geode can read")
}

/// Splice the sender's settings into one file on disk, and say what moved.
///
/// Read, splice, write back — and the write refuses anything that is not an
/// ordinary file, so a settings path can never be a way to write through a
/// symlink. `extract_overrides` has written through one since before any of
/// this and is left as it is; this is new code and is right from the start,
/// which makes the difference a decision rather than an oversight.
///
/// A file the recipient has made read-only is left alone and reported, for the
/// same reason a whole-file write that fails is: a settings file they kept
/// their own copy of should cost them a line in a report, never their game.
/// The file's permissions are put back after the write, so a splice does not
/// quietly hand a 0600 config wider permissions than its owner gave it.
pub async fn splice_file(full: &Path, overrides: &[Override]) -> Result<Vec<Applied>> {
    let meta = tokio::fs::symlink_metadata(full).await?;
    if !meta.is_file() {
        return Err(Error::msg(
            "this is not an ordinary file, so Geode did not write to it".to_string(),
        ));
    }
    if meta.permissions().readonly() {
        return Err(Error::msg(
            "this file is read-only, so what you already had is still there".to_string(),
        ));
    }
    let bytes = tokio::fs::read(full).await?;
    let (next, applied) = apply(&bytes, overrides)?;
    if next != bytes {
        crate::server::write_atomic(full, &next).await?;
        tokio::fs::set_permissions(full, meta.permissions()).await?;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn over(path: &str, key: &str, value: Option<&str>, was: Option<&str>, at: u32) -> Override {
        Override {
            path: path.into(),
            key: vec![key.into()],
            value: value.map(str::to_string),
            was: was.map(str::to_string),
            at,
        }
    }

    /// A format is what a file holds, not what it is called. The two
    /// key-and-value shapes are told apart by their own content, and a file in
    /// a format this cannot read per key says so rather than being guessed at.
    #[test]
    fn a_files_format_is_read_from_its_bytes() {
        assert_eq!(
            sniff(b"renderDistance=8\n"),
            Some(Format::KeyValue(kv::Separator::Equals))
        );
        assert_eq!(
            sniff(b"version:3955\nfov:70\n"),
            Some(Format::KeyValue(kv::Separator::Colon))
        );
        // A properties file is read at its equals even though its value holds
        // an escaped colon — and even though its keys do.
        assert_eq!(
            sniff(b"level-type=minecraft\\:normal\n"),
            Some(Format::KeyValue(kv::Separator::Equals))
        );
        assert_eq!(
            sniff(b"minecraft\\:block.anvil.land=true\n"),
            Some(Format::KeyValue(kv::Separator::Equals))
        );
        // A file with sections in it is read as TOML, because nothing can
        // read it a line at a time.
        assert_eq!(sniff(b"[general]\na = 1\n"), Some(Format::Toml));
        // Not yet read per key: the JSON family, and bytes.
        assert_eq!(sniff(b"{\"renderDistance\": 8}"), None);
        assert_eq!(sniff(&[0x00, 0xff, 0xfe]), None);
    }

    /// What the admin presses Review for: the keys that moved, and nothing
    /// else — not the timestamp comment Java writes as line 2, not a comment
    /// a mod reflowed, and not a key whose spacing changed.
    #[test]
    fn a_diff_reports_the_settings_that_moved_and_nothing_a_mod_rewrote() {
        let published = b"#Sat Oct 17 23:58:04 EDT 2026\n\
                          # Range: 0 ~ 32\nrenderDistance=8\nfov=70\ngone=1\n";
        let now = b"#Mon Oct 19 11:02:55 EDT 2026\n\
                    # Range: 0 ~ 32 (chunks)\nrenderDistance = 12\nfov=70\nadded=true\n";
        let mut moved = diff(published, now).unwrap();
        moved.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(
            moved,
            vec![
                Moved {
                    key: vec!["added".into()],
                    was: None,
                    now: Some("true".into()),
                },
                Moved {
                    key: vec!["gone".into()],
                    was: Some("1".into()),
                    now: None,
                },
                Moved {
                    key: vec!["renderDistance".into()],
                    was: Some("8".into()),
                    now: Some("12".into()),
                },
            ]
        );

        // The same file rewritten by the mod that owns it, with every value
        // the same: a new timestamp, reflowed comments, a new default. Nothing
        // the admin changed, so nothing to push.
        let rewritten = b"#Mon Oct 19 11:02:55 EDT 2026\n\
                          #Range: 0 ~ 32\nrenderDistance = 8\nfov = 70\ngone=1\n";
        assert_eq!(diff(published, rewritten).unwrap(), Vec::new());
    }

    /// A push lands on the player's own copy: their value for that key is
    /// replaced, their value for every other key is not, and Geode can say
    /// which of the two it was for each one — the setting they had changed
    /// themselves, and the setting that still held what the server published.
    #[test]
    fn applying_a_push_replaces_the_named_settings_and_says_what_was_there() {
        let mine = b"renderDistance=24\nfov=70\n";
        let (next, applied) = apply(
            mine,
            &[
                over("config/a.properties", "renderDistance", Some("12"), Some("8"), 3),
                over("config/a.properties", "fov", Some("90"), Some("70"), 3),
            ],
        )
        .unwrap();
        assert_eq!(next, b"renderDistance=12\nfov=90\n");
        assert_eq!(
            applied,
            vec![
                Applied {
                    key: vec!["renderDistance".into()],
                    had: Some("24".into()),
                    now: Some("12".into()),
                    theirs: true,
                },
                Applied {
                    key: vec!["fov".into()],
                    had: Some("70".into()),
                    now: Some("90".into()),
                    theirs: false,
                },
            ]
        );
    }

    /// A setting already holding the pushed value is not written at all, so
    /// the same push applied twice changes nothing the second time — which is
    /// what makes a player's own change back stick for ever.
    #[test]
    fn a_setting_that_already_holds_the_pushed_value_is_left_alone() {
        let mine = b"renderDistance=12\n";
        let (next, applied) = apply(
            mine,
            &[over("config/a.properties", "renderDistance", Some("12"), Some("8"), 3)],
        )
        .unwrap();
        assert_eq!(next, mine);
        assert_eq!(applied, Vec::new());
    }

    /// The second format, reached through the same three questions as the
    /// first: a Forge TOML config sniffs as itself, a diff names the setting
    /// by its section and its own name, and a push lands on that one setting
    /// and leaves the comment documenting it where it was.
    #[test]
    fn a_toml_config_is_sniffed_diffed_and_spliced_by_section_and_name() {
        let published = b"[general]\n\t#Range: 0 ~ 4\n\tlevel = 2\n\tbrand = \"XBOX\"\n";
        let now = b"[general]\n\t#Range: 0 ~ 4\n\tlevel = 4\n\tbrand = \"XBOX\"\n";
        assert_eq!(sniff(published), Some(Format::Toml));
        assert_eq!(
            diff(published, now).unwrap(),
            vec![Moved {
                key: vec!["general".into(), "level".into()],
                was: Some("2".into()),
                now: Some("4".into()),
            }]
        );

        // The player's own copy, where they had already set it to something
        // of their own — so the push moves it and says it was theirs.
        let mine = b"[general]\n\t#Range: 0 ~ 4\n\tlevel = 3\n\tbrand = \"XBOX\"\n";
        let (next, applied) = apply(
            mine,
            &[Override {
                path: "config/a.toml".into(),
                key: vec!["general".into(), "level".into()],
                value: Some("4".into()),
                was: Some("2".into()),
                at: 3,
            }],
        )
        .unwrap();
        assert_eq!(next, now);
        assert_eq!(
            applied,
            vec![Applied {
                key: vec!["general".into(), "level".into()],
                had: Some("3".into()),
                now: Some("4".into()),
                theirs: true,
            }]
        );
    }

    /// A key named inside another key cannot exist in a file of one setting
    /// per line, so a pack that names one is refused by name rather than
    /// splicing bytes somewhere unintended.
    #[test]
    fn a_nested_key_is_refused_for_a_file_that_has_no_nesting() {
        let err = apply(
            b"a=1\n",
            &[Override {
                path: "config/a.properties".into(),
                key: vec!["general".into(), "a".into()],
                value: Some("2".into()),
                was: None,
                at: 1,
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("general.a"), "{err}");
    }

    /// A value holding a line break cannot be one line of a file that holds
    /// one setting per line: spliced in, the lines after the first would be
    /// settings nobody pushed, sitting in a file whose whole promise to the
    /// player is that nothing else in it moved. So it is refused by name and
    /// not a byte is written.
    #[test]
    fn a_value_of_more_than_one_line_is_refused_for_a_file_of_one_setting_per_line() {
        let mine = b"renderDistance=24\nfov=70\n";
        let err = apply(
            mine,
            &[over(
                "config/a.properties",
                "fov",
                Some("70\ninjected=yes"),
                None,
                3,
            )],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("fov") && err.contains("one line"), "{err}");

        // And the setting beside it is still the only thing that could have
        // changed, so a push naming both writes neither.
        let err = apply(
            mine,
            &[
                over("config/a.properties", "renderDistance", Some("12"), None, 3),
                over("config/a.properties", "fov", Some("90\r\nevil=1"), None, 3),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("one line"), "{err}");
    }
}

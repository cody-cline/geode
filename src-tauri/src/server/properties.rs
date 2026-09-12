//! `server.properties`, read and written without losing what is already there.
//!
//! The file is the server's, not ours. It arrives full of Mojang's comments and
//! in Mojang's order, and a round-trip through a map would hand it back sorted
//! and stripped — which is both rude and a nuisance to diff. So edits are
//! applied line by line, and anything we do not understand is copied through
//! untouched.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths::Paths;

/// One setting, in the order the file lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    pub key: String,
    pub value: String,
}

/// Read the settings out of a properties file's text.
pub fn parse(text: &str) -> Vec<Property> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        // `!` is a comment marker in the Java properties format too.
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        out.push(Property {
            key: key.trim().to_string(),
            // Values are kept verbatim: a MOTD's trailing space is a choice,
            // and section signs and colons are ordinary characters here.
            value: value.to_string(),
        });
    }
    out
}

/// Rewrite `original`, replacing the values of any keys in `changes`.
///
/// Comments, blank lines and ordering survive. A key that is not already in the
/// file is appended rather than dropped.
pub fn apply(original: &str, changes: &[Property]) -> String {
    let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out = String::with_capacity(original.len() + 64);

    for line in original.lines() {
        let trimmed = line.trim_start();
        let is_setting =
            !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with('!');

        let replacement = is_setting
            .then(|| trimmed.split_once('='))
            .flatten()
            .and_then(|(key, _)| {
                let key = key.trim();
                changes
                    .iter()
                    .find(|c| c.key == key)
                    .map(|c| (key, c.value.as_str()))
            });

        match replacement {
            Some((key, value)) => {
                written.insert(key);
                out.push_str(&format!("{key}={value}\n"));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    for change in changes {
        if !written.contains(change.key.as_str()) {
            out.push_str(&format!("{}={}\n", change.key, change.value));
        }
    }
    out
}

fn file(paths: &Paths, id: &str) -> std::path::PathBuf {
    paths.server_run(id).join("server.properties")
}

/// The server's settings, or nothing at all when it has never been started —
/// the file is written by the server on its first run, not by us.
pub async fn read(paths: &Paths, id: &str) -> Result<Vec<Property>> {
    match tokio::fs::read_to_string(file(paths, id)).await {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

pub async fn write(paths: &Paths, id: &str, changes: &[Property]) -> Result<()> {
    let path = file(paths, id);
    let original = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, apply(&original, changes)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
#Minecraft server properties
#Fri Sep 12 03:00:00 GMT 2026
enable-jmx-monitoring=false
level-name=world
motd=A Minecraft Server
max-players=20
";

    fn change(key: &str, value: &str) -> Property {
        Property {
            key: key.into(),
            value: value.into(),
        }
    }

    #[test]
    fn reads_settings_and_ignores_comments() {
        let props = parse(SAMPLE);
        assert_eq!(props.len(), 4);
        assert_eq!(props[0].key, "enable-jmx-monitoring");
        assert_eq!(props[1].value, "world");
    }

    #[test]
    fn a_value_may_contain_anything_including_equals() {
        let props = parse("motd=Welcome! x=y\n");
        assert_eq!(props[0].key, "motd");
        assert_eq!(props[0].value, "Welcome! x=y");
    }

    #[test]
    fn an_empty_value_is_still_a_setting() {
        let props = parse("resource-pack=\n");
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].value, "");
    }

    #[test]
    fn writing_keeps_the_comments_and_the_order() {
        let out = apply(SAMPLE, &[change("max-players", "40")]);
        assert!(out.starts_with("#Minecraft server properties"));
        assert!(out.contains("max-players=40"));
        assert!(!out.contains("max-players=20"));
        // Untouched settings stay where they were, in the order they were.
        let keys: Vec<String> = parse(&out).into_iter().map(|p| p.key).collect();
        assert_eq!(
            keys,
            vec!["enable-jmx-monitoring", "level-name", "motd", "max-players"]
        );
    }

    #[test]
    fn a_setting_the_file_lacks_is_added_rather_than_lost() {
        let out = apply(SAMPLE, &[change("view-distance", "12")]);
        assert!(out.contains("view-distance=12"));
        assert_eq!(parse(&out).len(), 5);
    }

    #[test]
    fn nothing_else_is_disturbed() {
        let out = apply(SAMPLE, &[change("motd", "Hello")]);
        let props = parse(&out);
        assert_eq!(props[3].value, "20");
        assert_eq!(props[2].value, "Hello");
    }

    #[test]
    fn an_empty_file_takes_every_setting() {
        let out = apply("", &[change("motd", "New"), change("max-players", "8")]);
        let props = parse(&out);
        assert_eq!(props.len(), 2);
    }
}

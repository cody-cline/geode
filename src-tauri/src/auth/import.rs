//! Importing an existing Minecraft session from another launcher.
//!
//! Registering an Azure application and getting it approved by Mojang is a lot
//! of ceremony for a personal install. If another launcher on this machine is
//! already signed in, its session token belongs to the same user and the same
//! account — Geode can just use it.
//!
//! What this deliberately does *not* do:
//!
//! - It never touches the other launcher's OAuth client ID or refresh token.
//!   Refreshing with someone else's application registration would be
//!   impersonating their app. We take only the Minecraft access token, which
//!   is the credential for *the user's* account, not for the launcher.
//! - Because of that, an imported session cannot be renewed here. It lasts as
//!   long as the issuing launcher's token does (around 24 hours). Re-opening
//!   that launcher refreshes it, and Geode re-reads the file on next launch.
//! - Tokens are never logged, never written anywhere except Geode's own
//!   accounts file, and never sent anywhere except Minecraft's own API.
//!
//! The parser is deliberately schema-agnostic. Launchers restructure their
//! account files between releases, so rather than encoding one layout we walk
//! the JSON for anything token-shaped and ask Minecraft which one is real.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};

const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

/// A launcher whose account store we know how to look for.
struct Source {
    label: &'static str,
    /// Paths relative to the user's home directory.
    candidates: &'static [&'static str],
}

const SOURCES: &[Source] = &[
    Source {
        label: "Prism Launcher",
        candidates: &[
            ".local/share/PrismLauncher/accounts.json",
            ".var/app/org.prismlauncher.PrismLauncher/data/PrismLauncher/accounts.json",
            "Library/Application Support/PrismLauncher/accounts.json",
            "AppData/Roaming/PrismLauncher/accounts.json",
        ],
    },
    Source {
        label: "MultiMC",
        candidates: &[
            ".local/share/multimc/accounts.json",
            ".var/app/org.multimc.MultiMC/data/multimc/accounts.json",
            "AppData/Roaming/MultiMC/accounts.json",
        ],
    },
    Source {
        label: "ATLauncher",
        candidates: &[
            ".local/share/ATLauncher/accounts.json",
            "AppData/Roaming/ATLauncher/accounts.json",
        ],
    },
    Source {
        label: "Minecraft Launcher",
        candidates: &[
            ".minecraft/launcher_accounts.json",
            "Library/Application Support/minecraft/launcher_accounts.json",
            "AppData/Roaming/.minecraft/launcher_accounts.json",
        ],
    },
];

/// An account we found and confirmed is live.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub username: String,
    /// Dashed profile UUID, as reported by Minecraft rather than by the file.
    pub uuid: String,
    pub source_label: String,
    pub source_path: String,
    /// Kept out of `Serialize` so a token never crosses the IPC boundary.
    #[serde(skip)]
    pub access_token: String,
}

fn account_files() -> Vec<(&'static str, PathBuf)> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for source in SOURCES {
        for candidate in source.candidates {
            let path = home.join(candidate);
            if path.is_file() {
                found.push((source.label, path));
            }
        }
    }
    found
}

/// Collect every string in the document that could be a Minecraft access token.
///
/// Modern tokens are JWTs: three base64url segments separated by dots, and
/// several hundred characters long. Matching on shape rather than on a key name
/// keeps this working across launcher format changes.
fn collect_token_candidates(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if looks_like_jwt(s) && !out.contains(s) {
                out.push(s.clone());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_token_candidates(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_token_candidates(item, out);
            }
        }
        _ => {}
    }
}

fn looks_like_jwt(s: &str) -> bool {
    if s.len() < 100 || s.len() > 8192 {
        return false;
    }
    let segments: Vec<&str> = s.split('.').collect();
    if segments.len() != 3 {
        return false;
    }
    segments.iter().all(|seg| {
        !seg.is_empty()
            && seg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
    })
}

#[derive(Debug, serde::Deserialize)]
struct ProfileResponse {
    id: String,
    name: String,
}

/// Ask Minecraft whether a token is valid, and who it belongs to.
///
/// This is the only place a candidate token is sent anywhere, and it goes to
/// Mojang's own API. A rejected token is simply not a Minecraft token — an
/// account file holds Xbox Live and MSA tokens too, which look identical.
async fn identify(client: &reqwest::Client, token: &str) -> Option<(String, String)> {
    let resp = client
        .get(MC_PROFILE_URL)
        .bearer_auth(token)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let profile: ProfileResponse = resp.json().await.ok()?;
    Some((profile.name, super::store::dash_uuid(&profile.id)))
}

/// The outcome of scanning one launcher's account file.
struct FileScan {
    found: Vec<ImportCandidate>,
    /// How many token-shaped strings we tried. Distinguishes "this launcher has
    /// no account" from "it has one but the session lapsed", which need very
    /// different advice.
    tokens_tried: usize,
}

/// Find usable sessions in one launcher's account file.
async fn scan_file(client: &reqwest::Client, label: &'static str, path: &Path) -> FileScan {
    let empty = FileScan {
        found: Vec::new(),
        tokens_tried: 0,
    };

    let Ok(bytes) = tokio::fs::read(path).await else {
        return empty;
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return empty;
    };

    let mut candidates = Vec::new();
    collect_token_candidates(&json, &mut candidates);
    let tokens_tried = candidates.len();

    let mut found: Vec<ImportCandidate> = Vec::new();
    for token in candidates {
        // An account file holds Xbox Live and MSA tokens too, which look
        // identical; only the Minecraft one answers here.
        if let Some((username, uuid)) = identify(client, &token).await {
            if found.iter().any(|c| c.uuid == uuid) {
                continue;
            }
            found.push(ImportCandidate {
                username,
                uuid,
                source_label: label.to_string(),
                source_path: path.to_string_lossy().to_string(),
                access_token: token,
            });
        }
    }
    FileScan {
        found,
        tokens_tried,
    }
}

/// What a scan turned up, including why nothing was usable.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportScan {
    pub candidates: Vec<ImportCandidate>,
    /// One line per launcher we found but could not import from.
    pub notes: Vec<String>,
}

/// Every live session Geode can find on this machine.
pub async fn discover(client: &reqwest::Client) -> ImportScan {
    let mut scan = ImportScan::default();

    for (label, path) in account_files() {
        let result = scan_file(client, label, &path).await;

        if result.found.is_empty() {
            scan.notes.push(if result.tokens_tried == 0 {
                format!("{label} is installed but has no account signed in.")
            } else {
                format!(
                    "{label} has a saved account, but its session has expired. \
                     Open {label} — it refreshes on start — then scan again."
                )
            });
            continue;
        }

        for candidate in result.found {
            if !scan
                .candidates
                .iter()
                .any(|c: &ImportCandidate| c.uuid == candidate.uuid)
            {
                scan.candidates.push(candidate);
            }
        }
    }

    scan
}

/// Just the usable sessions, for callers that do not surface the notes.
pub async fn discover_candidates(client: &reqwest::Client) -> Vec<ImportCandidate> {
    discover(client).await.candidates
}

/// Re-read a source file looking for a still-valid token for one account.
///
/// Called when an imported session has expired: if the user has since opened
/// the other launcher, it will have refreshed the token in place and we can
/// pick the new one up without asking them to do anything.
pub async fn refresh_from_source(
    client: &reqwest::Client,
    source_path: &str,
    uuid: &str,
) -> Result<String> {
    let path = PathBuf::from(source_path);
    if !path.is_file() {
        return Err(Error::msg(format!(
            "the launcher this account came from is gone ({source_path})"
        )));
    }

    for candidate in scan_file(client, "", &path).await.found {
        if candidate.uuid == uuid {
            return Ok(candidate.access_token);
        }
    }

    Err(Error::msg(
        "that session has expired. Open the launcher you imported it from — \
         it refreshes on start — then launch again.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jwt() -> String {
        format!(
            "{}.{}.{}",
            "a".repeat(40),
            "b".repeat(40),
            "c".repeat(40)
        )
    }

    #[test]
    fn recognises_jwt_shape() {
        assert!(looks_like_jwt(&fake_jwt()));
    }

    #[test]
    fn rejects_short_and_malformed_strings() {
        assert!(!looks_like_jwt("hello"));
        assert!(!looks_like_jwt(&"a".repeat(300))); // no dots
        assert!(!looks_like_jwt(&format!("{}.{}", "a".repeat(80), "b".repeat(80)))); // two segments
        assert!(!looks_like_jwt(&format!(
            "{}.{}.{}",
            "a".repeat(40),
            "b b b".repeat(20),
            "c".repeat(40)
        ))); // spaces are not base64url
    }

    #[test]
    fn finds_tokens_at_any_depth() {
        let token = fake_jwt();
        let doc = serde_json::json!({
            "accounts": [
                { "type": "MSA", "ygg": { "token": token, "exp": 123 },
                  "profile": { "name": "Steve" } }
            ],
            "formatVersion": 3
        });
        let mut out = Vec::new();
        collect_token_candidates(&doc, &mut out);
        assert_eq!(out, vec![token]);
    }

    #[test]
    fn deduplicates_repeated_tokens() {
        let token = fake_jwt();
        let doc = serde_json::json!({ "a": token, "b": token });
        let mut out = Vec::new();
        collect_token_candidates(&doc, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn ignores_documents_with_no_tokens() {
        let doc = serde_json::json!({ "accounts": [], "formatVersion": 3 });
        let mut out = Vec::new();
        collect_token_candidates(&doc, &mut out);
        assert!(out.is_empty());
    }
}

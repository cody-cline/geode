//! Account persistence.
//!
//! Tokens live in `accounts.json` with owner-only permissions. This is the same
//! posture as the official launcher and Prism: a plain file readable by the
//! user's own processes. It is not protection against local malware — anything
//! running as the user could equally read a keyring-unlocked secret.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Microsoft,
    Offline,
    /// A live session borrowed from another launcher on this machine. Online
    /// play works, but the token cannot be renewed here — see [`super::import`].
    Imported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// The Minecraft profile UUID (dashed).
    pub id: String,
    pub username: String,
    pub kind: AccountKind,
    #[serde(default)]
    pub mc_access_token: Option<String>,
    #[serde(default)]
    pub mc_expires_at: i64,
    #[serde(default)]
    pub msa_refresh_token: Option<String>,
    #[serde(default)]
    pub added_at: i64,
    /// For imported accounts: the file to re-read when the session expires.
    #[serde(default)]
    pub source_path: Option<String>,
    /// Human-readable name of the launcher it came from.
    #[serde(default)]
    pub source_label: Option<String>,
}

impl Account {
    pub fn offline(username: String) -> Self {
        Self {
            id: super::offline_uuid(&username),
            username,
            kind: AccountKind::Offline,
            mc_access_token: None,
            mc_expires_at: 0,
            msa_refresh_token: None,
            added_at: chrono::Utc::now().timestamp(),
            source_path: None,
            source_label: None,
        }
    }

    /// Wrap a session lifted from another launcher.
    ///
    /// Minecraft tokens run about 24 hours and the file gives no expiry we can
    /// trust, so assume a conservative 12 and let the re-read path handle it
    /// being wrong in either direction.
    pub fn imported(candidate: &super::import::ImportCandidate) -> Self {
        Self {
            id: candidate.uuid.clone(),
            username: candidate.username.clone(),
            kind: AccountKind::Imported,
            mc_access_token: Some(candidate.access_token.clone()),
            mc_expires_at: chrono::Utc::now().timestamp() + 12 * 3600,
            msa_refresh_token: None,
            added_at: chrono::Utc::now().timestamp(),
            source_path: Some(candidate.source_path.clone()),
            source_label: Some(candidate.source_label.clone()),
        }
    }

    /// Refresh a little early so a token cannot expire between the check and
    /// the game actually contacting the session server.
    pub fn token_expired(&self) -> bool {
        self.mc_expires_at - 300 <= chrono::Utc::now().timestamp()
    }
}

/// What the frontend sees. Never includes tokens.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: String,
    pub username: String,
    pub kind: AccountKind,
    pub active: bool,
    pub needs_reauth: bool,
    /// Which launcher an imported session came from.
    pub source_label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsFile {
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub active: Option<String>,
}

impl AccountsFile {
    pub async fn load(paths: &Paths) -> Result<Self> {
        let path = paths.accounts_file();
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| Error::json("accounts.json", e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn save(&self, paths: &Paths) -> Result<()> {
        let path = paths.accounts_file();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        write_private(&path, &serde_json::to_vec_pretty(self)?).await
    }

    pub fn views(&self) -> Vec<AccountView> {
        self.accounts
            .iter()
            .map(|a| AccountView {
                id: a.id.clone(),
                username: a.username.clone(),
                kind: a.kind,
                active: self.active.as_deref() == Some(a.id.as_str()),
                needs_reauth: a.kind == AccountKind::Microsoft && a.msa_refresh_token.is_none(),
                source_label: a.source_label.clone(),
            })
            .collect()
    }

    pub fn active_account(&self) -> Option<&Account> {
        let id = self.active.as_deref()?;
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Add or replace by profile id, and select it.
    pub fn upsert(&mut self, account: Account) {
        self.active = Some(account.id.clone());
        match self.accounts.iter_mut().find(|a| a.id == account.id) {
            Some(existing) => *existing = account,
            None => self.accounts.push(account),
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.accounts.retain(|a| a.id != id);
        if self.active.as_deref() == Some(id) {
            self.active = self.accounts.first().map(|a| a.id.clone());
        }
    }
}

/// Write a file only the owning user can read.
async fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    tokio::fs::write(path, bytes).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(path).await?.permissions();
        perms.set_mode(0o600);
        tokio::fs::set_permissions(path, perms).await?;
    }
    Ok(())
}

/// A ready-to-launch session.
#[derive(Debug, Clone)]
pub struct Session {
    pub username: String,
    pub uuid: String,
    pub access_token: String,
    pub user_type: &'static str,
}

/// Resolve the active account into a launchable session, silently refreshing an
/// expired Microsoft token when we still hold a refresh token.
pub async fn session_for(
    client: &reqwest::Client,
    paths: &Paths,
    client_id: Option<&str>,
    account_id: Option<&str>,
) -> Result<Session> {
    let mut file = AccountsFile::load(paths).await?;

    let id = account_id
        .map(str::to_string)
        .or_else(|| file.active.clone())
        .ok_or(Error::NoAccount)?;

    let account = file
        .accounts
        .iter()
        .find(|a| a.id == id)
        .cloned()
        .ok_or(Error::NoAccount)?;

    if account.kind == AccountKind::Offline {
        return Ok(Session {
            username: account.username,
            uuid: account.id,
            // Vanilla accepts any placeholder here; the session server is never
            // contacted because there is nothing to validate.
            access_token: "0".into(),
            user_type: "legacy",
        });
    }

    if !account.token_expired() {
        if let Some(token) = account.mc_access_token.clone() {
            return Ok(Session {
                username: account.username,
                uuid: account.id,
                access_token: token,
                user_type: "msa",
            });
        }
    }

    // An imported session has no refresh token by design. Instead, re-read the
    // launcher it came from: if that launcher has been opened since, it will
    // have renewed the token in place and the user needs to do nothing.
    if account.kind == AccountKind::Imported {
        let source = account
            .source_path
            .clone()
            .ok_or_else(|| Error::msg("this imported account has no source launcher recorded"))?;

        let token = super::import::refresh_from_source(client, &source, &account.id).await?;

        let mut updated = account.clone();
        updated.mc_access_token = Some(token.clone());
        updated.mc_expires_at = chrono::Utc::now().timestamp() + 12 * 3600;
        let (uuid, username) = (updated.id.clone(), updated.username.clone());
        file.upsert(updated);
        file.save(paths).await?;

        return Ok(Session {
            username,
            uuid,
            access_token: token,
            user_type: "msa",
        });
    }

    let refresh = account
        .msa_refresh_token
        .clone()
        .ok_or(Error::AuthExpired)?;
    let client_id = client_id
        .ok_or_else(|| Error::msg("no Microsoft client ID is configured — set one in Settings"))?;

    let msa = super::refresh_msa(client, client_id, &refresh).await?;
    let (token, expires_at, profile) = super::minecraft_session(client, &msa.access_token).await?;

    let mut updated = account.clone();
    updated.username = profile.name.clone();
    updated.mc_access_token = Some(token.clone());
    updated.mc_expires_at = expires_at;
    if msa.refresh_token.is_some() {
        updated.msa_refresh_token = msa.refresh_token.clone();
    }
    let uuid = updated.id.clone();
    let username = updated.username.clone();
    file.upsert(updated);
    file.save(paths).await?;

    Ok(Session {
        username,
        uuid,
        access_token: token,
        user_type: "msa",
    })
}

/// Minecraft profile ids come back undashed; the game wants them dashed.
pub fn dash_uuid(raw: &str) -> String {
    let clean: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if clean.len() != 32 {
        return raw.to_string();
    }
    format!(
        "{}-{}-{}-{}-{}",
        &clean[0..8],
        &clean[8..12],
        &clean[12..16],
        &clean[16..20],
        &clean[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashes_undashed_uuid() {
        assert_eq!(
            dash_uuid("069a79f444e94726a5befca90e38aaf5"),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
    }

    #[test]
    fn leaves_dashed_uuid_alone() {
        let already = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
        assert_eq!(dash_uuid(already), already);
    }

    #[test]
    fn removing_active_selects_another() {
        let mut file = AccountsFile::default();
        file.upsert(Account::offline("One".into()));
        file.upsert(Account::offline("Two".into()));
        let two = file.active.clone().unwrap();
        file.remove(&two);
        assert_eq!(file.accounts.len(), 1);
        assert!(file.active.is_some());
    }
}

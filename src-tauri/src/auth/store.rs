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
    /// A session borrowed from another launcher, which Geode no longer does.
    ///
    /// The variant stays only so an `accounts.json` written before that was
    /// removed still parses: drop it and serde fails the whole file, which
    /// would lose every account on the install rather than the one that is
    /// obsolete. [`AccountsFile::load`] discards these as it reads them, and
    /// nothing ever writes one.
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
    /// Whether Microsoft has refused to renew this sign-in.
    ///
    /// Set only when Microsoft answered and the answer was no — the
    /// `invalid_grant` of [`crate::auth::refresh_msa`] — and never when a
    /// renewal simply could not reach it. Without this, an account whose
    /// token was revoked server-side still holds the string on disk and the
    /// Accounts screen calls it healthy while every launch fails.
    ///
    /// The refresh token itself is deliberately left alone: a stored flag
    /// cleared by the next successful renewal costs nothing, where throwing
    /// the token away is unrecoverable, and a renewal is the only thing that
    /// can tell the difference.
    #[serde(default)]
    pub msa_rejected: bool,
    #[serde(default)]
    pub added_at: i64,
    /// Left on the struct so an old file still parses; never written.
    #[serde(default, skip_serializing)]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing)]
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
            msa_rejected: false,
            added_at: chrono::Utc::now().timestamp(),
            source_path: None,
            source_label: None,
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
                let mut file: Self =
                    serde_json::from_slice(&bytes).map_err(|e| Error::json("accounts.json", e))?;
                // Borrowed sessions are gone. They could never be renewed
                // here, so one in an old file is at best hours from useless;
                // dropping it quietly beats offering an account that cannot
                // play. `active` is repaired rather than left pointing at
                // something no longer in the list.
                file.accounts.retain(|a| a.kind != AccountKind::Imported);
                if !file
                    .active
                    .as_deref()
                    .is_some_and(|id| file.accounts.iter().any(|a| a.id == id))
                {
                    file.active = file.accounts.first().map(|a| a.id.clone());
                }
                Ok(file)
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
                // Two ways a Microsoft account needs signing in again, and the
                // second is the common one: a token that was never there, and
                // one Microsoft has since refused. Reading only the first
                // called a revoked account healthy, badge and all, while every
                // launch it made failed.
                needs_reauth: a.kind == AccountKind::Microsoft
                    && (a.msa_refresh_token.is_none() || a.msa_rejected),
            })
            .collect()
    }

    /// Whether this install has an account that has proved it owns Minecraft:
    /// Java Edition.
    ///
    /// One kind qualifies, and it was checked against Minecraft itself rather
    /// than taken on trust: a Microsoft sign-in is refused with
    /// [`crate::error::Error::NoEntitlement`] when the store holds no Java
    /// licence and no Java profile. Borrowed sessions used to count too; with
    /// those gone this is simply "somebody signed in here".
    ///
    /// This is what an offline account is allowed behind. An offline account
    /// exists to play a copy you already own — single-player, or a LAN or
    /// private server with authentication off — and not to play without
    /// owning one. Mojang's rule for any launcher that talks to their APIs is
    /// that ownership is verified before gameplay "regardless of server
    /// configuration", so the check cannot live in the server's settings; it
    /// has to be here. Asked once per install, which is the shape Prism
    /// Launcher settled on for the same reason.
    pub fn owns_the_game(&self) -> bool {
        self.accounts
            .iter()
            .any(|a| a.kind == AccountKind::Microsoft)
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
pub(crate) async fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
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

/// The account a launch is for: the one it named, else the active one.
fn resolve(file: &AccountsFile, account_id: Option<&str>) -> Result<Account> {
    let id = account_id
        .map(str::to_string)
        .or_else(|| file.active.clone())
        .ok_or(Error::NoAccount)?;
    file.accounts
        .iter()
        .find(|a| a.id == id)
        .cloned()
        .ok_or(Error::NoAccount)
}

/// A session that plays as this account without a sign-in behind it.
///
/// Minecraft identifies a player by UUID and by nothing else — a world's
/// character lives at `playerdata/<uuid>.dat` — so this substitutes the token
/// and leaves the UUID exactly as it is. For a Microsoft account
/// [`Account::id`] *is* the real dashed Mojang UUID, which is what makes this
/// the player walking back into their own world as themselves.
///
/// The alternative, and the bug: adding an offline account instead hands them
/// `super::offline_uuid(name)`, derived from the name rather than taken from
/// the profile. That is a different player. Their own character is still in
/// the same world, keyed by the real UUID, intact and unreachable. Prism
/// Launcher substitutes name and token and leaves the UUID alone; so does
/// this.
pub fn offline_session(account: &Account) -> Session {
    Session {
        username: account.username.clone(),
        uuid: account.id.clone(),
        // Vanilla accepts any placeholder here; the session server is never
        // contacted because there is nothing to validate.
        access_token: "0".into(),
        // Matching the offline accounts this sits beside. There is no valid
        // token, so there is nothing for "msa" to claim.
        user_type: "legacy",
    }
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
    let account = resolve(&file, account_id)?;

    if account.kind == AccountKind::Offline {
        return Ok(offline_session(&account));
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

    let refresh = account
        .msa_refresh_token
        .clone()
        .ok_or(Error::AuthExpired)?;
    let client_id = client_id
        .ok_or_else(|| Error::msg("This account cannot be refreshed in this copy of Geode."))?;

    let msa = match super::refresh_msa(client, client_id, &refresh).await {
        Ok(msa) => msa,
        Err(e) => return Err(renewal_failed(&mut file, paths, &account.id, e).await),
    };
    let (token, expires_at, profile) = super::minecraft_session(client, &msa.access_token).await?;

    let mut updated = account.clone();
    updated.username = profile.name.clone();
    updated.mc_access_token = Some(token.clone());
    updated.mc_expires_at = expires_at;
    if msa.refresh_token.is_some() {
        updated.msa_refresh_token = msa.refresh_token.clone();
    }
    // A renewal that worked settles any earlier refusal, so the badge goes
    // away on its own rather than waiting for somebody to sign in again.
    updated.msa_rejected = false;
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

/// Apply a failed renewal to the stored account, and hand back the error the
/// player reads.
///
/// This is the one place that decides what a failure to renew *means*.
/// [`Error::AuthExpired`] is Microsoft's own refusal and the only thing worth
/// remembering; everything else is Geode not having reached Microsoft and must
/// leave the account exactly as it was, token and all. Marking a good account
/// because somebody's wifi blipped would sign them out until they noticed,
/// which is a worse bug than the one that brought us here.
async fn renewal_failed(file: &mut AccountsFile, paths: &Paths, id: &str, e: Error) -> Error {
    if matches!(e, Error::AuthExpired) {
        // `iter_mut` rather than `upsert`, which would also make this the
        // active account as a side effect of recording bad news about it.
        if let Some(account) = file.accounts.iter_mut().find(|a| a.id == id) {
            account.msa_rejected = true;
        }
        // If this cannot be written the player still has to read about the
        // sign-in, which is the problem they can do something about.
        let _ = file.save(paths).await;
    }
    e
}

/// A launchable session for an account whose Microsoft sign-in could not be
/// renewed, keeping that account's own name and UUID.
///
/// Only ever reached because somebody asked for it: see
/// [`crate::commands::launch_instance`]. Falling back here on a failure would
/// tell a player they are signed in when they are not, and let them believe
/// they can join servers they cannot.
pub async fn offline_session_for(paths: &Paths, account_id: Option<&str>) -> Result<Session> {
    let file = AccountsFile::load(paths).await?;
    Ok(offline_session(&resolve(&file, account_id)?))
}

/// Whether playing offline is worth offering for this account, and the name it
/// would play under.
///
/// `Some` only for a Microsoft account that cannot produce an online session
/// without the network: no Minecraft token, or an expired one. That is a fact
/// about the account and not a verdict on anything that just happened, which
/// is why the only caller is [`session_for`]'s own failure path in
/// `launch_instance`: asked there, it answers "could this account have played
/// offline instead" about the step that actually failed. Asked after any
/// failure it answered for launches that never reached a sign-in at all.
///
/// An offline account answers `None` too: it already is one.
///
/// No [`AccountsFile::owns_the_game`] check, and none is missing: the account
/// being offered is itself the Microsoft sign-in that proof is made of. This
/// opens nothing that was shut — it lets somebody who signed in here play the
/// copy they signed in with, as themselves, while the network is down.
pub async fn offline_offer(paths: &Paths, account_id: Option<&str>) -> Result<Option<String>> {
    let file = AccountsFile::load(paths).await?;
    let Ok(account) = resolve(&file, account_id) else {
        return Ok(None);
    };
    let stuck = account.kind == AccountKind::Microsoft
        && (account.mc_access_token.is_none() || account.token_expired());
    Ok(stuck.then(|| account.username.clone()))
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

    fn with(kinds: &[AccountKind]) -> AccountsFile {
        let mut file = AccountsFile::default();
        for (n, kind) in kinds.iter().enumerate() {
            let mut account = Account::offline(format!("Player{n}"));
            account.kind = *kind;
            file.upsert(account);
        }
        file
    }

    /// An offline account plays a copy you already own, so something on this
    /// install has to have proved that once. Nothing has, on a fresh one.
    #[test]
    fn a_fresh_install_owns_nothing() {
        assert!(!AccountsFile::default().owns_the_game());
    }

    /// The case the rule is actually about: offline accounts cannot vouch for
    /// each other, or the check would be one click away from meaning nothing.
    #[test]
    fn offline_accounts_do_not_prove_ownership_between_themselves() {
        assert!(!with(&[AccountKind::Offline, AccountKind::Offline]).owns_the_game());
    }

    /// Microsoft sign-in is refused without a Java entitlement, so reaching
    /// the accounts file at all is the proof.
    #[test]
    fn a_microsoft_account_proves_it() {
        assert!(with(&[AccountKind::Microsoft]).owns_the_game());
    }

    /// A borrowed session does not, and cannot: Geode no longer takes one, so
    /// the only way an account of this kind exists is an `accounts.json`
    /// written before that changed — and `load` drops those on the way in.
    #[test]
    fn a_borrowed_session_does_not_prove_it() {
        assert!(!with(&[AccountKind::Imported]).owns_the_game());
    }

    /// The migration, and the reason `AccountKind::Imported` is still a
    /// variant at all. An install that had one before account import was
    /// removed must still be able to read its own file: dropping the variant
    /// outright makes serde reject the whole document, which would lose every
    /// account on that machine rather than the one that is obsolete.
    #[tokio::test]
    async fn a_file_holding_a_borrowed_session_still_loads_and_drops_it() {
        let scratch = crate::paths::Scratch::new("accounts-migrate");
        let written = serde_json::json!({
            "accounts": [
                { "id": "11111111-1111-1111-1111-111111111111", "username": "Borrowed",
                  "kind": "imported", "mcAccessToken": "t", "mcExpiresAt": 0 },
                { "id": "22222222-2222-2222-2222-222222222222", "username": "Real",
                  "kind": "microsoft", "msaRefreshToken": "r", "mcExpiresAt": 0 }
            ],
            "active": "11111111-1111-1111-1111-111111111111"
        });
        tokio::fs::create_dir_all(&scratch.paths.root).await.unwrap();
        tokio::fs::write(
            scratch.paths.accounts_file(),
            serde_json::to_vec(&written).unwrap(),
        )
        .await
        .unwrap();

        let file = AccountsFile::load(&scratch.paths).await.expect("the file still reads");
        assert_eq!(file.accounts.len(), 1, "the borrowed session should be gone");
        assert_eq!(file.accounts[0].username, "Real");
        assert_eq!(
            file.active.as_deref(),
            Some("22222222-2222-2222-2222-222222222222"),
            "active pointed at the dropped account and should have moved"
        );
    }

    /// Removing the owner closes the gate again, rather than leaving an
    /// install permanently unlocked by an account that is no longer there.
    #[test]
    fn taking_the_owning_account_away_closes_it_again() {
        let mut file = with(&[AccountKind::Microsoft, AccountKind::Offline]);
        let owner = file
            .accounts
            .iter()
            .find(|a| a.kind == AccountKind::Microsoft)
            .map(|a| a.id.clone())
            .expect("the owning account is there");
        assert!(file.owns_the_game());
        file.remove(&owner);
        assert!(!file.owns_the_game());
    }

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

    /// A Microsoft account as it sits on disk: `id` is the real dashed Mojang
    /// UUID, which is the whole reason the fix below is possible.
    fn signed_in(username: &str, uuid: &str) -> Account {
        Account {
            id: uuid.to_string(),
            username: username.to_string(),
            kind: AccountKind::Microsoft,
            mc_access_token: None,
            mc_expires_at: 0,
            msa_refresh_token: Some("refresh".into()),
            msa_rejected: false,
            added_at: 0,
            source_path: None,
            source_label: None,
        }
    }

    /// **The point of all of this.** Minecraft knows a player by UUID and by
    /// nothing else: a world's character lives at `playerdata/<uuid>.dat`. An
    /// offline session for a Microsoft account whose sign-in could not be
    /// renewed must therefore come back as *that player* — real name, real
    /// UUID — or the player opens their own world as a stranger with an empty
    /// inventory while their character sits in the same world, unreachable.
    #[test]
    fn an_offline_session_keeps_the_accounts_own_uuid() {
        let real = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
        let session = offline_session(&signed_in("Notch", real));

        assert_eq!(session.uuid, real, "the session must carry the real UUID");
        assert_eq!(session.username, "Notch");
        assert_ne!(
            session.uuid,
            super::super::offline_uuid("Notch"),
            "this is the bug: a name-derived UUID is a different player in the same world"
        );
        assert_eq!(session.user_type, "legacy");
        assert!(
            !session.access_token.is_empty(),
            "vanilla wants a placeholder, not an empty string"
        );
    }

    /// An offline account is unchanged by any of this: its id was always
    /// name-derived, and that is correct for it.
    #[test]
    fn an_offline_account_still_plays_as_its_derived_uuid() {
        let account = Account::offline("Steve".into());
        let session = offline_session(&account);
        assert_eq!(session.uuid, super::super::offline_uuid("Steve"));
    }

    /// Microsoft answered, and the answer was no. That is worth remembering:
    /// it is the only thing that can make `needs_reauth` true for an account
    /// that still holds a refresh token, and without it the Accounts screen
    /// shows a revoked account as healthy while every launch fails.
    #[tokio::test]
    async fn microsofts_own_refusal_is_remembered_and_shows_on_the_account() {
        let scratch = crate::paths::Scratch::new("accounts-refused");
        let mut file = AccountsFile::default();
        file.upsert(signed_in("Notch", "069a79f4-44e9-4726-a5be-fca90e38aaf5"));
        tokio::fs::create_dir_all(&scratch.paths.root).await.unwrap();

        let id = file.accounts[0].id.clone();
        assert!(!file.views()[0].needs_reauth, "healthy before the refusal");

        let back = renewal_failed(&mut file, &scratch.paths, &id, Error::AuthExpired).await;
        assert!(matches!(back, Error::AuthExpired), "the error is passed on");

        let reread = AccountsFile::load(&scratch.paths).await.unwrap();
        assert!(
            reread.views()[0].needs_reauth,
            "the Accounts screen has to say this account needs signing in again"
        );
        assert_eq!(
            reread.accounts[0].msa_refresh_token.as_deref(),
            Some("refresh"),
            "the token stays on disk — a flag is reversible, throwing it away is not"
        );
    }

    /// The bug not to introduce while fixing the other one. Geode could not
    /// reach Microsoft; Microsoft did not say anything. Nothing about the
    /// account is known, so nothing about it may be written — marking it here
    /// would sign somebody out over a wifi blip.
    #[tokio::test]
    async fn a_renewal_that_never_reached_microsoft_changes_nothing() {
        let scratch = crate::paths::Scratch::new("accounts-unreachable");
        let mut file = AccountsFile::default();
        file.upsert(signed_in("Notch", "069a79f4-44e9-4726-a5be-fca90e38aaf5"));
        tokio::fs::create_dir_all(&scratch.paths.root).await.unwrap();
        file.save(&scratch.paths).await.unwrap();

        let id = file.accounts[0].id.clone();
        let transport = Error::msg(
            "could not reach Microsoft to renew this sign-in: error sending request for url",
        );
        renewal_failed(&mut file, &scratch.paths, &id, transport).await;

        assert!(!file.accounts[0].msa_rejected, "nothing in memory either");
        let reread = AccountsFile::load(&scratch.paths).await.unwrap();
        assert!(
            !reread.views()[0].needs_reauth,
            "an unreachable Microsoft is not a verdict on the account"
        );
        assert!(reread.accounts[0].msa_refresh_token.is_some());
    }

    /// Who could be offered a choice, and who could not. Read off the
    /// account's own state rather than the text of a launch error — and asked
    /// only where a sign-in has just failed, which is the caller's half of it.
    #[tokio::test]
    async fn playing_offline_is_offered_only_where_it_would_help() {
        let scratch = crate::paths::Scratch::new("accounts-offer");
        tokio::fs::create_dir_all(&scratch.paths.root).await.unwrap();

        let mut file = AccountsFile::default();
        let stale = signed_in("Notch", "069a79f4-44e9-4726-a5be-fca90e38aaf5");
        file.upsert(stale.clone());
        file.save(&scratch.paths).await.unwrap();
        assert_eq!(
            offline_offer(&scratch.paths, None).await.unwrap().as_deref(),
            Some("Notch"),
            "an expired Microsoft token is exactly the state this is for"
        );

        // A launch that got past the sign-in left a token good for a day, so
        // even if this were asked after a later failure there would be nothing
        // to offer.
        let mut fresh = stale.clone();
        fresh.mc_access_token = Some("token".into());
        fresh.mc_expires_at = chrono::Utc::now().timestamp() + 86_400;
        file.upsert(fresh);
        file.save(&scratch.paths).await.unwrap();
        assert_eq!(offline_offer(&scratch.paths, None).await.unwrap(), None);

        // An offline account already is one.
        let mut offline = AccountsFile::default();
        offline.upsert(Account::offline("Steve".into()));
        offline.save(&scratch.paths).await.unwrap();
        assert_eq!(offline_offer(&scratch.paths, None).await.unwrap(), None);

        // And an install with no accounts has nobody to offer.
        AccountsFile::default().save(&scratch.paths).await.unwrap();
        assert_eq!(offline_offer(&scratch.paths, None).await.unwrap(), None);
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

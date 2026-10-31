//! Microsoft account authentication.
//!
//! Signing in to Minecraft is a four-hop chain:
//!
//! 1. MSA OAuth (device code) -> Microsoft access + refresh token
//! 2. Xbox Live user authenticate -> XBL token + user hash
//! 3. XSTS authorize -> XSTS token scoped to Minecraft services
//! 4. `login_with_xbox` -> the Minecraft token the game actually wants
//!
//! We use the device code grant rather than an embedded webview: no browser
//! engine to babysit, no redirect URI to register, and the user signs in inside
//! their own browser where their password manager already works.

pub mod store;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Error, Result};

const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";
const MC_ENTITLEMENTS_URL: &str = "https://api.minecraftservices.com/entitlements/mcstore";
const SCOPE: &str = "XboxLive.signin offline_access";

/// The Azure application id Geode signs in with.
///
/// Committed, like [`crate::config::default_curseforge_key`] and for the same
/// reasons. A client id is not a secret — it travels in cleartext in the
/// authorization URL every sign-in opens — and there is no box to paste one
/// into anywhere in the app, so withholding it would not protect anything. It
/// would only mean nobody could sign in to any build that is still this
/// software, including a distribution's.
///
/// This one is on Mojang's allow list, approved 30 October 2026 for Geode.
/// That approval is keyed to the id rather than to the name, and it is the
/// thing of value here: an id that is not on the list walks the whole OAuth
/// chain correctly and is refused at `login_with_xbox` with a 403.
///
/// **Fork or rebrand this and you must register your own and have it approved
/// in its own right.** Signing in with this one from something else is
/// impersonating this application, and Mojang answers abuse by taking an id
/// off the list — which signs out everybody using the real thing, at once,
/// rather than whoever misused it. Setting it to `""` compiles and turns
/// Microsoft sign-in off, which is a supported way to build.
///
/// By building with this id you accept the Microsoft Identity Platform terms
/// of use: <https://docs.microsoft.com/en-us/legal/microsoft-identity-platform/terms-of-use>
const DEFAULT_CLIENT_ID: &str = "7f70c2d8-5361-4c2e-817b-3ca971ab5a9b";

/// The id to sign in with: `GEODE_CLIENT_ID` at build time if it was given,
/// else [`DEFAULT_CLIENT_ID`]. Empty either way means no Microsoft sign-in.
pub fn default_client_id() -> Option<String> {
    let id = option_env!("GEODE_CLIENT_ID")
        .unwrap_or(DEFAULT_CLIENT_ID)
        .trim();
    (!id.is_empty()).then(|| id.to_string())
}

// ---------------------------------------------------------------------------
// Device code grant
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCodeStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default)]
    message: String,
}

fn default_interval() -> u64 {
    5
}

pub async fn begin_device_code(
    client: &reqwest::Client,
    client_id: &str,
) -> Result<DeviceCodeStart> {
    let resp = client
        .post(DEVICE_CODE_URL)
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .send()
        .await?;

    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(Error::msg(format!(
            "Microsoft rejected the sign-in request ({status}). {}",
            describe_oauth_error(&text)
        )));
    }

    let r: DeviceCodeResponse =
        serde_json::from_str(&text).map_err(|e| Error::json("device code response", e))?;
    Ok(DeviceCodeStart {
        device_code: r.device_code,
        user_code: r.user_code,
        verification_uri: r.verification_uri,
        expires_in: r.expires_in,
        interval: r.interval.max(1),
        message: r.message,
    })
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct MsaTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64,
}

/// Poll the token endpoint until the user finishes signing in, or the code
/// expires. Honours `authorization_pending` and `slow_down` per RFC 8628.
pub async fn poll_device_code(
    client: &reqwest::Client,
    client_id: &str,
    start: &DeviceCodeStart,
) -> Result<MsaTokens> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in.max(60));
    let mut interval = std::time::Duration::from_secs(start.interval);

    loop {
        if std::time::Instant::now() >= deadline {
            return Err(Error::msg(
                "the sign-in code expired before it was used — start over",
            ));
        }
        tokio::time::sleep(interval).await;

        let resp = client
            .post(TOKEN_URL)
            .form(&[
                ("client_id", client_id),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &start.device_code),
            ])
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;

        if status.is_success() {
            let t: TokenResponse =
                serde_json::from_str(&text).map_err(|e| Error::json("token response", e))?;
            return Ok(to_msa_tokens(t));
        }

        match oauth_error_code(&text).as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval += std::time::Duration::from_secs(5);
                continue;
            }
            Some("authorization_declined") => {
                return Err(Error::msg("sign-in was declined"));
            }
            Some("expired_token") => {
                return Err(Error::msg("the sign-in code expired — start over"));
            }
            _ => {
                return Err(Error::msg(format!(
                    "sign-in failed ({status}). {}",
                    describe_oauth_error(&text)
                )))
            }
        }
    }
}

pub async fn refresh_msa(
    client: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<MsaTokens> {
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", SCOPE),
        ])
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(Error::AuthExpired);
    }
    let t: TokenResponse = resp.json().await?;
    Ok(to_msa_tokens(t))
}

fn to_msa_tokens(t: TokenResponse) -> MsaTokens {
    MsaTokens {
        expires_at: chrono::Utc::now().timestamp() + t.expires_in.unwrap_or(3600),
        access_token: t.access_token,
        refresh_token: t.refresh_token,
    }
}

/// The `error` code an OAuth endpoint returned, if it returned one.
///
/// Every OAuth endpoint in this chain answers failures the same shape, so one
/// reader serves all of them.
pub(crate) fn oauth_error_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
        .as_str()
        .map(str::to_string)
}

pub(crate) fn describe_oauth_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error_description")
                .and_then(|d| d.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(300).collect())
}

// ---------------------------------------------------------------------------
// Xbox Live -> Minecraft
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct XboxResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Debug, Deserialize)]
struct DisplayClaims {
    xui: Vec<XuiClaim>,
}

#[derive(Debug, Deserialize)]
struct XuiClaim {
    uhs: String,
}

async fn xbl_authenticate(
    client: &reqwest::Client,
    msa_access_token: &str,
) -> Result<(String, String)> {
    let body = json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            // The `d=` prefix marks this as an MSA (not Xbox) ticket.
            "RpsTicket": format!("d={msa_access_token}")
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });

    let resp = client
        .post(XBL_URL)
        .json(&body)
        .header("Accept", "application/json")
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(Error::msg(format!(
            "Xbox Live sign-in failed (HTTP {})",
            resp.status()
        )));
    }
    let r: XboxResponse = resp.json().await?;
    let uhs = r
        .display_claims
        .xui
        .first()
        .map(|c| c.uhs.clone())
        .ok_or_else(|| Error::msg("Xbox Live returned no user hash"))?;
    Ok((r.token, uhs))
}

async fn xsts_authorize(client: &reqwest::Client, xbl_token: &str) -> Result<(String, String)> {
    let body = json!({
        "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl_token] },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT"
    });

    let resp = client
        .post(XSTS_URL)
        .json(&body)
        .header("Accept", "application/json")
        .send()
        .await?;

    let status = resp.status();
    let text = resp.text().await?;

    if !status.is_success() {
        // XSTS reports account-level problems through a numeric XErr rather
        // than the HTTP status, and the raw codes mean nothing to a user.
        let xerr = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("XErr").and_then(|x| x.as_u64()));
        let message = match xerr {
            Some(2148916233) => {
                "this Microsoft account has no Xbox profile — create one at xbox.com and try again"
            }
            Some(2148916235) => "Xbox Live is not available in this account's country",
            Some(2148916236) | Some(2148916237) => {
                "this account needs adult verification before it can use Xbox Live"
            }
            Some(2148916238) => {
                "this is a child account — add it to a Microsoft Family group to sign in"
            }
            _ => "Xbox Live authorization failed",
        };
        return Err(Error::msg(message.to_string()));
    }

    let r: XboxResponse =
        serde_json::from_str(&text).map_err(|e| Error::json("XSTS response", e))?;
    let uhs = r
        .display_claims
        .xui
        .first()
        .map(|c| c.uhs.clone())
        .ok_or_else(|| Error::msg("XSTS returned no user hash"))?;
    Ok((r.token, uhs))
}

#[derive(Debug, Deserialize)]
struct McLoginResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub skins: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Entitlements {
    #[serde(default)]
    items: Vec<serde_json::Value>,
}

/// Exchange a Microsoft access token for a Minecraft session.
pub async fn minecraft_session(
    client: &reqwest::Client,
    msa_access_token: &str,
) -> Result<(String, i64, McProfile)> {
    let (xbl_token, _) = xbl_authenticate(client, msa_access_token).await?;
    let (xsts_token, uhs) = xsts_authorize(client, &xbl_token).await?;

    let resp = client
        .post(MC_LOGIN_URL)
        .json(&json!({ "identityToken": format!("XBL3.0 x={uhs};{xsts_token}") }))
        .send()
        .await?;
    // A 403 here is Minecraft refusing this build's application id, which is
    // the developer's problem and not the player's: there is nothing they can
    // do about it, no setting that fixes it, and the detail only reads as
    // "this app is broken". So it says what it means for them — sign-in is
    // not available — and the reason stays in the logs. See
    // `docs/releasing.md` for what actually causes it.
    if resp.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(Error::msg(
            "Microsoft sign-in is unavailable in this copy of Geode. Signing \
             in through another launcher on this machine still works.",
        ));
    }
    if !resp.status().is_success() {
        return Err(Error::msg(format!(
            "Minecraft services rejected the sign-in (HTTP {})",
            resp.status()
        )));
    }
    let login: McLoginResponse = resp.json().await?;
    let expires_at = chrono::Utc::now().timestamp() + login.expires_in.unwrap_or(86_400);

    // A valid session does not imply a licence — demo and Bedrock-only
    // accounts authenticate fine but have no Java Edition profile.
    let ent: Entitlements = client
        .get(MC_ENTITLEMENTS_URL)
        .bearer_auth(&login.access_token)
        .send()
        .await?
        .json()
        .await
        .unwrap_or(Entitlements { items: vec![] });

    let profile_resp = client
        .get(MC_PROFILE_URL)
        .bearer_auth(&login.access_token)
        .send()
        .await?;

    if profile_resp.status() == reqwest::StatusCode::NOT_FOUND || ent.items.is_empty() {
        return Err(Error::NoEntitlement);
    }
    if !profile_resp.status().is_success() {
        return Err(Error::msg(format!(
            "could not read the Minecraft profile (HTTP {})",
            profile_resp.status()
        )));
    }

    let profile: McProfile = profile_resp.json().await?;
    Ok((login.access_token, expires_at, profile))
}

/// Offline accounts get the same UUID vanilla derives for cracked servers, so
/// single-player worlds keep the same player data across launchers.
/// Vanilla calls `UUID.nameUUIDFromBytes("OfflinePlayer:<name>")`, which is an
/// MD5 digest stamped as a version-3 UUID.
pub fn offline_uuid(username: &str) -> String {
    let mut bytes = md5_digest(format!("OfflinePlayer:{username}").as_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x30; // version 3
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// Minimal RFC 1321 MD5. Used only to reproduce Minecraft's offline UUIDs.
fn md5_digest(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );

    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);

        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_known_vectors() {
        assert_eq!(
            hex::encode(md5_digest(b"")),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(
            hex::encode(md5_digest(b"abc")),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hex::encode(md5_digest(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn offline_uuid_matches_vanilla() {
        // Vanilla derives this for the name "Notch" in offline mode.
        assert_eq!(
            offline_uuid("Notch"),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
    }

    #[test]
    fn offline_uuid_is_stable() {
        assert_eq!(offline_uuid("Steve"), offline_uuid("Steve"));
        assert_ne!(offline_uuid("Steve"), offline_uuid("Alex"));
    }
}

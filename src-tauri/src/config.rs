use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

fn default_memory() -> u32 {
    4096
}

/// A conservative, widely-recommended G1 tuning. Users can replace it wholesale
/// in Settings; we never merge or second-guess what they type.
fn default_java_args() -> String {
    "-XX:+UnlockExperimentalVMOptions -XX:+UseG1GC -XX:G1NewSizePercent=20 \
     -XX:G1ReservePercent=20 -XX:MaxGCPauseMillis=50 -XX:G1HeapRegionSize=32M"
        .to_string()
}

fn default_concurrency() -> usize {
    12
}

fn default_theme() -> String {
    "dark".into()
}

fn default_accent() -> String {
    "#4ade80".into()
}

/// A CurseForge key compiled in via `GEODE_CURSEFORGE_KEY`, if there was one.
///
/// CurseForge issues keys per developer and their terms say a key is
/// non-transferable and must not be disclosed to a third party. A *private*
/// build never reaches a third party, so baking your own key into your own
/// binary does not engage that clause — and it saves pasting it into every
/// fresh config.
///
/// Build a copy for anyone else and that stops being true, which is why this is
/// opt-in at compile time rather than a checked-in constant. Prism and most
/// open-source launchers do ship a shared key; that is a knowing trade they
/// have made, not a pattern to copy by default.
pub fn default_curseforge_key() -> Option<String> {
    option_env!("GEODE_CURSEFORGE_KEY")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Default heap size for new instances, in MiB.
    pub memory_mb: u32,
    pub java_args: String,
    /// Explicit JVM to use instead of auto-detection.
    pub java_path: Option<String>,
    /// Azure application id for Microsoft sign-in.
    pub client_id: Option<String>,
    /// CurseForge API key. Falls back to one baked in at build time; see
    /// [`default_curseforge_key`].
    pub curseforge_api_key: Option<String>,
    /// Google OAuth client id for sharing a pack through the user's own Drive.
    /// In the clear, like `client_id`: it rides in the authorization URL the
    /// user's browser opens, so it is not a secret to begin with.
    pub google_client_id: Option<String>,
    /// Only for the case where Google answers `invalid_client`. A Desktop app
    /// is a public client and its secret is not confidential by Google's own
    /// words, but it is redacted anyway — it is called a secret, and a field
    /// called a secret should not round-trip through the webview.
    pub google_client_secret: Option<String>,
    pub theme: String,
    pub accent: String,
    /// Hide the launcher window while the game runs.
    pub hide_on_launch: bool,
    pub concurrent_downloads: usize,
    /// Show snapshots and other non-release versions in the version picker.
    pub show_snapshots: bool,
    /// The one Geode version the person pressed "Skip this one" for.
    ///
    /// Kept here rather than in memory because the point of a skip is that it
    /// outlives the sitting it was made in; and kept as the single version
    /// rather than a list because a skip is not a way to turn updates off —
    /// the next release replaces it. See [`crate::update::offer`].
    pub skipped_update: Option<String>,
    /// How players reach this machine to fetch a shared server's pack.
    pub share: ShareConfig,
}

/// How this machine serves shared servers' packs to players.
///
/// One port for every shared server: the secret in a link, not the port, picks
/// which server's pack a request gets, so an admin with five servers opens one
/// port, once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShareConfig {
    pub port: u16,
    pub mapping: PortMapping,
    /// Used in links instead of the address Geode detects: a domain, a tunnel's
    /// `host:port`, or an `https://` URL in front of this machine.
    pub public_address: Option<String>,
}

/// Whether Geode opens the pack port on the router itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PortMapping {
    /// Ask the router over UPnP, renew the lease, and remove it on exit.
    #[default]
    Auto,
    /// Leave the router alone; the admin forwards the port themselves.
    Manual,
}

impl Default for ShareConfig {
    fn default() -> Self {
        Self {
            // Beside Minecraft's own range rather than in it: admins running
            // several servers count up from 25565, and this should not be the
            // port one of them lands on.
            port: 25590,
            mapping: PortMapping::Auto,
            public_address: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            memory_mb: default_memory(),
            java_args: default_java_args(),
            java_path: None,
            // Not seeded from the build-time id, for the same reason as the
            // key below — and this one had to learn it the hard way. Seeding
            // it here only reaches a config.json that does not exist yet: a
            // file written before the id was baked in holds `"clientId":
            // null`, `#[serde(default)]` fills absent fields and not explicit
            // nulls, so the compiled-in id stayed shadowed for ever and the
            // app asked for an id it already had. `effective_client_id`
            // layers instead.
            client_id: None,
            // Deliberately not seeded from the build-time key: that would get
            // written into config.json on first save, and a later `null` there
            // would then shadow it. `effective_curseforge_key` layers instead.
            curseforge_api_key: None,
            google_client_id: crate::drive::default_google_client_id(),
            google_client_secret: None,
            theme: default_theme(),
            accent: default_accent(),
            hide_on_launch: false,
            concurrent_downloads: default_concurrency(),
            show_snapshots: false,
            skipped_update: None,
            share: ShareConfig::default(),
        }
    }
}

impl Config {
    pub async fn load(paths: &Paths) -> Result<Self> {
        match tokio::fs::read(paths.config_file()).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| Error::json("config.json", e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn save(&self, paths: &Paths) -> Result<()> {
        tokio::fs::create_dir_all(&paths.root).await?;
        tokio::fs::write(paths.config_file(), serde_json::to_vec_pretty(self)?).await?;
        Ok(())
    }

    /// The Azure application id to actually sign in with: whatever the user
    /// saved, else whatever was compiled in from `GEODE_CLIENT_ID`. Every
    /// caller that is about to talk to Microsoft reads this rather than the
    /// field, so a private build signs in without anyone typing anything.
    pub fn effective_client_id(&self) -> Option<String> {
        self.client_id
            .as_ref()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .or_else(crate::auth::default_client_id)
    }

    /// The key to actually send to CurseForge: whatever the user saved, else
    /// whatever was compiled in.
    pub fn effective_curseforge_key(&self) -> Option<String> {
        self.curseforge_api_key
            .as_ref()
            .map(|k| k.trim())
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .or_else(default_curseforge_key)
    }

    /// The config as the frontend sees it — the CurseForge key is replaced with
    /// booleans so a secret never has to round-trip through the webview.
    pub fn redacted(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            let from_config = self
                .curseforge_api_key
                .as_ref()
                .is_some_and(|k| !k.trim().is_empty());
            obj.remove("curseforgeApiKey");
            obj.insert(
                "hasCurseforgeKey".into(),
                serde_json::Value::Bool(self.effective_curseforge_key().is_some()),
            );
            // Lets Settings explain where the key came from, and hide a
            // "Remove" button that could not actually remove a compiled-in one.
            obj.insert(
                "curseforgeKeyFromBuild".into(),
                serde_json::Value::Bool(!from_config && default_curseforge_key().is_some()),
            );
            // `clientId` itself stays as the user typed it, so the Settings
            // field keeps meaning "what you saved". Whether sign-in *works*
            // is a different question once a build can carry its own id.
            obj.insert(
                "hasClientId".into(),
                serde_json::Value::Bool(self.effective_client_id().is_some()),
            );
            obj.insert(
                "clientIdFromBuild".into(),
                serde_json::Value::Bool(
                    !self
                        .client_id
                        .as_ref()
                        .is_some_and(|id| !id.trim().is_empty())
                        && crate::auth::default_client_id().is_some(),
                ),
            );
            let has_google_secret = self
                .google_client_secret
                .as_ref()
                .is_some_and(|secret| !secret.trim().is_empty());
            obj.remove("googleClientSecret");
            obj.insert(
                "hasGoogleClientSecret".into(),
                serde_json::Value::Bool(has_google_secret),
            );
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every credential compiled in with `option_env!` has to be listed in
    /// `build.rs`'s `BAKED`, or Cargo never learns that the build depends on
    /// it: setting the value and rebuilding then reuses the cached binary and
    /// appears to do nothing at all, with no error anywhere.
    ///
    /// That is not hypothetical. `GEODE_GOOGLE_CLIENT_ID` was added with the
    /// Drive sharing and never listed, so a private build could carry a Google
    /// client id in `.env` and still ask the user to paste one in. This reads
    /// both sides and makes the next omission a test failure instead.
    #[test]
    fn every_compiled_in_credential_is_one_cargo_watches() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let build = std::fs::read_to_string(root.join("build.rs")).expect("build.rs");
        let baked = build
            .split("const BAKED")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .expect("build.rs has no BAKED list");

        // Every compile-time credential read anywhere under src/, which is the
        // only way a value reaches the binary. The needle is spelled in two
        // halves so this test does not find its own source.
        let needle = concat!("option", "_env!(", "\"");
        let mut used: Vec<String> = Vec::new();
        let mut walk = vec![root.join("src")];
        while let Some(dir) = walk.pop() {
            for entry in std::fs::read_dir(&dir).expect("src is readable").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("a source file");
                for after in text.split(needle).skip(1) {
                    if let Some(name) = after.split('"').next() {
                        used.push(name.to_string());
                    }
                }
            }
        }
        assert!(!used.is_empty(), "found no option_env! at all, so this proves nothing");

        let missing: Vec<&String> = used.iter().filter(|name| !baked.contains(*name)).collect();
        assert!(
            missing.is_empty(),
            "compiled in but not in build.rs's BAKED, so Cargo will not rebuild when they \
             change and the value silently never arrives: {missing:?}"
        );
    }

    fn with_key(key: &str) -> Config {
        Config {
            curseforge_api_key: Some(key.into()),
            ..Config::default()
        }
    }

    #[test]
    fn user_key_wins_over_build_key() {
        let config = with_key("user-key");
        assert_eq!(
            config.effective_curseforge_key().as_deref(),
            Some("user-key")
        );
    }

    #[test]
    fn blank_user_key_falls_through() {
        let config = with_key("   ");
        // Whitespace is not a key; it must not mask a compiled-in one.
        assert_eq!(config.effective_curseforge_key(), default_curseforge_key());
    }

    #[test]
    fn effective_key_matches_build_when_unset() {
        let config = Config::default();
        assert_eq!(config.effective_curseforge_key(), default_curseforge_key());
    }

    /// The same rule as the CurseForge key, for the field Google calls a
    /// secret even though a desktop client's is not confidential.
    #[test]
    fn redacted_never_leaks_the_google_secret() {
        let config = Config {
            google_client_id: Some("1234-abc.apps.googleusercontent.com".into()),
            google_client_secret: Some("secret-from-google".into()),
            ..Config::default()
        };
        let v = config.redacted();
        let text = serde_json::to_string(&v).unwrap();
        assert!(
            !text.contains("secret-from-google"),
            "the Google secret leaked to the frontend"
        );
        assert_eq!(v["hasGoogleClientSecret"], serde_json::json!(true));
        // The client id stays in the clear: it rides in the URL the user's own
        // browser opens, exactly as the Azure client id does.
        assert_eq!(
            v["googleClientId"],
            serde_json::json!("1234-abc.apps.googleusercontent.com")
        );
        assert_eq!(
            Config::default().redacted()["hasGoogleClientSecret"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn redacted_never_leaks_the_key() {
        let config = with_key("super-secret");
        let v = config.redacted();
        let text = serde_json::to_string(&v).unwrap();
        assert!(!text.contains("super-secret"), "key leaked to the frontend");
        assert_eq!(v["hasCurseforgeKey"], serde_json::json!(true));
        assert_eq!(v["curseforgeKeyFromBuild"], serde_json::json!(false));
    }

    /// The bug this guards: seeding `Default` with the compiled-in id put it
    /// into config.json on first save, and a config.json written *before* the
    /// id existed holds `"clientId": null` instead. `#[serde(default)]` fills
    /// fields that are *absent*, never ones that are explicitly null, so that
    /// file turned the id back into `None` on every load and the app asked
    /// for an id it was already carrying. The id has to come from the
    /// layering in `effective_client_id` and never from here.
    ///
    /// Asserted on `Default` rather than on a loaded file because this holds
    /// with or without the `.env` that CI does not have.
    #[test]
    fn the_build_time_client_id_is_never_written_into_the_config() {
        assert_eq!(Config::default().client_id, None);
    }

    #[test]
    fn an_id_saved_in_settings_wins_over_the_compiled_in_one() {
        let config = Config {
            client_id: Some("typed-into-settings".into()),
            ..Config::default()
        };
        assert_eq!(
            config.effective_client_id().as_deref(),
            Some("typed-into-settings")
        );
    }

    /// A field cleared to spaces is a field with nothing in it, not an id
    /// that happens to be blank — otherwise clearing the box would turn
    /// sign-in off in a build that can sign in perfectly well.
    #[test]
    fn a_blank_client_id_falls_back_rather_than_disabling_sign_in() {
        let config = Config {
            client_id: Some("   ".into()),
            ..Config::default()
        };
        assert_eq!(config.effective_client_id(), crate::auth::default_client_id());
    }
}

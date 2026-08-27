//! Mojang's "piston" metadata: the version manifest and per-version JSON that
//! describe how to assemble and launch any Minecraft version.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::net::get_json;

pub const VERSION_MANIFEST: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    pub versions: Vec<ManifestVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestVersion {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    pub time: String,
    pub release_time: String,
    #[serde(default)]
    pub sha1: String,
}

pub async fn fetch_manifest(client: &reqwest::Client) -> Result<VersionManifest> {
    get_json(client, VERSION_MANIFEST).await
}

// ---------------------------------------------------------------------------
// Version JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub main_class: String,
    #[serde(default)]
    pub assets: Option<String>,
    #[serde(default)]
    pub asset_index: Option<AssetIndexRef>,
    #[serde(default)]
    pub downloads: HashMap<String, Artifact>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    /// Pre-1.13 versions use a single space-separated string instead of
    /// the structured `arguments` object.
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub java_version: Option<JavaVersionRef>,
    /// Set by loader manifests (Fabric, Forge, …) that layer onto a vanilla one.
    #[serde(default)]
    pub inherits_from: Option<String>,
    #[serde(default)]
    pub logging: Option<Logging>,
    #[serde(default)]
    pub release_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    pub total_size: Option<u64>,
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersionRef {
    #[serde(default)]
    pub component: String,
    pub major_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Logging {
    #[serde(default)]
    pub client: Option<LoggingClient>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingClient {
    pub argument: String,
    pub file: LoggingFile,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingFile {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

// ---------------------------------------------------------------------------
// Libraries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Legacy natives mapping: os name -> classifier template, e.g.
    /// `{"linux": "natives-linux"}` or `{"windows": "natives-windows-${arch}"}`.
    #[serde(default)]
    pub natives: Option<HashMap<String, String>>,
    #[serde(default)]
    pub extract: Option<Extract>,
    /// Maven base URL, used by Fabric/Quilt/Forge manifests that omit
    /// `downloads` and expect the launcher to construct the URL itself.
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<Artifact>,
    #[serde(default)]
    pub classifiers: HashMap<String, Artifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extract {
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: HashMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Argument>,
    #[serde(default)]
    pub jvm: Vec<Argument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    Plain(String),
    Conditional {
        rules: Vec<Rule>,
        value: ArgumentValue,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgumentValue {
    One(String),
    Many(Vec<String>),
}

impl ArgumentValue {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            ArgumentValue::One(s) => vec![s],
            ArgumentValue::Many(v) => v,
        }
    }
}

// ---------------------------------------------------------------------------
// Rule evaluation
// ---------------------------------------------------------------------------

pub fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

pub fn current_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "x86" => "x86",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Mojang's rules use `x86`/`x86_64`/`arm64`, but some older manifests match on
/// the raw `os.arch` JVM value. Accept both spellings.
fn arch_matches(want: &str) -> bool {
    let cur = current_arch();
    match want {
        "x86" => cur == "x86",
        "x86_64" | "amd64" => cur == "x86_64",
        "arm64" | "aarch64" => cur == "arm64",
        other => other == cur,
    }
}

/// Evaluate a rule list. Rules are applied in order and the last matching one
/// wins; an empty list allows. `features` carries launcher-side toggles such as
/// `is_demo_user` and `has_custom_resolution`.
pub fn rules_allow(rules: &[Rule], features: &HashMap<String, bool>) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if !rule_matches(rule, features) {
            continue;
        }
        allowed = rule.action == "allow";
    }
    allowed
}

fn rule_matches(rule: &Rule, features: &HashMap<String, bool>) -> bool {
    if let Some(os) = &rule.os {
        if let Some(name) = &os.name {
            if name != current_os() {
                return false;
            }
        }
        if let Some(arch) = &os.arch {
            if !arch_matches(arch) {
                return false;
            }
        }
        // `os.version` is a regex against the kernel/OS version. It only ever
        // appears to single out old Windows builds, so on other platforms we
        // treat it as satisfied rather than pull in a regex engine.
        if os.version.is_some() && current_os() == "windows" {
            return false;
        }
    }
    for (feature, want) in &rule.features {
        if features.get(feature).copied().unwrap_or(false) != *want {
            return false;
        }
    }
    true
}

impl Library {
    pub fn is_allowed(&self, features: &HashMap<String, bool>) -> bool {
        rules_allow(&self.rules, features)
    }

    /// Classifier for this library's native payload on the current OS, if any.
    /// Modern (1.19+) manifests instead ship natives as ordinary libraries
    /// gated by rules, in which case this returns `None`.
    pub fn native_classifier(&self) -> Option<String> {
        let natives = self.natives.as_ref()?;
        let raw = natives.get(current_os())?;
        let arch_bits = if cfg!(target_pointer_width = "64") {
            "64"
        } else {
            "32"
        };
        Some(raw.replace("${arch}", arch_bits))
    }

    /// True when this library is *only* a native payload — those go into the
    /// natives directory, not the classpath.
    pub fn is_native_only(&self) -> bool {
        self.natives.is_some()
    }
}

/// Merge a loader manifest onto the vanilla manifest it inherits from.
///
/// Loader libraries are prepended: Fabric and Forge both ship patched copies of
/// classes that must shadow the vanilla ones, and the JVM resolves the first
/// match on the classpath.
pub fn merge_inherited(child: VersionJson, parent: VersionJson) -> VersionJson {
    let mut merged = parent;

    merged.id = child.id;
    if !child.kind.is_empty() {
        merged.kind = child.kind;
    }
    if !child.main_class.is_empty() {
        merged.main_class = child.main_class;
    }
    if child.assets.is_some() {
        merged.assets = child.assets;
    }
    if child.asset_index.is_some() {
        merged.asset_index = child.asset_index;
    }
    if child.java_version.is_some() {
        merged.java_version = child.java_version;
    }
    if child.logging.is_some() {
        merged.logging = child.logging;
    }
    for (k, v) in child.downloads {
        merged.downloads.insert(k, v);
    }

    let mut libs = child.libraries;
    libs.extend(merged.libraries);
    merged.libraries = dedupe_libraries(libs);

    match (child.arguments, merged.arguments.take()) {
        (Some(c), Some(mut p)) => {
            p.game.extend(c.game);
            p.jvm.extend(c.jvm);
            merged.arguments = Some(p);
        }
        (Some(c), None) => merged.arguments = Some(c),
        (None, p) => merged.arguments = p,
    }

    if let Some(args) = child.minecraft_arguments {
        merged.minecraft_arguments = Some(args);
    }

    merged.inherits_from = None;
    merged
}

/// Keep the first occurrence of each `group:artifact[:classifier]`, dropping
/// later (lower-priority) versions. Without this a Forge instance ends up with
/// two ASM versions on the classpath and crashes at startup.
fn dedupe_libraries(libs: Vec<Library>) -> Vec<Library> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(libs.len());
    for lib in libs {
        let parts: Vec<&str> = lib.name.split(':').collect();
        let key = match parts.len() {
            0 | 1 => lib.name.clone(),
            2 | 3 => format!("{}:{}", parts[0], parts[1]),
            _ => format!("{}:{}:{}", parts[0], parts[1], parts[3]),
        };
        if seen.insert(key) {
            out.push(lib);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feats() -> HashMap<String, bool> {
        HashMap::new()
    }

    #[test]
    fn empty_rules_allow() {
        assert!(rules_allow(&[], &feats()));
    }

    #[test]
    fn disallow_other_os_wins() {
        let rules = vec![
            Rule {
                action: "allow".into(),
                os: None,
                features: HashMap::new(),
            },
            Rule {
                action: "disallow".into(),
                os: Some(OsRule {
                    name: Some(current_os().to_string()),
                    version: None,
                    arch: None,
                }),
                features: HashMap::new(),
            },
        ];
        assert!(!rules_allow(&rules, &feats()));
    }

    #[test]
    fn allow_only_matching_os() {
        let rules = vec![Rule {
            action: "allow".into(),
            os: Some(OsRule {
                name: Some("some-other-os".into()),
                version: None,
                arch: None,
            }),
            features: HashMap::new(),
        }];
        assert!(!rules_allow(&rules, &feats()));
    }

    #[test]
    fn feature_gated_argument() {
        let mut features = HashMap::new();
        features.insert("is_demo_user".to_string(), true);
        let rules = vec![Rule {
            action: "allow".into(),
            os: None,
            features: [("is_demo_user".to_string(), true)].into_iter().collect(),
        }];
        assert!(rules_allow(&rules, &features));
        assert!(!rules_allow(&rules, &feats()));
    }

    #[test]
    fn dedupe_keeps_first() {
        let libs = vec![
            Library {
                name: "org.ow2.asm:asm:9.6".into(),
                downloads: None,
                rules: vec![],
                natives: None,
                extract: None,
                url: None,
            },
            Library {
                name: "org.ow2.asm:asm:9.3".into(),
                downloads: None,
                rules: vec![],
                natives: None,
                extract: None,
                url: None,
            },
        ];
        let out = dedupe_libraries(libs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "org.ow2.asm:asm:9.6");
    }
}

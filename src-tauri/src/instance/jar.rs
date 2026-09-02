//! What a mod jar says about itself.
//!
//! The dependency graph is read from the jars rather than from Modrinth or
//! CurseForge. The jar is what the loader actually enforces, it is right even
//! when the provider's metadata is not, and it works for mods that were
//! dropped into the folder by hand. Provider data is only a fallback for jars
//! that declare nothing we can read.

use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Ids every mod depends on that no file in the folder provides. Left in, they
/// would show up as missing dependencies on literally everything.
const PLATFORM_IDS: &[&str] = &[
    "minecraft",
    "java",
    "fabricloader",
    "fabric-loader",
    "quilt_loader",
    "quiltloader",
    "neoforge",
    "forge",
    "fml",
    "javafml",
    "mcp",
];

/// A jar's own account of what it is and what it needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JarMeta {
    pub name: Option<String>,
    pub version: Option<String>,
    /// Every id this file satisfies: its own, its declared aliases, and the
    /// ids of the jars it bundles.
    pub provides: Vec<String>,
    /// Ids it requires, with platform ids and anything it bundles removed.
    pub requires: Vec<String>,
}

/// A read, stamped with the size and mtime it was valid for.
type Cached = (u64, u64, Option<JarMeta>);

/// Jar metadata keyed by path.
///
/// Reading a folder of mods means opening every jar, and the ones that bundle
/// their dependencies — Forgified Fabric API carries about fifty — mean
/// opening those too. Doing that on every visit to the Content tab would be
/// felt, and jars do not change unless the file does.
#[derive(Default)]
pub struct MetaCache {
    inner: RwLock<HashMap<PathBuf, Cached>>,
}

impl MetaCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `path`, reusing the last result while size and mtime are unchanged.
    pub async fn get(&self, path: &Path) -> Option<JarMeta> {
        let stamp = stamp(path).await;
        if let Some((size, mtime, meta)) = self.inner.read().ok().and_then(|m| m.get(path).cloned())
        {
            if (size, mtime) == stamp {
                return meta;
            }
        }

        let owned = path.to_path_buf();
        let meta = tokio::task::spawn_blocking(move || read(&owned))
            .await
            .ok()
            .flatten();

        if let Ok(mut map) = self.inner.write() {
            map.insert(path.to_path_buf(), (stamp.0, stamp.1, meta.clone()));
        }
        meta
    }
}

async fn stamp(path: &Path) -> (u64, u64) {
    match tokio::fs::metadata(path).await {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (m.len(), mtime)
        }
        Err(_) => (0, 0),
    }
}

/// Read one jar. Blocking — callers go through [`MetaCache`].
pub fn read(path: &Path) -> Option<JarMeta> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut meta = declaration(&mut zip)?;

    // A bundled jar satisfies its ids without being a separate file, so those
    // ids count as provided here. Skipping this is how a mod that carries its
    // own dependencies ends up looking like it is missing them.
    for name in nested_jar_names(&mut zip) {
        let Some(bytes) = entry_bytes(&mut zip, &name) else {
            continue;
        };
        let Ok(mut inner) = zip::ZipArchive::new(Cursor::new(bytes)) else {
            continue;
        };
        if let Some(nested) = declaration(&mut inner) {
            meta.provides.extend(nested.provides);
        }
    }

    normalise(&mut meta);
    Some(meta)
}

fn normalise(meta: &mut JarMeta) {
    for list in [&mut meta.provides, &mut meta.requires] {
        for id in list.iter_mut() {
            *id = id.trim().to_ascii_lowercase();
        }
        list.retain(|id| !id.is_empty());
        list.sort();
        list.dedup();
    }
    meta.requires
        .retain(|id| !PLATFORM_IDS.contains(&id.as_str()) && !meta.provides.contains(id));
}

fn declaration<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Option<JarMeta> {
    if let Some(text) =
        entry_text(zip, "fabric.mod.json").or_else(|| entry_text(zip, "quilt.mod.json"))
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            return Some(from_fabric(&value));
        }
    }
    let text = entry_text(zip, "META-INF/neoforge.mods.toml")
        .or_else(|| entry_text(zip, "META-INF/mods.toml"))?;
    from_mods_toml(&text)
}

fn nested_jar_names<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Vec<String> {
    zip.file_names()
        .filter(|n| {
            n.ends_with(".jar")
                && (n.starts_with("META-INF/jars/") || n.starts_with("META-INF/jarjar/"))
        })
        .map(str::to_string)
        .collect()
}

fn entry_text<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    let mut s = String::new();
    entry.read_to_string(&mut s).ok()?;
    Some(s)
}

fn entry_bytes<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// `fabric.mod.json`, and Quilt's variant of it.
///
/// Fabric writes `depends` as an object of id to version range; Quilt writes an
/// array, whose entries may be bare ids or objects that can mark themselves
/// optional. Both shapes appear in the wild, sometimes in the same folder.
fn from_fabric(value: &serde_json::Value) -> JarMeta {
    let root = value.get("quilt_loader").unwrap_or(value);
    let named = root.get("metadata").unwrap_or(root);

    let mut provides = Vec::new();
    if let Some(id) = root.get("id").and_then(|v| v.as_str()) {
        provides.push(id.to_string());
    }
    if let Some(list) = root.get("provides").and_then(|v| v.as_array()) {
        provides.extend(list.iter().filter_map(entry_id));
    }

    let mut requires = Vec::new();
    match root.get("depends") {
        Some(serde_json::Value::Object(map)) => {
            requires.extend(map.keys().cloned());
        }
        Some(serde_json::Value::Array(list)) => {
            requires.extend(
                list.iter()
                    .filter(|d| d.get("optional").and_then(|o| o.as_bool()) != Some(true))
                    .filter_map(entry_id),
            );
        }
        _ => {}
    }

    JarMeta {
        name: named
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        version: root
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        provides,
        requires,
    }
}

/// An id written either bare or as an object with an `id` field.
fn entry_id(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| value.get("id").and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// `META-INF/neoforge.mods.toml`, and Forge's older `mods.toml`.
///
/// Dependencies live under `[[dependencies.<modid>]]`, one array per mod in the
/// file. Older files mark a dependency with `mandatory`, newer ones with
/// `type`, and both still ship.
fn from_mods_toml(text: &str) -> Option<JarMeta> {
    let root: toml::Table = text.parse().ok()?;
    let mods = root.get("mods")?.as_array()?;

    let provides: Vec<String> = mods
        .iter()
        .filter_map(|m| m.get("modId").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect();

    let mut requires = Vec::new();
    if let Some(table) = root.get("dependencies").and_then(|v| v.as_table()) {
        for entries in table.values().filter_map(|v| v.as_array()) {
            for entry in entries {
                let required = match entry.get("type").and_then(|v| v.as_str()) {
                    Some(kind) => kind.eq_ignore_ascii_case("required"),
                    // Pre-`type` files; absent `mandatory` meant required.
                    None => entry
                        .get("mandatory")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                };
                if required {
                    if let Some(id) = entry.get("modId").and_then(|v| v.as_str()) {
                        requires.push(id.to_string());
                    }
                }
            }
        }
    }

    let first = mods.first();
    Some(JarMeta {
        name: first
            .and_then(|m| m.get("displayName"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        version: first
            .and_then(|m| m.get("version"))
            .and_then(|v| v.as_str())
            // `${file.jarVersion}` is filled in by the loader from the jar
            // manifest, so it says nothing here.
            .filter(|v| !v.starts_with("${"))
            .map(str::to_string),
        provides,
        requires,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_fabric(json: &str) -> JarMeta {
        let mut meta = from_fabric(&serde_json::from_str(json).unwrap());
        normalise(&mut meta);
        meta
    }

    #[test]
    fn reads_a_fabric_mod() {
        let meta = parse_fabric(
            r#"{"id":"sodium","version":"0.5.0","name":"Sodium",
                "depends":{"minecraft":">=1.21","fabricloader":"*","fabric-api":"*"}}"#,
        );
        assert_eq!(meta.name.as_deref(), Some("Sodium"));
        assert_eq!(meta.provides, vec!["sodium"]);
        // minecraft and the loader are the platform, not installed files.
        assert_eq!(meta.requires, vec!["fabric-api"]);
    }

    #[test]
    fn reads_quilts_array_of_dependencies() {
        let meta = parse_fabric(
            r#"{"quilt_loader":{"id":"example","version":"1.0",
                "metadata":{"name":"Example"},
                "provides":[{"id":"legacy_name"}],
                "depends":[{"id":"cloth-config"},{"id":"emi","optional":true},"quilt_loader"]}}"#,
        );
        assert_eq!(meta.name.as_deref(), Some("Example"));
        assert_eq!(meta.provides, vec!["example", "legacy_name"]);
        // Optional dependencies are the author saying "works well with", and
        // pulling them into the graph would make half a folder look required.
        assert_eq!(meta.requires, vec!["cloth-config"]);
    }

    #[test]
    fn reads_a_neoforge_mod() {
        let meta = from_mods_toml(
            r#"
            modLoader = "javafml"
            [[mods]]
            modId = "jei"
            displayName = "Just Enough Items"
            version = "19.0.0"
            [[dependencies.jei]]
            modId = "neoforge"
            type = "required"
            [[dependencies.jei]]
            modId = "cloth_config"
            type = "required"
            [[dependencies.jei]]
            modId = "emi"
            type = "optional"
            "#,
        )
        .map(|mut m| {
            normalise(&mut m);
            m
        })
        .unwrap();
        assert_eq!(meta.name.as_deref(), Some("Just Enough Items"));
        assert_eq!(meta.version.as_deref(), Some("19.0.0"));
        assert_eq!(meta.provides, vec!["jei"]);
        assert_eq!(meta.requires, vec!["cloth_config"]);
    }

    #[test]
    fn treats_a_forge_dependency_without_a_type_as_required() {
        let meta = from_mods_toml(
            r#"
            [[mods]]
            modId = "old"
            displayName = "Old Mod"
            version = "${file.jarVersion}"
            [[dependencies.old]]
            modId = "patchouli"
            [[dependencies.old]]
            modId = "jei"
            mandatory = false
            "#,
        )
        .map(|mut m| {
            normalise(&mut m);
            m
        })
        .unwrap();
        // A version the loader substitutes tells the user nothing.
        assert_eq!(meta.version, None);
        assert_eq!(meta.requires, vec!["patchouli"]);
    }

    #[test]
    fn a_bundled_dependency_is_not_a_missing_one() {
        let mut meta = JarMeta {
            provides: vec!["ffapi".into(), "fabric-api-base".into()],
            requires: vec![
                "fabric-api-base".into(),
                "minecraft".into(),
                "sodium".into(),
            ],
            ..Default::default()
        };
        normalise(&mut meta);
        assert_eq!(meta.requires, vec!["sodium"]);
    }
}

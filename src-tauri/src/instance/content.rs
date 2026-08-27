//! Managing the files inside an instance: mods, resource packs, shader packs.
//!
//! Disk is the source of truth. `instance.json` only records *provenance* —
//! which project and version a file came from — so that a jar dragged in by
//! hand still shows up, and deleting a file outside the launcher does not leave
//! a phantom entry.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    Mod,
    ResourcePack,
    ShaderPack,
    DataPack,
}

impl ContentKind {
    pub fn folder(self) -> &'static str {
        match self {
            ContentKind::Mod => "mods",
            ContentKind::ResourcePack => "resourcepacks",
            ContentKind::ShaderPack => "shaderpacks",
            ContentKind::DataPack => "datapacks",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mod" => Some(ContentKind::Mod),
            "resourcepack" => Some(ContentKind::ResourcePack),
            "shaderpack" | "shader" => Some(ContentKind::ShaderPack),
            "datapack" => Some(ContentKind::DataPack),
            _ => None,
        }
    }
}

/// Provenance for one installed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentRecord {
    pub file_name: String,
    pub kind: ContentKind,
    /// `modrinth` or `curseforge`.
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
    pub name: String,
    pub version_number: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
    /// Installed as a dependency rather than chosen directly. Shown dimmed and
    /// offered for cleanup when the parent goes away.
    #[serde(default)]
    pub dependency: bool,
}

/// One row in the instance's content list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentItem {
    pub file_name: String,
    pub kind: ContentKind,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub dependency: bool,
    /// Read from the jar itself when we have no provenance record.
    pub from_disk: bool,
}

const DISABLED_SUFFIX: &str = ".disabled";

/// A disabled file is the same file with `.disabled` appended — the convention
/// every other launcher uses, so toggling here is visible to them too.
fn strip_disabled(name: &str) -> (&str, bool) {
    match name.strip_suffix(DISABLED_SUFFIX) {
        Some(base) => (base, false),
        None => (name, true),
    }
}

fn is_content_file(name: &str, kind: ContentKind) -> bool {
    let (base, _) = strip_disabled(name);
    let lower = base.to_ascii_lowercase();
    match kind {
        ContentKind::Mod => lower.ends_with(".jar"),
        ContentKind::ResourcePack | ContentKind::DataPack => lower.ends_with(".zip"),
        ContentKind::ShaderPack => lower.ends_with(".zip") || lower.ends_with(".zip.txt"),
    }
}

/// List everything of one kind in an instance, joining disk state with any
/// provenance we recorded at install time.
pub async fn list(
    paths: &Paths,
    instance: &super::Instance,
    kind: ContentKind,
) -> Result<Vec<ContentItem>> {
    let dir = paths.game_dir(&instance.id).join(kind.folder());
    let mut items = Vec::new();

    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(items),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !is_content_file(&file_name, kind) {
            continue;
        }
        let (base_name, enabled) = strip_disabled(&file_name);
        let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);

        // Records are keyed by the enabled filename so toggling does not orphan
        // the provenance.
        let record = instance
            .content
            .iter()
            .find(|r| r.file_name == base_name && r.kind == kind);

        let item = match record {
            Some(r) => ContentItem {
                file_name: file_name.clone(),
                kind,
                name: r.name.clone(),
                version: r.version_number.clone(),
                enabled,
                size,
                provider: Some(r.provider.clone()),
                project_id: Some(r.project_id.clone()),
                version_id: Some(r.version_id.clone()),
                icon_url: r.icon_url.clone(),
                slug: r.slug.clone(),
                dependency: r.dependency,
                from_disk: false,
            },
            None => {
                let meta = if kind == ContentKind::Mod {
                    read_jar_metadata(&entry.path()).await
                } else {
                    None
                };
                let (name, version) = meta.unwrap_or_else(|| (base_name.to_string(), String::new()));
                ContentItem {
                    file_name: file_name.clone(),
                    kind,
                    name,
                    version,
                    enabled,
                    size,
                    provider: None,
                    project_id: None,
                    version_id: None,
                    icon_url: None,
                    slug: None,
                    dependency: false,
                    from_disk: true,
                }
            }
        };
        items.push(item);
    }

    items.sort_by_key(|i| i.name.to_lowercase());
    Ok(items)
}

/// Best-effort display name and version for a mod jar we did not install.
/// Covers Fabric, Quilt, and Forge/NeoForge metadata; anything else falls back
/// to the filename.
async fn read_jar_metadata(jar: &Path) -> Option<(String, String)> {
    let jar = jar.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&jar).ok()?;
        let mut zip = zip::ZipArchive::new(file).ok()?;

        if let Some(found) = read_entry(&mut zip, "fabric.mod.json")
            .or_else(|| read_entry(&mut zip, "quilt.mod.json"))
        {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&found) {
                // Quilt nests its metadata under `quilt_loader`.
                let root = v.get("quilt_loader").unwrap_or(&v);
                let meta = root.get("metadata").unwrap_or(root);
                let name = meta
                    .get("name")
                    .and_then(|n| n.as_str())
                    .or_else(|| root.get("id").and_then(|n| n.as_str()))?
                    .to_string();
                let version = root
                    .get("version")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                return Some((name, version));
            }
        }

        let toml = read_entry(&mut zip, "META-INF/neoforge.mods.toml")
            .or_else(|| read_entry(&mut zip, "META-INF/mods.toml"))?;
        parse_mods_toml(&toml)
    })
    .await
    .ok()
    .flatten()
}

fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    let mut s = String::new();
    entry.read_to_string(&mut s).ok()?;
    Some(s)
}

/// Pull `displayName` and `version` out of a `mods.toml`. A real TOML parser
/// would be overkill for two quoted scalars in a known layout.
fn parse_mods_toml(toml: &str) -> Option<(String, String)> {
    let mut name = None;
    let mut version = None;
    for line in toml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("displayName") {
            name = quoted_value(rest);
        } else if let Some(rest) = line.strip_prefix("version") {
            version = quoted_value(rest);
        }
        if name.is_some() && version.is_some() {
            break;
        }
    }
    let name = name?;
    // `${file.jarVersion}` is substituted by the loader from the jar manifest,
    // so it is meaningless to show.
    let version = version
        .filter(|v| !v.starts_with("${"))
        .unwrap_or_default();
    Some((name, version))
}

fn quoted_value(rest: &str) -> Option<String> {
    let after_eq = rest.split_once('=')?.1.trim();
    let quote = after_eq.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let body = &after_eq[1..];
    Some(body[..body.find(quote)?].to_string())
}

pub fn content_dir(paths: &Paths, instance_id: &str, kind: ContentKind) -> PathBuf {
    paths.game_dir(instance_id).join(kind.folder())
}

/// Resolve a user-supplied filename to a path inside the content folder,
/// rejecting anything that tries to escape it.
fn resolve_file(paths: &Paths, instance_id: &str, kind: ContentKind, file: &str) -> Result<PathBuf> {
    let rel = Path::new(file);
    if !crate::paths::is_safe_relative(rel) || rel.components().count() != 1 {
        return Err(Error::msg("invalid filename"));
    }
    Ok(content_dir(paths, instance_id, kind).join(rel))
}

pub async fn set_enabled(
    paths: &Paths,
    instance_id: &str,
    kind: ContentKind,
    file: &str,
    enabled: bool,
) -> Result<String> {
    let current = resolve_file(paths, instance_id, kind, file)?;
    let (base, currently_enabled) = strip_disabled(file);
    if currently_enabled == enabled {
        return Ok(file.to_string());
    }

    let new_name = if enabled {
        base.to_string()
    } else {
        format!("{base}{DISABLED_SUFFIX}")
    };
    let target = resolve_file(paths, instance_id, kind, &new_name)?;
    tokio::fs::rename(&current, &target).await?;
    Ok(new_name)
}

pub async fn remove(
    paths: &Paths,
    instance: &mut super::Instance,
    kind: ContentKind,
    file: &str,
) -> Result<()> {
    let path = resolve_file(paths, instance.id.as_str(), kind, file)?;
    tokio::fs::remove_file(&path).await?;

    let (base, _) = strip_disabled(file);
    instance
        .content
        .retain(|r| !(r.file_name == base && r.kind == kind));
    Ok(())
}

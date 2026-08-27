pub mod content;
pub mod modpack;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::meta::Loader;
use crate::paths::Paths;

/// Per-instance overrides. `None` means "inherit the global setting", which is
/// why these are options rather than values copied at creation time — changing
/// the global default should move every instance that never opted out.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstanceOverrides {
    pub memory_mb: Option<u32>,
    pub java_args: Option<String>,
    pub java_path: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fullscreen: Option<bool>,
    /// Shell command run before launch; a non-zero exit aborts the launch.
    pub pre_launch_command: Option<String>,
    pub post_exit_command: Option<String>,
    /// Connect straight to a server on launch.
    pub quick_play_server: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub mc_version: String,
    #[serde(default)]
    pub loader: Loader,
    #[serde(default)]
    pub loader_version: String,
    /// An emoji, or a filename inside the instance directory.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub last_played: Option<i64>,
    #[serde(default)]
    pub play_seconds: u64,
    #[serde(default)]
    pub overrides: InstanceOverrides,
    /// Provenance for content installed from Modrinth or CurseForge, keyed by
    /// filename. The files on disk stay the source of truth for what is
    /// present; this only records where each one came from.
    #[serde(default)]
    pub content: Vec<content::ContentRecord>,
    /// Set when the instance was created from a modpack.
    #[serde(default)]
    pub pack: Option<PackSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSource {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
}

impl Instance {
    pub fn new(name: String, mc_version: String, loader: Loader, loader_version: String) -> Self {
        Self {
            id: slugify(&name),
            name,
            mc_version,
            loader,
            loader_version,
            icon: None,
            group: None,
            notes: String::new(),
            created_at: chrono::Utc::now().timestamp(),
            last_played: None,
            play_seconds: 0,
            overrides: InstanceOverrides::default(),
            content: Vec::new(),
            pack: None,
        }
    }

    pub async fn save(&self, paths: &Paths) -> Result<()> {
        let dir = paths.instance(&self.id);
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(dir.join("instance.json"), serde_json::to_vec_pretty(self)?).await?;
        Ok(())
    }
}

/// Directory-safe id derived from the display name. Instances keep their id for
/// life even if renamed, so this only has to be unique at creation.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        format!("instance-{}", &uuid::Uuid::new_v4().to_string()[..8])
    } else {
        trimmed
    }
}

/// Append a numeric suffix until the id is free on disk.
pub async fn unique_id(paths: &Paths, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while tokio::fs::metadata(paths.instance(&candidate)).await.is_ok() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    candidate
}

pub async fn load(paths: &Paths, id: &str) -> Result<Instance> {
    let path = paths.instance(id).join("instance.json");
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| Error::NoSuchInstance(id.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::json("instance.json", e))
}

pub async fn list(paths: &Paths) -> Result<Vec<Instance>> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(paths.instances()).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // A directory without a readable instance.json is not ours — skip it
        // rather than failing the whole listing.
        if let Ok(instance) = load(paths, &id).await {
            out.push(instance);
        }
    }

    out.sort_by(|a, b| {
        b.last_played
            .unwrap_or(0)
            .cmp(&a.last_played.unwrap_or(0))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

pub async fn create(
    paths: &Paths,
    name: String,
    mc_version: String,
    loader: Loader,
    loader_version: String,
) -> Result<Instance> {
    let mut instance = Instance::new(name, mc_version, loader, loader_version);
    instance.id = unique_id(paths, &instance.id).await;

    let game_dir = paths.game_dir(&instance.id);
    for sub in ["mods", "config", "resourcepacks", "shaderpacks", "saves"] {
        tokio::fs::create_dir_all(game_dir.join(sub)).await?;
    }
    instance.save(paths).await?;
    Ok(instance)
}

pub async fn delete(paths: &Paths, id: &str) -> Result<()> {
    let dir = paths.instance(id);
    // Guard against a malformed id deleting something outside the tree.
    if !dir.starts_with(paths.instances()) || dir == paths.instances() {
        return Err(Error::msg("refusing to delete an unexpected path"));
    }
    tokio::fs::remove_dir_all(&dir)
        .await
        .map_err(|_| Error::NoSuchInstance(id.to_string()))?;
    Ok(())
}

pub async fn duplicate(paths: &Paths, id: &str, new_name: String) -> Result<Instance> {
    let source = load(paths, id).await?;
    let mut copy = source.clone();
    copy.id = unique_id(paths, &slugify(&new_name)).await;
    copy.name = new_name;
    copy.created_at = chrono::Utc::now().timestamp();
    copy.last_played = None;
    copy.play_seconds = 0;

    copy_dir(&paths.game_dir(id), &paths.game_dir(&copy.id)).await?;
    copy.save(paths).await?;
    Ok(copy)
}

/// Recursive copy. Instance trees are shallow but can hold thousands of files
/// (world regions), so this walks iteratively rather than recursing.
pub async fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    let mut stack = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        tokio::fs::create_dir_all(&dst).await?;
        let mut entries = match tokio::fs::read_dir(&src).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let target = dst.join(entry.file_name());
            if file_type.is_dir() {
                stack.push((entry.path(), target));
            } else if file_type.is_file() {
                tokio::fs::copy(entry.path(), target).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_normalizes() {
        assert_eq!(slugify("My Cool Pack 1.21"), "my-cool-pack-1-21");
        assert_eq!(slugify("  spaces  "), "spaces");
    }

    #[test]
    fn slugify_collapses_runs() {
        assert_eq!(slugify("a///b"), "a-b");
    }

    #[test]
    fn slugify_falls_back_for_empty() {
        assert!(slugify("!!!").starts_with("instance-"));
    }
}

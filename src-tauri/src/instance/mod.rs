pub mod content;
pub mod jar;
pub mod logs;
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
    /// An uploaded image, named relative to the instance's `media` folder.
    /// Never an absolute path: `instance.json` has to survive the data
    /// directory moving.
    #[serde(default)]
    pub icon: Option<String>,
    /// The artwork across the top of the instance's page, same rules.
    #[serde(default)]
    pub banner: Option<String>,
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
    /// The server this was joined from. Read to recognise a link opened again,
    /// and to hold a typed address to the certificate it had; nothing updates
    /// an instance from its server yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined: Option<crate::share::link::Link>,
}

/// Where an instance came from, and enough of the source's identity to keep
/// showing it. An instance built from a pack should look like that pack, not
/// like a generic folder that once had files copied into it — so the artwork
/// and blurb are stored rather than re-fetched, and survive being offline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PackSource {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub banner_url: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub page_url: Option<String>,
    /// The pack version id, so an update check knows what it is comparing to.
    #[serde(default)]
    pub version_id: Option<String>,
}

impl PackSource {
    /// A pack installed from a provider, keeping its artwork and blurb.
    pub fn from_project(
        project: &crate::providers::Project,
        version_number: String,
        version_id: String,
    ) -> Self {
        Self {
            name: project.name.clone(),
            version: version_number,
            provider: Some(project.provider.as_str().to_string()),
            project_id: Some(project.id.clone()),
            slug: Some(project.slug.clone()),
            icon_url: project.icon_url.clone(),
            banner_url: project.banner_url.clone(),
            summary: (!project.summary.is_empty()).then(|| project.summary.clone()),
            page_url: (!project.page_url.is_empty()).then(|| project.page_url.clone()),
            version_id: Some(version_id),
        }
    }

    /// A pack imported from a file on disk: a name and version, nothing to
    /// link back to.
    pub fn local(name: String, version: String, provider: Option<String>) -> Self {
        Self {
            name,
            version,
            provider,
            project_id: None,
            slug: None,
            icon_url: None,
            banner_url: None,
            summary: None,
            page_url: None,
            version_id: None,
        }
    }
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
            banner: None,
            group: None,
            notes: String::new(),
            created_at: chrono::Utc::now().timestamp(),
            last_played: None,
            play_seconds: 0,
            overrides: InstanceOverrides::default(),
            content: Vec::new(),
            pack: None,
            joined: None,
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
    while tokio::fs::metadata(paths.instance(&candidate))
        .await
        .is_ok()
    {
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
    // A copy is the player's own, not a join. Kept, its newer date would make
    // the pin it carries the one a typed address is held to — a certificate
    // the server may have replaced since, and a later join already knows.
    copy.joined = None;

    copy_dir(&paths.game_dir(id), &paths.game_dir(&copy.id)).await?;
    // The copy keeps the original's `icon` and `banner` names, so the files
    // they refer to have to come along or it would point at nothing.
    let _ = copy_dir(&paths.instance_media(id), &paths.instance_media(&copy.id)).await;
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

    /// Duplicating a joined instance makes a copy that is not itself a join.
    #[tokio::test]
    async fn a_copy_of_a_joined_instance_is_not_a_join() {
        let paths = Paths {
            root: std::env::temp_dir().join(format!("geode-dup-{}", uuid::Uuid::new_v4())),
        };
        let mut original = create(&paths, "Survival".into(), "1.21.1".into(), Loader::Fabric, "0.16.9".into())
            .await
            .unwrap();
        original.joined = Some(
            crate::share::link::Link::parse(&format!(
                "geode://join?addr=203.0.113.7:25590&code=GEODE-ABCDE-FGHJK&pin={}",
                "ab".repeat(32)
            ))
            .unwrap(),
        );
        original.save(&paths).await.unwrap();

        let copy = duplicate(&paths, &original.id, "Backup".into()).await.unwrap();
        assert!(copy.joined.is_none());
        assert!(load(&paths, &copy.id).await.unwrap().joined.is_none());
        assert!(load(&paths, &original.id).await.unwrap().joined.is_some());

        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn slugify_falls_back_for_empty() {
        assert!(slugify("!!!").starts_with("instance-"));
    }
}

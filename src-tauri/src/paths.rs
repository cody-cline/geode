use std::path::{Path, PathBuf};

/// On-disk layout for everything Geode owns.
///
/// Libraries, assets and Java runtimes are shared across every instance —
/// installing ten 1.21 instances costs one copy of the ~400 MB asset store,
/// not ten. Only `instances/<id>/.minecraft` is per-instance.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self::new()
    }
}

impl Paths {
    pub fn new() -> Self {
        // GEODE_HOME lets you run a throwaway copy against a scratch directory
        // without touching a real install.
        let root = std::env::var_os("GEODE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::data_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("geode")
            });
        Self { root }
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.json")
    }
    pub fn accounts_file(&self) -> PathBuf {
        self.root.join("accounts.json")
    }
    pub fn instances(&self) -> PathBuf {
        self.root.join("instances")
    }
    pub fn instance(&self, id: &str) -> PathBuf {
        self.instances().join(id)
    }
    /// Modpack plans. Not under `instances/` because a plan is not one: nothing
    /// is downloaded for it, and the same plan can be built more than once.
    pub fn plans(&self) -> PathBuf {
        self.root.join("plans")
    }
    pub fn plan(&self, id: &str) -> PathBuf {
        self.plans().join(format!("{id}.json"))
    }
    /// The game's working directory — what vanilla calls `.minecraft`.
    pub fn game_dir(&self, id: &str) -> PathBuf {
        self.instance(id).join(".minecraft")
    }
    pub fn libraries(&self) -> PathBuf {
        self.root.join("libraries")
    }
    pub fn assets(&self) -> PathBuf {
        self.root.join("assets")
    }
    pub fn asset_objects(&self) -> PathBuf {
        self.assets().join("objects")
    }
    pub fn asset_indexes(&self) -> PathBuf {
        self.assets().join("indexes")
    }
    pub fn versions(&self) -> PathBuf {
        self.root.join("versions")
    }
    /// Cached merged version manifest for a resolved (mc + loader) combo.
    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.versions().join(id)
    }
    pub fn version_json(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.json"))
    }
    pub fn version_jar(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.jar"))
    }
    pub fn natives(&self, id: &str) -> PathBuf {
        self.root.join("natives").join(id)
    }
    pub fn java(&self) -> PathBuf {
        self.root.join("java")
    }
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }
    pub fn icon_cache(&self) -> PathBuf {
        self.cache().join("icons")
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Resolve a Maven coordinate (`group:artifact:version[:classifier][@ext]`)
    /// to its path inside the shared library store.
    pub fn maven(&self, coord: &str) -> PathBuf {
        self.libraries().join(maven_relative_path(coord))
    }

    pub async fn ensure_all(&self) -> std::io::Result<()> {
        for d in [
            self.root.clone(),
            self.instances(),
            self.plans(),
            self.libraries(),
            self.asset_objects(),
            self.asset_indexes(),
            self.versions(),
            self.java(),
            self.icon_cache(),
            self.logs(),
        ] {
            tokio::fs::create_dir_all(&d).await?;
        }
        Ok(())
    }
}

/// `net.fabricmc:intermediary:1.21.4` -> `net/fabricmc/intermediary/1.21.4/intermediary-1.21.4.jar`
///
/// Also handles the `:classifier` and `@extension` suffixes that Forge and the
/// natives entries use, e.g.
/// `org.lwjgl:lwjgl:3.3.3:natives-linux` and `net.minecraftforge:forge:1.20.1-47.2.0:installer@jar`.
pub fn maven_relative_path(coord: &str) -> PathBuf {
    let (coord, ext) = match coord.split_once('@') {
        Some((c, e)) => (c, e),
        None => (coord, "jar"),
    };

    let parts: Vec<&str> = coord.split(':').collect();
    let group = parts.first().copied().unwrap_or_default();
    let artifact = parts.get(1).copied().unwrap_or_default();
    let version = parts.get(2).copied().unwrap_or_default();
    let classifier = parts.get(3).copied();

    let filename = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.{ext}"),
        None => format!("{artifact}-{version}.{ext}"),
    };

    let mut p = PathBuf::new();
    for seg in group.split('.') {
        p.push(seg);
    }
    p.push(artifact);
    p.push(version);
    p.push(filename);
    p
}

/// Reject path components that would escape the destination directory.
/// Used when extracting archives we did not create (mrpacks, natives, JREs).
pub fn is_safe_relative(p: &Path) -> bool {
    use std::path::Component;
    !p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_plain() {
        assert_eq!(
            maven_relative_path("net.fabricmc:intermediary:1.21.4"),
            PathBuf::from("net/fabricmc/intermediary/1.21.4/intermediary-1.21.4.jar")
        );
    }

    #[test]
    fn maven_with_classifier() {
        assert_eq!(
            maven_relative_path("org.lwjgl:lwjgl:3.3.3:natives-linux"),
            PathBuf::from("org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar")
        );
    }

    #[test]
    fn maven_with_extension() {
        assert_eq!(
            maven_relative_path("de.oceanlabs.mcp:mcp_config:1.20.1:mappings@txt"),
            PathBuf::from("de/oceanlabs/mcp/mcp_config/1.20.1/mcp_config-1.20.1-mappings.txt")
        );
    }

    #[test]
    fn rejects_traversal() {
        assert!(!is_safe_relative(Path::new("../evil")));
        assert!(!is_safe_relative(Path::new("/etc/passwd")));
        assert!(is_safe_relative(Path::new("mods/cool.jar")));
    }
}

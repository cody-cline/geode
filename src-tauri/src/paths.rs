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
    /// This machine's identity for sharing: the certificate players pin.
    /// Losing it breaks every link already handed out, by design.
    pub fn share_dir(&self) -> PathBuf {
        self.root.join("share")
    }
    pub fn accounts_file(&self) -> PathBuf {
        self.root.join("accounts.json")
    }
    /// The Google Drive connection: a refresh token and the folder Geode made.
    /// Beside `accounts.json` and written the same way, because it holds the
    /// same kind of thing.
    pub fn google_file(&self) -> PathBuf {
        self.root.join("google.json")
    }
    pub fn instances(&self) -> PathBuf {
        self.root.join("instances")
    }
    pub fn instance(&self, id: &str) -> PathBuf {
        self.instances().join(id)
    }
    /// Images the user uploaded for this instance — its icon and its banner.
    ///
    /// Beside `instance.json` rather than inside `.minecraft`, so exporting or
    /// wiping the game directory never takes the artwork with it.
    pub fn instance_media(&self, id: &str) -> PathBuf {
        self.instance(id).join("media")
    }
    /// What the last update from this instance's server changed in its
    /// settings files, for the panel that tells the player about it.
    ///
    /// Beside `instance.json` rather than inside `.minecraft`, for the reason
    /// `media/` is: no export and no pack ever carries it. And a file of its
    /// own rather than a field on `instance.json`, which long jobs rewrite
    /// whole — the hazard `server::share`'s module header describes.
    pub fn instance_sync(&self, id: &str) -> PathBuf {
        self.instance(id).join("last-sync.json")
    }
    /// Modpack plans. Not under `instances/` because a plan is not one: nothing
    /// is downloaded for it, and the same plan can be built more than once.
    pub fn plans(&self) -> PathBuf {
        self.root.join("plans")
    }
    pub fn plan(&self, id: &str) -> PathBuf {
        self.plans().join(format!("{id}.json"))
    }
    pub fn servers(&self) -> PathBuf {
        self.root.join("servers")
    }
    pub fn server(&self, id: &str) -> PathBuf {
        self.servers().join(id)
    }
    /// A server's uploaded icon and banner, kept out of the run directory so a
    /// backup of the server is the server and not its decoration.
    pub fn server_media(&self, id: &str) -> PathBuf {
        self.server(id).join("media")
    }
    /// A server's working directory — where the jar runs, and where the world,
    /// `server.properties` and `eula.txt` live. Kept beside `server.json`
    /// rather than containing it, so a backup of the run directory is exactly
    /// the server's own data and none of our bookkeeping.
    pub fn server_run(&self, id: &str) -> PathBuf {
        self.server(id).join("run")
    }
    /// Beside the run directory rather than inside it, so a backup is never
    /// swept into the next one.
    pub fn server_backups(&self, id: &str) -> PathBuf {
        self.server(id).join("backups")
    }
    /// The copy of each synced settings file as it was when its settings were
    /// last published, which is what the next push is measured against.
    ///
    /// Beside the run directory for the same reasons `media/` and `backups/`
    /// are: a backup covers `run/` alone, so this can never be swept into one
    /// and handed back later as though it were the admin's own file, and the
    /// pack's own allowlisted walk of `run/` cannot see it, so it never
    /// travels to a player.
    pub fn server_published(&self, id: &str) -> PathBuf {
        self.server(id).join("published")
    }
    /// Where a server arriving from another machine is assembled.
    ///
    /// Outside `servers/` on purpose. [`crate::server::list`] reads every
    /// directory under `servers/`, so a half-arrived server there would show
    /// in the app as a real one the moment its `server.json` landed. A folder
    /// here is a transfer whose process died: nothing reads it, and the next
    /// attempt at that server clears it.
    pub fn transfers(&self) -> PathBuf {
        self.root.join("transfers")
    }
    pub fn transfer(&self, id: &str) -> PathBuf {
        self.transfers().join(id)
    }
    /// The `geoded` hosts this copy of Geode can reach, and the token for
    /// each. Beside `accounts.json` and `google.json`, written the same way,
    /// because it holds the same kind of thing.
    pub fn hosts_file(&self) -> PathBuf {
        self.root.join("hosts.json")
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
            self.servers(),
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

/// A data directory of its own under the system's temporary directory,
/// removed when the test ends however it ends.
///
/// A `remove_dir_all` at the end of a test does not run when the test panics,
/// which is exactly when the directory is worth having gone; a drop guard
/// does. The name carries a uuid rather than the process id so a reused pid
/// cannot hand a test the previous run's files.
///
/// Bind the guard for as long as the test uses its paths. `Scratch::new(..)`
/// whose `paths` are taken and whose guard is not kept drops at the end of
/// that statement, and the directory the test then fills is left behind —
/// which is the leak this exists to stop.
#[cfg(test)]
pub struct Scratch {
    pub paths: Paths,
}

#[cfg(test)]
impl Scratch {
    pub fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("geode-{tag}-{}", uuid::Uuid::new_v4()));
        Self {
            paths: Paths { root },
        }
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.paths.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_root_is_gone_once_its_guard_drops() {
        let root = {
            let scratch = Scratch::new("drops");
            std::fs::create_dir_all(scratch.paths.instances()).unwrap();
            scratch.paths.root.clone()
        };
        assert!(!root.exists(), "{} outlived its guard", root.display());
    }

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

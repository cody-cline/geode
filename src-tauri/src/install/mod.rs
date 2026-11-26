//! Turning "I want 1.21.4 with Fabric" into a directory tree the JVM can run.

pub mod blocked;
pub mod content;
pub mod identity;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::java;
use crate::meta::{self, piston, Loader};
use crate::net::{Download, Progress};
use crate::paths::{maven_relative_path, Paths};

const RESOURCES_BASE: &str = "https://resources.download.minecraft.net";

pub struct Installed {
    pub version_id: String,
    pub version: piston::VersionJson,
    pub java: PathBuf,
    pub natives_dir: PathBuf,
    /// The client jar to put on the classpath. For loaders this is an alias of
    /// the shared jar named after `version_id` — see [`alias_client_jar`].
    pub client_jar: PathBuf,
    /// Absolute path to the log4j config the game should use, if the version
    /// ships one.
    pub log_config: Option<PathBuf>,
}

/// Install (or verify) everything needed to launch, then return the resolved
/// launch inputs. Safe to re-run: every step short-circuits on valid files.
#[allow(clippy::too_many_arguments)]
pub async fn install_version(
    client: &reqwest::Client,
    paths: &Paths,
    manifests: &meta::ManifestCache,
    mc_version: &str,
    loader: Loader,
    loader_version: &str,
    java_override: Option<&Path>,
    progress: &Progress,
) -> Result<Installed> {
    paths.ensure_all().await?;

    progress.stage("Reading version metadata", 1).await;
    let manifest = manifests.get(client, paths).await?;
    let vanilla = meta::vanilla_version_json(client, paths, &manifest, mc_version).await?;
    progress.advance(1, mc_version).await;

    // The client jar is both a launch input and a processor input for
    // Forge/NeoForge, so it has to land before the loader install runs.
    let client_jar = paths.version_jar(mc_version)?;
    if let Some(art) = vanilla.downloads.get("client") {
        progress.stage("Downloading Minecraft", 1).await;
        crate::net::fetch_one(
            client,
            &Download::new(art.url.clone(), client_jar.clone())
                .sha1(art.sha1.clone())
                .size(art.size)
                .label(format!("minecraft {mc_version}")),
        )
        .await?;
        progress.advance(1, "client jar").await;
    }

    let required_java = java::required_major(&vanilla);
    let java_path = java::ensure(client, paths, required_java, java_override, progress).await?;

    // Layer the loader on top of vanilla.
    let version_id = meta::version_id(mc_version, loader, loader_version);
    let version = match loader {
        Loader::Vanilla => vanilla,
        Loader::Fabric | Loader::Quilt => {
            progress.stage("Fetching loader metadata", 1).await;
            let flavor = if loader == Loader::Fabric {
                meta::fabric::Flavor::Fabric
            } else {
                meta::fabric::Flavor::Quilt
            };
            let profile = meta::fabric::profile(client, flavor, mc_version, loader_version).await?;
            progress.advance(1, loader.display_name()).await;
            piston::merge_inherited(profile, vanilla)
        }
        Loader::NeoForge | Loader::Forge => {
            let flavor = if loader == Loader::NeoForge {
                meta::forge::Flavor::NeoForge
            } else {
                meta::forge::Flavor::Forge
            };
            let profile = meta::forge::install(
                client,
                paths,
                flavor,
                mc_version,
                loader_version,
                &client_jar,
                &java_path,
                progress,
            )
            .await?;
            piston::merge_inherited(profile, vanilla)
        }
    };

    // Cache the merged manifest so a relaunch skips all the meta round-trips.
    tokio::fs::create_dir_all(paths.version_dir(&version_id)?).await?;
    tokio::fs::write(
        paths.version_json(&version_id)?,
        serde_json::to_vec_pretty(&version)?,
    )
    .await?;

    let client_jar = alias_client_jar(paths, &client_jar, mc_version, &version_id).await?;

    let features = launch_features();
    download_libraries(client, paths, &version, &features, progress).await?;
    let natives_dir = extract_natives(paths, &version, &features, progress).await?;
    download_assets(client, paths, &version, progress).await?;
    let log_config = download_log_config(client, paths, &version).await?;

    progress.finish().await;

    Ok(Installed {
        version_id,
        version,
        java: java_path,
        natives_dir,
        client_jar,
        log_config,
    })
}

/// Give the shared client jar a second name matching the resolved version id.
///
/// NeoForge and Forge pass `-DignoreList=client-extra,${version_name}.jar` so
/// BootstrapLauncher can keep the unpatched client jar out of the module layer.
/// That match is on the *filename*, so a jar sitting at `1.21.1.jar` while
/// `version_name` is `neoforge-21.1.248` is not excluded — and the game dies
/// with a duplicate-module error before it ever draws a frame.
///
/// A hard link keeps one copy of the ~25 MB jar on disk; a plain copy is the
/// fallback for filesystems or platforms that will not link.
async fn alias_client_jar(
    paths: &Paths,
    shared: &Path,
    mc_version: &str,
    version_id: &str,
) -> Result<PathBuf> {
    if version_id == mc_version {
        return Ok(shared.to_path_buf());
    }

    let alias = paths.version_jar(version_id)?;
    if let Ok(existing) = tokio::fs::metadata(&alias).await {
        // Re-link if the shared jar was replaced (a corrupt download redownloaded).
        let source_len = tokio::fs::metadata(shared)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if existing.len() == source_len && source_len > 0 {
            return Ok(alias);
        }
        let _ = tokio::fs::remove_file(&alias).await;
    }

    if let Some(parent) = alias.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    match tokio::fs::hard_link(shared, &alias).await {
        Ok(()) => Ok(alias),
        Err(_) => {
            tokio::fs::copy(shared, &alias).await?;
            Ok(alias)
        }
    }
}

/// Feature flags used when evaluating argument and library rules. We do not
/// support demo mode or quick-play yet, and custom resolution is decided per
/// launch, so this is the conservative baseline.
pub fn launch_features() -> HashMap<String, bool> {
    HashMap::new()
}

/// Where a library's jar lives in the shared store, and where to get it.
fn library_download(lib: &piston::Library, paths: &Paths) -> Option<Download> {
    if let Some(downloads) = &lib.downloads {
        if let Some(art) = &downloads.artifact {
            if !art.url.is_empty() {
                let rel = art
                    .path
                    .clone()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| maven_relative_path(&lib.name));
                return Some(
                    Download::new(art.url.clone(), paths.libraries().join(rel))
                        .sha1(art.sha1.clone())
                        .size(art.size)
                        .label(lib.name.clone()),
                );
            }
        }
    }
    // Fabric/Quilt list libraries as a coordinate plus a maven base URL.
    let base = lib
        .url
        .clone()
        .unwrap_or_else(|| "https://libraries.minecraft.net/".to_string());
    let rel = maven_relative_path(&lib.name);
    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        rel.to_string_lossy().replace('\\', "/")
    );
    Some(Download::new(url, paths.libraries().join(rel)).label(lib.name.clone()))
}

/// The native payload for a library on this OS, for manifests that still use
/// the legacy `natives` + `classifiers` layout.
fn native_download(lib: &piston::Library, paths: &Paths) -> Option<(Download, Vec<String>)> {
    let classifier = lib.native_classifier()?;
    let downloads = lib.downloads.as_ref()?;
    let art = downloads.classifiers.get(&classifier)?;
    let rel = art
        .path
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| maven_relative_path(&format!("{}:{}", lib.name, classifier)));
    let exclude = lib
        .extract
        .as_ref()
        .map(|e| e.exclude.clone())
        .unwrap_or_default();
    Some((
        Download::new(art.url.clone(), paths.libraries().join(rel))
            .sha1(art.sha1.clone())
            .size(art.size)
            .label(format!("{} ({classifier})", lib.name)),
        exclude,
    ))
}

async fn download_libraries(
    client: &reqwest::Client,
    paths: &Paths,
    version: &piston::VersionJson,
    features: &HashMap<String, bool>,
    progress: &Progress,
) -> Result<()> {
    let mut downloads = Vec::new();
    for lib in &version.libraries {
        if !lib.is_allowed(features) {
            continue;
        }
        if let Some((native, _)) = native_download(lib, paths) {
            downloads.push(native);
        }
        if !lib.is_native_only() {
            if let Some(d) = library_download(lib, paths) {
                downloads.push(d);
            }
        }
    }

    progress
        .stage("Downloading libraries", downloads.len() as u64)
        .await;
    crate::net::fetch_all(client, downloads, progress, 12).await
}

/// Unpack native `.so`/`.dll`/`.dylib` payloads into a per-version directory
/// that gets passed as `java.library.path`.
async fn extract_natives(
    paths: &Paths,
    version: &piston::VersionJson,
    features: &HashMap<String, bool>,
    progress: &Progress,
) -> Result<PathBuf> {
    let dir = paths.natives(&version.id)?;
    tokio::fs::create_dir_all(&dir).await?;

    let mut jobs: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for lib in &version.libraries {
        if !lib.is_allowed(features) {
            continue;
        }
        if let Some((download, exclude)) = native_download(lib, paths) {
            jobs.push((download.dest, exclude));
        } else if lib.name.contains(":natives-") {
            // 1.19+ ships natives as ordinary rule-gated libraries.
            if let Some(d) = library_download(lib, paths) {
                jobs.push((d.dest, Vec::new()));
            }
        }
    }

    if jobs.is_empty() {
        return Ok(dir);
    }

    progress
        .stage("Extracting natives", jobs.len() as u64)
        .await;
    for (jar, exclude) in jobs {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || extract_native_jar(&jar, &dir, &exclude))
            .await
            .map_err(|e| Error::msg(format!("native extraction panicked: {e}")))??;
        progress.advance(1, "natives").await;
    }
    Ok(dir)
}

/// The most one native payload inside a library jar may inflate to.
///
/// These are `.so`/`.dll`/`.dylib` files, and the largest real one in a
/// Minecraft classpath is around a megabyte — LWJGL's are hundreds of
/// kilobytes each. 16 MB is a long way past any of them and still a bound,
/// which `read_to_end` on its own was not.
const MAX_NATIVE_BYTES: u64 = 16 * 1024 * 1024;

fn extract_native_jar(jar: &Path, dest: &Path, exclude: &[String]) -> Result<()> {
    if !jar.exists() {
        return Ok(());
    }
    let file = std::fs::File::open(jar)?;
    let mut zip = zip::ZipArchive::new(file)?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();

        // Manifests, signatures and the `META-INF` tree are never native code.
        if name.starts_with("META-INF/") || exclude.iter().any(|e| name.starts_with(e)) {
            continue;
        }
        let is_native = name.ends_with(".so")
            || name.ends_with(".dll")
            || name.ends_with(".dylib")
            || name.ends_with(".jnilib");
        if !is_native {
            continue;
        }

        // Flatten: the JVM only scans the top level of java.library.path.
        let Some(filename) = Path::new(&name).file_name() else {
            continue;
        };
        let out_path = dest.join(filename);
        let mut buf = Vec::new();
        // Counted on the bytes as they inflate, not on the size the entry's
        // header declares, which a crafted jar can understate. Mojang's own
        // libraries arrive sha1-pinned and are not the worry; a loader
        // manifest's are listed as a coordinate and a maven base with no
        // digest at all (`library_download`), so what lands in `libraries/`
        // for one of those is whatever that host served.
        (&mut entry).take(MAX_NATIVE_BYTES + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > MAX_NATIVE_BYTES {
            return Err(Error::msg(format!(
                "'{name}' in {} unpacks to more than {} MB, which is more than any native \
                 library is",
                jar.display(),
                MAX_NATIVE_BYTES / (1024 * 1024)
            )));
        }
        std::fs::write(&out_path, buf)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&out_path)?.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&out_path, perms);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Assets
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AssetIndex {
    objects: HashMap<String, AssetObject>,
    /// Pre-1.7 versions read assets from a plain directory tree instead of the
    /// hashed object store.
    #[serde(default)]
    r#virtual: bool,
    #[serde(default)]
    map_to_resources: bool,
}

#[derive(Debug, Deserialize)]
struct AssetObject {
    hash: String,
    size: u64,
}

async fn download_assets(
    client: &reqwest::Client,
    paths: &Paths,
    version: &piston::VersionJson,
    progress: &Progress,
) -> Result<()> {
    let Some(index_ref) = &version.asset_index else {
        return Ok(());
    };

    progress.stage("Fetching asset index", 1).await;
    let index_path = paths.asset_indexes().join(format!("{}.json", index_ref.id));
    crate::net::fetch_one(
        client,
        &Download::new(index_ref.url.clone(), index_path.clone())
            .sha1(Some(index_ref.sha1.clone()))
            .size(Some(index_ref.size))
            .label("asset index"),
    )
    .await?;
    progress.advance(1, &index_ref.id).await;

    let raw = tokio::fs::read(&index_path).await?;
    let index: AssetIndex =
        serde_json::from_slice(&raw).map_err(|e| Error::json("asset index", e))?;

    let downloads: Vec<Download> = index
        .objects
        .iter()
        .map(|(name, obj)| {
            let prefix = &obj.hash[..2];
            Download::new(
                format!("{RESOURCES_BASE}/{prefix}/{}", obj.hash),
                paths.asset_objects().join(prefix).join(&obj.hash),
            )
            .sha1(Some(obj.hash.clone()))
            .size(Some(obj.size))
            .label(name.clone())
        })
        .collect();

    progress
        .stage("Downloading assets", downloads.len() as u64)
        .await;
    // Asset objects are small and numerous; higher concurrency pays off here.
    crate::net::fetch_all(client, downloads, progress, 24).await?;

    if index.r#virtual || index.map_to_resources {
        materialize_virtual_assets(paths, &index, &index_ref.id, progress).await?;
    }
    Ok(())
}

/// Old versions expect `assets/virtual/<index>/<name>` as real files rather
/// than the content-addressed object store.
async fn materialize_virtual_assets(
    paths: &Paths,
    index: &AssetIndex,
    index_id: &str,
    progress: &Progress,
) -> Result<()> {
    let root = paths.assets().join("virtual").join(index_id);
    progress
        .stage("Preparing legacy assets", index.objects.len() as u64)
        .await;

    for (name, obj) in &index.objects {
        let rel = Path::new(name);
        if !crate::paths::is_safe_relative(rel) {
            continue;
        }
        let dest = root.join(rel);
        if tokio::fs::metadata(&dest).await.is_ok() {
            progress.advance(1, name).await;
            continue;
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let src = paths.asset_objects().join(&obj.hash[..2]).join(&obj.hash);
        tokio::fs::copy(&src, &dest).await?;
        progress.advance(1, name).await;
    }
    Ok(())
}

/// Where the game should look for assets. Legacy indexes get the virtual tree.
pub fn assets_dir_for(paths: &Paths, version: &piston::VersionJson) -> PathBuf {
    let Some(index) = &version.asset_index else {
        return paths.assets();
    };
    // `pre-1.6` and `legacy` are the two index ids Mojang uses for the old
    // layout; checking the id avoids re-reading the index JSON at launch.
    if index.id == "pre-1.6" || index.id == "legacy" {
        paths.assets().join("virtual").join(&index.id)
    } else {
        paths.assets()
    }
}

async fn download_log_config(
    client: &reqwest::Client,
    paths: &Paths,
    version: &piston::VersionJson,
) -> Result<Option<PathBuf>> {
    let Some(logging) = &version.logging else {
        return Ok(None);
    };
    let Some(client_log) = &logging.client else {
        return Ok(None);
    };

    let dest = paths.assets().join("log_configs").join(&client_log.file.id);
    crate::net::fetch_one(
        client,
        &Download::new(client_log.file.url.clone(), dest.clone())
            .sha1(Some(client_log.file.sha1.clone()))
            .size(Some(client_log.file.size))
            .label("log config"),
    )
    .await?;
    Ok(Some(dest))
}

/// Every jar that belongs on the classpath, in manifest order.
pub fn classpath(
    paths: &Paths,
    version: &piston::VersionJson,
    client_jar: &Path,
    features: &HashMap<String, bool>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for lib in &version.libraries {
        if !lib.is_allowed(features) || lib.is_native_only() {
            continue;
        }
        if let Some(d) = library_download(lib, paths) {
            out.push(d.dest);
        }
    }
    // The client jar goes last so loader-patched classes shadow vanilla ones.
    out.push(client_jar.to_path_buf());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    /// A native payload is read against a ceiling, not into whatever the jar
    /// asks for.
    ///
    /// Mojang's own libraries arrive sha1-pinned, so they are not the worry.
    /// A loader manifest lists its libraries as a coordinate plus a maven base
    /// with no digest at all ([`library_download`]), so what lands in the
    /// shared `libraries/` tree for one of those is whatever that host served
    /// — and `read_to_end` then allocated however much it inflated to.
    ///
    /// The honest payload beside it is the other half: a ceiling that refused
    /// a real native would break every launch.
    #[test]
    fn a_native_payload_larger_than_any_real_one_is_refused() {
        use std::io::Write;

        let scratch = Scratch::new("natives");
        std::fs::create_dir_all(&scratch.paths.root).unwrap();
        let jar = scratch.paths.root.join("lwjgl-natives-linux.jar");
        {
            let file = std::fs::File::create(&jar).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("libhonest.so", options).unwrap();
            zip.write_all(b"ELF, more or less").unwrap();
            zip.start_file("libbomb.so", options).unwrap();
            zip.write_all(&vec![0u8; MAX_NATIVE_BYTES as usize + 1]).unwrap();
            zip.finish().unwrap();
        }
        let on_disk = std::fs::metadata(&jar).unwrap().len();
        assert!(
            on_disk < MAX_NATIVE_BYTES / 100,
            "the control is wrong: the jar is {on_disk} bytes, so a check on the file's \
             own size would have caught this and it proves nothing about inflation"
        );

        let dest = scratch.paths.natives("1.21.1").expect("a real version id");
        std::fs::create_dir_all(&dest).unwrap();
        let err = extract_native_jar(&jar, &dest, &[])
            .expect_err("a native larger than any real one is refused");
        assert!(
            err.to_string().contains("more than any native library is"),
            "refused for some other reason: {err}"
        );
        assert!(
            !dest.join("libbomb.so").exists(),
            "the oversized payload was written anyway"
        );
        // The honest entry before it still landed, and is intact.
        assert_eq!(
            std::fs::read(dest.join("libhonest.so")).unwrap(),
            b"ELF, more or less"
        );
    }
}

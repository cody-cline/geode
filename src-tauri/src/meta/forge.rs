//! NeoForge and Forge (1.13+).
//!
//! Unlike Fabric, these do not publish a ready-to-use launch profile. Their
//! installer jar carries an `install_profile.json` describing a chain of
//! *processors* — small Java programs that patch and remap the vanilla client
//! jar on the user's machine. We reimplement the client-side half of the
//! official installer: extract, fetch the processor libraries, run the chain.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::meta::piston::{Library, VersionJson};
use crate::net::{sha1_file, Download, Progress};
use crate::paths::{maven_relative_path, Paths};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    NeoForge,
    Forge,
}

const NEO_VERSIONS: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
const NEO_MAVEN: &str = "https://maven.neoforged.net/releases";
const FORGE_METADATA: &str =
    "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml";
const FORGE_MAVEN: &str = "https://maven.minecraftforge.net";

#[derive(Debug, Deserialize)]
struct NeoVersions {
    versions: Vec<String>,
}

/// Loader builds for a Minecraft version, newest first.
pub async fn versions(
    client: &reqwest::Client,
    flavor: Flavor,
    mc_version: &str,
) -> Result<Vec<String>> {
    match flavor {
        Flavor::NeoForge => {
            let all: NeoVersions = crate::net::get_json(client, NEO_VERSIONS).await?;
            let prefix = neo_prefix(mc_version);
            let mut matched: Vec<String> = all
                .versions
                .into_iter()
                .filter(|v| v.starts_with(&prefix))
                .collect();
            matched.reverse();
            Ok(matched)
        }
        Flavor::Forge => {
            let xml = client.get(FORGE_METADATA).send().await?.text().await?;
            let prefix = format!("{mc_version}-");
            let mut matched: Vec<String> = xml
                .split("<version>")
                .skip(1)
                .filter_map(|chunk| chunk.split("</version>").next())
                .filter(|v| v.starts_with(&prefix))
                .map(|s| s.to_string())
                .collect();
            matched.reverse();
            Ok(matched)
        }
    }
}

/// NeoForge drops the leading `1.` and encodes the patch as its minor:
/// Minecraft `1.21.4` -> NeoForge `21.4.x`, and `1.21` -> `21.0.x`.
fn neo_prefix(mc_version: &str) -> String {
    let rest = mc_version.strip_prefix("1.").unwrap_or(mc_version);
    let mut it = rest.split('.');
    let major = it.next().unwrap_or("0");
    let minor = it.next().unwrap_or("0");
    format!("{major}.{minor}.")
}

fn installer_url(flavor: Flavor, mc_version: &str, loader: &str) -> String {
    match flavor {
        Flavor::NeoForge => {
            format!("{NEO_MAVEN}/net/neoforged/neoforge/{loader}/neoforge-{loader}-installer.jar")
        }
        // Forge loader ids already include the game version (`1.20.1-47.2.0`),
        // but older releases append the game version a second time.
        Flavor::Forge => {
            let full = if loader.starts_with(mc_version) {
                loader.to_string()
            } else {
                format!("{mc_version}-{loader}")
            };
            format!("{FORGE_MAVEN}/net/minecraftforge/forge/{full}/forge-{full}-installer.jar")
        }
    }
}

pub fn version_id(flavor: Flavor, mc_version: &str, loader: &str) -> String {
    match flavor {
        Flavor::NeoForge => format!("neoforge-{loader}"),
        Flavor::Forge => {
            if loader.starts_with(mc_version) {
                format!("{loader}-forge")
            } else {
                format!("{mc_version}-forge-{loader}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// install_profile.json
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallProfile {
    #[serde(default)]
    data: HashMap<String, SidedValue>,
    #[serde(default)]
    processors: Vec<Processor>,
    #[serde(default)]
    libraries: Vec<Library>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SidedValue {
    client: String,
    #[allow(dead_code)]
    #[serde(default)]
    server: String,
}

#[derive(Debug, Deserialize)]
struct Processor {
    /// Absent means "run on every side".
    #[serde(default)]
    sides: Option<Vec<String>>,
    jar: String,
    #[serde(default)]
    classpath: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    outputs: HashMap<String, String>,
}

/// Download the installer, run its client processors, and return the loader's
/// version JSON (still carrying `inheritsFrom`).
#[allow(clippy::too_many_arguments)] // the installer genuinely needs all of it
pub async fn install(
    client: &reqwest::Client,
    paths: &Paths,
    flavor: Flavor,
    mc_version: &str,
    loader: &str,
    vanilla_jar: &Path,
    java_exe: &Path,
    progress: &Progress,
) -> Result<VersionJson> {
    progress.stage("Fetching installer", 1).await;

    let url = installer_url(flavor, mc_version, loader);
    let installer_path = paths
        .cache()
        .join("installers")
        .join(format!("{}-{loader}.jar", flavor_slug(flavor)));
    crate::net::fetch_one(client, &Download::new(url, installer_path.clone())).await?;
    progress.advance(1, "installer").await;

    // Unpack in a blocking task: `zip` is synchronous and these archives run to
    // tens of megabytes.
    let installer_for_read = installer_path.clone();
    let libraries_dir = paths.libraries();
    let work_dir = paths.cache().join("forge-work").join(loader);
    let work_for_extract = work_dir.clone();

    let unpacked = tokio::task::spawn_blocking(move || {
        unpack_installer(&installer_for_read, &libraries_dir, &work_for_extract)
    })
    .await
    .map_err(|e| Error::msg(format!("installer unpack panicked: {e}")))??;

    let profile: InstallProfile = serde_json::from_slice(&unpacked.install_profile)
        .map_err(|e| Error::json("install_profile.json", e))?;
    let version: VersionJson = serde_json::from_slice(&unpacked.version_json)
        .map_err(|e| Error::json("version.json", e))?;
    let extracted = unpacked.extracted;

    // Processor libraries live in the same shared store as game libraries.
    progress
        .stage(
            "Downloading loader libraries",
            profile.libraries.len() as u64,
        )
        .await;
    let mut downloads = Vec::new();
    for lib in &profile.libraries {
        if let Some(d) = library_download(lib, paths) {
            downloads.push(d);
        }
    }
    crate::net::fetch_all(client, downloads, progress, 8).await?;

    if profile.processors.is_empty() {
        return Ok(version);
    }

    // Build the substitution table the processor args are templated against.
    let mut tokens: HashMap<String, String> = HashMap::new();
    for (key, value) in &profile.data {
        tokens.insert(
            key.clone(),
            resolve_data_value(&value.client, paths, &extracted),
        );
    }
    tokens.insert("SIDE".into(), "client".into());
    tokens.insert("MINECRAFT_JAR".into(), path_str(vanilla_jar));
    tokens.insert("ROOT".into(), path_str(&work_dir));
    tokens.insert("INSTALLER".into(), path_str(&installer_path));
    tokens.insert("LIBRARY_DIR".into(), path_str(&paths.libraries()));

    let client_processors: Vec<&Processor> = profile
        .processors
        .iter()
        .filter(|p| match &p.sides {
            Some(sides) => sides.iter().any(|s| s == "client"),
            None => true,
        })
        .collect();

    progress
        .stage("Patching Minecraft", client_processors.len() as u64)
        .await;

    for (i, proc) in client_processors.iter().enumerate() {
        progress.check_cancelled()?;
        if outputs_valid(&proc.outputs, &tokens, paths).await {
            progress.advance(1, "already patched").await;
            continue;
        }
        run_processor(proc, &tokens, paths, java_exe).await?;
        progress
            .advance(1, &format!("step {}/{}", i + 1, client_processors.len()))
            .await;
    }

    // `path` names the loader jar itself, which some installers only ship
    // inside the installer's embedded maven tree (already extracted above).
    if let Some(p) = &profile.path {
        let jar = paths.maven(p);
        if !jar.exists() {
            return Err(Error::msg(format!(
                "loader jar {p} missing after install — the installer may have changed format"
            )));
        }
    }

    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    Ok(version)
}

fn flavor_slug(f: Flavor) -> &'static str {
    match f {
        Flavor::NeoForge => "neoforge",
        Flavor::Forge => "forge",
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

/// Pull `install_profile.json`, `version.json`, the `data/` payloads, and the
/// embedded `maven/` tree out of the installer jar.
/// What we pull out of an installer jar.
struct UnpackedInstaller {
    install_profile: Vec<u8>,
    version_json: Vec<u8>,
    /// `/data/...` payload paths as referenced by the profile, mapped to where
    /// they landed on disk.
    extracted: HashMap<String, PathBuf>,
}

fn unpack_installer(
    installer: &Path,
    libraries_dir: &Path,
    work_dir: &Path,
) -> Result<UnpackedInstaller> {
    let file = std::fs::File::open(installer)?;
    let mut zip = zip::ZipArchive::new(file)?;

    let mut profile_raw = Vec::new();
    let mut version_raw = Vec::new();
    let mut extracted = HashMap::new();

    std::fs::create_dir_all(work_dir)?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();

        match name.as_str() {
            "install_profile.json" => {
                entry.read_to_end(&mut profile_raw)?;
                continue;
            }
            "version.json" => {
                entry.read_to_end(&mut version_raw)?;
                continue;
            }
            _ => {}
        }

        // `data/` holds binary patches referenced as `/data/client.lzma`.
        if name.starts_with("data/") {
            let rel = Path::new(&name);
            if !crate::paths::is_safe_relative(rel) {
                continue;
            }
            let dest = work_dir.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&dest)?;
            std::io::copy(&mut entry, &mut out)?;
            extracted.insert(format!("/{name}"), dest);
            continue;
        }

        // `maven/` is a pre-populated library tree — mostly the loader's own
        // universal jar, which is published nowhere else.
        if let Some(rel) = name.strip_prefix("maven/") {
            let rel_path = Path::new(rel);
            if !crate::paths::is_safe_relative(rel_path) {
                continue;
            }
            let dest = libraries_dir.join(rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&dest)?;
            std::io::copy(&mut entry, &mut out)?;
        }
    }

    if profile_raw.is_empty() {
        return Err(Error::msg(
            "installer has no install_profile.json (Forge below 1.13 is not supported)",
        ));
    }
    if version_raw.is_empty() {
        return Err(Error::msg("installer has no version.json"));
    }
    Ok(UnpackedInstaller {
        install_profile: profile_raw,
        version_json: version_raw,
        extracted,
    })
}

/// A `data` entry is one of: a maven coordinate in brackets, a path into the
/// installer jar, or a literal string.
fn resolve_data_value(raw: &str, paths: &Paths, extracted: &HashMap<String, PathBuf>) -> String {
    if let Some(coord) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return path_str(&paths.maven(coord));
    }
    if raw.starts_with('/') {
        if let Some(p) = extracted.get(raw) {
            return path_str(p);
        }
    }
    raw.to_string()
}

/// Expand `{TOKEN}` and `[maven:coord]` in a processor argument.
fn expand_arg(raw: &str, tokens: &HashMap<String, String>, paths: &Paths) -> String {
    if let Some(coord) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return path_str(&paths.maven(coord));
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &rest[start + 1..start + end];
        match tokens.get(key) {
            Some(v) => out.push_str(v),
            // Unknown token: leave it verbatim so the failure surfaces in the
            // processor's own error rather than as a silent empty string.
            None => out.push_str(&rest[start..start + end + 1]),
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

async fn outputs_valid(
    outputs: &HashMap<String, String>,
    tokens: &HashMap<String, String>,
    paths: &Paths,
) -> bool {
    if outputs.is_empty() {
        return false;
    }
    for (path_tpl, sha_tpl) in outputs {
        let path = PathBuf::from(expand_arg(path_tpl, tokens, paths));
        let expected = expand_arg(sha_tpl, tokens, paths);
        if expected.is_empty() {
            return false;
        }
        match sha1_file(&path).await {
            Ok(actual) if actual.eq_ignore_ascii_case(&expected) => {}
            _ => return false,
        }
    }
    true
}

async fn run_processor(
    proc: &Processor,
    tokens: &HashMap<String, String>,
    paths: &Paths,
    java_exe: &Path,
) -> Result<()> {
    let jar_path = paths.maven(&proc.jar);
    let main_class = read_main_class(&jar_path).await?;

    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut cp: Vec<String> = proc
        .classpath
        .iter()
        .map(|c| path_str(&paths.maven(c)))
        .collect();
    cp.push(path_str(&jar_path));

    let args: Vec<String> = proc
        .args
        .iter()
        .map(|a| expand_arg(a, tokens, paths))
        .collect();

    let output = tokio::process::Command::new(java_exe)
        .arg("-cp")
        .arg(cp.join(sep))
        .arg(&main_class)
        .args(&args)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        let tail: String = detail.lines().rev().take(12).collect::<Vec<_>>().join("\n");
        return Err(Error::msg(format!(
            "loader install step `{main_class}` failed:\n{tail}"
        )));
    }
    Ok(())
}

async fn read_main_class(jar: &Path) -> Result<String> {
    let jar = jar.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<String> {
        let file = std::fs::File::open(&jar)
            .map_err(|e| Error::msg(format!("processor jar {}: {e}", jar.display())))?;
        let mut zip = zip::ZipArchive::new(file)?;
        let mut manifest = String::new();
        zip.by_name("META-INF/MANIFEST.MF")?
            .read_to_string(&mut manifest)?;
        manifest
            .lines()
            .find_map(|l| l.strip_prefix("Main-Class:"))
            .map(|s| s.trim().to_string())
            .ok_or_else(|| Error::msg(format!("{} has no Main-Class", jar.display())))
    })
    .await
    .map_err(|e| Error::msg(format!("reading manifest panicked: {e}")))?
}

/// Build a download for a library entry, preferring the explicit artifact URL
/// and falling back to the manifest's maven base.
fn library_download(lib: &Library, paths: &Paths) -> Option<Download> {
    if let Some(dl) = &lib.downloads {
        if let Some(art) = &dl.artifact {
            let rel = art
                .path
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| maven_relative_path(&lib.name));
            if art.url.is_empty() {
                // Supplied by the installer's embedded maven tree.
                return None;
            }
            return Some(
                Download::new(art.url.clone(), paths.libraries().join(rel))
                    .sha1(art.sha1.clone())
                    .size(art.size)
                    .label(lib.name.clone()),
            );
        }
    }
    let base = lib.url.clone()?;
    let rel = maven_relative_path(&lib.name);
    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        rel.to_string_lossy().replace('\\', "/")
    );
    Some(Download::new(url, paths.libraries().join(rel)).label(lib.name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neo_prefix_from_patch_version() {
        assert_eq!(neo_prefix("1.21.4"), "21.4.");
        assert_eq!(neo_prefix("1.20.2"), "20.2.");
    }

    #[test]
    fn neo_prefix_from_minor_version() {
        assert_eq!(neo_prefix("1.21"), "21.0.");
    }

    #[test]
    fn forge_version_id_avoids_double_game_version() {
        assert_eq!(
            version_id(Flavor::Forge, "1.20.1", "1.20.1-47.2.0"),
            "1.20.1-47.2.0-forge"
        );
        assert_eq!(
            version_id(Flavor::Forge, "1.20.1", "47.2.0"),
            "1.20.1-forge-47.2.0"
        );
    }

    #[test]
    fn expands_tokens() {
        let paths = Paths {
            root: PathBuf::from("/root"),
        };
        let mut tokens = HashMap::new();
        tokens.insert("SIDE".to_string(), "client".to_string());
        assert_eq!(
            expand_arg("--side={SIDE}", &tokens, &paths),
            "--side=client"
        );
    }

    #[test]
    fn leaves_unknown_tokens_alone() {
        let paths = Paths {
            root: PathBuf::from("/root"),
        };
        let tokens = HashMap::new();
        assert_eq!(expand_arg("{NOPE}", &tokens, &paths), "{NOPE}");
    }

    #[test]
    fn expands_maven_brackets() {
        let paths = Paths {
            root: PathBuf::from("/root"),
        };
        let tokens = HashMap::new();
        // Built from `Paths` rather than written out, because the separator is
        // the platform's: a literal with forward slashes passes on Linux and
        // fails on Windows for a reason that has nothing to do with whether a
        // Maven coordinate expands correctly.
        let expected = paths
            .libraries()
            .join("net")
            .join("minecraft")
            .join("client")
            .join("1.20.1")
            .join("client-1.20.1-slim.jar");
        assert_eq!(
            expand_arg("[net.minecraft:client:1.20.1:slim]", &tokens, &paths),
            expected.to_string_lossy()
        );
    }
}

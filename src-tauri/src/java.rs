//! Finding and provisioning Java runtimes.
//!
//! Minecraft is strict about its JVM: 1.16 and below need Java 8, 1.17 needs
//! 16, 1.18–1.20.4 need 17, and 1.20.5+ need 21. Users rarely have the right
//! one, so we detect what is installed and download a matching Temurin JRE when
//! nothing fits.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::net::{Download, Progress};
use crate::paths::Paths;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JavaInstall {
    pub path: PathBuf,
    pub major: u32,
    /// Full version string, e.g. `21.0.5+11`.
    pub version: String,
    pub vendor: String,
    /// True when Geode downloaded it, meaning we may also delete it.
    pub managed: bool,
}

fn java_binary_name() -> &'static str {
    if cfg!(windows) {
        "javaw.exe"
    } else {
        "java"
    }
}

/// Directories that commonly hold JVMs on each platform.
fn search_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    if cfg!(target_os = "linux") {
        roots.extend(
            [
                "/usr/lib/jvm",
                "/usr/lib64/jvm",
                "/usr/java",
                "/opt/java",
                "/opt/jdk",
                "/var/lib/flatpak/runtime",
            ]
            .iter()
            .map(PathBuf::from),
        );
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".sdkman/candidates/java"));
            roots.push(home.join(".jdks"));
            roots.push(home.join(".gradle/jdks"));
        }
    } else if cfg!(target_os = "macos") {
        roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join("Library/Java/JavaVirtualMachines"));
        }
    } else {
        for base in [
            "C:\\Program Files\\Java",
            "C:\\Program Files\\Eclipse Adoptium",
        ] {
            roots.push(PathBuf::from(base));
        }
    }
    roots
}

/// Candidate `java` executables: the search roots (one level deep, plus the
/// macOS `Contents/Home` layout), `JAVA_HOME`, and whatever is on `PATH`.
fn candidate_binaries(paths: &Paths) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let bin = java_binary_name();

    let mut roots = search_roots();
    roots.push(paths.java());

    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            for rel in ["bin", "jre/bin", "Contents/Home/bin"] {
                let candidate = dir.join(rel).join(bin);
                if candidate.is_file() {
                    out.push(candidate);
                }
            }
        }
    }

    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let candidate = PathBuf::from(home).join("bin").join(bin);
        if candidate.is_file() {
            out.push(candidate);
        }
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(bin);
            if candidate.is_file() {
                out.push(candidate);
            }
        }
    }
    out
}

/// Ask a JVM what it is. `java -version` writes to stderr, and the first line
/// looks like `openjdk version "21.0.5" 2024-10-15`.
async fn probe(binary: &Path) -> Option<JavaInstall> {
    let out = tokio::process::Command::new(binary)
        .arg("-version")
        .output()
        .await
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );

    let first = text.lines().next()?;
    let version = first.split('"').nth(1)?.to_string();
    let major = parse_major(&version)?;

    let vendor = text
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("Unknown")
        .to_string();

    Some(JavaInstall {
        path: binary.to_path_buf(),
        major,
        version,
        vendor,
        managed: false,
    })
}

/// `1.8.0_412` -> 8, `21.0.5` -> 21.
pub fn parse_major(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '_', '-', '+']);
    let first = parts.next()?;
    if first == "1" {
        parts.next()?.parse().ok()
    } else {
        first.parse().ok()
    }
}

/// Every distinct JVM we can find, sorted newest-major first.
pub async fn detect(paths: &Paths) -> Vec<JavaInstall> {
    let managed_root = paths.java();
    let mut seen = HashSet::new();
    let mut found = Vec::new();

    for binary in candidate_binaries(paths) {
        // Resolve symlinks so `/usr/bin/java` and the JVM it points at do not
        // both show up in the picker.
        let canonical = std::fs::canonicalize(&binary).unwrap_or_else(|_| binary.clone());
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if let Some(mut install) = probe(&canonical).await {
            install.managed = canonical.starts_with(&managed_root);
            found.push(install);
        }
    }

    found.sort_by(|a, b| b.major.cmp(&a.major).then(a.path.cmp(&b.path)));
    found
}

/// The Java major version a given Minecraft version needs. The version JSON
/// states this directly for 1.17+; older versions predate the field and want 8.
pub fn required_major(version: &crate::meta::piston::VersionJson) -> u32 {
    version
        .java_version
        .as_ref()
        .map(|j| j.major_version)
        .unwrap_or(8)
}

// ---------------------------------------------------------------------------
// Provisioning
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AdoptiumAsset {
    binary: AdoptiumBinary,
    #[serde(default)]
    release_name: String,
}

#[derive(Debug, Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
}

#[derive(Debug, Deserialize)]
struct AdoptiumPackage {
    link: String,
    #[serde(default)]
    size: Option<u64>,
}

fn adoptium_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

fn adoptium_arch() -> Result<&'static str> {
    Ok(match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        "arm" => "arm",
        "x86" => "x86",
        other => {
            return Err(Error::msg(format!(
                "no prebuilt Java runtime for this CPU architecture ({other})"
            )))
        }
    })
}

/// Download and unpack a Temurin JRE for the requested major version.
pub async fn provision(
    client: &reqwest::Client,
    paths: &Paths,
    major: u32,
    progress: &Progress,
) -> Result<JavaInstall> {
    provision_image(client, paths, major, "jre", progress).await
}

/// Download and unpack a Temurin runtime, choosing Adoptium's image type.
///
/// `jre` runs things and is all a Minecraft server needs. `jdk` additionally
/// carries a compiler, which SpigotMC's BuildTools requires because it builds
/// Spigot from source rather than downloading it.
pub async fn provision_image(
    client: &reqwest::Client,
    paths: &Paths,
    major: u32,
    image: &str,
    progress: &Progress,
) -> Result<JavaInstall> {
    progress.stage(&format!("Finding Java {major}"), 1).await;

    let url = format!(
        "https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture={}&image_type={image}&os={}&vendor=eclipse",
        adoptium_arch()?,
        adoptium_os()
    );
    let assets: Vec<AdoptiumAsset> = crate::net::get_json(client, &url).await?;
    let asset = assets.into_iter().next().ok_or_else(|| {
        Error::msg(format!(
            "Adoptium has no Java {major} build for this system"
        ))
    })?;

    // A JDK is kept apart from the JRE of the same version: they are different
    // downloads, and letting one overwrite the other would mean whichever was
    // fetched last decides whether compiling works.
    let dest_dir = paths.java().join(if image == "jdk" {
        format!("temurin-{major}-jdk")
    } else {
        format!("temurin-{major}")
    });
    // A previous half-finished attempt would otherwise shadow the new one.
    let _ = tokio::fs::remove_dir_all(&dest_dir).await;
    tokio::fs::create_dir_all(&dest_dir).await?;

    let is_zip = asset.binary.package.link.ends_with(".zip");
    let archive = paths.cache().join(if is_zip {
        format!("{image}.zip")
    } else {
        format!("{image}.tar.gz")
    });

    progress
        .stage(&format!("Downloading Java {major}"), 1)
        .await;
    let download = Download::new(asset.binary.package.link.clone(), archive.clone())
        // Adoptium's `checksum` is SHA-256; our downloader verifies SHA-1, so
        // we check size only and rely on TLS for integrity.
        .size(asset.binary.package.size)
        .label(format!("Temurin {major}"));
    crate::net::fetch_one(client, &download).await?;
    progress.advance(1, "downloaded").await;

    progress.stage("Unpacking Java", 1).await;
    let dest_for_unpack = dest_dir.clone();
    let archive_for_unpack = archive.clone();
    tokio::task::spawn_blocking(move || {
        if is_zip {
            unpack_zip(&archive_for_unpack, &dest_for_unpack)
        } else {
            unpack_targz(&archive_for_unpack, &dest_for_unpack)
        }
    })
    .await
    .map_err(|e| Error::msg(format!("unpacking Java panicked: {e}")))??;
    let _ = tokio::fs::remove_file(&archive).await;
    progress.advance(1, "unpacked").await;

    let binary = find_java_binary(&dest_dir)
        .ok_or_else(|| Error::msg("downloaded Java archive contained no java binary"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&binary)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary, perms)?;
    }

    let mut install = probe(&binary).await.unwrap_or(JavaInstall {
        path: binary.clone(),
        major,
        version: asset.release_name.clone(),
        vendor: "Eclipse Temurin".into(),
        managed: true,
    });
    install.managed = true;
    Ok(install)
}

/// Archives nest everything under a single top-level directory whose name we
/// do not know ahead of time, so walk for the binary instead of guessing.
fn find_java_binary(root: &Path) -> Option<PathBuf> {
    let bin = java_binary_name();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(bin)
                && path.parent().and_then(|p| p.file_name()) == Some(std::ffi::OsStr::new("bin"))
            {
                return Some(path);
            }
        }
    }
    None
}

fn unpack_targz(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.set_preserve_permissions(true);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !crate::paths::is_safe_relative(&path) {
            continue;
        }
        entry.unpack(dest.join(&path))?;
    }
    Ok(())
}

fn unpack_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut writer = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut writer)?;
    }
    Ok(())
}

/// Pick a runtime for `major`: an explicit override wins, then any detected
/// install with a matching major, then a fresh download.
pub async fn ensure(
    client: &reqwest::Client,
    paths: &Paths,
    major: u32,
    override_path: Option<&Path>,
    progress: &Progress,
) -> Result<PathBuf> {
    if let Some(explicit) = override_path {
        if explicit.is_file() {
            return Ok(explicit.to_path_buf());
        }
        return Err(Error::msg(format!(
            "configured Java path does not exist: {}",
            explicit.display()
        )));
    }

    if let Some(found) = detect(paths).await.into_iter().find(|j| j.major == major) {
        return Ok(found.path);
    }

    let installed = provision(client, paths, major, progress).await?;
    Ok(installed.path)
}

/// Whether this `java` has a compiler beside it, making it a JDK.
fn has_compiler(java: &Path) -> bool {
    let name = if cfg!(windows) { "javac.exe" } else { "javac" };
    java.parent().map(|d| d.join(name).is_file()).unwrap_or(false)
}

/// A JDK of exactly this major version, installed if the machine has none.
///
/// Distinct from [`ensure`] because a JRE cannot compile: BuildTools checks for
/// a compiler and stops. Most systems that have Java at all have a JDK, so the
/// download is a fallback rather than the usual path.
pub async fn ensure_jdk(
    client: &reqwest::Client,
    paths: &Paths,
    major: u32,
    progress: &Progress,
) -> Result<PathBuf> {
    if let Some(found) = detect(paths)
        .await
        .into_iter()
        .find(|j| j.major == major && has_compiler(&j.path))
    {
        return Ok(found.path);
    }
    let installed = provision_image(client, paths, major, "jdk", progress).await?;
    Ok(installed.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_and_modern_versions() {
        assert_eq!(parse_major("1.8.0_412"), Some(8));
        assert_eq!(parse_major("21.0.5"), Some(21));
        assert_eq!(parse_major("17"), Some(17));
        assert_eq!(parse_major("21.0.5+11"), Some(21));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_major("not-a-version"), None);
    }
}

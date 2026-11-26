//! Finding and provisioning Java runtimes.
//!
//! Minecraft is strict about its JVM: 1.16 and below need Java 8, 1.17 needs
//! 16, 1.18–1.20.4 need 17, and 1.20.5+ need 21. Users rarely have the right
//! one, so we detect what is installed and download a matching Temurin JRE when
//! nothing fits.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Context, Error, Result};
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
    /// The archive's SHA-256, which Adoptium publishes beside every binary.
    /// Read because the same response hands us the URL: without it, nothing
    /// about these bytes is checked beyond their length.
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

/// The SHA-256 Adoptium published for a download, or the refusal to install a
/// runtime there is no way to check.
///
/// A missing checksum refuses rather than falling back to the size check. What
/// this guards against is a tampered answer from Adoptium — a proxy, a CDN, a
/// CA — and that same answer can leave the `checksum` field out as easily as
/// it can swap the bytes, so a fallback would hand the whole check back to
/// whoever wanted it gone. An empty string counts as absent: it is a hash that
/// can never match, and "expected ''" is not something to show anybody. The
/// value itself is passed on exactly as it arrived, because that is what
/// `Download::sha256` compares against.
///
/// A function of its own so that [`tests::a_download_with_no_checksum_is_refused`]
/// can reach the refusal at all: [`provision_image`] only gets this far by
/// asking the real api.adoptium.net, whose address is a hardcoded `format!`,
/// and an offline test has nowhere to point a stub.
fn published_checksum(package: &AdoptiumPackage, major: u32) -> Result<String> {
    package
        .checksum
        .clone()
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| {
            Error::msg(format!(
                "Pick a Java {major} already on this machine under Settings → Java & Memory, or \
                 try again later. Adoptium published no checksum for its Java {major} download, \
                 and Geode will not install a runtime it cannot check against one."
            ))
        })
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

    // Read before anything is removed or downloaded: a refusal must not first
    // delete an install that was working. Why it is a refusal rather than a
    // fallback is at [`published_checksum`], and the ordering is pinned by
    // [`tests::the_checksum_is_read_before_the_destination_is_cleared`].
    let checksum = published_checksum(&asset.binary.package, major)?;

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
        // Adoptium's `checksum` is SHA-256, and `Download::sha256` verifies it
        // as the bytes stream past. The size stays beside it because it is
        // free, not because it proves anything: a hash is what decides whether
        // this archive gets unpacked. It also keeps the shared cache file name
        // honest — every major downloads to the same `jre.tar.gz`, and a
        // length is no way to tell one runtime's archive from another's.
        .sha256(Some(checksum))
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
    // No `set_preserve_permissions(true)`: the rwx bits in an entry's mode are
    // applied either way, so `bin/java` still comes out executable without it.
    // All the flag adds is honouring setuid, setgid and the sticky bit from the
    // archive, and an archive is not something that gets to hand this machine a
    // setuid binary.
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !crate::paths::is_safe_relative(&path) {
            continue;
        }
        // `unpack_in` rather than `unpack`, and the check above is not a
        // substitute for it: `is_safe_relative` reads the entry's own name, and
        // a name can be innocent while the place it lands is not. Temurin
        // tarballs really do carry symlinks, so `jdk/lib` can arrive as a link
        // pointing anywhere on disk and `jdk/lib/authorized_keys` then writes
        // through it — both names pass the guard above and the file lands
        // outside `dest`. `unpack_in` canonicalises each entry's parent and
        // refuses when it leaves `dest`, which is the only check here that can
        // see a symlink at all. The name check stays as the cheaper first pass.
        entry.unpack_in(dest).ctx("unpacking the Java runtime")?;
    }
    Ok(())
}

pub(crate) fn unpack_zip(archive: &Path, dest: &Path) -> Result<()> {
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

    /// One entry of a tarball built for a test. Modes are spelled out because
    /// every claim below is about what mode reaches the disk.
    #[cfg(unix)]
    enum Item<'a> {
        Dir(&'a str),
        /// Path, mode, contents.
        File(&'a str, u32, &'a [u8]),
        /// Path, and what the symlink points at.
        Link(&'a str, &'a str),
    }

    #[cfg(unix)]
    fn tarball(archive: &Path, items: &[Item<'_>]) {
        let out = std::fs::File::create(archive).unwrap();
        let gz = flate2::write::GzEncoder::new(out, flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        for item in items {
            let mut header = tar::Header::new_gnu();
            header.set_mtime(0);
            match *item {
                Item::Dir(path) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_mode(0o755);
                    header.set_size(0);
                    tar.append_data(&mut header, path, &[][..]).unwrap();
                }
                Item::File(path, mode, body) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_mode(mode);
                    header.set_size(body.len() as u64);
                    tar.append_data(&mut header, path, body).unwrap();
                }
                Item::Link(path, target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_mode(0o777);
                    header.set_size(0);
                    tar.append_link(&mut header, path, target).unwrap();
                }
            }
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// The whole reason `unpack_in` is used rather than `unpack`.
    ///
    /// Neither entry name here is suspect — `is_safe_relative` sees no `..` and
    /// no root in either — so the name check passes both and cannot be what
    /// stops this. What stops it is the destination check inside `unpack_in`,
    /// and with plain `unpack` the file really did land two directories above
    /// `dest`: this test was written against that code and watched it happen.
    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_carry_a_file_out_of_the_destination() {
        let scratch = crate::paths::Scratch::new("java-escape");
        let root = scratch.paths.root.clone();
        let dest = root.join("dest");
        let outside = root.join("outside");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let archive = root.join("temurin.tar.gz");
        tarball(
            &archive,
            &[
                Item::Dir("jdk/"),
                Item::Link("jdk/lib", "../../outside"),
                Item::File("jdk/lib/authorized_keys", 0o644, b"pwned"),
            ],
        );

        let refused = unpack_targz(&archive, &dest).expect_err("the archive escaped");
        assert!(
            refused.to_string().contains("unpacking the Java runtime"),
            "{refused}"
        );
        let landed = outside.join("authorized_keys");
        assert!(
            !landed.exists(),
            "{} was written outside the destination",
            landed.display()
        );
    }

    /// The same escape aimed with an absolute path instead of `..`, since that
    /// is the other way to write a link target and the name check sees neither.
    #[cfg(unix)]
    #[test]
    fn an_absolute_symlink_target_cannot_either() {
        let scratch = crate::paths::Scratch::new("java-escape-abs");
        let root = scratch.paths.root.clone();
        let dest = root.join("dest");
        let outside = root.join("outside");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let archive = root.join("temurin.tar.gz");
        tarball(
            &archive,
            &[
                Item::Dir("jdk/"),
                Item::Link("jdk/lib", outside.to_str().unwrap()),
                Item::File("jdk/lib/authorized_keys", 0o644, b"pwned"),
            ],
        );

        assert!(unpack_targz(&archive, &dest).is_err());
        assert!(!outside.join("authorized_keys").exists());
    }

    /// The containment check refuses an escape, not symlinks — real Temurin
    /// tarballs are full of them, and a fix that turned every one of them into
    /// a failed install would break every install there is. `bin/java` also has
    /// to come out executable, which is what the archive's own mode is for.
    #[cfg(unix)]
    #[test]
    fn a_tarball_shaped_like_a_real_jdk_still_unpacks() {
        let scratch = crate::paths::Scratch::new("java-normal");
        let root = scratch.paths.root.clone();
        let dest = root.join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let archive = root.join("temurin.tar.gz");
        tarball(
            &archive,
            &[
                Item::Dir("jdk-21.0.5+11-jre/"),
                Item::Dir("jdk-21.0.5+11-jre/bin/"),
                Item::File("jdk-21.0.5+11-jre/bin/java", 0o755, b"#!/bin/false\n"),
                Item::Dir("jdk-21.0.5+11-jre/lib/"),
                Item::File("jdk-21.0.5+11-jre/lib/libjvm.so", 0o644, b"elf"),
                // A link pointing back inside the archive, and a file written
                // through it. Both are ordinary and both must still work.
                Item::Link("jdk-21.0.5+11-jre/lib/server", "../lib"),
                Item::File("jdk-21.0.5+11-jre/lib/server/classlist", 0o644, b"java/lang/Object"),
            ],
        );

        unpack_targz(&archive, &dest).unwrap();

        let java = find_java_binary(&dest).expect("no java binary came out");
        assert_eq!(java, dest.join("jdk-21.0.5+11-jre/bin/java"));
        assert_eq!(mode_of(&java), 0o755, "bin/java came out non-executable");
        assert_eq!(
            std::fs::read_to_string(dest.join("jdk-21.0.5+11-jre/lib/classlist")).unwrap(),
            "java/lang/Object",
            "a file written through an inward symlink did not arrive"
        );
    }

    /// Why `set_preserve_permissions(true)` is gone. It was never what made
    /// `bin/java` executable — the test above pins that the rwx bits arrive
    /// without it — and the one thing it did add was letting an archive set
    /// setuid on a binary this machine is about to run.
    #[cfg(unix)]
    #[test]
    fn a_setuid_bit_in_the_archive_does_not_reach_the_disk() {
        let scratch = crate::paths::Scratch::new("java-setuid");
        let root = scratch.paths.root.clone();
        let dest = root.join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let archive = root.join("temurin.tar.gz");
        tarball(
            &archive,
            &[
                Item::Dir("jdk/"),
                Item::Dir("jdk/bin/"),
                Item::File("jdk/bin/java", 0o4755, b"#!/bin/false\n"),
            ],
        );

        unpack_targz(&archive, &dest).unwrap();

        let java = dest.join("jdk/bin/java");
        assert_eq!(mode_of(&java), 0o755, "the setuid bit survived extraction");
    }

    /// A JSON package, the shape Adoptium's answer carries one in.
    fn package(json: &str) -> AdoptiumPackage {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"))
    }

    const LINK: &str = r#""link": "https://example.invalid/OpenJDK21.tar.gz", "size": 1234"#;

    /// A download Adoptium published no checksum for is refused, and not
    /// quietly downgraded to the size check — which was the whole point of
    /// reading the field, and had nothing exercising it.
    ///
    /// Four shapes of absent, because this is deserialised from somebody
    /// else's JSON and all four really describe the same answer: the field
    /// gone, `null`, empty, and whitespace. A fallback would make a tampered
    /// answer able to turn the check off by dropping a key, which is cheaper
    /// than swapping the bytes.
    #[test]
    fn a_download_with_no_checksum_is_refused() {
        for json in [
            format!("{{{LINK}}}"),
            format!(r#"{{{LINK}, "checksum": null}}"#),
            format!(r#"{{{LINK}, "checksum": ""}}"#),
            format!(r#"{{{LINK}, "checksum": " \t "}}"#),
        ] {
            let refused = published_checksum(&package(&json), 21)
                .expect_err(&format!("a runtime nothing can check was accepted: {json}"));
            let said = refused.to_string();
            assert!(
                said.contains("published no checksum"),
                "the refusal should say what is missing: {said}"
            );
            // And it is not a dead end: a Java already on the machine can be
            // pointed at, which is the only thing a person can do about this.
            assert!(
                said.contains("Settings → Java & Memory"),
                "the refusal should say what to do instead: {said}"
            );
        }

        // And a published one is handed on exactly as it arrived, because
        // `Download::sha256` compares against it byte for byte.
        let real = "a07e4e2f43d9e2b1f4d5c6b7a8998a0b1c2d3e4f50617283940a5b6c7d8e9f01";
        assert_eq!(
            published_checksum(&package(&format!(r#"{{{LINK}, "checksum": "{real}"}}"#)), 21)
                .expect("a published checksum is what this is for"),
            real
        );
    }

    /// The ordering the comment at that refusal's call site claims: it is read
    /// before the destination is cleared, so a Java that cannot be checked
    /// never first deletes the install that was working.
    ///
    /// Read off this file's own text, which is a strange test and the only one
    /// available. Reaching that refusal inside [`provision_image`] means the
    /// real api.adoptium.net has already answered, its address is a hardcoded
    /// `format!`, and nothing offline can hand it an answer with the field left
    /// out. The cheaper test is the vacuous one: `provision_image` called with
    /// no network fails at `get_json`, before both the refusal and the
    /// `remove_dir_all`, so a file left in the destination survives whichever
    /// order those two are in and proves neither.
    #[test]
    fn the_checksum_is_read_before_the_destination_is_cleared() {
        let body = include_str!("java.rs")
            .split_once("pub async fn provision_image(")
            .expect("provision_image is in this file")
            .1;
        let read = body
            .find("published_checksum(")
            .expect("the checksum is read in provision_image");
        let cleared = body
            .find("remove_dir_all(&dest_dir)")
            .expect("the destination is cleared in provision_image");
        assert!(
            read < cleared,
            "the destination is cleared before the checksum is read, so a \
             refusal now deletes a working install on its way out"
        );
    }

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

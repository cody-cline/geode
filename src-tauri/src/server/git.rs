//! Finding Git, and getting it where it is missing.
//!
//! Only Spigot needs this: it is not published as a jar, it is compiled from
//! source by SpigotMC's BuildTools, and BuildTools drives a real `git` binary.
//! Every other server Geode installs is a download.
//!
//! What Geode can do about a missing Git differs by platform, and not by
//! preference:
//!
//! - **Windows** has an official portable build. The Git for Windows project
//!   publishes MinGit — a plain zip that unpacks anywhere, needs no installer
//!   and touches nothing outside its own folder. Geode fetches it and puts it
//!   on the build's PATH. (BuildTools will fetch a portable Git itself if it
//!   finds none, but that copy is pinned to a version from 2024, arrives as a
//!   self-extracting `.7z.exe`, and its own message calls the feature beta.
//!   Having Git already there means that path is never taken.)
//! - **Linux and macOS** have no equivalent. The Git project publishes no
//!   portable build for either; what exists is one person's static build each,
//!   and quietly fetching an unofficial copy of the tool that then runs hooks
//!   and pulls source is not a thing to do on somebody's behalf. So Geode uses
//!   the route the platform sanctions — Apple's command line tools, or the
//!   distribution's package manager — and asks first, because both install
//!   software system-wide.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::net::{Download, Progress};
use crate::paths::Paths;

/// Newest Git for Windows, whatever that is today. The asset list is read from
/// the release rather than a version being written down here.
const WINDOWS_RELEASE: &str = "https://api.github.com/repos/git-for-windows/git/releases/latest";

/// Where a Geode-fetched Git lives. Beside the Java runtimes Geode fetches,
/// because it is the same kind of thing: a tool the build needs, kept in
/// Geode's own directory rather than installed onto the machine.
fn portable_dir(paths: &Paths) -> PathBuf {
    paths.root.join("tools").join("git")
}

/// The directories a Geode-fetched Git must contribute to PATH.
///
/// Two, not one. `git.exe` lives in `cmd`, but BuildTools also runs `sh`, which
/// MinGit keeps in `usr/bin` — with only the first on PATH the build gets far
/// enough to look like it is working and then fails.
pub fn path_entries(paths: &Paths) -> Vec<PathBuf> {
    let root = portable_dir(paths);
    if root.join("cmd").join("git.exe").is_file() {
        vec![root.join("cmd"), root.join("usr").join("bin")]
    } else {
        Vec::new()
    }
}

/// What Geode knows about Git on this machine, and what it can do about it.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub present: bool,
    /// What `git --version` said, trimmed to the version itself.
    pub version: Option<String>,
    /// True when Geode fetched this copy rather than finding it on the machine.
    pub portable: bool,
    /// One sentence saying what confirming would do. Empty when it is present.
    pub plan: String,
    /// The exact command Geode would run, shown before it runs.
    pub command: Option<String>,
    /// Whether Geode can finish the job itself once confirmed.
    pub automatic: bool,
    /// What to do by hand — always populated, because any of this can fail.
    pub manual: String,
}

/// Where Git is, if it is anywhere.
///
/// The machine's own copy wins over Geode's: someone who installed Git chose a
/// version and configured it, and a private copy shadowing that would be a
/// surprise rather than a service.
pub async fn find(paths: &Paths) -> Option<(PathBuf, String, bool)> {
    if let Some(version) = version_of(Path::new("git")).await {
        return Some((PathBuf::from("git"), version, false));
    }
    let own = portable_dir(paths).join("cmd").join("git.exe");
    if let Some(version) = version_of(&own).await {
        return Some((own, version, true));
    }
    None
}

/// Run `git --version` and keep the version it prints.
async fn version_of(git: &Path) -> Option<String> {
    let out = tokio::process::Command::new(git)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    // "git version 2.51.0" — and on macOS, "(Apple Git-154)" after it.
    Some(
        line.trim()
            .strip_prefix("git version ")
            .unwrap_or(line.trim())
            .to_string(),
    )
}

pub async fn status(paths: &Paths) -> GitStatus {
    if let Some((_, version, portable)) = find(paths).await {
        return GitStatus {
            present: true,
            version: Some(version),
            portable,
            plan: String::new(),
            command: None,
            automatic: false,
            manual: String::new(),
        };
    }

    let (plan, command, automatic, manual) = if cfg!(target_os = "windows") {
        (
            "Geode will download Git for Windows' portable build and keep it in its own \
             folder. Nothing is installed onto Windows and nothing outside Geode changes."
                .to_string(),
            None,
            true,
            "Or install Git for Windows yourself from https://git-scm.com/download/win."
                .to_string(),
        )
    } else if cfg!(target_os = "macos") {
        (
            "Geode will ask macOS for Apple's command line tools, which include Git. \
             macOS runs the install itself and will ask you to agree to it."
                .to_string(),
            Some("xcode-select --install".to_string()),
            true,
            "Or install Git yourself with `brew install git`, or from \
             https://git-scm.com/download/mac."
                .to_string(),
        )
    } else if let Some(install) = package_manager_command().await {
        (
            format!(
                "Geode will run `{install}` as administrator. Your system will ask for \
                 your password before anything is installed."
            ),
            Some(install.clone()),
            true,
            format!("Or run `{install}` yourself in a terminal."),
        )
    } else {
        (
            "Git has to come from your distribution, and Geode did not recognise its \
             package manager."
                .to_string(),
            None,
            false,
            "Install the `git` package however this system installs packages, then \
             check again."
                .to_string(),
        )
    };

    GitStatus {
        present: false,
        version: None,
        portable: false,
        plan,
        command,
        automatic,
        manual,
    }
}

/// The install command for whichever package manager this machine has.
///
/// Ordered by specificity, not popularity: a system with both `apt-get` and
/// `snap` wants `apt-get`, and Fedora keeps a vestigial `yum` that is `dnf` in
/// a hat. Nothing here is run — the answer is shown to the user first.
#[cfg(target_os = "linux")]
const MANAGERS: &[(&str, &str)] = &[
    ("apt-get", "apt-get install -y git"),
    ("dnf", "dnf install -y git"),
    ("pacman", "pacman -S --noconfirm git"),
    ("zypper", "zypper --non-interactive install git"),
    ("apk", "apk add git"),
    ("xbps-install", "xbps-install -y git"),
    ("emerge", "emerge --ask=n dev-vcs/git"),
];

#[cfg(target_os = "linux")]
async fn package_manager_command() -> Option<String> {
    let mut present = Vec::new();
    for (program, _) in MANAGERS {
        if which(program).await.is_some() {
            present.push(*program);
        }
    }
    pick_manager(&present).map(str::to_string)
}

/// The install command for the best of the package managers a machine has.
#[cfg(target_os = "linux")]
fn pick_manager(present: &[&str]) -> Option<&'static str> {
    MANAGERS
        .iter()
        .find(|(program, _)| present.contains(program))
        .map(|(_, command)| *command)
}

#[cfg(not(target_os = "linux"))]
async fn package_manager_command() -> Option<String> {
    None
}

/// Whether a program can be found on PATH, by asking the system to find it.
///
/// Linux-only, like both its callers — [`package_manager_command`] and
/// [`elevated`]. Windows unpacks MinGit and macOS asks for the command line
/// tools, so neither goes looking for a package manager or `pkexec` first,
/// and compiled there this is dead code: a warning, which is a failure.
#[cfg(target_os = "linux")]
async fn which(program: &str) -> Option<PathBuf> {
    let out = tokio::process::Command::new("command")
        .arg("-v")
        .arg(program)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    (!first.is_empty()).then(|| PathBuf::from(first))
}

/// Do whatever [`status`] said confirming would do.
///
/// Only ever reached through a confirmation, because on two of three platforms
/// it installs software system-wide.
pub async fn install(
    client: &reqwest::Client,
    paths: &Paths,
    progress: &Progress,
) -> Result<GitStatus> {
    if find(paths).await.is_some() {
        return Ok(status(paths).await);
    }

    if cfg!(target_os = "windows") {
        fetch_mingit(client, paths, progress).await?;
    } else if cfg!(target_os = "macos") {
        // Returns as soon as macOS has the request; the installer itself is a
        // system dialog the user drives, so there is nothing here to wait on.
        progress.stage("Asking macOS for the command line tools", 0).await;
        run_visible("xcode-select", &["--install"]).await?;
        return Err(Error::msg(
            "macOS is installing its command line tools. Finish that, then try again.",
        ));
    } else {
        let command = package_manager_command()
            .await
            .ok_or_else(|| Error::msg("Geode did not recognise this system's package manager."))?;
        let stage = format!("Installing Git ({command})");
        progress.stage(&stage, 0).await;
        elevated(&command).await?;
    }

    let after = status(paths).await;
    if !after.present {
        return Err(Error::msg(
            "Git still is not there. It may have been cancelled, or it may need a \
             restart to appear on PATH.",
        ));
    }
    Ok(after)
}

/// Run a package install as root, through the graphical prompt.
///
/// `pkexec` is the only way to do this from a desktop app without a terminal:
/// it shows the system's own authentication dialog, naming the program it is
/// about to run as root. Geode never sees the password.
#[cfg(target_os = "linux")]
async fn elevated(command: &str) -> Result<()> {
    if which("pkexec").await.is_none() {
        return Err(Error::msg(format!(
            "Installing Git needs administrator rights, and this system has no graphical \
             way to ask for them. Run `sudo {command}` in a terminal instead."
        )));
    }
    let parts: Vec<&str> = command.split_whitespace().collect();
    run_visible("pkexec", &parts).await
}

#[cfg(not(target_os = "linux"))]
async fn elevated(_command: &str) -> Result<()> {
    Err(Error::msg("not supported on this platform"))
}

/// Run a command and turn a non-zero exit into its own words.
async fn run_visible(program: &str, args: &[&str]) -> Result<()> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| Error::msg(format!("could not run {program}: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.trim();
    Err(Error::msg(if said.is_empty() {
        format!("{program} failed ({})", out.status)
    } else {
        format!("{program} failed: {said}")
    }))
}

/// Unpack the official portable Git into Geode's own folder.
///
/// Public so it can be tested off Windows: everything here up to running the
/// binary is platform-independent, and it is the part that breaks when the
/// release page changes shape.
pub async fn fetch_mingit(
    client: &reqwest::Client,
    paths: &Paths,
    progress: &Progress,
) -> Result<()> {
    progress.stage("Finding the newest portable Git", 1).await;
    let (name, url) = newest_mingit(client).await?;
    progress.advance(1, &name).await;

    let archive = paths.cache().join(&name);
    tokio::fs::create_dir_all(paths.cache()).await?;
    progress.stage("Downloading Git", 1).await;
    crate::net::fetch_one(
        client,
        &Download::new(url, archive.clone()).label(name.clone()),
    )
    .await?;
    progress.advance(1, &name).await;

    progress.stage("Unpacking Git", 1).await;
    let dest = portable_dir(paths);
    // A half-unpacked copy from a failed attempt would satisfy the PATH check
    // without being usable, so the folder is replaced rather than merged into.
    let _ = tokio::fs::remove_dir_all(&dest).await;
    tokio::fs::create_dir_all(&dest).await?;
    let into = dest.clone();
    tokio::task::spawn_blocking(move || crate::java::unpack_zip(&archive, &into))
        .await
        .map_err(|e| Error::msg(format!("unpacking Git panicked: {e}")))??;
    progress.advance(1, "git").await;

    if !dest.join("cmd").join("git.exe").is_file() {
        return Err(Error::msg(
            "the portable Git archive did not contain cmd/git.exe",
        ));
    }
    Ok(())
}

/// The 64-bit MinGit asset on the newest Git for Windows release.
async fn newest_mingit(client: &reqwest::Client) -> Result<(String, String)> {
    #[derive(serde::Deserialize)]
    struct Release {
        assets: Vec<Asset>,
    }
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }

    let release: Release = crate::net::get_json(client, WINDOWS_RELEASE).await?;
    let want = arch_suffix();
    release
        .assets
        .into_iter()
        .find(|a| is_mingit(&a.name, want))
        .map(|a| (a.name, a.browser_download_url))
        .ok_or_else(|| {
            Error::msg(format!(
                "the newest Git for Windows release has no MinGit{want} archive"
            ))
        })
}

/// Which MinGit this machine wants.
///
/// Git for Windows publishes an ARM build, and Windows on ARM would otherwise
/// silently get the x64 one and run it under emulation — slower, and one more
/// thing between a long compile and an explanation when it fails.
fn arch_suffix() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "-arm64.zip"
    } else {
        "-64-bit.zip"
    }
}

/// `MinGit-2.55.0.5-64-bit.zip`, and none of the other seven archives beside it.
///
/// The BusyBox build swaps the usual Unix tools for BusyBox applets, which is
/// most of a shell environment but not the one BuildTools' patch steps expect.
fn is_mingit(name: &str, arch_suffix: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("mingit-") && lower.ends_with(arch_suffix) && !lower.contains("busybox")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    /// Every MinGit-ish name a real release carries, against both architectures
    /// — one asset should match, and it should be a different one each time.
    #[test]
    fn one_asset_per_architecture_and_never_busybox() {
        const ON_THE_RELEASE: &[&str] = &[
            "MinGit-2.55.0.5-32-bit.zip",
            "MinGit-2.55.0.5-64-bit.zip",
            "MinGit-2.55.0.5-arm64.zip",
            "MinGit-2.55.0.5-busybox-32-bit.zip",
            "MinGit-2.55.0.5-busybox-64-bit.zip",
            "MinGit-2.55.0.5-busybox-arm64.zip",
            "PortableGit-2.55.0.5-64-bit.7z.exe",
            "Git-2.55.0.5-64-bit.exe",
        ];
        for (suffix, want) in [
            ("-64-bit.zip", "MinGit-2.55.0.5-64-bit.zip"),
            ("-arm64.zip", "MinGit-2.55.0.5-arm64.zip"),
        ] {
            let matched: Vec<_> = ON_THE_RELEASE
                .iter()
                .filter(|n| is_mingit(n, suffix))
                .collect();
            assert_eq!(matched, vec![&want], "for {suffix}");
        }
    }

    #[test]
    fn a_version_line_keeps_only_the_version() {
        // The parsing is in version_of; this is the shape it has to survive.
        for (line, want) in [
            ("git version 2.51.0", "2.51.0"),
            ("git version 2.39.5 (Apple Git-154)", "2.39.5 (Apple Git-154)"),
        ] {
            let got = line.trim().strip_prefix("git version ").unwrap();
            assert_eq!(got, want);
        }
    }

    /// Several distributions ship more than one of these. The one that owns
    /// the system's packages has to win, and it is not alphabetical: Fedora
    /// keeps a `yum` that is `dnf` in a hat, and Ubuntu images often carry
    /// `apk` for building containers.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_distribution_s_own_package_manager_wins() {
        assert_eq!(pick_manager(&["apk", "apt-get"]), Some("apt-get install -y git"));
        assert_eq!(pick_manager(&["apk", "dnf"]), Some("dnf install -y git"));
        assert_eq!(pick_manager(&["pacman"]), Some("pacman -S --noconfirm git"));
        // Nothing recognised is a real answer, not a fallback to guessing.
        assert_eq!(pick_manager(&["snap", "flatpak"]), None);
        assert_eq!(pick_manager(&[]), None);
    }

    /// This machine's real answer, whatever it is: either Geode knows how to
    /// offer the install, or it says it cannot and points at the manual route.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn this_machine_gets_a_coherent_answer() {
        let found = package_manager_command().await;
        match &found {
            Some(command) => assert!(
                command.contains("git"),
                "{command} does not install git"
            ),
            None => println!("no recognised package manager here"),
        }
    }

    /// Whichever way this machine answers, the answer has to be usable: a
    /// present Git names its version, and a missing one says what to do.
    #[tokio::test]
    async fn the_status_is_always_actionable() {
        let scratch = Scratch::new("git-status");
        let paths = scratch.paths.clone();
        let status = status(&paths).await;
        if status.present {
            let version = status.version.expect("a present Git reports its version");
            assert!(
                version.starts_with(|c: char| c.is_ascii_digit()),
                "{version:?} is not a version"
            );
            assert!(!status.portable, "this is the machine's own Git, not Geode's");
        } else {
            assert!(!status.plan.is_empty(), "a missing Git needs a plan");
            assert!(!status.manual.is_empty(), "and a way to do it by hand");
        }
    }

    #[tokio::test]
    async fn no_portable_git_contributes_no_path() {
        let scratch = Scratch::new("git-path");
        let paths = scratch.paths.clone();
        assert!(path_entries(&paths).is_empty());
    }
}

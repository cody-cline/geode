//! Running a server installer or builder, headless.
//!
//! Several server platforms publish no server jar at all. Forge, NeoForge and
//! Quilt publish an *installer* — a jar you run, which downloads libraries and
//! assembles the server in place. Spigot goes further: its licence does not
//! permit anyone to redistribute a built jar, so SpigotMC publishes BuildTools
//! and every user compiles their own. That is why these entries were greyed
//! out: there was nothing to download and launch.
//!
//! All of them are the same shape from here — fetch a jar, run it with
//! arguments, watch it work, then look at what appeared on disk — so one runner
//! serves them and each caller says what it expects to find afterwards.
//!
//! What an installer leaves behind depends on the era, and is found by looking
//! rather than by reconstructing a path from a version string. Guessing wrong
//! means a server that installs cleanly and then will not start.

use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::{Error, Result};
use crate::net::{Download, Progress};
use crate::paths::Paths;

/// How to start what an installer produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum LaunchKind {
    /// `java -jar <jar> nogui` — vanilla, Paper, Fabric, Spigot, and Forge
    /// before 1.17. The jar name is the server's own `jar` field.
    #[default]
    Jar,
    /// `java @<file> nogui` — modern Forge and NeoForge, which produce no
    /// runnable jar at all. The path is relative to the run directory.
    ArgFile(String),
}

/// How many of the process's own lines to keep for a failure message.
const TAIL: usize = 15;

/// Download a tool and run it, streaming its output into the progress detail.
///
/// `work_dir` is both the working directory and, for every tool here, where the
/// result is written. The output is streamed rather than buffered because these
/// runs are long — a Forge install fetches a full library set, and a Spigot
/// build compiles Minecraft from source for ten minutes or more — and a still
/// progress bar for that long is indistinguishable from a hang.
#[allow(clippy::too_many_arguments)] // one tool invocation's parts, travelling together
pub async fn run_tool(
    client: &reqwest::Client,
    java: &Path,
    url: &str,
    file_name: &str,
    cache_dir: &Path,
    work_dir: &Path,
    args: &[String],
    heap_mb: Option<u32>,
    stage: &str,
    path_prefix: &[std::path::PathBuf],
    progress: &Progress,
) -> Result<()> {
    tokio::fs::create_dir_all(work_dir).await?;
    tokio::fs::create_dir_all(cache_dir).await?;
    let tool = cache_dir.join(file_name);

    progress.stage("Downloading the installer", 1).await;
    crate::net::fetch_one(
        client,
        &Download::new(url.to_string(), tool.clone()).label(file_name.to_string()),
    )
    .await?;
    progress.advance(1, file_name).await;

    // Indeterminate: these tools do not say how much is left, and inventing a
    // total would be inventing progress.
    progress.stage(stage, 0).await;

    let mut command = tokio::process::Command::new(java);
    // Left to itself the JVM sizes the heap from the machine's memory, which is
    // how BuildTools ends up refusing to start on a small one.
    if let Some(mb) = heap_mb {
        command.arg(format!("-Xmx{mb}M"));
    }
    // Prepended, so a Git of Geode's own is found by a tool that shells out to
    // one — and so a Git the machine already has still wins, because the check
    // that produced this list would have found that first and left it empty.
    if !path_prefix.is_empty() {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let combined = std::env::join_paths(
            path_prefix
                .iter()
                .cloned()
                .chain(std::env::split_paths(&existing)),
        )
        .map_err(|e| Error::msg(format!("could not build a PATH for the tool: {e}")))?;
        command.env("PATH", combined);
    }
    let mut child = command
        .arg("-jar")
        .arg(&tool)
        .args(args)
        .current_dir(work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Nothing here asks questions, and a tool that blocks on a prompt we
        // cannot answer would hang forever.
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| Error::msg(format!("could not start Java at {}: {e}", java.display())))?;

    // Both pipes feed one tail, so a failure message keeps whichever stream
    // actually explained it.
    let tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut readers = Vec::new();

    if let Some(out) = child.stdout.take() {
        readers.push(tokio::spawn(pump(
            Box::pin(BufReader::new(out)),
            tail.clone(),
            Some(progress.clone()),
        )));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(tokio::spawn(pump(
            Box::pin(BufReader::new(err)),
            tail.clone(),
            Some(progress.clone()),
        )));
    }
    for reader in readers {
        let _ = reader.await;
    }

    let status = child.wait().await?;
    if !status.success() {
        // The tool's own last words are far more useful than an exit code.
        let lines = tail.lock().map(|t| t.join("\n")).unwrap_or_default();
        return Err(Error::msg(format!("it failed ({status}):\n{lines}")));
    }
    Ok(())
}

/// Read a pipe to exhaustion, reporting each line and keeping the last few.
async fn pump<R>(
    mut reader: std::pin::Pin<Box<BufReader<R>>>,
    tail: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    progress: Option<Progress>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = reader.as_mut().lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if let Some(p) = &progress {
            p.advance(0, &line).await;
        }
        if let Ok(mut t) = tail.lock() {
            t.push(line);
            if t.len() > TAIL {
                t.remove(0);
            }
        }
    }
}

/// Characters BuildTools refuses to see anywhere in its working directory.
const AWKWARD_PATH_CHARS: &str = "'#~()";

/// Check what BuildTools needs before committing to a download and ten minutes
/// of compiling.
///
/// Each of these is something it checks itself and then exits on, with a
/// message aimed at someone watching a terminal. Checking first means the
/// answer arrives immediately and says what to do about it.
pub async fn preflight_buildtools(paths: &Paths, workspace: &Path) -> Result<()> {
    let path = workspace.to_string_lossy().to_string();
    if let Some(bad) = path.chars().find(|c| AWKWARD_PATH_CHARS.contains(*c)) {
        return Err(Error::msg(format!(
            "Spigot is compiled with BuildTools, which refuses to run in a path containing \
             '{bad}' — and Geode would build it in {path}"
        )));
    }

    // The frontend asks about Git before it gets here and offers to sort it
    // out, so this is the backstop for the case where it was never asked —
    // a plan build, a retry after Git was uninstalled.
    if super::git::find(paths).await.is_none() {
        return Err(Error::msg(
            "Spigot is compiled with SpigotMC's BuildTools, which needs Git, and Git is \
             not on this machine. Open Add server and pick Spigot again to have Geode \
             sort that out — or choose Paper, which runs every Spigot plugin and installs \
             without building anything.",
        ));
    }

    // BuildTools runs `sh -c exit` before anything else and stops if it cannot.
    // On Windows that `sh` comes from Git, so this is only worth asking where
    // the system is expected to have one of its own.
    #[cfg(unix)]
    if !runs("sh", &["-c", "exit"]).await {
        return Err(Error::msg(
            "BuildTools needs a POSIX shell it can run, and `sh` could not be started.",
        ));
    }

    Ok(())
}

/// Whether a command can be run at all and exits cleanly.
#[cfg(unix)]
async fn runs(program: &str, args: &[&str]) -> bool {
    tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Work out how to start the server an installer just built.
///
/// Tried in order: an argument file is decisive, because modern Forge and
/// NeoForge leave one *and* leave loose jars that are not the server.
pub async fn detect(run_dir: &Path) -> Result<(LaunchKind, String)> {
    if let Some(args) = find_arg_file(run_dir).await {
        return Ok((LaunchKind::ArgFile(args), String::new()));
    }
    if let Some(jar) = find_server_jar(run_dir).await {
        return Ok((LaunchKind::Jar, jar));
    }
    Err(Error::msg(
        "the installer finished but left nothing recognisable to start — \
         no argument file and no server jar",
    ))
}

/// The platform's argument file, wherever the installer put it.
///
/// Forge writes `libraries/net/minecraftforge/forge/<version>/unix_args.txt`
/// and NeoForge `libraries/net/neoforged/neoforge/<version>/unix_args.txt`, so
/// this searches for the file instead of rebuilding either path. The
/// `user_jvm_args.txt` in the run root is deliberately out of scope: it holds
/// only commented-out heap defaults, and Geode sets those itself.
pub async fn find_arg_file(run_dir: &Path) -> Option<String> {
    let wanted = if cfg!(windows) {
        "win_args.txt"
    } else {
        "unix_args.txt"
    };

    let mut stack = vec![run_dir.join("libraries")];
    while let Some(dir) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(wanted) {
                return path
                    .strip_prefix(run_dir)
                    .ok()
                    .map(|rel| rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    None
}

/// A runnable server jar in a directory, ignoring the installer itself.
///
/// The choice matters: Quilt's installer leaves a 500-byte
/// `quilt-server-launch.jar` beside the 50MB vanilla `server.jar` it
/// downloaded, and only the small one knows how to load mods.
pub async fn find_server_jar(dir: &Path) -> Option<String> {
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    let mut fallback: Option<String> = None;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".jar") || is_tool_jar(&name) {
            continue;
        }
        if is_loader_jar(&name) {
            return Some(name);
        }
        fallback.get_or_insert(name);
    }
    fallback
}

/// The tool we just ran, which is sitting in the directory it wrote to.
fn is_tool_jar(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("installer") || lower.starts_with("buildtools")
}

/// A jar that is specifically the modded server launcher rather than the
/// vanilla jar sitting next to it.
fn is_loader_jar(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("forge-") || lower.starts_with("neoforge-") || lower.starts_with("quilt-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quilts_launcher_wins_over_the_vanilla_jar_beside_it() {
        // The installer leaves both; only one loads mods.
        assert!(is_loader_jar("quilt-server-launch.jar"));
        assert!(!is_loader_jar("server.jar"));
        assert!(!is_loader_jar("minecraft_server.1.16.5.jar"));
    }

    #[test]
    fn older_forge_jars_are_recognised() {
        // Before 1.17 Forge produced a single runnable jar rather than an
        // argument file, and those servers must still start.
        assert!(is_loader_jar("forge-1.16.5-36.2.34.jar"));
        assert!(is_loader_jar("neoforge-21.1.95.jar"));
    }

    #[test]
    fn the_tool_is_never_mistaken_for_what_it_produced() {
        assert!(is_tool_jar("forge-1.20.1-47.3.0-installer.jar"));
        assert!(is_tool_jar("BuildTools.jar"));
        assert!(!is_tool_jar("spigot-1.21.1.jar"));
    }

    #[test]
    fn the_launch_kind_round_trips_and_defaults_to_a_plain_jar() {
        let args = LaunchKind::ArgFile("libraries/x/unix_args.txt".into());
        let text = serde_json::to_string(&args).unwrap();
        assert_eq!(serde_json::from_str::<LaunchKind>(&text).unwrap(), args);

        // Absent from an older server.json it has to read as the plain jar
        // launch every existing server already uses.
        assert_eq!(LaunchKind::default(), LaunchKind::Jar);
        let jar: LaunchKind = serde_json::from_str(r#"{"kind":"jar"}"#).unwrap();
        assert_eq!(jar, LaunchKind::Jar);
    }
}

//! Assembling the command line and running the game.

pub mod log;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::auth::store::Session;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::install::Installed;
use crate::instance::Instance;
use crate::meta::piston::{Argument, VersionJson};
use crate::meta::Loader;
use crate::paths::Paths;

pub const LAUNCHER_NAME: &str = "geode";

fn classpath_separator() -> &'static str {
    if cfg!(windows) {
        ";"
    } else {
        ":"
    }
}

/// Expand `${placeholder}` tokens in a launch argument.
fn substitute(raw: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &rest[start + 2..start + end];
        match vars.get(key) {
            Some(value) => out.push_str(value),
            // Leaving unknown tokens intact makes a missing variable visible in
            // the crash log instead of silently passing an empty argument.
            None => out.push_str(&rest[start..start + end + 1]),
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

fn collect_args(
    args: &[Argument],
    vars: &HashMap<String, String>,
    features: &HashMap<String, bool>,
) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        match arg {
            Argument::Plain(s) => out.push(substitute(s, vars)),
            Argument::Conditional { rules, value } => {
                if crate::meta::piston::rules_allow(rules, features) {
                    for v in value.clone().into_vec() {
                        out.push(substitute(&v, vars));
                    }
                }
            }
        }
    }
    out
}

/// The full command line, ready to hand to the OS.
pub struct LaunchPlan {
    pub java: PathBuf,
    pub args: Vec<String>,
    pub game_dir: PathBuf,
}

pub fn build_plan(
    paths: &Paths,
    config: &Config,
    instance: &Instance,
    installed: &Installed,
    session: &Session,
) -> Result<LaunchPlan> {
    let version: &VersionJson = &installed.version;
    let game_dir = paths.game_dir(&instance.id);
    let features = crate::install::launch_features();

    let classpath = crate::install::classpath(paths, version, &installed.client_jar, &features)
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(classpath_separator());

    let assets_dir = crate::install::assets_dir_for(paths, version);
    let assets_index = version
        .asset_index
        .as_ref()
        .map(|a| a.id.clone())
        .or_else(|| version.assets.clone())
        .unwrap_or_else(|| "legacy".into());

    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("auth_player_name".into(), session.username.clone());
    vars.insert("auth_uuid".into(), session.uuid.clone());
    vars.insert("auth_access_token".into(), session.access_token.clone());
    vars.insert("auth_session".into(), format!("token:{}", session.access_token));
    vars.insert("user_type".into(), session.user_type.into());
    // Mojang's own launcher sends its XUID and client id here; the game only
    // forwards them to telemetry, and an empty value is accepted.
    vars.insert("auth_xuid".into(), String::new());
    vars.insert("clientid".into(), String::new());
    vars.insert("user_properties".into(), "{}".into());

    vars.insert("version_name".into(), installed.version_id.clone());
    vars.insert("version_type".into(), version.kind.clone());
    vars.insert("game_directory".into(), game_dir.to_string_lossy().into());
    vars.insert("assets_root".into(), assets_dir.to_string_lossy().into());
    vars.insert("game_assets".into(), assets_dir.to_string_lossy().into());
    vars.insert("assets_index_name".into(), assets_index);
    vars.insert(
        "natives_directory".into(),
        installed.natives_dir.to_string_lossy().into(),
    );
    vars.insert(
        "library_directory".into(),
        paths.libraries().to_string_lossy().into(),
    );
    vars.insert("classpath".into(), classpath.clone());
    vars.insert("classpath_separator".into(), classpath_separator().into());
    vars.insert("launcher_name".into(), LAUNCHER_NAME.into());
    vars.insert("launcher_version".into(), env!("CARGO_PKG_VERSION").into());

    let width = instance.overrides.width.unwrap_or(854);
    let height = instance.overrides.height.unwrap_or(480);
    vars.insert("resolution_width".into(), width.to_string());
    vars.insert("resolution_height".into(), height.to_string());

    let mut args: Vec<String> = Vec::new();

    // --- JVM ---
    let memory = instance.overrides.memory_mb.unwrap_or(config.memory_mb);
    args.push(format!("-Xmx{memory}M"));
    args.push(format!("-Xms{}M", (memory / 2).max(512)));

    let user_args = instance
        .overrides
        .java_args
        .clone()
        .unwrap_or_else(|| config.java_args.clone());
    args.extend(
        user_args
            .split_whitespace()
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );

    // Switches the game to structured XML logging, which the console pane
    // parses back into levelled entries. Forge and NeoForge ship their own
    // log4j config; Mojang's replaces it and silences mod-loading output, so
    // they keep theirs and the console falls back to plain lines.
    if let Some(log_config) = &installed.log_config {
        if !matches!(instance.loader, Loader::Forge | Loader::NeoForge) {
            args.push(format!(
                "-Dlog4j.configurationFile={}",
                log_config.to_string_lossy()
            ));
        }
    }

    match &version.arguments {
        Some(arguments) if !arguments.jvm.is_empty() => {
            args.extend(collect_args(&arguments.jvm, &vars, &features));
        }
        _ => {
            // Pre-1.13 manifests carry no JVM arguments at all.
            args.push(format!(
                "-Djava.library.path={}",
                installed.natives_dir.to_string_lossy()
            ));
            args.push("-cp".into());
            args.push(classpath.clone());
        }
    }

    if version.main_class.is_empty() {
        return Err(Error::msg("version manifest has no main class"));
    }
    args.push(version.main_class.clone());

    // --- Game ---
    match (&version.arguments, &version.minecraft_arguments) {
        (Some(arguments), _) if !arguments.game.is_empty() => {
            args.extend(collect_args(&arguments.game, &vars, &features));
        }
        (_, Some(legacy)) => {
            args.extend(legacy.split_whitespace().map(|a| substitute(a, &vars)));
        }
        _ => {}
    }

    if instance.overrides.fullscreen.unwrap_or(false) {
        args.push("--fullscreen".into());
    } else if instance.overrides.width.is_some() || instance.overrides.height.is_some() {
        args.push("--width".into());
        args.push(width.to_string());
        args.push("--height".into());
        args.push(height.to_string());
    }

    if let Some(server) = &instance.overrides.quick_play_server {
        if !server.trim().is_empty() {
            args.push("--quickPlayMultiplayer".into());
            args.push(server.clone());
        }
    }

    Ok(LaunchPlan {
        java: installed.java.clone(),
        args,
        game_dir,
    })
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitEvent {
    pub instance_id: String,
    pub code: Option<i32>,
    pub success: bool,
    pub played_seconds: u64,
}

/// Handle to a running game, kept in app state so the UI can stop it.
pub struct RunningGame {
    pub pid: u32,
    pub started_at: std::time::Instant,
    kill: tokio::sync::watch::Sender<bool>,
}

impl RunningGame {
    pub fn request_stop(&self) {
        let _ = self.kill.send(true);
    }
}

/// Run a shell command and fail the launch if it does not succeed.
async fn run_hook(command: &str, game_dir: &Path) -> Result<()> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let (program, first_arg) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };

    let status = tokio::process::Command::new(program)
        .arg(first_arg)
        .arg(trimmed)
        .current_dir(game_dir)
        .status()
        .await?;

    if !status.success() {
        return Err(Error::msg(format!(
            "pre-launch command exited with status {status}"
        )));
    }
    Ok(())
}

/// Spawn the game and stream its output. Returns once the process has started;
/// the caller gets an `ExitEvent` when it finishes.
pub async fn spawn(
    app: &AppHandle,
    paths: &Paths,
    instance: &Instance,
    plan: LaunchPlan,
    registry: crate::state::GameRegistry,
) -> Result<u32> {
    tokio::fs::create_dir_all(&plan.game_dir).await?;

    if let Some(hook) = &instance.overrides.pre_launch_command {
        run_hook(hook, &plan.game_dir).await?;
    }

    let mut child = tokio::process::Command::new(&plan.java)
        .args(&plan.args)
        .current_dir(&plan.game_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| {
            Error::msg(format!(
                "could not start Java at {}: {e}",
                plan.java.display()
            ))
        })?;

    let pid = child.id().unwrap_or(0);
    let (kill_tx, mut kill_rx) = tokio::sync::watch::channel(false);
    let started_at = std::time::Instant::now();

    registry.insert(
        instance.id.clone(),
        RunningGame {
            pid,
            started_at,
            kill: kill_tx,
        },
    );

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // stdout carries the log4j XML stream; stderr is plain text (JVM crashes,
    // native errors) and must never be parsed as XML.
    if let Some(out) = stdout {
        let app = app.clone();
        let id = instance.id.clone();
        tokio::spawn(async move {
            let mut parser = log::Log4jParser::new();
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                for entry in parser.push(&line) {
                    let _ = app.emit("geode://log", entry.with_instance(&id));
                }
            }
            for entry in parser.flush() {
                let _ = app.emit("geode://log", entry.with_instance(&id));
            }
        });
    }

    if let Some(err) = stderr {
        let app = app.clone();
        let id = instance.id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = app.emit("geode://log", log::LogEntry::stderr(&id, line));
            }
        });
    }

    let app_for_wait = app.clone();
    let instance_id = instance.id.clone();
    let post_exit = instance.overrides.post_exit_command.clone();
    let game_dir = plan.game_dir.clone();
    let paths = paths.clone();

    tokio::spawn(async move {
        let status = tokio::select! {
            status = child.wait() => status,
            _ = kill_rx.changed() => {
                let _ = child.start_kill();
                child.wait().await
            }
        };

        registry.remove(&instance_id);
        let played = started_at.elapsed().as_secs();

        // Record playtime even on a crash — time spent is time spent.
        if let Ok(mut inst) = crate::instance::load(&paths, &instance_id).await {
            inst.play_seconds += played;
            inst.last_played = Some(chrono::Utc::now().timestamp());
            let _ = inst.save(&paths).await;
        }

        let (code, success) = match status {
            Ok(s) => (s.code(), s.success()),
            Err(_) => (None, false),
        };

        if let Some(hook) = post_exit {
            let _ = run_hook(&hook, &game_dir).await;
        }

        let _ = app_for_wait.emit(
            "geode://exit",
            ExitEvent {
                instance_id,
                code,
                success,
                played_seconds: played,
            },
        );
    });

    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::piston::ArgumentValue;

    #[test]
    fn substitutes_known_variables() {
        let mut vars = HashMap::new();
        vars.insert("auth_player_name".to_string(), "Steve".to_string());
        assert_eq!(substitute("${auth_player_name}", &vars), "Steve");
        assert_eq!(
            substitute("--username=${auth_player_name}!", &vars),
            "--username=Steve!"
        );
    }

    #[test]
    fn leaves_unknown_variables_visible() {
        let vars = HashMap::new();
        assert_eq!(substitute("${mystery}", &vars), "${mystery}");
    }

    #[test]
    fn handles_unterminated_placeholder() {
        let vars = HashMap::new();
        assert_eq!(substitute("${oops", &vars), "${oops");
    }

    #[test]
    fn conditional_args_respect_rules() {
        let vars = HashMap::new();
        let features = HashMap::new();
        let args = vec![
            Argument::Plain("--always".into()),
            Argument::Conditional {
                rules: vec![crate::meta::piston::Rule {
                    action: "allow".into(),
                    os: None,
                    features: [("is_demo_user".to_string(), true)].into_iter().collect(),
                }],
                value: ArgumentValue::One("--demo".into()),
            },
        ];
        assert_eq!(collect_args(&args, &vars, &features), vec!["--always"]);
    }
}

//! Assembling the command line and running the game.

pub mod log;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::auth::store::Session;
use crate::events::{self, EventSink};
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
    vars.insert(
        "auth_session".into(),
        format!("token:{}", session.access_token),
    );
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
    // Quick play is not a flag to push on the end: it is a *feature* the
    // version's own manifest declares, with its own conditional argument in
    // the game args. So the feature is turned on and the manifest emits the
    // flag itself — which means a version that does not have quick play gets
    // no flag at all, rather than an argument its own argument parser has
    // never heard of.
    //
    // Checked against Mojang's manifests: 1.20 declares
    // `is_quick_play_multiplayer`, 1.19.4 does not, and 1.12.2 has no
    // `arguments` object whatever. Pre-1.20 gets no substitute, and needs
    // none — the row in `servers.dat` is the answer there, and that works on
    // every version.
    //
    // Set only for the game args, after the classpath and the JVM args are
    // settled, so no library's own rules are disturbed by it.
    let mut game_features = features.clone();
    if let Some(server) = instance
        .overrides
        .quick_play_server
        .as_deref()
        .map(str::trim)
        .filter(|server| !server.is_empty())
    {
        game_features.insert("is_quick_play_multiplayer".into(), true);
        vars.insert("quickPlayMultiplayer".into(), server.to_string());
    }

    match (&version.arguments, &version.minecraft_arguments) {
        (Some(arguments), _) if !arguments.game.is_empty() => {
            args.extend(collect_args(&arguments.game, &vars, &game_features));
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

    /// A game registered as running with no process behind it, so a test can
    /// exercise what Geode refuses to do while one is open.
    #[cfg(test)]
    pub fn none_really() -> Self {
        Self {
            pid: 0,
            started_at: std::time::Instant::now(),
            kill: tokio::sync::watch::channel(false).0,
        }
    }
}

/// Run a shell command and fail the launch if it does not succeed.
/// The command a launch spawns, built but not started.
///
/// Its own function so the three things an instance can change about it — the
/// wrapper, the environment and the working directory — can be read off a
/// constructed command in a test, without a JVM or a download in the way.
fn game_command(
    plan: &LaunchPlan,
    overrides: &crate::instance::InstanceOverrides,
) -> tokio::process::Command {
    let (program, leading) = wrapped(&plan.java, overrides.wrapper_command.as_deref());
    let mut command = tokio::process::Command::new(program);
    command
        .args(leading)
        .args(&plan.args)
        // Added to Geode's own environment rather than replacing it: the game
        // still needs HOME, PATH, DISPLAY and the rest of what a desktop
        // session puts there, and an instance that had to restate all of it
        // would be a worse place to put one variable than the shell Geode was
        // started from.
        .envs(&overrides.env)
        .current_dir(&plan.game_dir);
    command
}

/// The program to run and whatever has to come before the Java path.
///
/// With no wrapper this is just the JVM. With one — `gamemoderun`,
/// `mangohud`, `prime-run`, `env FOO=bar` — the wrapper becomes the program
/// and the Java path becomes its first argument, which is exactly what those
/// tools expect and what Prism does with the same setting.
fn wrapped(java: &std::path::Path, wrapper: Option<&str>) -> (std::path::PathBuf, Vec<String>) {
    let words = wrapper.map(split_words).unwrap_or_default();
    let Some((program, rest)) = words.split_first() else {
        return (java.to_path_buf(), Vec::new());
    };
    let mut leading: Vec<String> = rest.to_vec();
    leading.push(java.to_string_lossy().into_owned());
    (std::path::PathBuf::from(program), leading)
}

/// Split a wrapper command into words, honouring quotes.
///
/// Not a shell: no expansion, no globs, no operators, because the wrapper is
/// one program with arguments and anything more belongs in a pre-launch hook,
/// which is run by a shell on purpose. Quotes are honoured only so a path
/// with a space in it can be written at all.
fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if any {
                    words.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            None => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(word);
    }
    words
}

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
///
/// `sink` is where the game's log lines and its exit go — a webview for the
/// desktop app, and nothing at all for a caller that only wants the process.
pub async fn spawn(
    sink: &EventSink,
    paths: &Paths,
    instance: &Instance,
    plan: LaunchPlan,
    registry: crate::state::GameRegistry,
) -> Result<u32> {
    tokio::fs::create_dir_all(&plan.game_dir).await?;

    if let Some(hook) = &instance.overrides.pre_launch_command {
        run_hook(hook, &plan.game_dir).await?;
    }

    // What actually runs: the JVM, or the JVM inside whatever the instance
    // asked to wrap it in, with whatever environment the instance asked for.
    let mut command = game_command(&plan, &instance.overrides);
    let program = command.as_std().get_program().to_os_string();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| {
            Error::msg(format!(
                "could not start {}: {e}",
                std::path::Path::new(&program).display()
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
        let sink = Some(sink.clone());
        let id = instance.id.clone();
        tokio::spawn(async move {
            let mut parser = log::Log4jParser::new();
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                for entry in parser.push(&line) {
                    events::emit(&sink, "geode://log", &entry.with_instance(&id));
                }
            }
            for entry in parser.flush() {
                events::emit(&sink, "geode://log", &entry.with_instance(&id));
            }
        });
    }

    if let Some(err) = stderr {
        let sink = Some(sink.clone());
        let id = instance.id.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                events::emit(&sink, "geode://log", &log::LogEntry::stderr(&id, line));
            }
        });
    }

    let sink_for_wait = Some(sink.clone());
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

        events::emit(
            &sink_for_wait,
            "geode://exit",
            &ExitEvent {
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

    /// The three things an instance can change about the command a launch
    /// spawns, read off the command itself.
    ///
    /// The environment is the point of this one. The JVM used to be spawned
    /// with whatever Geode was started with and nothing else, so the only way
    /// to give the game a driver override or a Wayland choice was to start
    /// the whole launcher with it — which is why a Prism instance carrying
    /// four of them could not be moved here.
    #[test]
    fn an_instance_can_change_the_environment_the_game_runs_in() {
        let plan = LaunchPlan {
            java: PathBuf::from("/jvm/bin/java"),
            args: vec!["-Xmx2G".into(), "net.minecraft.client.main.Main".into()],
            game_dir: PathBuf::from("/games/survival"),
        };
        let mut overrides = crate::instance::InstanceOverrides {
            wrapper_command: Some("gamemoderun".into()),
            ..Default::default()
        };
        overrides
            .env
            .insert("__GL_THREADED_OPTIMIZATIONS".into(), "0".into());
        // A value with an `=` in it, which a real one has:
        // `SDL_LOGGING=video=verbose`.
        overrides
            .env
            .insert("SDL_LOGGING".into(), "video=verbose".into());

        let command = game_command(&plan, &overrides);
        let std = command.as_std();

        assert_eq!(std.get_program(), "gamemoderun");
        let args: Vec<_> = std.get_args().collect();
        assert_eq!(
            args,
            vec!["/jvm/bin/java", "-Xmx2G", "net.minecraft.client.main.Main"],
            "the JVM has to be the wrapper's first argument, before the game's own"
        );

        let env: std::collections::HashMap<_, _> = std
            .get_envs()
            .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
            .collect();
        assert_eq!(
            env.get(std::ffi::OsStr::new("__GL_THREADED_OPTIMIZATIONS")).map(|v| v.as_os_str()),
            Some(std::ffi::OsStr::new("0"))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new("SDL_LOGGING")).map(|v| v.as_os_str()),
            Some(std::ffi::OsStr::new("video=verbose"))
        );
        // Added, not replacing: nothing here clears the inherited environment,
        // because the game still needs HOME, PATH and DISPLAY.
        assert!(
            !std.get_envs().any(|(_, v)| v.is_none()),
            "something was removed from the environment rather than added to it"
        );

        assert_eq!(std.get_current_dir(), Some(std::path::Path::new("/games/survival")));
    }

    /// With no wrapper the JVM is the program, exactly as before.
    #[test]
    fn no_wrapper_runs_java_itself() {
        let (program, leading) = wrapped(std::path::Path::new("/jvm/bin/java"), None);
        assert_eq!(program, std::path::PathBuf::from("/jvm/bin/java"));
        assert!(leading.is_empty());

        // A wrapper of only spaces is the same as none, so an emptied field
        // cannot produce a launch that tries to exec "".
        let (program, leading) = wrapped(std::path::Path::new("/jvm/bin/java"), Some("   "));
        assert_eq!(program, std::path::PathBuf::from("/jvm/bin/java"));
        assert!(leading.is_empty());
    }

    /// With one, the wrapper becomes the program and Java its last argument —
    /// which is what `gamemoderun java …` and its kin expect.
    #[test]
    fn a_wrapper_takes_the_jvm_as_its_argument() {
        let (program, leading) = wrapped(
            std::path::Path::new("/jvm/bin/java"),
            Some("mangohud --dlsym"),
        );
        assert_eq!(program, std::path::PathBuf::from("mangohud"));
        assert_eq!(leading, vec!["--dlsym", "/jvm/bin/java"]);
    }

    /// Quotes exist so a path with a space in it can be written at all. This
    /// is not a shell and does not pretend to be one.
    #[test]
    fn a_wrapper_can_name_a_path_with_a_space_in_it() {
        assert_eq!(
            split_words("\"/opt/my tools/gamemoderun\" -v"),
            vec!["/opt/my tools/gamemoderun", "-v"]
        );
        assert_eq!(split_words("  prime-run   "), vec!["prime-run"]);
        assert!(split_words("").is_empty());
        // An empty quoted word is a word, so `--flag ""` keeps its argument.
        assert_eq!(split_words("--flag \"\""), vec!["--flag", ""]);
    }

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

    /// The plan for an instance pointed at a server, on a given version.
    ///
    /// `game_args` is the version's own `arguments.game`, in Mojang's JSON, so
    /// the fixture under test is the shape the real manifests are in rather
    /// than one of ours.
    fn plan_for(game_args: serde_json::Value, quick_play: Option<&str>) -> Vec<String> {
        let version: VersionJson = serde_json::from_value(serde_json::json!({
            "id": "test",
            "mainClass": "net.minecraft.client.main.Main",
            "arguments": { "game": game_args, "jvm": [] },
        }))
        .unwrap();

        let mut instance = crate::instance::Instance::new(
            "Joined".into(),
            "1.21.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        );
        instance.overrides.quick_play_server = quick_play.map(str::to_string);

        let installed = Installed {
            version_id: "test".into(),
            version,
            java: PathBuf::from("/usr/bin/java"),
            natives_dir: PathBuf::from("/natives"),
            client_jar: PathBuf::from("/client.jar"),
            log_config: None,
        };
        let session = Session {
            username: "Steve".into(),
            uuid: "u".into(),
            access_token: "t".into(),
            user_type: "msa",
        };
        build_plan(
            &Paths {
                root: PathBuf::from("/geode"),
            },
            &Config::default(),
            &instance,
            &installed,
            &session,
        )
        .unwrap()
        .args
    }

    /// The flag comes from the version's own manifest, so a version that does
    /// not have quick play is passed nothing rather than an argument it has
    /// never heard of.
    ///
    /// Taken from Mojang's own manifests: 1.20 carries this conditional,
    /// 1.19.4 carries none. Push the flag unconditionally instead — which is
    /// what this used to do — and the second half of this test fails.
    #[test]
    fn quick_play_is_passed_only_by_a_version_that_declares_it() {
        let declares_it = serde_json::json!([
            "--username",
            "${auth_player_name}",
            {
                "rules": [{ "action": "allow", "features": { "is_quick_play_multiplayer": true } }],
                "value": ["--quickPlayMultiplayer", "${quickPlayMultiplayer}"]
            }
        ]);
        let args = plan_for(declares_it.clone(), Some("play.example.com"));
        let at = args.iter().position(|a| a == "--quickPlayMultiplayer");
        assert!(at.is_some(), "the flag was not passed: {args:?}");
        assert_eq!(args[at.unwrap() + 1], "play.example.com");

        // 1.19.4's shape: an `arguments` object with no quick play in it.
        let does_not = serde_json::json!(["--username", "${auth_player_name}"]);
        let args = plan_for(does_not, Some("play.example.com"));
        assert!(
            !args.iter().any(|a| a.contains("quickPlay")),
            "a version without quick play was passed it anyway: {args:?}"
        );
        // And not the address on its own either, which would be a stray
        // positional argument.
        assert!(!args.iter().any(|a| a == "play.example.com"), "{args:?}");

        // An instance pointed at nothing passes nothing, on either version.
        for shape in [declares_it, serde_json::json!([])] {
            for server in [None, Some(""), Some("   ")] {
                let args = plan_for(shape.clone(), server);
                assert!(!args.iter().any(|a| a.contains("quickPlay")), "{args:?}");
            }
        }
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

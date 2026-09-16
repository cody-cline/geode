//! Learning what commands a server actually has.
//!
//! A server knows its own commands exactly: vanilla's, every mod's and every
//! plugin's, all registered into one Brigadier dispatcher. It hands that whole
//! tree to each client that joins — which is the only complete account of it
//! anywhere, since nothing in a mod jar declares the commands it registers.
//!
//! Reading it off the wire would mean joining the world as a visible player,
//! because the packet that carries it exists only in the PLAY state. So Geode
//! reads the same tree from inside the server instead, with nobody joining: a
//! small Java agent goes on the server's own command line, notes the dispatcher
//! the first time anything registers a command on it, and writes the tree out
//! as JSON.
//!
//! The hook point is Brigadier rather than Minecraft because Brigadier is not
//! Mojang's obfuscated code — the server ships it as a plain library, so
//! `com.mojang.brigadier.CommandDispatcher` has that name on vanilla, Paper,
//! Forge and Fabric alike, and no per-version mapping table is needed to find
//! it. See `agent/src/geode/probe/Probe.java`.
//!
//! Two things about the agent matter from this side, both learned by breaking
//! real servers rather than by reading about them:
//!
//! - The flag has to come **before** `@argfile`. A Forge or NeoForge argument
//!   file names the main class, so JVM option parsing stops there and anything
//!   after it is passed to the game instead — where it is not a JVM flag at
//!   all, and the server refuses to start.
//! - A jar the JVM cannot open is fatal *before any Java runs*, so a truncated
//!   agent would stop the server booting, and no amount of care inside the
//!   agent could help. That is why [`ensure`] verifies the jar opens as an
//!   archive and simply declines to pass the flag if it does not.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::paths::Paths;

/// The agent, built by `agent/build.sh` and committed alongside its source.
///
/// Carried in the binary rather than downloaded: it has to be there the first
/// time a server starts, offline included, and it is the same 130-odd KB for
/// every install.
const AGENT: &[u8] = include_bytes!("../../resources/geode-probe.jar");

/// One command, and the words that may follow it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandNode {
    pub name: String,
    /// Literal children — the next word, when the next word is a fixed choice.
    pub children: Vec<CommandNode>,
    /// Argument children, by the name the server gave them.
    ///
    /// Kept apart from [`CommandNode::children`] because these cannot be
    /// completed: `<player>` is a description of what to type, not something to
    /// type. Showing them still tells you what the command wants.
    pub args: Vec<String>,
}

/// How deep the tree is worth carrying to the frontend.
///
/// `/execute` nests far deeper than this by redirecting to itself, and nobody
/// is completing the twelfth word of a command from a list.
const MAX_DEPTH: usize = 12;

fn agent_jar(paths: &Paths) -> PathBuf {
    paths.root.join("tools").join("geode-probe.jar")
}

/// Where a server's tree lives: beside Geode's own record of the server, not in
/// the run directory, so it never looks like something the server wrote.
pub fn tree_file(paths: &Paths, id: &str) -> PathBuf {
    paths.server(id).join("commands.json")
}

/// Put the agent on disk and return the `-javaagent:` argument for this server.
///
/// `None` means "start the server without it". Every failure here takes that
/// path: a server that starts with no autocomplete is a small loss, and a
/// server that will not start is not.
pub async fn agent_arg(paths: &Paths, id: &str) -> Option<String> {
    let jar = ensure(paths).await?;
    let tree = tree_file(paths, id);
    // `-javaagent:<jar>=<args>` and the agent splits its own argument on
    // commas, so a path containing either character would be read as something
    // else entirely. Rather than invent an escape nobody would ever exercise,
    // decline and start the server without a probe.
    let jar = jar.to_str()?;
    let tree = tree.to_str()?;
    if [jar, tree].iter().any(|p| p.contains(',') || p.contains('=')) {
        return None;
    }
    Some(format!("-javaagent:{jar}={tree}"))
}

/// Write the agent out if it is missing or stale, and prove it is usable.
async fn ensure(paths: &Paths) -> Option<PathBuf> {
    let jar = agent_jar(paths);

    // Compare the whole file, not its length: a jar rebuilt from the same
    // sources is very often the same size, and shipping a stale agent would be
    // a confusing bug to chase.
    if tokio::fs::read(&jar).await.is_ok_and(|on_disk| on_disk == AGENT) && opens_as_archive(&jar) {
        return Some(jar);
    }

    if let Some(parent) = jar.parent() {
        tokio::fs::create_dir_all(parent).await.ok()?;
    }
    // Written beside and renamed, so a server can never start against a
    // half-written jar — which the JVM would refuse to open, and refuse
    // loudly enough to stop the server.
    let tmp = jar.with_extension("jar.tmp");
    tokio::fs::write(&tmp, AGENT).await.ok()?;
    tokio::fs::rename(&tmp, &jar).await.ok()?;

    opens_as_archive(&jar).then_some(jar)
}

/// Whether the JVM will be able to open this as an agent jar.
///
/// Checked because the alternative is discovering it at server start, where a
/// bad jar is not a degraded feature but a dead server: the JVM aborts during
/// initialisation, before any of the agent's own care can apply.
fn opens_as_archive(jar: &Path) -> bool {
    let Ok(file) = std::fs::File::open(jar) else {
        return false;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return false;
    };
    let found = zip.by_name("META-INF/MANIFEST.MF").is_ok();
    found
}

/// The commands this server had the last time it ran.
///
/// Read from the file rather than from the running server: a tree that
/// survives a restart is what makes the console useful before the server is
/// up, and the agent rewrites it whenever what it sees changes.
pub async fn read(paths: &Paths, id: &str) -> Vec<CommandNode> {
    let Ok(text) = tokio::fs::read_to_string(tree_file(paths, id)).await else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut roots = children_of(&value, 0);
    // Bukkit registers every plugin command a second time under its plugin's
    // namespace — `essentials:afk` beside `afk` — which doubles the list
    // without adding anything anyone types. The game's own client hides them.
    roots.retain(|node| !node.name.contains(':'));
    roots
}

fn children_of(node: &serde_json::Value, depth: usize) -> Vec<CommandNode> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }
    let Some(children) = node.get("children").and_then(|c| c.as_object()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (name, child) in children {
        // Arguments are recorded on their parent, since they are shown rather
        // than completed.
        if child.get("type").and_then(|t| t.as_str()) == Some("argument") {
            continue;
        }
        out.push(CommandNode {
            name: name.clone(),
            children: children_of(child, depth + 1),
            args: argument_names(child),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn argument_names(node: &serde_json::Value) -> Vec<String> {
    let Some(children) = node.get("children").and_then(|c| c.as_object()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = children
        .iter()
        .filter(|(_, child)| child.get("type").and_then(|t| t.as_str()) == Some("argument"))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    /// The agent is committed, so the thing most likely to go wrong is it being
    /// absent or truncated — which would stop servers booting rather than
    /// degrade quietly.
    #[test]
    fn the_agent_is_present_and_is_a_jar() {
        assert!(AGENT.len() > 50_000, "agent jar looks truncated");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(AGENT)).expect("a zip");
        let manifest = {
            use std::io::Read;
            let mut entry = zip.by_name("META-INF/MANIFEST.MF").expect("a manifest");
            let mut s = String::new();
            entry.read_to_string(&mut s).expect("readable");
            s
        };
        assert!(
            manifest.contains("Premain-Class: geode.probe.Probe"),
            "the manifest does not name the agent: {manifest}"
        );
        // Fabric Loader refuses to start a server that has two ASMs on its
        // class path, and a -javaagent jar is always on the class path. Ours
        // must therefore not be carrying one under its own name.
        assert!(
            zip.by_name("org/objectweb/asm/ClassReader.class").is_err(),
            "ASM is unshaded — this stops every Fabric server booting"
        );
        assert!(
            zip.by_name("geode/probe/zzasm/ClassReader.class").is_ok(),
            "the shaded ASM is missing"
        );
        assert!(
            zip.by_name("geode-probe-hook.jar").is_ok(),
            "the bootstrap hook jar is missing, so nothing would be woven"
        );
    }

    /// The one failure the agent cannot defend against, so this side must.
    ///
    /// A jar the JVM cannot open is fatal during VM initialisation — the server
    /// does not start at all, and no `catch` inside the agent is ever reached.
    /// Verified by breaking a real server with a truncated jar, which is why
    /// [`ensure`] replaces a bad copy and refuses to hand back one it cannot
    /// open.
    #[tokio::test]
    async fn a_damaged_agent_on_disk_is_replaced_rather_than_passed_to_a_server() {
        let scratch = Scratch::new("probe-test");
        let paths = scratch.paths.clone();
        let jar = agent_jar(&paths);
        tokio::fs::create_dir_all(jar.parent().unwrap()).await.unwrap();
        tokio::fs::write(&jar, b"not a zip, and half-written at that")
            .await
            .unwrap();
        assert!(!opens_as_archive(&jar), "the setup should be broken");

        let arg = agent_arg(&paths, "some-server").await;

        assert!(arg.is_some(), "a damaged jar should be repaired, not skipped");
        assert!(
            opens_as_archive(&jar),
            "the agent left on disk must be one the JVM can open"
        );
        assert_eq!(tokio::fs::read(&jar).await.unwrap(), AGENT);
    }

    /// A data directory whose name would corrupt the flag gets no probe rather
    /// than a mangled one.
    ///
    /// `-javaagent:<jar>=<args>` is delimited by `=`, and the agent splits its
    /// own argument on `,` — so either character in a path silently truncates
    /// what the agent is told, and it would write its tree somewhere else or
    /// nowhere.
    #[tokio::test]
    async fn a_path_that_would_break_the_flag_is_declined() {
        for awkward in ["comma,dir", "equals=dir"] {
            let scratch = Scratch::new(&format!("probe-{awkward}"));
            let paths = scratch.paths.clone();
            assert_eq!(
                agent_arg(&paths, "server").await,
                None,
                "a path containing the delimiter should be refused: {awkward}"
            );
        }

        // The ordinary case still works, so the check is not simply refusing
        // everything.
        let scratch = Scratch::new("probe-plain");
        let fine = scratch.paths.clone();
        assert!(agent_arg(&fine, "server").await.is_some());
    }

    fn parse(json: &str) -> Vec<CommandNode> {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let mut roots = children_of(&value, 0);
        roots.retain(|n| !n.name.contains(':'));
        roots
    }

    #[test]
    fn arguments_are_described_and_literals_are_completed() {
        // The shape a real server produces, trimmed: one literal with a literal
        // child and an argument child.
        let roots = parse(
            r#"{"type":"root","children":{
                 "spark":{"type":"literal","children":{
                    "profiler":{"type":"literal","executable":true},
                    "args":{"type":"argument","parser":"brigadier:string"}}},
                 "gamemode":{"type":"literal","children":{
                    "gamemode":{"type":"argument","parser":"minecraft:gamemode"}}}}}"#,
        );
        let names: Vec<&str> = roots.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["gamemode", "spark"]);

        let spark = &roots[1];
        assert_eq!(
            spark.children.iter().map(|c| &c.name).collect::<Vec<_>>(),
            vec!["profiler"],
            "only literals are completable"
        );
        assert_eq!(spark.args, vec!["args"]);
        // A command whose only child is an argument has nothing to complete but
        // still has something to say.
        assert!(roots[0].children.is_empty());
        assert_eq!(roots[0].args, vec!["gamemode"]);
    }

    /// A server older than Brigadier reports names and nothing else.
    ///
    /// Minecraft 1.13 introduced the command tree; before it, a server knew
    /// only a flat list of names, so that is what the probe sends back. It
    /// still completes the first word, which is the part anyone types.
    #[test]
    fn a_pre_brigadier_server_returns_names_with_no_structure() {
        let roots = parse(
            r#"{"_probe":{"format":1,"nodes":3,"legacy":true},"type":"root","children":{
                 "afk":{"type":"literal"},
                 "gamemode":{"type":"literal"},
                 "worldedit":{"type":"literal"}}}"#,
        );
        assert_eq!(
            roots.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
            vec!["afk", "gamemode", "worldedit"]
        );
        // No argument structure exists to report, and claiming otherwise would
        // be inventing it.
        assert!(roots.iter().all(|n| n.children.is_empty() && n.args.is_empty()));
    }

    #[test]
    fn a_plugins_namespaced_twin_is_dropped() {
        let roots = parse(
            r#"{"type":"root","children":{
                 "afk":{"type":"literal"},
                 "essentials:afk":{"type":"literal"},
                 "minecraft:gamemode":{"type":"literal"}}}"#,
        );
        assert_eq!(
            roots.iter().map(|n| &n.name).collect::<Vec<_>>(),
            vec!["afk"]
        );
    }

    #[test]
    fn a_tree_that_is_missing_or_nonsense_is_simply_empty() {
        assert!(parse(r#"{"type":"root"}"#).is_empty());
        assert!(parse(r#"{}"#).is_empty());
    }

    /// `/execute` redirects to itself, so a real tree nests without end. The
    /// agent writes redirects as paths rather than recursing, but the depth cap
    /// is what guarantees this side terminates whatever it is handed.
    #[test]
    fn a_tree_deeper_than_anyone_types_is_cut_off() {
        let mut json = String::from(r#"{"type":"root","children":{"a":{"type":"literal""#);
        for _ in 0..40 {
            json.push_str(r#","children":{"a":{"type":"literal""#);
        }
        for _ in 0..40 {
            json.push_str("}}");
        }
        json.push_str("}}}");

        let roots = parse(&json);
        let mut depth = 0;
        let mut here = &roots;
        while let Some(first) = here.first() {
            depth += 1;
            here = &first.children;
        }
        assert_eq!(depth, MAX_DEPTH, "the cap should be what stops the walk");
    }
}

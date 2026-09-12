//! What a mod jar says about itself.
//!
//! The dependency graph is read from the jars rather than from Modrinth or
//! CurseForge. The jar is what the loader actually enforces, it is right even
//! when the provider's metadata is not, and it works for mods that were
//! dropped into the folder by hand. Provider data is only a fallback for jars
//! that declare nothing we can read.

use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Ids every mod depends on that no file in the folder provides. Left in, they
/// would show up as missing dependencies on literally everything.
const PLATFORM_IDS: &[&str] = &[
    "minecraft",
    "java",
    "fabricloader",
    "fabric-loader",
    "quilt_loader",
    "quiltloader",
    "neoforge",
    "forge",
    "fml",
    "javafml",
    "mcp",
];

/// A jar's own account of what it is and what it needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JarMeta {
    pub name: Option<String>,
    pub version: Option<String>,
    /// Ids the jar itself answers to: its own, plus any declared aliases.
    pub primary: Vec<String>,
    /// Ids satisfied by jars packed inside it.
    ///
    /// Held apart from [`JarMeta::primary`] because a mod carrying a copy of a
    /// library is a worse answer to "what provides this" than the library
    /// itself, when both are installed.
    pub bundled: Vec<String>,
    /// Ids it requires, platform ids aside.
    ///
    /// Deliberately still lists what the jar bundles. Fabric Loader prefers an
    /// installed copy over a packed one, so Mod Menu carrying its own
    /// fabric-resource-loader-v0 does not stop Fabric API being what actually
    /// answers for it — and that relationship is the point of the graph.
    pub requires: Vec<String>,
    /// Commands it registers, where the format declares them.
    ///
    /// Only Bukkit-family plugins do: a mod registers its commands in code, so
    /// the only way to learn those is to ask a running server.
    pub commands: Vec<String>,
}

/// A read, stamped with the size and mtime it was valid for.
type Cached = (u64, u64, Option<JarMeta>);

/// Jar metadata keyed by path.
///
/// Reading a folder of mods means opening every jar, and the ones that bundle
/// their dependencies — Forgified Fabric API carries about fifty — mean
/// opening those too. Doing that on every visit to the Content tab would be
/// felt, and jars do not change unless the file does.
#[derive(Default)]
pub struct MetaCache {
    inner: RwLock<HashMap<PathBuf, Cached>>,
}

impl MetaCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `path`, reusing the last result while size and mtime are unchanged.
    pub async fn get(&self, path: &Path) -> Option<JarMeta> {
        let stamp = stamp(path).await;
        if let Some((size, mtime, meta)) = self.inner.read().ok().and_then(|m| m.get(path).cloned())
        {
            if (size, mtime) == stamp {
                return meta;
            }
        }

        let owned = path.to_path_buf();
        let meta = tokio::task::spawn_blocking(move || read(&owned))
            .await
            .ok()
            .flatten();

        if let Ok(mut map) = self.inner.write() {
            map.insert(path.to_path_buf(), (stamp.0, stamp.1, meta.clone()));
        }
        meta
    }
}

async fn stamp(path: &Path) -> (u64, u64) {
    match tokio::fs::metadata(path).await {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (m.len(), mtime)
        }
        Err(_) => (0, 0),
    }
}

/// Read one jar. Blocking — callers go through [`MetaCache`].
pub fn read(path: &Path) -> Option<JarMeta> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut meta = declaration(&mut zip)?;

    // A bundled jar satisfies its ids without being a separate file, so those
    // ids count as provided here. Skipping this is how a mod that carries its
    // own dependencies ends up looking like it is missing them.
    for name in nested_jar_names(&mut zip) {
        let Some(bytes) = entry_bytes(&mut zip, &name) else {
            continue;
        };
        let Ok(mut inner) = zip::ZipArchive::new(Cursor::new(bytes)) else {
            continue;
        };
        if let Some(nested) = declaration(&mut inner) {
            meta.bundled.extend(nested.primary);
        }
    }

    normalise(&mut meta);
    Some(meta)
}

fn normalise(meta: &mut JarMeta) {
    for list in [
        &mut meta.primary,
        &mut meta.bundled,
        &mut meta.requires,
        // A plugin routinely gives two commands an alias in common —
        // EssentialsX declares 697 entries and means 694.
        &mut meta.commands,
    ] {
        for id in list.iter_mut() {
            *id = id.trim().to_ascii_lowercase();
        }
        list.retain(|id| !id.is_empty());
        list.sort();
        list.dedup();
    }
    meta.bundled.retain(|id| !meta.primary.contains(id));
    meta.requires
        .retain(|id| !PLATFORM_IDS.contains(&id.as_str()));
}

fn declaration<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Option<JarMeta> {
    if let Some(text) =
        entry_text(zip, "fabric.mod.json").or_else(|| entry_text(zip, "quilt.mod.json"))
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            return Some(from_fabric(&value));
        }
    }
    if let Some(text) = entry_text(zip, "META-INF/neoforge.mods.toml")
        .or_else(|| entry_text(zip, "META-INF/mods.toml"))
    {
        if let Some(meta) = from_mods_toml(&text) {
            return Some(meta);
        }
    }

    // A Bukkit plugin. One jar may legitimately carry both manifests so it runs
    // on Spigot and on Paper alike; Paper reads its own first, so this does too,
    // and either way it is one thing in the graph.
    if let Some(text) = entry_text(zip, "paper-plugin.yml") {
        if let Some(meta) = from_paper_plugin(&text) {
            return Some(meta);
        }
    }
    from_plugin_yml(&entry_text(zip, "plugin.yml")?)
}

fn nested_jar_names<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Vec<String> {
    zip.file_names()
        .filter(|n| {
            n.ends_with(".jar")
                && (n.starts_with("META-INF/jars/") || n.starts_with("META-INF/jarjar/"))
        })
        .map(str::to_string)
        .collect()
}

fn entry_text<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    let mut s = String::new();
    entry.read_to_string(&mut s).ok()?;
    Some(s)
}

fn entry_bytes<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

// ---------------------------------------------------------------------------
// Bukkit and Paper plugin manifests
//
// These are YAML, and what we want out of them is five keys at known depths.
// That is read here rather than by pulling in a YAML parser, but "read the
// lines" is not enough on its own: a plugin.yml routinely declares a *command*
// called `version`, so keys are anchored to indentation rather than matched
// anywhere they appear.
// ---------------------------------------------------------------------------

/// Everything from `#` onwards, unless the `#` is inside quotes or butted up
/// against a value — `version: 1.0#2` has no comment in it.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &c) in bytes.iter().enumerate() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                return &line[..i];
            }
            None => {}
        }
    }
    line
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// A scalar, taken as written. Never parsed as a number: SnakeYAML reads an
/// unquoted `version: 1.20` as a float and hands back "1.2".
fn unquote(value: &str) -> String {
    let v = value.trim();
    let bytes = v.as_bytes();
    if v.len() >= 2
        && ((bytes[0] == b'"' && bytes[v.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[v.len() - 1] == b'\''))
    {
        return v[1..v.len() - 1].to_string();
    }
    v.to_string()
}

/// A top-level `key: value`. Only at indent zero, which is what keeps a command
/// named `version` from answering for the plugin's own.
fn root_scalar(text: &str, key: &str) -> Option<String> {
    for raw in text.lines() {
        let line = strip_comment(raw);
        if indent_of(line) != 0 {
            continue;
        }
        let Some(rest) = line.trim_end().strip_prefix(key) else {
            continue;
        };
        let Some(value) = rest.strip_prefix(':') else {
            continue;
        };
        let value = unquote(value);
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// Split a flow sequence's innards on commas that are not inside quotes.
fn split_flow(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                current.push(c);
            }
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                current.push(c);
            }
            None if c == ',' => {
                out.push(std::mem::take(&mut current));
            }
            None => current.push(c),
        }
    }
    out.push(current);
    out.into_iter()
        .map(|s| unquote(&s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// A top-level list, in either form it actually takes.
///
/// Both of these are normal, and the second is what every build plugin emits —
/// SnakeYAML puts block dashes at the *same* column as their key, so requiring
/// them to be indented further misses exactly the well-formed files:
///
/// ```yaml
/// depend: [Vault, WorldEdit]
/// softdepend:
/// - PlaceholderAPI
/// ```
fn root_list(text: &str, key: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().map(strip_comment).collect();
    let Some(at) = lines.iter().position(|line| {
        indent_of(line) == 0
            && line
                .trim_end()
                .strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(':'))
    }) else {
        return Vec::new();
    };

    let after = lines[at].trim_end().split_once(':').map(|(_, v)| v).unwrap_or("").trim();

    // Flow form, which may run past the end of its own line.
    if after.starts_with('[') {
        let mut buf = String::new();
        let mut depth = 0usize;
        for line in &lines[at..] {
            let part = if buf.is_empty() {
                line.split_once(':').map(|(_, v)| v).unwrap_or(line)
            } else {
                line
            };
            for c in part.chars() {
                if c == '[' {
                    depth += 1;
                    continue;
                }
                if c == ']' {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return split_flow(&buf);
                    }
                    continue;
                }
                if depth > 0 {
                    buf.push(c);
                }
            }
        }
        return split_flow(&buf);
    }

    // Block form. Entries may sit at the key's own indent or deeper; anything
    // else at indent zero ends the list.
    let mut out = Vec::new();
    for line in &lines[at + 1..] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix('-')) {
            let value = unquote(item);
            if !value.is_empty() {
                out.push(value);
            }
            continue;
        }
        if indent_of(line) == 0 {
            break;
        }
    }
    out
}

/// Bukkit normalises spaces to underscores on both a plugin's own name and
/// every name it depends on, so the graph has to match the same way.
fn plugin_id(name: &str) -> String {
    name.trim().replace(' ', "_")
}

/// `plugin.yml` — Bukkit, Spigot, Purpur and every fork of them.
fn from_plugin_yml(text: &str) -> Option<JarMeta> {
    let name = root_scalar(text, "name")?;

    let mut primary = vec![plugin_id(&name)];
    // `provides` are other names this plugin answers to, exactly like an alias.
    primary.extend(root_list(text, "provides").iter().map(|p| plugin_id(p)));

    // Only `depend` is a requirement. `softdepend` is optional by definition,
    // `loadbefore` points the other way — "load me before these" — and
    // `libraries` are Maven coordinates that will never be a file in the
    // plugins folder. Treating any of those as a requirement invents missing
    // dependencies, or edges that run backwards.
    let requires = root_list(text, "depend")
        .iter()
        .map(|d| plugin_id(d))
        .collect();

    Some(JarMeta {
        name: Some(name),
        version: root_scalar(text, "version"),
        primary,
        bundled: Vec::new(),
        requires,
        commands: declared_commands(text),
    })
}

/// The commands a `plugin.yml` registers, with their aliases.
///
/// Bukkit requires every command to be declared here — a plugin that forgets
/// gets a null command back at runtime — so unlike mods, whose commands are
/// registered in code and only knowable by asking a running server, a plugin's
/// are readable from the jar while the server is stopped.
///
/// ```yaml
/// commands:
///   heal:
///     description: Heals a player
///     aliases: [h, mend]
///   feed:
/// ```
fn declared_commands(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().map(strip_comment).collect();
    let Some(at) = lines
        .iter()
        .position(|l| indent_of(l) == 0 && l.trim_end().starts_with("commands:"))
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    // The first non-empty line decides the column commands sit at. Anything
    // deeper is a command's own settings; anything shallower ends the section.
    let mut column: Option<usize> = None;
    for line in &lines[at + 1..] {
        if line.trim().is_empty() {
            continue;
        }
        let indent = indent_of(line);
        let at_column = *column.get_or_insert(indent);
        if indent < at_column {
            break;
        }
        if indent > at_column {
            // A command's own block. `aliases` is the only part worth having:
            // a server accepts them exactly as it accepts the command.
            if line.trim_start().starts_with("aliases:") {
                out.extend(nested_list(line));
            }
            continue;
        }
        if let Some((name, _)) = line.trim().split_once(':') {
            let name = unquote(name);
            if !name.is_empty() {
                out.push(name);
            }
        }
    }
    out.into_iter().map(|c| c.to_lowercase()).collect()
}

/// The values of an inline `aliases: [a, b]` or `aliases: a`.
///
/// Only the flow and scalar forms. The block form puts its entries on later
/// lines, which the caller is already walking one at a time — and a plugin
/// writing aliases that way is rare enough to be worth less than the tangle of
/// threading state through that walk.
fn nested_list(line: &str) -> Vec<String> {
    let after = line.split_once(':').map(|(_, v)| v).unwrap_or("").trim();
    if let Some(inner) = after.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return split_flow(inner);
    }
    let single = unquote(after);
    if single.is_empty() {
        Vec::new()
    } else {
        vec![single]
    }
}

/// `paper-plugin.yml` — a map rather than lists, where `required` alone decides
/// whether a dependency is hard and `load` alone decides ordering. The two are
/// independent, so hardness is never inferred from the ordering.
fn from_paper_plugin(text: &str) -> Option<JarMeta> {
    let name = root_scalar(text, "name")?;

    let lines: Vec<&str> = text.lines().map(strip_comment).collect();
    let mut requires = Vec::new();

    if let Some(at) = lines
        .iter()
        .position(|l| indent_of(l) == 0 && l.trim_end().starts_with("dependencies:"))
    {
        // Inside `dependencies`, each named plugin is a key whose block may
        // carry `required`. Anything back at indent zero ends the section.
        let mut pending: Option<(usize, String)> = None;
        let mut required = true;
        for line in &lines[at + 1..] {
            if line.trim().is_empty() {
                continue;
            }
            let indent = indent_of(line);
            if indent == 0 {
                break;
            }
            let trimmed = line.trim();

            if let Some((key_indent, plugin)) = &pending {
                if indent > *key_indent {
                    if let Some(rest) = trimmed.strip_prefix("required:") {
                        required = unquote(rest) != "false";
                    }
                    continue;
                }
                if required {
                    requires.push(plugin_id(plugin));
                }
                pending = None;
            }

            let Some((key, _)) = trimmed.split_once(':') else {
                continue;
            };
            // `bootstrap` and `server` are the two phases, not plugin names.
            if key == "bootstrap" || key == "server" {
                continue;
            }
            pending = Some((indent, key.to_string()));
            // Paper defaults a dependency to required.
            required = true;
        }
        if let Some((_, plugin)) = pending {
            if required {
                requires.push(plugin_id(&plugin));
            }
        }
    }

    Some(JarMeta {
        name: Some(name.clone()),
        version: root_scalar(text, "version"),
        primary: vec![plugin_id(&name)],
        bundled: Vec::new(),
        requires,
        // paper-plugin.yml has no commands section — Paper plugins register
        // theirs in code, the way a mod does.
        commands: Vec::new(),
    })
}

/// `fabric.mod.json`, and Quilt's variant of it.
///
/// Fabric writes `depends` as an object of id to version range; Quilt writes an
/// array, whose entries may be bare ids or objects that can mark themselves
/// optional. Both shapes appear in the wild, sometimes in the same folder.
fn from_fabric(value: &serde_json::Value) -> JarMeta {
    let root = value.get("quilt_loader").unwrap_or(value);
    let named = root.get("metadata").unwrap_or(root);

    let mut primary = Vec::new();
    if let Some(id) = root.get("id").and_then(|v| v.as_str()) {
        primary.push(id.to_string());
    }
    if let Some(list) = root.get("provides").and_then(|v| v.as_array()) {
        primary.extend(list.iter().filter_map(entry_id));
    }

    let mut requires = Vec::new();
    match root.get("depends") {
        Some(serde_json::Value::Object(map)) => {
            requires.extend(map.keys().cloned());
        }
        Some(serde_json::Value::Array(list)) => {
            requires.extend(
                list.iter()
                    .filter(|d| d.get("optional").and_then(|o| o.as_bool()) != Some(true))
                    .filter_map(entry_id),
            );
        }
        _ => {}
    }

    JarMeta {
        name: named
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        version: root
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        primary,
        bundled: Vec::new(),
        requires,
        commands: Vec::new(),
    }
}

/// An id written either bare or as an object with an `id` field.
fn entry_id(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| value.get("id").and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// `META-INF/neoforge.mods.toml`, and Forge's older `mods.toml`.
///
/// Dependencies live under `[[dependencies.<modid>]]`, one array per mod in the
/// file. Older files mark a dependency with `mandatory`, newer ones with
/// `type`, and both still ship.
fn from_mods_toml(text: &str) -> Option<JarMeta> {
    let root: toml::Table = text.parse().ok()?;
    let mods = root.get("mods")?.as_array()?;

    let primary: Vec<String> = mods
        .iter()
        .filter_map(|m| m.get("modId").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect();

    let mut requires = Vec::new();
    if let Some(table) = root.get("dependencies").and_then(|v| v.as_table()) {
        for entries in table.values().filter_map(|v| v.as_array()) {
            for entry in entries {
                let required = match entry.get("type").and_then(|v| v.as_str()) {
                    Some(kind) => kind.eq_ignore_ascii_case("required"),
                    // Pre-`type` files; absent `mandatory` meant required.
                    None => entry
                        .get("mandatory")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                };
                if required {
                    if let Some(id) = entry.get("modId").and_then(|v| v.as_str()) {
                        requires.push(id.to_string());
                    }
                }
            }
        }
    }

    let first = mods.first();
    Some(JarMeta {
        name: first
            .and_then(|m| m.get("displayName"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        version: first
            .and_then(|m| m.get("version"))
            .and_then(|v| v.as_str())
            // `${file.jarVersion}` is filled in by the loader from the jar
            // manifest, so it says nothing here.
            .filter(|v| !v.starts_with("${"))
            .map(str::to_string),
        primary,
        bundled: Vec::new(),
        requires,
        commands: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_fabric(json: &str) -> JarMeta {
        let mut meta = from_fabric(&serde_json::from_str(json).unwrap());
        normalise(&mut meta);
        meta
    }

    #[test]
    fn reads_a_fabric_mod() {
        let meta = parse_fabric(
            r#"{"id":"sodium","version":"0.5.0","name":"Sodium",
                "depends":{"minecraft":">=1.21","fabricloader":"*","fabric-api":"*"}}"#,
        );
        assert_eq!(meta.name.as_deref(), Some("Sodium"));
        assert_eq!(meta.primary, vec!["sodium"]);
        // minecraft and the loader are the platform, not installed files.
        assert_eq!(meta.requires, vec!["fabric-api"]);
    }

    #[test]
    fn a_bukkit_plugin_declares_itself() {
        // Deliberately carries the traps: a command named `version` above the
        // real one, a block list at its key's own indent, and two keys that
        // look like dependencies and are not.
        let meta = from_plugin_yml(
            r#"
name: Shop Keeper
main: com.example.Main
commands:
  version:
    description: shows the version
version: '2.14.0'
depend: [Vault, WorldEdit]
softdepend:
- PlaceholderAPI
loadbefore:
  - SomeOtherPlugin
libraries:
  - com.zaxxer:HikariCP:5.1.0
"#,
        )
        .unwrap();

        assert_eq!(meta.name.as_deref(), Some("Shop Keeper"));
        // The plugin's own version, not the command that shares its name.
        assert_eq!(meta.version.as_deref(), Some("2.14.0"));
        // Bukkit matches names with spaces turned into underscores.
        assert_eq!(meta.primary, vec!["Shop_Keeper"]);
        // Only the hard list. A soft dependency is optional, `loadbefore`
        // points the other way, and a library is a Maven coordinate that will
        // never be a file in the plugins folder.
        assert_eq!(meta.requires, vec!["Vault", "WorldEdit"]);
    }

    #[test]
    fn a_plugin_that_needs_nothing_says_so() {
        let meta = from_plugin_yml("name: Solo\nversion: 1.0\ndepend: []\n").unwrap();
        assert!(meta.requires.is_empty());
        // Taken as written: parsed as a number this would come back as "1".
        assert_eq!(meta.version.as_deref(), Some("1.0"));
    }

    #[test]
    fn a_comment_is_not_part_of_the_value() {
        let meta =
            from_plugin_yml("name: Thing\nversion: 1.0\ndepend: [Vault] # for economy\n").unwrap();
        assert_eq!(meta.requires, vec!["Vault"]);
    }

    #[test]
    fn a_paper_plugin_takes_hardness_from_required_alone() {
        let meta = from_paper_plugin(
            r#"
name: ShopKeeper
version: '2.14.0'
dependencies:
  server:
    Vault:
      load: BEFORE
      required: true
    PlaceholderAPI:
      load: AFTER
      required: false
"#,
        )
        .unwrap();

        // Vault is required and PlaceholderAPI is not. Which way each one
        // loads says nothing about whether it is needed.
        assert_eq!(meta.requires, vec!["Vault"]);
        assert_eq!(meta.primary, vec!["ShopKeeper"]);
    }

    #[test]
    fn reads_quilts_array_of_dependencies() {
        let meta = parse_fabric(
            r#"{"quilt_loader":{"id":"example","version":"1.0",
                "metadata":{"name":"Example"},
                "provides":[{"id":"legacy_name"}],
                "depends":[{"id":"cloth-config"},{"id":"emi","optional":true},"quilt_loader"]}}"#,
        );
        assert_eq!(meta.name.as_deref(), Some("Example"));
        assert_eq!(meta.primary, vec!["example", "legacy_name"]);
        // Optional dependencies are the author saying "works well with", and
        // pulling them into the graph would make half a folder look required.
        assert_eq!(meta.requires, vec!["cloth-config"]);
    }

    /// EssentialsX's own shape, near enough: commands at one column, their
    /// settings deeper, aliases worth as much as the command itself.
    #[test]
    fn reads_the_commands_a_plugin_registers() {
        let meta = from_plugin_yml(
            r#"
name: Essentials
version: 2.20.1
depend: [Vault]
commands:
  heal:
    description: Heals a player
    usage: /<command> [player]
    aliases: [h]
  feed:
    description: Feeds a player
    aliases:
      - eat
  version:
    description: Shows the plugin version
permissions:
  essentials.heal:
    default: op
"#,
        )
        .unwrap();
        // `version` is a command here *and* a root key, which is the trap this
        // file's header warns about: it must be read as one, not the other.
        assert_eq!(meta.version.as_deref(), Some("2.20.1"));
        assert_eq!(meta.commands, vec!["heal", "h", "feed", "version"]);
        // The section ends where indentation returns; permissions are not
        // commands, and `essentials.heal` is nobody's command name.
        assert!(!meta.commands.iter().any(|c| c.contains("essentials")));
    }

    #[test]
    fn a_plugin_with_no_commands_declares_none() {
        let meta = from_plugin_yml("name: Vault\nversion: 1.7\n").unwrap();
        assert!(meta.commands.is_empty());
    }

    #[test]
    fn reads_a_neoforge_mod() {
        let meta = from_mods_toml(
            r#"
            modLoader = "javafml"
            [[mods]]
            modId = "jei"
            displayName = "Just Enough Items"
            version = "19.0.0"
            [[dependencies.jei]]
            modId = "neoforge"
            type = "required"
            [[dependencies.jei]]
            modId = "cloth_config"
            type = "required"
            [[dependencies.jei]]
            modId = "emi"
            type = "optional"
            "#,
        )
        .map(|mut m| {
            normalise(&mut m);
            m
        })
        .unwrap();
        assert_eq!(meta.name.as_deref(), Some("Just Enough Items"));
        assert_eq!(meta.version.as_deref(), Some("19.0.0"));
        assert_eq!(meta.primary, vec!["jei"]);
        assert_eq!(meta.requires, vec!["cloth_config"]);
    }

    #[test]
    fn treats_a_forge_dependency_without_a_type_as_required() {
        let meta = from_mods_toml(
            r#"
            [[mods]]
            modId = "old"
            displayName = "Old Mod"
            version = "${file.jarVersion}"
            [[dependencies.old]]
            modId = "patchouli"
            [[dependencies.old]]
            modId = "jei"
            mandatory = false
            "#,
        )
        .map(|mut m| {
            normalise(&mut m);
            m
        })
        .unwrap();
        // A version the loader substitutes tells the user nothing.
        assert_eq!(meta.version, None);
        assert_eq!(meta.requires, vec!["patchouli"]);
    }

    #[test]
    fn a_bundled_id_stays_out_of_the_jars_own_identity() {
        let mut meta = JarMeta {
            primary: vec!["modmenu".into()],
            // A jar can bundle a copy of something it also names itself.
            bundled: vec!["modmenu".into(), "fabric-resource-loader-v0".into()],
            requires: vec![
                "fabric-resource-loader-v0".into(),
                "minecraft".into(),
                "sodium".into(),
            ],
            ..Default::default()
        };
        normalise(&mut meta);
        assert_eq!(meta.primary, vec!["modmenu"]);
        assert_eq!(meta.bundled, vec!["fabric-resource-loader-v0"]);
        // The requirement survives being bundled: an installed Fabric API is
        // still what the loader would use, and what the graph should show.
        assert_eq!(meta.requires, vec!["fabric-resource-loader-v0", "sodium"]);
    }
}

//! Reading a Minecraft crash report for the mod that broke.
//!
//! A crash report is a wall of Java, and almost all of it is irrelevant. What
//! matters is in four places, and this reads all four rather than guessing
//! from any one of them:
//!
//! - the `Description:` line and the exception under it, which say what the
//!   game was doing;
//! - Forge's `Suspected Mods:` block, which is the loader's own conclusion;
//! - the sections where the report names a mod outright — Forge's `-- MOD
//!   <id> --`, NeoForge's `-- Mod loading issue for: <id> --`, a
//!   `Loading errors encountered:` list, a mixin that failed to apply;
//! - the stack trace, where a frame's package may belong to a mod.
//!
//! Those are not equally good evidence, and saying so is the point. A frame
//! is the weakest: half the stack of any modded crash belongs to mods that
//! did nothing wrong, and the topmost mod frame is routinely a mod whose
//! mixin merely sits on the method that broke. A mod the report names is
//! better, because the report named it. `Suspected Mods` is better still,
//! because that is the loader having already worked it out. So every suspect
//! carries which of the three it came from and the line it came from, and
//! when none of them names anything the answer is that nothing does — not
//! whichever mod happens to appear first in a trace.
//!
//! Nothing here reads the loader's version or the game's to decide how to
//! parse: the shapes below are all present in reports from the same loader
//! version, and a report either has a section or does not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// How much of a line is quoted back as the evidence. A stack frame carries
/// several hundred characters of mixin bookkeeping after the part that means
/// anything, and a quote nobody can read is not evidence.
const MAX_QUOTE: usize = 240;

/// How many suspects are worth offering. Past a handful this stops being an
/// answer and becomes the wall of text it replaced.
const MAX_SUSPECTS: usize = 4;

/// How many of those may be stack frames — the weakest evidence, and the only
/// kind a modded crash supplies in bulk.
const MAX_FRAMES: usize = 2;

/// A mod this instance has a record of, for matching a suspect to a file that
/// can actually be turned off.
#[derive(Debug, Clone)]
pub struct Known {
    /// The name on disk, which is what turning it off goes by. Already
    /// carries `.disabled` when it is off.
    pub file: String,
    pub name: String,
    pub enabled: bool,
    /// The ids the jar answers to. Display names vary between loaders; ids do
    /// not, so this is what a report is matched against.
    pub mod_ids: Vec<String>,
}

/// Where a suspect came from, worst first — so the derived order ranks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Evidence {
    /// A stack frame whose package belongs to this mod. Weak: it may be the
    /// mod that broke, or a mod whose mixin sits on the method that broke.
    Frame,
    /// The report names this mod as the one that failed.
    Named,
    /// The loader's own `Suspected Mods` conclusion.
    Suspected,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mod_id: Option<String>,
    /// The mod's display name, as the report gave it or as its own mod list
    /// does. `None` when the report only ever named an id or a jar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The jar the report named, when it named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jar: Option<String>,
    pub evidence: Evidence,
    /// The line the conclusion was drawn from, so nobody has to take this on
    /// trust.
    pub quote: String,
    /// The installed file to turn off. `None` when this instance has no
    /// record of the mod — a crash report from a pack that has since been
    /// changed names mods that are not here any more, and offering to turn one
    /// of those off would be offering to do nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Whether that file is switched on right now.
    pub enabled: bool,
}

/// A parsed crash report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// The file it was read from.
    pub name: String,
    /// Unix seconds, from the file itself.
    pub modified: i64,
    /// What the game said it was doing. Empty when the report has no
    /// `Description:` line, which a truncated one may not.
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<String>,
    /// Strongest evidence first. Empty means the report names no mod, which is
    /// an answer and has to be shown as one.
    pub suspects: Vec<Suspect>,
}

/// A crash report found on disk.
pub struct Found {
    pub name: String,
    pub modified: i64,
    pub text: String,
}

/// The most recent crash report in `dir`, if one was written.
pub async fn newest(dir: &Path) -> Option<Found> {
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().map(|e| e != "txt").unwrap_or(true) {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if best.as_ref().map(|(seen, _)| modified > *seen).unwrap_or(true) {
            best = Some((modified, path));
        }
    }
    let (modified, path) = best?;
    let text = tokio::fs::read_to_string(&path).await.ok()?;
    Some(Found {
        name: path.file_name()?.to_string_lossy().to_string(),
        modified: modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default(),
        text,
    })
}

/// Read a crash report, matching whatever it names against what is installed.
pub fn analyse(found: Found, known: &[Known]) -> Report {
    let lines: Vec<&str> = found.text.lines().map(|l| l.trim_end()).collect();
    let catalog = mod_list(&lines);

    let mut suspects: Vec<Suspect> = Vec::new();
    collect(&mut suspects, suspected(&lines));
    collect(&mut suspects, named(&lines));
    collect(&mut suspects, frames(&lines, known));

    for suspect in suspects.iter_mut() {
        // The report's own mod list is where an id becomes a name and a jar.
        // Guessing either from the id would be wrong often enough to matter:
        // Oculus ships as `mekalus-mc1.20.1-1.8.0.1.jar`.
        if let Some(entry) = suspect.mod_id.as_ref().and_then(|id| catalog.get(id)) {
            suspect.name = suspect.name.clone().or_else(|| Some(entry.name.clone()));
            // Fabric's listing has no jar column, so there is nothing to fill
            // in from it — and an empty file name is worse than none.
            if suspect.jar.is_none() && !entry.jar.is_empty() {
                suspect.jar = Some(entry.jar.clone());
            }
        }
        if let Some(installed) = matching(suspect, known) {
            suspect.file = Some(installed.file.clone());
            suspect.enabled = installed.enabled;
            // What this machine calls it beats what the report called it: that
            // is the name on the row the button turns off.
            suspect.name = Some(installed.name.clone());
        }
    }

    // Two sections about one mod — a jar refused, then the mod inside it named
    // — are one suspect, which is only knowable once both have reached the
    // same file.
    let mut shown: Vec<String> = Vec::new();
    suspects.retain(|suspect| match &suspect.file {
        Some(file) if shown.contains(file) => false,
        Some(file) => {
            shown.push(file.clone());
            true
        }
        None => true,
    });

    suspects.truncate(MAX_SUSPECTS);
    Report {
        name: found.name,
        modified: found.modified,
        description: description(&lines).unwrap_or_default(),
        exception: exception(&lines),
        suspects,
    }
}

/// Add suspects, keeping the strongest evidence for each mod and the order
/// they were found in within one kind of evidence.
///
/// Called strongest first, so a mod already here was found on better evidence
/// than whatever is being added for it now.
fn collect(into: &mut Vec<Suspect>, found: Vec<Suspect>) {
    for suspect in found {
        let same = |a: &Suspect| match (&a.mod_id, &suspect.mod_id) {
            (Some(x), Some(y)) => x == y,
            // Two sections naming the same jar and no id — the two halves of
            // one NeoForge rejection — are one suspect, not two.
            _ => a.jar.is_some() && a.jar == suspect.jar,
        };
        if into.iter().any(same) {
            continue;
        }
        into.push(suspect);
    }
}

/// The installed mod a suspect is about, by id first and by jar name second.
///
/// By id because ids are what a report names and what a jar answers to; by
/// jar name because a report that could not read a jar at all still printed
/// its file name, and that is then the only handle there is.
fn matching<'a>(suspect: &Suspect, known: &'a [Known]) -> Option<&'a Known> {
    if let Some(id) = &suspect.mod_id {
        let found = known.iter().find(|k| {
            k.mod_ids
                .iter()
                .any(|other| other.eq_ignore_ascii_case(id))
        });
        if found.is_some() {
            return found;
        }
    }
    let jar = suspect.jar.as_ref()?;
    known
        .iter()
        .find(|k| k.file.trim_end_matches(".disabled").eq_ignore_ascii_case(jar))
}

/// What the game said it was doing.
fn description(lines: &[&str]) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix("Description:"))
        .map(|rest| rest.trim().to_string())
}

/// The throwable under the description: the first unindented line of code-like
/// text after it, before the walkthrough separator.
///
/// Unindented because every continuation — the frames, the `- Mod (id) …`
/// entries of a loader error — is indented, and the first line is the one
/// naming the exception.
fn exception(lines: &[&str]) -> Option<String> {
    let after = lines
        .iter()
        .position(|line| line.starts_with("Description:"))?;
    lines
        .iter()
        .skip(after + 1)
        .take_while(|line| !line.starts_with("A detailed walkthrough"))
        .find(|line| {
            !line.trim().is_empty()
                && !line.starts_with(char::is_whitespace)
                && !line.starts_with("//")
                && !line.starts_with('#')
        })
        .map(|line| quote(line))
}

/// Forge's own conclusion, when it reached one.
///
/// The suspects are the indented lines under the header, so the `NONE` Forge
/// writes on the header line when it could not tell — which is most reports —
/// is not read at all. It could not name a mod if it were: ids are lower case
/// and that is not.
fn suspected(lines: &[&str]) -> Vec<Suspect> {
    let Some(at) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("Suspected Mods:"))
    else {
        return Vec::new();
    };
    entries(lines, at)
        .into_iter()
        .filter_map(|line| {
            let (name, mod_id) = named_mod(line)?;
            Some(Suspect {
                mod_id: Some(mod_id),
                name,
                jar: None,
                evidence: Evidence::Suspected,
                quote: quote(line),
                file: None,
                enabled: false,
            })
        })
        .collect()
}

/// Every mod the report names as the one that failed.
fn named(lines: &[&str]) -> Vec<Suspect> {
    let mut out = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if let Some(found) = loading_issue(lines, at, line) {
            out.push(found);
        } else if line.trim_start().starts_with("- ") || line.contains("encountered an error") {
            // A `Loading errors encountered:` entry, which NeoForge writes
            // under the exception rather than in a section of its own.
            if let Some((name, mod_id)) = named_mod(line) {
                out.push(Suspect {
                    mod_id: Some(mod_id),
                    name,
                    jar: None,
                    evidence: Evidence::Named,
                    quote: quote(line),
                    file: None,
                    enabled: false,
                });
            }
        } else if let Some(mod_id) = failed_mixin(line) {
            out.push(Suspect {
                mod_id: Some(mod_id),
                name: None,
                jar: None,
                evidence: Evidence::Named,
                quote: quote(line),
                file: None,
                enabled: false,
            });
        }
    }
    out
}

/// A section about one mod that would not load: Forge's `-- MOD <id> --` and
/// NeoForge's `-- Mod loading issue[ for: <id>] --`.
///
/// NeoForge writes the headerless form for a file it could not read as a mod
/// at all, and then the only name there is is the jar in the failure message.
fn loading_issue(lines: &[&str], at: usize, line: &str) -> Option<Suspect> {
    let header = line.strip_prefix("-- ")?.strip_suffix(" --")?;
    let lower = header.to_ascii_lowercase();
    let mut mod_id = if let Some(rest) = lower.strip_prefix("mod loading issue for:") {
        Some(rest.trim().to_string())
    } else if let Some(rest) = lower.strip_prefix("mod ") {
        Some(rest.trim().to_string())
    } else if lower == "mod loading issue" {
        None
    } else {
        return None;
    };
    mod_id = mod_id.filter(|id| is_mod_id(id));

    let body = section(lines, at);
    let mut jar = None;
    // The line the conclusion came from, which is the one worth quoting. The
    // failure message is the fallback: it says what went wrong even when the
    // name came from the header.
    let mut from = None;
    let mut reason = None;
    for line in &body {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            if key == "mod file" && !value.starts_with('<') {
                jar = file_name(value);
            }
            if key == "failure message" {
                reason = Some(*line);
            }
        }
    }
    // A rejected jar's name is in prose in the failure message, and so is a
    // mod whose requirement could not be met — the section header names
    // neither.
    if jar.is_none() {
        if let Some((line, found)) = body.iter().find_map(|l| jar_in(l).map(|j| (*l, j))) {
            jar = Some(found);
            from = Some(line);
        }
    }
    if mod_id.is_none() {
        if let Some((line, found)) = body.iter().find_map(|l| {
            l.contains("mod ")
                .then(|| named_mod(l))
                .flatten()
                .map(|(_, id)| (*l, id))
        }) {
            mod_id = Some(found);
            from = Some(line);
        }
    }
    if mod_id.is_none() && jar.is_none() {
        return None;
    }
    let quoted = from
        .or(reason)
        .map(|line| line.split_once(':').map_or(line, |(_, rest)| rest))
        .unwrap_or(header);
    Some(Suspect {
        mod_id,
        name: None,
        jar,
        evidence: Evidence::Named,
        quote: quote(quoted),
        file: None,
        enabled: false,
    })
}

/// One section's own lines: after its `-- X --` header, up to the next section
/// — and never into the stack trace below it, which is hundreds of frames
/// carrying jar names and mod ids belonging to mods this section is not about.
fn section<'a>(lines: &[&'a str], at: usize) -> Vec<&'a str> {
    lines
        .iter()
        .skip(at + 1)
        .take_while(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("-- ")
                && trimmed != "Stacktrace:"
                && !trimmed.starts_with("A detailed walkthrough")
        })
        .filter(|line| !line.trim().is_empty())
        .copied()
        .collect()
}

/// The mod owning a mixin that failed to apply.
///
/// `from mod <id>` on its own means nothing: on NeoForge every frame lists
/// every mixin that touched the method, each with its owner. Only a line
/// saying the mixin *failed* names a culprit, and a stack frame never is one.
fn failed_mixin(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("at ") || !trimmed.contains("from mod ") {
        return None;
    }
    if !trimmed.contains("FAILED") && !trimmed.contains("MixinApplyError") {
        return None;
    }
    let rest = trimmed.split("from mod ").nth(1)?;
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .collect();
    is_mod_id(&id).then_some(id)
}

/// Package segments in the stack trace that belong to a mod this instance has.
///
/// Measured against what is installed rather than against the report's own mod
/// list, because a frame is only worth raising at all when there is a file
/// behind it — and because an id that is not installed cannot be turned off.
fn frames(lines: &[&str], known: &[Known]) -> Vec<Suspect> {
    let mut out = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        let Some(call) = trimmed.strip_prefix("at ") else {
            continue;
        };
        for segment in segments(call) {
            if STOP_WORDS.contains(&segment) {
                continue;
            }
            let Some(installed) = known.iter().find(|k| {
                k.mod_ids
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case(segment))
            }) else {
                continue;
            };
            let id = segment.to_ascii_lowercase();
            if out.iter().any(|s: &Suspect| s.mod_id.as_deref() == Some(&id)) {
                continue;
            }
            out.push(Suspect {
                mod_id: Some(id),
                name: Some(installed.name.clone()),
                jar: None,
                evidence: Evidence::Frame,
                quote: quote(trimmed),
                file: None,
                enabled: false,
            });
            if out.len() == MAX_FRAMES {
                return out;
            }
        }
    }
    out
}

/// The dotted package of one stack frame, as its segments.
///
/// A frame reads `at knot//net.vulkanmod.vulkan.Vulkan.initVulkan(Vulkan.java:139)`
/// or `at TRANSFORMER/minecraft@1.21.1/net.minecraft.client.Minecraft.<init>(…)`,
/// and may trail hundreds of characters of jar and mixin bookkeeping — which
/// holds jar names and `from mod` ids that are not what this frame is. So
/// everything from the call parentheses on is dropped, and so is every
/// loader-layer prefix before the package.
///
/// The method at the end goes too. It is not part of any package, and a
/// method may be called anything: Mixin's own
/// `InjectionInfo$InjectorEntry.create` is in the stack of every mixin that
/// failed to apply, and reading that as the mod Create offered to turn Create
/// off for a crash in somebody else's mixin.
fn segments(call: &str) -> Vec<&str> {
    let code = call.split('(').next().unwrap_or(call);
    let package = code.rsplit('/').next().unwrap_or(code);
    let mut parts: Vec<&str> = package.split('.').filter(|s| !s.is_empty()).collect();
    parts.pop();
    parts
}

/// Package segments that are nobody's mod id, however much they look like one.
const STOP_WORDS: &[&str] = &[
    "api", "base", "client", "com", "common", "core", "dev", "impl", "internal", "io", "java",
    "jdk", "knot", "lib", "loader", "main", "me", "minecraft", "mixin", "mixins", "mod", "mods",
    "net", "org", "server", "sun", "util", "utils",
];

/// Display name and jar for each mod the report lists, by id.
struct Listed {
    name: String,
    jar: String,
}

/// The report's own list of what was loaded.
///
/// Two shapes. Forge and NeoForge write a `Mod List:` table of
/// `jar | Name | id | version | state | manifest`; Fabric writes `Fabric
/// Mods:` as `id: Name version`, nested by dependency. Neither is evidence
/// about the crash — both are how an id gets a readable name.
fn mod_list(lines: &[&str]) -> HashMap<String, Listed> {
    let mut out = HashMap::new();

    if let Some(at) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("Mod List:"))
    {
        for entry in deeper(lines, at) {
            let columns: Vec<&str> = entry.split('|').map(str::trim).collect();
            if columns.len() < 3 || !is_mod_id(columns[2]) {
                continue;
            }
            out.insert(
                columns[2].to_string(),
                Listed {
                    name: columns[1].to_string(),
                    jar: columns[0].to_string(),
                },
            );
        }
    }

    if let Some(at) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("Fabric Mods:"))
    {
        for entry in deeper(lines, at) {
            let Some((id, rest)) = entry.trim().split_once(": ") else {
                continue;
            };
            if !is_mod_id(id) {
                continue;
            }
            // `Create 0.5.1-j-build.1631+mc1.20.1`: the version is the last
            // word, and a name may have spaces in it.
            let name = rest.trim().rsplit_once(' ').map(|(n, _)| n).unwrap_or(rest);
            out.insert(
                id.to_string(),
                Listed {
                    name: name.trim().to_string(),
                    // Fabric lists no jar at all. The id is the handle, and a
                    // guessed file name would be a wrong one.
                    jar: String::new(),
                },
            );
        }
    }
    out.retain(|_, listed| !listed.name.is_empty());
    out
}

/// The lines of the block headed by `lines[at]`: everything indented further,
/// up to the next line that is not.
fn deeper<'a>(lines: &[&'a str], at: usize) -> Vec<&'a str> {
    let head = indent(lines[at]);
    lines
        .iter()
        .skip(at + 1)
        .take_while(|line| line.trim().is_empty() || indent(line) > head)
        .filter(|line| !line.trim().is_empty())
        .copied()
        .collect()
}

/// The block's own entries: its least-indented lines, so a suspect's issue
/// tracker URL or a dependency's details are not read as entries of their own.
fn entries<'a>(lines: &[&'a str], at: usize) -> Vec<&'a str> {
    let block = deeper(lines, at);
    let Some(shallowest) = block.iter().map(|line| indent(line)).min() else {
        return block;
    };
    block
        .into_iter()
        .filter(|line| indent(line) == shallowest)
        .collect()
}

/// How deep a line is indented. A tab counts as one, which is all these
/// reports use.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// A mod and its id out of prose: `Mekalus (Oculus) (oculus), Version: …`,
/// `- SpaceCraft (spacecraft) encountered an error …`, `- mod Voxy (voxy)
/// 0.2.14-alpha requires …`.
///
/// The id is the first parenthesised group that reads like one, which is what
/// gets this right when the display name has brackets of its own.
fn named_mod(line: &str) -> Option<(Option<String>, String)> {
    let mut rest = line;
    let mut consumed = 0;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let close = after.find(')')?;
        let candidate = &after[..close];
        if is_mod_id(candidate) {
            let before = line[..consumed + open].trim();
            // `net.minecraftforge.fml.ModLoadingException: LowDragLib (ldlib)`
            // — the mod's name is what follows the exception that carried it,
            // not the whole line.
            let before = before.rsplit_once(": ").map_or(before, |(_, name)| name);
            let name = before
                .trim_start_matches(['-', '*', '•'])
                .trim()
                .trim_start_matches("mod ")
                .trim();
            return Some((
                (!name.is_empty()).then(|| name.to_string()),
                candidate.to_string(),
            ));
        }
        consumed += open + 1 + close + 1;
        rest = &after[close + 1..];
    }
    None
}

/// Whether this reads like a mod id. Both loaders require lower case, and
/// requiring it is what keeps a display name or a class name out.
fn is_mod_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
}

/// The file name at the end of a path, whichever slash the report used. A
/// Windows path in a report read on Linux still has to come apart.
fn file_name(path: &str) -> Option<String> {
    let name = path.trim().rsplit(['/', '\\']).next()?.trim();
    name.to_ascii_lowercase()
        .ends_with(".jar")
        .then(|| name.to_string())
}

/// A jar named in prose: `File mods\voxy-0.2.14-alpha.jar is a Fabric mod …`.
fn jar_in(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|word| word.to_ascii_lowercase().ends_with(".jar"))
        .and_then(file_name)
}

/// One line, trimmed and cut to something a person can read.
fn quote(line: &str) -> String {
    let text = line.trim();
    if text.len() <= MAX_QUOTE {
        return text.to_string();
    }
    let mut end = MAX_QUOTE;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from real crash reports on a real machine. Invented ones would
    /// have agreed with whatever this parser does; these did not.
    const FORGE_MOD_FAILED: &str = include_str!("crash/forge-mod-failed.txt");
    const FORGE_SUSPECTED: &str = include_str!("crash/forge-suspected.txt");
    const FORGE_MOD_LIST: &str = include_str!("crash/forge-mod-list.txt");
    const NEOFORGE_MOD_ISSUE: &str = include_str!("crash/neoforge-mod-issue.txt");
    const NEOFORGE_REJECTED_JAR: &str = include_str!("crash/neoforge-rejected-jar.txt");
    const NEOFORGE_LOADING_ERRORS: &str = include_str!("crash/neoforge-loading-errors.txt");
    const FORGE_LOADING_EXCEPTION: &str = include_str!("crash/forge-loading-exception.txt");
    const FABRIC_MIXIN_APPLY: &str = include_str!("crash/fabric-mixin-apply.txt");
    const NO_MOD_NAMED: &str = include_str!("crash/no-mod-named.txt");

    fn read(text: &str, known: &[Known]) -> Report {
        analyse(
            Found {
                name: "crash-2026-10-21_12.00.00-client.txt".into(),
                modified: 1_700_000_000,
                text: text.to_string(),
            },
            known,
        )
    }

    fn installed(file: &str, name: &str, ids: &[&str]) -> Known {
        Known {
            file: file.into(),
            name: name.into(),
            enabled: true,
            mod_ids: ids.iter().map(|i| i.to_string()).collect(),
        }
    }

    fn ids(report: &Report) -> Vec<&str> {
        report
            .suspects
            .iter()
            .filter_map(|s| s.mod_id.as_deref())
            .collect()
    }

    #[test]
    fn the_description_and_the_exception_are_read_off_the_top() {
        let report = read(FORGE_MOD_FAILED, &[]);
        assert_eq!(report.description, "Mod loading error has occurred");
        assert_eq!(
            report.exception.as_deref(),
            Some("java.lang.Exception: Mod Loading has failed"),
            "the exception line carries a trailing carriage return in real reports"
        );
    }

    #[test]
    fn a_forge_mod_section_names_the_mod_that_failed_to_load() {
        let report = read(FORGE_MOD_FAILED, &[]);
        let quark = &report.suspects[0];
        assert_eq!(quark.mod_id.as_deref(), Some("quark"));
        assert_eq!(quark.evidence, Evidence::Named);
        assert_eq!(quark.jar.as_deref(), Some("Quark-4.0-462.jar"));
        assert_eq!(
            quark.quote, "Quark (quark) has failed to load correctly",
            "the quote is the report's own failure message"
        );
    }

    #[test]
    fn suspected_mods_outranks_everything_else_in_the_report() {
        let report = read(FORGE_SUSPECTED, &[]);
        let first = &report.suspects[0];
        assert_eq!(first.mod_id.as_deref(), Some("oculus"));
        assert_eq!(first.evidence, Evidence::Suspected);
        assert_eq!(
            first.name.as_deref(),
            Some("Mekalus (Oculus)"),
            "the display name has brackets of its own, and the id is the inner pair"
        );
    }

    #[test]
    fn a_neoforge_mod_loading_issue_names_the_mod_and_its_jar() {
        let report = read(NEOFORGE_MOD_ISSUE, &[]);
        let first = &report.suspects[0];
        assert_eq!(first.mod_id.as_deref(), Some("colorwheel"));
        assert_eq!(
            first.jar.as_deref(),
            Some("colorwheel-neoforge-1.2.9+mc1.21.1.jar")
        );
        assert!(first.quote.starts_with("Mod colorwheel is incompatible"));
    }

    /// A jar the loader would not read as a mod at all has no id in its
    /// section — only a file name, in prose, with a Windows separator.
    #[test]
    fn a_rejected_jar_is_named_even_with_no_mod_information() {
        let report = read(NEOFORGE_REJECTED_JAR, &[]);
        let jars: Vec<&str> = report.suspects.iter().filter_map(|s| s.jar.as_deref()).collect();
        assert!(
            jars.contains(&"voxy-0.2.14-alpha.jar"),
            "the jar in the failure message was missed: {jars:?}"
        );
        assert!(
            ids(&report).contains(&"voxy"),
            "the second issue names the mod outright: {:?}",
            ids(&report)
        );
    }

    #[test]
    fn a_loading_error_names_the_mod_that_raised_it() {
        let report = read(NEOFORGE_LOADING_ERRORS, &[]);
        assert_eq!(ids(&report), vec!["spacecraft"]);
        assert_eq!(report.suspects[0].evidence, Evidence::Named);
    }

    /// Every NeoForge stack frame lists every mixin that touched the method,
    /// each with the mod it came from. Reading those as blame would accuse a
    /// dozen innocent mods of every crash.
    #[test]
    fn from_mod_in_a_stack_frame_is_not_evidence() {
        let report = read(NEOFORGE_LOADING_ERRORS, &[]);
        for innocent in ["create", "veil", "flywheel", "sable", "ponder"] {
            assert!(
                !ids(&report).contains(&innocent),
                "{innocent} was blamed for a mixin it merely has: {:?}",
                ids(&report)
            );
        }
    }

    #[test]
    fn a_mixin_that_failed_to_apply_names_the_mod_it_came_from() {
        let report = read(FABRIC_MIXIN_APPLY, &[]);
        assert!(
            ids(&report).contains(&"valkyrienskies"),
            "the failing mixin's owner was missed: {:?}",
            ids(&report)
        );
        assert_eq!(report.suspects[0].evidence, Evidence::Named);
    }

    #[test]
    fn a_fabric_mod_listing_gives_an_id_its_display_name() {
        let report = read(FABRIC_MIXIN_APPLY, &[]);
        // Nothing in this report blames Create; the listing is only where a
        // name comes from, so check it through the parser that reads it.
        let listed = mod_list(&FABRIC_MIXIN_APPLY.lines().collect::<Vec<_>>());
        assert_eq!(listed.get("create").map(|l| l.name.as_str()), Some("Create"));
        assert!(listed.get("create").unwrap().jar.is_empty());
        assert!(!report.suspects.is_empty());
    }

    /// Forge's table is the only place an id and its jar appear together, and
    /// the jar is not derivable from the id: Oculus ships as `mekalus-…`.
    #[test]
    fn a_forge_mod_list_gives_an_id_its_name_and_its_jar() {
        let report = read(FORGE_MOD_LIST, &[]);
        let first = &report.suspects[0];
        assert_eq!(first.mod_id.as_deref(), Some("oculus"));
        assert_eq!(first.name.as_deref(), Some("Mekalus (Oculus)"));
        assert_eq!(first.jar.as_deref(), Some("mekalus-mc1.20.1-1.8.0.1.jar"));
    }

    /// The honest answer when the report names nothing. A vanilla-shaped crash
    /// has a stack full of the game's own code and the loader's, and blaming
    /// whatever appears first in it would be a guess dressed as a finding.
    #[test]
    fn a_report_naming_no_mod_produces_no_suspects() {
        let report = read(NO_MOD_NAMED, &[]);
        assert!(
            report.suspects.is_empty(),
            "invented a culprit: {:?}",
            report.suspects
        );
        assert_eq!(report.description, "Initializing game");
        assert!(report.exception.unwrap().contains("OutOfMemoryError"));
    }

    /// The same report, with the mod whose package is in the trace installed.
    /// Now there is a frame worth raising — as a frame, which is the weakest
    /// thing this says.
    #[test]
    fn a_frame_is_only_raised_for_a_mod_this_instance_has() {
        let known = [installed("vulkanmod-0.5.jar", "VulkanMod", &["vulkanmod"])];
        let report = read(NO_MOD_NAMED, &known);
        assert_eq!(ids(&report), vec!["vulkanmod"]);
        let frame = &report.suspects[0];
        assert_eq!(frame.evidence, Evidence::Frame);
        assert_eq!(frame.file.as_deref(), Some("vulkanmod-0.5.jar"));
        assert!(
            frame.quote.starts_with("at knot//net.vulkanmod"),
            "the quote should be the frame itself: {}",
            frame.quote
        );
    }

    /// Only a mod with a record here can be turned off, and the button is what
    /// `file` decides. A report from a pack that has since changed names mods
    /// that are not installed any more.
    #[test]
    fn a_mod_this_instance_does_not_have_is_named_but_not_offered() {
        let report = read(FORGE_MOD_FAILED, &[]);
        assert_eq!(report.suspects[0].mod_id.as_deref(), Some("quark"));
        assert_eq!(report.suspects[0].file, None);

        let known = [installed("Quark-4.0-462.jar", "Quark", &["quark"])];
        let with_it = read(FORGE_MOD_FAILED, &known);
        assert_eq!(with_it.suspects[0].file.as_deref(), Some("Quark-4.0-462.jar"));
        assert!(with_it.suspects[0].enabled);
    }

    /// A jar the loader could not read declares no id, so the file name is the
    /// only handle — and it still has to reach the file.
    #[test]
    fn a_suspect_with_only_a_jar_name_still_finds_its_file() {
        let known = [Known {
            file: "voxy-0.2.14-alpha.jar.disabled".into(),
            name: "Voxy".into(),
            enabled: false,
            mod_ids: vec![],
        }];
        let report = read(NEOFORGE_REJECTED_JAR, &known);
        let found = report
            .suspects
            .iter()
            .find(|s| s.jar.as_deref() == Some("voxy-0.2.14-alpha.jar"))
            .expect("the rejected jar");
        assert_eq!(found.file.as_deref(), Some("voxy-0.2.14-alpha.jar.disabled"));
        assert!(!found.enabled, "it is already off, so do not offer to turn it off");
    }

    /// Forge puts the whole thing on one line, exception class and all, and
    /// the mod's name is the part after it — not the part the class is in.
    #[test]
    fn a_mods_name_is_not_the_exception_that_carried_it() {
        let report = read(FORGE_LOADING_EXCEPTION, &[]);
        let first = &report.suspects[0];
        assert_eq!(first.mod_id.as_deref(), Some("ldlib"));
        assert_eq!(first.name.as_deref(), Some("LowDragLib"));
        assert_eq!(first.jar.as_deref(), Some("ldlib-forge-1.20.1-1.0.40.b.jar"));
    }

    #[test]
    fn a_quote_is_cut_to_something_readable() {
        let report = read(FORGE_SUSPECTED, &[]);
        for suspect in &report.suspects {
            assert!(suspect.quote.len() <= MAX_QUOTE + 3, "{}", suspect.quote);
        }
    }

    #[test]
    fn a_mod_id_is_lower_case_and_a_class_name_is_not() {
        assert!(is_mod_id("oculus"));
        assert!(is_mod_id("watermedia_platform_extension"));
        assert!(is_mod_id("sodium-extra"));
        assert!(!is_mod_id("Oculus"));
        assert!(!is_mod_id("net.minecraft.client.Minecraft"));
        assert!(!is_mod_id(""));
        // What Forge writes under `Suspected Mods:` when it could not tell,
        // which must never read as a mod called NONE.
        assert!(!is_mod_id("NONE"));
    }

    /// A frame's trailing bookkeeping holds jar names and `from mod` ids that
    /// belong to other mods entirely, so it has to come off before anything
    /// looks for a package in it.
    #[test]
    fn a_frames_package_stops_at_the_call() {
        let frame = "at TRANSFORMER/minecraft@1.21.1/net.minecraft.client.gui.Gui.initModdedOverlays(Gui.java:1426) ~[client-1.21.1.jar%23185!/:?] {pl:mixin:APP:create.mixins.json:accessor.GuiAccessor from mod create}";
        let found = segments(frame.trim_start_matches("at "));
        assert!(found.contains(&"minecraft"));
        assert!(!found.iter().any(|s| s.contains("create")), "{found:?}");
    }

    /// The last thing in a frame is the method, and a method may be called
    /// anything at all. `InjectionInfo$InjectorEntry.create` is Mixin's own,
    /// and it is in the stack of every mixin that failed to apply — a real
    /// report on this machine, from an instance with Create installed, was
    /// read as Create's fault and offered a button to turn Create off.
    #[test]
    fn a_method_that_shares_a_mods_name_is_not_a_culprit() {
        let known = [installed(
            "create-fabric-0.5.1-j-build.1631+mc1.20.1.jar",
            "Create",
            &["create"],
        )];
        let frame =
            "\tat org.spongepowered.asm.mixin.injection.struct.InjectionInfo$InjectorEntry.create(InjectionInfo.java:196)";
        let found = frames(&[frame], &known);
        assert!(
            found.is_empty(),
            "blamed a mod for a method of its name: {found:?}"
        );
    }

    #[test]
    fn a_generic_package_segment_is_never_a_culprit() {
        // A mod really can declare an id this generic, and `net` or `client`
        // appears in every frame of every crash.
        let known = [installed("thing.jar", "Thing", &["client"])];
        let report = read(NO_MOD_NAMED, &known);
        assert!(report.suspects.is_empty(), "{:?}", report.suspects);
    }
}


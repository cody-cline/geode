//! Packaging up "here is what went wrong" for somebody else to read.
//!
//! When Minecraft breaks, the useful answer almost never comes from the error
//! alone — it comes from the error *plus* what was installed. A crash in
//! `rendering` means nothing on its own and means a great deal once you know
//! the pack is Fabric 1.21 with Sodium and Iris in it. So a report here is the
//! context first and the log second, which is the order a reader needs them in.
//!
//! Assembled in Rust rather than the frontend because an instance and a server
//! need the same thing, differing only in what they call their parts, and
//! because the pieces — installed content, crash reports, log tails — are all
//! already here.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// How much of the report may be log text.
///
/// A clipboard will take far more than this, but a reader will not: past a
/// point the extra lines push the useful ones out of view rather than adding
/// anything. The *end* of a log is kept, since that is where the failure is.
const MAX_LOG: usize = 60_000;

/// How much of a crash report may be included.
///
/// Kept from the *start*, unlike a log: a crash report leads with the
/// exception and the stack, and trails off into a system dump that matters
/// much less.
const MAX_CRASH: usize = 40_000;

/// What the thing being diagnosed is, in the words its own model uses.
pub struct Subject {
    /// "instance" or "server" — used in the prose so the report reads naturally.
    pub kind: &'static str,
    pub name: String,
    /// Ordered facts: ("Minecraft", "1.21.1"), ("Loader", "Forge 52.1.16"), …
    pub facts: Vec<(String, String)>,
    /// Installed mods or plugins, as "name version".
    pub content: Vec<String>,
    /// Where crash reports land, if this kind writes them.
    pub crash_dir: Option<PathBuf>,
}

/// The instruction at the top, addressed to whoever reads it.
///
/// Written for a model but true for a person: say what is wanted, say what is
/// included, and ask for the reasoning rather than only the fix, because a
/// pack that breaks once usually breaks again.
const PROMPT: &str = "\
I need help working out why my Minecraft setup is failing.

Below is the setup's configuration, the list of what is installed, and the \
relevant log or crash report. Please:

1. Identify the actual cause, quoting the line you concluded it from.
2. Say which mod, plugin or setting is responsible, if it is one of them.
3. Give the specific fix — a version to change, a file to edit, a conflict to \
resolve — rather than general advice.
4. If the log does not contain enough to be sure, say what is missing and how \
to get it, instead of guessing.";

/// Build the report. `log` is the text to include, already chosen by the caller
/// — the live console, or a log file the user was looking at.
pub async fn report(subject: &Subject, log: &str) -> String {
    let mut out = String::with_capacity(8 * 1024);
    out.push_str(PROMPT);
    out.push_str("\n\n## The ");
    out.push_str(subject.kind);
    out.push_str("\n\n");
    out.push_str("- Name: ");
    out.push_str(&subject.name);
    out.push('\n');
    for (key, value) in &subject.facts {
        out.push_str("- ");
        out.push_str(key);
        out.push_str(": ");
        out.push_str(value);
        out.push('\n');
    }

    out.push_str("\n## Installed (");
    out.push_str(&subject.content.len().to_string());
    out.push_str(")\n\n");
    if subject.content.is_empty() {
        out.push_str("Nothing — this is an unmodified setup.\n");
    } else {
        for item in &subject.content {
            out.push_str("- ");
            out.push_str(item);
            out.push('\n');
        }
    }

    // A crash report, when there is a fresh one, is worth more than any amount
    // of log: it is the same failure already narrowed down.
    if let Some(dir) = &subject.crash_dir {
        if let Some(found) = crate::crash::newest(dir).await {
            out.push_str("\n## Crash report: ");
            out.push_str(&found.name);
            out.push_str("\n\n```\n");
            out.push_str(&clamp(&found.text, MAX_CRASH, Keep::Start));
            out.push_str("\n```\n");
        }
    }

    out.push_str("\n## Log\n\n```\n");
    let trimmed = log.trim();
    if trimmed.is_empty() {
        out.push_str("(empty — nothing was captured)");
    } else {
        out.push_str(&clamp(trimmed, MAX_LOG, Keep::End));
    }
    out.push_str("\n```\n");
    out
}

enum Keep {
    Start,
    End,
}

/// Cut `text` to `max` bytes, saying so rather than trailing off silently.
///
/// A reader who cannot tell the text was cut will reason about a stack trace
/// that was never there.
fn clamp(text: &str, max: usize, keep: Keep) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let removed = text.len() - max;
    match keep {
        Keep::Start => {
            let end = floor_char_boundary(text, max);
            format!(
                "{}\n\n[... {removed} more bytes trimmed from the end ...]",
                &text[..end]
            )
        }
        Keep::End => {
            let start = ceil_char_boundary(text, text.len() - max);
            format!(
                "[... {removed} earlier bytes trimmed ...]\n\n{}",
                &text[start..]
            )
        }
    }
}

// `str::floor_char_boundary` is still unstable, and slicing a multi-byte
// character in half panics — which would turn "copy my log" into a crash.
fn floor_char_boundary(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_char_boundary(text: &str, mut at: usize) -> usize {
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// Read a log file's tail for the report, whatever its size.
pub async fn tail_of(path: &Path) -> Result<String> {
    let text = tokio::fs::read_to_string(path).await?;
    Ok(clamp(&text, MAX_LOG, Keep::End))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject() -> Subject {
        Subject {
            kind: "instance",
            name: "Test Pack".into(),
            facts: vec![
                ("Minecraft".into(), "1.21.1".into()),
                ("Loader".into(), "Forge 52.1.16".into()),
            ],
            content: vec!["JEI 19.0.0".into(), "Sodium 0.5.11".into()],
            crash_dir: None,
        }
    }

    #[tokio::test]
    async fn the_report_leads_with_what_is_wanted_and_the_setup() {
        let text = report(&subject(), "[12:00:00] [main/INFO]: Hello").await;
        // The ask comes first, because a reader decides what to do with the
        // rest based on it.
        assert!(text.starts_with("I need help working out why"));
        assert!(text.contains("- Minecraft: 1.21.1"));
        assert!(text.contains("- Loader: Forge 52.1.16"));
        assert!(text.contains("## Installed (2)"));
        assert!(text.contains("- JEI 19.0.0"));
        assert!(text.contains("Hello"));
    }

    #[tokio::test]
    async fn an_unmodified_setup_says_so_rather_than_showing_an_empty_list() {
        let mut s = subject();
        s.content.clear();
        let text = report(&s, "x").await;
        assert!(text.contains("## Installed (0)"));
        assert!(text.contains("unmodified setup"));
    }

    #[tokio::test]
    async fn nothing_captured_is_stated_rather_than_left_blank() {
        let text = report(&subject(), "   \n  ").await;
        assert!(text.contains("(empty — nothing was captured)"));
    }

    /// Trimming has to be visible: a reader who thinks they have the whole log
    /// will reason confidently about a cause that was cut out of it.
    #[test]
    fn a_trimmed_log_keeps_its_end_and_says_what_went() {
        let text: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        let cut = clamp(&text, 200, Keep::End);
        assert!(cut.len() < text.len());
        assert!(cut.contains("earlier bytes trimmed"));
        assert!(cut.trim_end().ends_with("line 4999"), "the end is the point");
    }

    #[test]
    fn a_trimmed_crash_report_keeps_its_start() {
        let text = format!("java.lang.NullPointerException\n{}", "at foo\n".repeat(5000));
        let cut = clamp(&text, 200, Keep::Start);
        assert!(cut.starts_with("java.lang.NullPointerException"));
        assert!(cut.contains("trimmed from the end"));
    }

    /// Cutting a multi-byte character in half panics, and "copy my log" is not
    /// a thing that should ever be able to crash.
    #[test]
    fn trimming_never_splits_a_character() {
        let text = "é".repeat(10_000);
        for max in [1, 2, 3, 51, 199, 5001] {
            let _ = clamp(&text, max, Keep::End);
            let _ = clamp(&text, max, Keep::Start);
        }
    }
}

//! Minecraft's own log files, listed, read back, and not kept forever.
//!
//! Both halves of the app want this and want it identically. A server writes
//! `logs/latest.log` and gzips yesterday's into `logs/2026-09-12-1.log.gz`; the
//! game client writes exactly the same shape into its own folder. So nothing
//! here rotates — Minecraft does that — and nothing here knows whether it is
//! looking at a server or an instance. The caller supplies the directory.
//!
//! What this adds is the other half: reading an archive back without leaving
//! the app, and throwing the oldest away so a long-lived setup does not
//! quietly fill a disk with its own history.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};

/// Enough of a log to find what went wrong, and not so much that handing it to
/// the UI stalls the window. The *end* is kept, because that is where the
/// crash is.
const MAX_TEXT: usize = 2 * 1024 * 1024;

/// The most a rotated log may inflate to before it is refused instead.
///
/// `MAX_TEXT` above is a *view*, applied after the whole file is already in
/// memory, so on its own it bounds nothing: a `.log.gz` is the one file in
/// `logs/` written by somebody else's server software, gzip turns a run of
/// spaces into a thousandth of its size, and a one-megabyte archive asking for
/// a gigabyte of memory is a thing the app would simply do.
///
/// Counted on the bytes as they inflate rather than on the size gzip's own
/// trailer states, which a crafted file can understate.
///
/// Refused rather than truncated, because this view keeps the *end* and
/// `take` keeps the beginning — cutting here would throw away the crash and
/// show the boot. 128 MB is far past any real day of log: a busy modded server
/// writing a hundred bytes a line would need over a million lines to reach it.
const MAX_GZIP: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFile {
    pub name: String,
    pub size: u64,
    /// Unix seconds, from the file itself — the name only carries a date.
    pub modified: i64,
    /// The log being written right now, which is never pruned.
    pub current: bool,
}

/// A log is `latest.log` or one of its gzipped predecessors, and nothing else
/// — certainly nothing with a path separator in it.
pub fn resolve(dir: &Path, name: &str) -> Result<PathBuf> {
    let rel = Path::new(name);
    let looks_right = name.ends_with(".log") || name.ends_with(".log.gz");
    if !crate::paths::is_safe_relative(rel) || rel.components().count() != 1 || !looks_right {
        return Err(Error::msg("invalid log name"));
    }
    Ok(dir.join(rel))
}

fn is_current(name: &str) -> bool {
    name == "latest.log"
}

/// Which archives to delete to keep only the newest `keep`.
///
/// Takes names and times so the decision is pure. `latest.log` is filtered out
/// before it gets here: deleting the log the server has open would do nothing
/// useful and confuse the server.
pub fn to_prune(archives: &[(String, i64)], keep: usize) -> Vec<String> {
    let mut sorted: Vec<(String, i64)> = archives.to_vec();
    // Newest first, with the name breaking ties so the result is stable when
    // two rotations share a second.
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    sorted
        .into_iter()
        .skip(keep)
        .map(|(name, _)| name)
        .collect()
}

pub async fn list(dir: &Path) -> Result<Vec<LogFile>> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".log") && !name.ends_with(".log.gz") {
            continue;
        }
        let meta = entry.metadata().await?;
        if !meta.is_file() {
            continue;
        }
        out.push(LogFile {
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default(),
            size: meta.len(),
            current: is_current(&name),
            name,
        });
    }

    // The current log first, then the rest newest-first: the order you would
    // look in.
    out.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then_with(|| b.modified.cmp(&a.modified))
            .then_with(|| b.name.cmp(&a.name))
    });
    Ok(out)
}

/// Read one log back as text, decompressing it if it has been rotated.
pub async fn read(dir: &Path, name: &str) -> Result<String> {
    read_within(dir, name, MAX_GZIP).await
}

/// [`read`] with the inflation ceiling a test can reach.
async fn read_within(dir: &Path, name: &str, ceiling: u64) -> Result<String> {
    let path = resolve(dir, name)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| Error::msg(format!("no log called {name}")))?;

    let gzipped = name.ends_with(".gz");
    let named = name.to_string();
    let text = tokio::task::spawn_blocking(move || -> Result<String> {
        let raw = if gzipped {
            let decoder = flate2::read::GzDecoder::new(&bytes[..]);
            let mut out = Vec::new();
            decoder.take(ceiling + 1).read_to_end(&mut out)?;
            if out.len() as u64 > ceiling {
                return Err(Error::msg(format!(
                    "{named} unpacks to more than {} MB, which is more log than Geode will open",
                    ceiling / (1024 * 1024)
                )));
            }
            out
        } else {
            bytes
        };

        // Logs are not valid UTF-8 often enough to care about: a mod printing
        // raw bytes should not make the file unreadable.
        let mut text = String::from_utf8_lossy(&raw).into_owned();
        if text.len() > MAX_TEXT {
            // Keep the end, and cut at a line break so the first line shown is
            // a whole one.
            //
            // The offset has to land on a character boundary before it is used
            // at all: a log full of non-ASCII — which mod output routinely is —
            // would otherwise slice through a multi-byte character and panic.
            let mut cut = text.len() - MAX_TEXT;
            while cut < text.len() && !text.is_char_boundary(cut) {
                cut += 1;
            }
            let start = text[cut..]
                .find('\n')
                .map(|i| cut + i + 1)
                .unwrap_or(cut);
            text = format!(
                "[Geode] this log is large; showing the last {} MB\n{}",
                MAX_TEXT / (1024 * 1024),
                &text[start..]
            );
        }
        Ok(text)
    })
    .await
    .map_err(|e| Error::msg(format!("reading the log panicked: {e}")))??;

    Ok(text)
}

pub async fn delete(dir: &Path, name: &str) -> Result<()> {
    if is_current(name) {
        return Err(Error::msg("that is the log being written to right now"));
    }
    tokio::fs::remove_file(resolve(dir, name)?).await?;
    Ok(())
}

/// Keep only the newest `keep` archived logs. Returns how many went.
pub async fn prune(dir: &Path, keep: usize) -> Result<usize> {
    let archives: Vec<(String, i64)> = list(dir)
        .await?
        .into_iter()
        .filter(|l| !l.current)
        .map(|l| (l.name, l.modified))
        .collect();

    let doomed = to_prune(&archives, keep);
    let count = doomed.len();
    for name in doomed {
        // One stubborn file should not stop the rest being cleaned up.
        let _ = tokio::fs::remove_file(dir.join(&name)).await;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archives(entries: &[(&str, i64)]) -> Vec<(String, i64)> {
        entries
            .iter()
            .map(|(n, t)| ((*n).to_string(), *t))
            .collect()
    }

    #[test]
    fn pruning_keeps_the_newest_archives() {
        let all = archives(&[
            ("2026-09-10-1.log.gz", 100),
            ("2026-09-12-1.log.gz", 300),
            ("2026-09-11-1.log.gz", 200),
        ]);
        assert_eq!(to_prune(&all, 2), vec!["2026-09-10-1.log.gz"]);
        assert_eq!(to_prune(&all, 1).len(), 2);
        assert!(to_prune(&all, 3).is_empty());
        assert!(to_prune(&all, 9).is_empty());
    }

    #[test]
    fn pruning_is_stable_when_two_rotations_share_a_second() {
        let all = archives(&[("b.log.gz", 100), ("a.log.gz", 100)]);
        // Whatever the order, the same one survives every time.
        assert_eq!(to_prune(&all, 1), vec!["a.log.gz"]);
    }

    #[test]
    fn keeping_none_deletes_every_archive() {
        let all = archives(&[("a.log.gz", 1), ("b.log.gz", 2)]);
        assert_eq!(to_prune(&all, 0).len(), 2);
    }

    /// A rotated log is refused on what it inflates to, not on what it weighs.
    ///
    /// The file is somebody else's server software's, `MAX_TEXT` is applied
    /// only after the whole thing is already in memory, and gzip will turn a
    /// gigabyte of spaces into a megabyte — so without this the view's own
    /// ceiling bounded nothing and a small file could ask for all the memory
    /// there is. Run at a small ceiling through `read_within` rather than
    /// building a real 128 MB bomb, which is `MAX_GZIP`'s only difference.
    #[tokio::test]
    async fn a_rotated_log_that_inflates_past_its_ceiling_is_refused() {
        use std::io::Write;

        let scratch = crate::paths::Scratch::new("log-bomb");
        let dir = scratch.paths.logs();
        std::fs::create_dir_all(&dir).unwrap();

        let write_gz = |name: &str, plain: &[u8]| {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            gz.write_all(plain).unwrap();
            std::fs::write(dir.join(name), gz.finish().unwrap()).unwrap();
        };

        // A run of spaces, which deflates to almost nothing: the archive on
        // disk is tiny and what it unpacks to is not.
        let plain = b" ".repeat(64 * 1024);
        write_gz("2026-09-12-1.log.gz", &plain);
        write_gz("2026-09-11-1.log.gz", b"the whole of a short log\n");

        let on_disk = std::fs::metadata(dir.join("2026-09-12-1.log.gz"))
            .unwrap()
            .len();
        assert!(
            on_disk < 8 * 1024,
            "the control is wrong: the archive is {on_disk} bytes, so a size check would \
             have caught it and this proves nothing about inflation"
        );

        let refusal = read_within(&dir, "2026-09-12-1.log.gz", 16 * 1024)
            .await
            .expect_err("a log that inflates past the ceiling is refused");
        assert!(
            refusal.to_string().contains("more log than Geode will open"),
            "refused for some other reason: {refusal}"
        );

        // And a log under the ceiling still reads, whole.
        let short = read_within(&dir, "2026-09-11-1.log.gz", 16 * 1024)
            .await
            .expect("a log inside the ceiling reads");
        assert_eq!(short, "the whole of a short log\n");
    }

    #[test]
    fn a_cut_offset_is_walked_to_a_character_boundary() {
        // The shape of the bug: a byte offset that lands inside a multi-byte
        // character. Slicing there panics, and a log full of non-ASCII is not
        // unusual — mod output is routinely full of it.
        let text = "é".repeat(10);
        let mut cut = 5; // mid-character, since 'é' is two bytes
        assert!(!text.is_char_boundary(cut));
        while cut < text.len() && !text.is_char_boundary(cut) {
            cut += 1;
        }
        assert!(text.is_char_boundary(cut));
        // The slice that used to panic now succeeds.
        assert!(!text[cut..].is_empty());
    }

    #[test]
    fn the_live_log_is_recognised() {
        assert!(is_current("latest.log"));
        assert!(!is_current("2026-09-12-1.log.gz"));
    }

    #[test]
    fn a_log_name_cannot_escape_the_folder() {
        let dir = PathBuf::from("/tmp/geode-test/logs");
        assert!(resolve(&dir, "../../etc/passwd.log").is_err());
        assert!(resolve(&dir, "nested/latest.log").is_err());
        assert!(resolve(&dir, "notalog.txt").is_err());
        assert!(resolve(&dir, "latest.log").is_ok());
        assert!(resolve(&dir, "2026-09-12-1.log.gz").is_ok());
    }
}

#[cfg(test)]
mod disk_tests {
    use super::*;
    use crate::paths::Scratch;
    use std::io::Write;

    /// A real folder of logs, because the parsing and the gzip path are the
    /// parts most likely to be wrong and neither shows up in a pure test.
    #[tokio::test]
    async fn a_real_folder_is_listed_newest_first_and_archives_decompress() {
        let scratch = Scratch::new("logs");
        let dir = scratch.paths.root.clone();
        tokio::fs::create_dir_all(&dir).await.unwrap();

        tokio::fs::write(dir.join("latest.log"), "current run\n")
            .await
            .unwrap();

        // The rotated one is gzipped, exactly as the server leaves it.
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"an older run\n").unwrap();
        let gz = encoder.finish().unwrap();
        tokio::fs::write(dir.join("2026-09-12-1.log.gz"), gz)
            .await
            .unwrap();

        // Something that is not a log at all, which must be ignored.
        tokio::fs::write(dir.join("notes.txt"), "ignore me").await.unwrap();

        let found = list(&dir).await.unwrap();
        assert_eq!(found.len(), 2, "the .txt should not be listed: {found:?}");
        assert!(found[0].current, "the live log comes first");
        assert_eq!(found[0].name, "latest.log");
        assert_eq!(found[1].name, "2026-09-12-1.log.gz");

        assert_eq!(read(&dir, "latest.log").await.unwrap(), "current run\n");
        assert_eq!(
            read(&dir, "2026-09-12-1.log.gz").await.unwrap(),
            "an older run\n",
            "a rotated log has to come back decompressed"
        );

        // The live log is never deletable, whatever is asked.
        assert!(delete(&dir, "latest.log").await.is_err());
        assert!(delete(&dir, "2026-09-12-1.log.gz").await.is_ok());
        assert_eq!(list(&dir).await.unwrap().len(), 1);
    }

    /// A folder that does not exist yet is not an error — an instance that has
    /// never been launched simply has no logs.
    #[tokio::test]
    async fn a_missing_folder_lists_nothing() {
        let dir = std::env::temp_dir().join("geode-logs-definitely-absent");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        assert!(list(&dir).await.unwrap().is_empty());
    }
}

//! What the last update from a server did to this instance's settings files,
//! and how the player puts one of them back.
//!
//! In a file of its own beside `instance.json`, for two reasons. A notice is
//! one `info` toast alive for four and a half seconds with its sentences run
//! together, which is nowhere to read a list of settings and what each one
//! used to be; and `instance.json` is rewritten whole by long jobs that loaded
//! it minutes earlier, which is the hazard `server::share`'s module header
//! spells out. So the launch toast says one sentence and points at the panel,
//! and the panel reads this.
//!
//! Only a setting the player had changed themselves earns a row. One they had
//! never touched held exactly what the server last published, so the server
//! changing it is not news and collapses into a count — a design that
//! interrupts a player over a setting they have never heard of is one they
//! stop reading.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;
use crate::share::manifest::Override;

/// One setting the server changed that the player's file did not agree with.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Yours {
    /// The file, as it sits inside the game directory.
    pub path: String,
    /// The setting, segment by segment.
    pub key: Vec<String>,
    /// What the player's own copy held. `None` when they had no such setting
    /// and this one was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub had: Option<String>,
    /// What the server set it to. `None` when the server took it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
}

/// The last update from the server, as the player's Settings tab shows it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LastSync {
    /// The server's push this was, so a panel can tell two updates apart.
    #[serde(default)]
    pub at: u32,
    /// When it ran, in seconds since the epoch.
    #[serde(default)]
    pub when: u64,
    /// Settings the player had changed themselves, which this update moved.
    #[serde(default)]
    pub yours: Vec<Yours>,
    /// How many other settings moved — ones whose value was still the
    /// server's own, so the player had never touched them.
    #[serde(default)]
    pub rest: usize,
    /// Files the server's changes could not be applied to, each naming why.
    #[serde(default)]
    pub skipped: Vec<String>,
}

impl LastSync {
    /// Whether there is anything here worth keeping or showing.
    pub fn worth_saying(&self) -> bool {
        !self.yours.is_empty() || self.rest > 0 || !self.skipped.is_empty()
    }
}

/// Write this instance's report, replacing the one before it.
///
/// Only ever called with something to say — see [`LastSync::worth_saying`] —
/// so an update that changed no settings leaves the last report that did
/// where the player can still read it.
pub async fn save(paths: &Paths, id: &str, report: &LastSync) -> Result<()> {
    crate::server::write_atomic(
        &paths.instance_sync(id),
        &serde_json::to_vec_pretty(report)?,
    )
    .await
}

/// This instance's report, or nothing when no update has changed a setting.
pub async fn load(paths: &Paths, id: &str) -> Result<Option<LastSync>> {
    match tokio::fs::read(paths.instance_sync(id)).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Put one setting back to what this instance had before the server changed
/// it, and take its row out of the report.
///
/// The remedy that makes the server winning acceptable at all: it wins over
/// the one setting it named, and because the push it came in never moves
/// again, nothing applies it a second time — so this sticks for ever, without
/// the player having to pin anything.
///
/// Named by the report rather than by the caller: the value written is the one
/// Geode recorded as theirs, so nothing arriving over the command boundary
/// decides what goes into a file.
pub async fn change_back(
    paths: &Paths,
    id: &str,
    path: &str,
    key: &[String],
) -> Result<LastSync> {
    let mut report = load(paths, id)
        .await?
        .ok_or_else(|| Error::msg("there is nothing recorded for this instance to change back"))?;
    let at = report
        .yours
        .iter()
        .position(|one| one.path == path && one.key == key)
        .ok_or_else(|| {
            Error::msg("that setting is not one the server changed in the last update")
        })?;
    let one = report.yours[at].clone();

    // `was` is left empty: this writes what this instance held, so whether
    // that was the player's own change is already settled — it is in the
    // report because it was.
    crate::settings::splice_file(
        &paths.game_dir(id).join(&one.path),
        &[Override {
            path: one.path.clone(),
            key: one.key.clone(),
            value: one.had.clone(),
            was: None,
            at: 0,
        }],
    )
    .await?;
    report.yours.remove(at);
    save(paths, id, &report).await?;
    Ok(report)
}

/// The report for one update: the settings that landed, sorted into the ones
/// the player had changed and a count of the ones they had not.
pub fn from_applied(
    at: u32,
    applied: &[(String, crate::settings::Applied)],
    skipped: Vec<String>,
) -> LastSync {
    let mut report = LastSync {
        at,
        when: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default(),
        skipped,
        ..LastSync::default()
    };
    for (path, one) in applied {
        if one.theirs {
            report.yours.push(Yours {
                path: path.clone(),
                key: one.key.clone(),
                had: one.had.clone(),
                now: one.now.clone(),
            });
        } else {
            report.rest += 1;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    fn applied(key: &str, had: &str, now: &str, theirs: bool) -> (String, crate::settings::Applied) {
        (
            "config/a.properties".to_string(),
            crate::settings::Applied {
                key: vec![key.to_string()],
                had: Some(had.to_string()),
                now: Some(now.to_string()),
                theirs,
            },
        )
    }

    /// A setting the player had changed earns a row; one they had not is a
    /// number, because a server changing a setting that still held the
    /// server's own value is not news.
    #[test]
    fn only_the_settings_the_player_had_changed_get_a_row() {
        let report = from_applied(
            4,
            &[
                applied("renderDistance", "24", "12", true),
                applied("fov", "70", "90", false),
                applied("gamma", "1", "2", false),
            ],
            vec!["config/locked.properties is read-only".into()],
        );
        assert_eq!(report.at, 4);
        assert_eq!(report.rest, 2);
        assert_eq!(
            report.yours,
            vec![Yours {
                path: "config/a.properties".into(),
                key: vec!["renderDistance".into()],
                had: Some("24".into()),
                now: Some("12".into()),
            }]
        );
        assert!(report.worth_saying());
        assert!(!LastSync::default().worth_saying());
    }

    /// Changing one back writes the value this instance had into the file and
    /// takes the row out, so the panel stops offering it.
    #[tokio::test]
    async fn changing_a_setting_back_writes_what_this_instance_had() {
        let scratch = Scratch::new("change-back");
        let paths = scratch.paths.clone();
        let game = paths.game_dir("inst");
        tokio::fs::create_dir_all(game.join("config")).await.unwrap();
        tokio::fs::write(game.join("config/a.properties"), "renderDistance=12\nfov=90\n")
            .await
            .unwrap();
        tokio::fs::create_dir_all(paths.instance("inst")).await.unwrap();
        save(
            &paths,
            "inst",
            &from_applied(4, &[applied("renderDistance", "24", "12", true)], Vec::new()),
        )
        .await
        .unwrap();

        let left = change_back(&paths, "inst", "config/a.properties", &["renderDistance".into()])
            .await
            .unwrap();
        assert!(left.yours.is_empty());
        assert_eq!(
            std::fs::read(game.join("config/a.properties")).unwrap(),
            b"renderDistance=24\nfov=90\n"
        );
        assert!(load(&paths, "inst").await.unwrap().unwrap().yours.is_empty());

        // And a setting that is not in the report is refused rather than
        // written from whatever the caller said.
        let err = change_back(&paths, "inst", "config/a.properties", &["fov".into()])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not one the server changed"), "{err}");
    }
}

//! The admin's side of a push: what moved, and sending it.
//!
//! The whole of the admin's workflow is the one they already had — open the
//! config file on the server, change the setting, save it — plus one button.
//! Geode compares the file against the copy it last published, shows the
//! settings that moved, and sends the ones the admin confirms.
//!
//! The button is the part the owner did not ask for, and it is the one place
//! this gives them less than their words did. An edit on disk cannot propagate
//! by itself, because mods rewrite their own config files on boot — they add
//! new default keys, bump a `_version` marker, reflow their comments — and
//! Java's `Properties.store()` writes a timestamp as line 2, so a file whose
//! values never changed still has new bytes. Ambient propagation would push
//! that noise to every player and show the admin a list of settings they never
//! touched. One click is the difference between a feature and a nuisance.
//!
//! The snapshot under [`crate::paths::Paths::server_published`] is what makes
//! the comparison per key rather than per byte, and it is the admin's own
//! machine's copy of what they last sent. Nothing like it exists on a player's
//! disk: a player stores one integer, and never a baseline.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;
use crate::share::manifest::{Override, Policy};

/// One file the admin could push from, and the settings that moved in it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Moved {
    /// The file, relative to the server's run directory — which is also the
    /// path it has in a player's instance.
    pub path: String,
    /// The setting, segment by segment. One segment for a file of one setting
    /// per line.
    pub key: Vec<String>,
    /// What the published copy held. `None` for a setting that is new since,
    /// which players have never been sent.
    pub was: Option<String>,
    /// What the file holds now. `None` for a setting the admin took out, which
    /// is the one change in this whole mechanism that cannot be undone by
    /// changing a value back — so it is offered switched off and labelled.
    pub now: Option<String>,
}

/// A settings file Geode cannot read one setting at a time.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unreadable {
    pub path: String,
    /// What to tell the admin about it, in a sentence.
    pub why: String,
}

/// What Review has to show.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    /// Every setting that has moved since the last push, file by file.
    pub moved: Vec<Moved>,
    /// Files kept in sync whose format Geode cannot read per setting, so
    /// nothing can be pushed from them: players were sent them once and they
    /// are the players' from then on. Named, with the reason, rather than
    /// quietly left out — a label reading "only the settings I change" over a
    /// file that silently does something else is the one thing this screen
    /// must not do.
    pub unreadable: Vec<Unreadable>,
    /// The settings this server already owns, newest push first. What lets a
    /// setting the admin changed and changed back be pushed again: the diff
    /// compares values, so such a setting is in nothing above, and an admin
    /// who cannot find it concludes pushing is broken.
    pub pushed: Vec<Override>,
}

/// One setting the admin confirmed, as the UI sends it back.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Wanted {
    pub path: String,
    pub key: Vec<String>,
    /// The raw value to push, or `None` to take the setting out of players'
    /// files.
    #[serde(default)]
    pub value: Option<String>,
    /// What this server last published, carried back so a player can be told
    /// whether the value in their file was their own change or this server's
    /// previous one. See [`Override::was`].
    #[serde(default)]
    pub was: Option<String>,
}

/// Take the snapshot every later push is measured against, for every file
/// under the paths this server keeps in sync.
///
/// Called when a path becomes [`Policy::Merge`] and at no other time. That
/// moment is also when every player is sent those files whole — the path's
/// `from` jumps, so nobody is caught up with it — so the files on disk now are
/// exactly what players have, which is what a baseline means.
///
/// Which files those are is read off the behaviour's own push number: a path
/// whose choice was stamped with the push this share is at is a path that
/// became [`Policy::Merge`] in the change just saved, and nothing else is
/// touched. That is what keeps the one hazard away — re-taking the snapshot of
/// a file the admin has unpushed edits in would make those edits the baseline
/// and lose them without a word — while still letting one call cover a folder
/// chosen whole and a single file picked out of it.
///
/// Only the files this server's pack actually carries, because those are the
/// only ones players have: a settings file nobody is sent is a file nothing
/// can be pushed out of, and a baseline for it would be a comparison against
/// a copy that reached nobody.
pub async fn publish_all(paths: &Paths, id: &str) -> Result<()> {
    let Some(share) = crate::server::share::load(paths, id).await? else {
        return Ok(());
    };
    for row in crate::share::pack::settings_sent(paths, id).await?.files {
        let just_chosen = share
            .settings
            .covering(&row.rel)
            .is_some_and(|named| named.from == share.settings.at);
        if row.sent && row.policy == Policy::Merge && just_chosen {
            snapshot(paths, id, &row.rel).await?;
        }
    }
    Ok(())
}

/// Which settings have moved since this server last published, per key.
pub async fn review(paths: &Paths, id: &str) -> Result<Review> {
    let share = crate::server::share::load(paths, id)
        .await?
        .ok_or_else(|| Error::msg("this server has not been shared yet — share it first"))?;
    let mut review = Review {
        pushed: {
            let mut pushed = share.settings.overrides.clone();
            pushed.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.path.cmp(&b.path)));
            pushed
        },
        ..Review::default()
    };
    if !share.include_config {
        return Ok(review);
    }

    // The files the pack carries and nothing else — the admin's picks, held
    // to the folders a pack may name by the one function that decides what
    // travels. A setting inside a file nobody is sent is a setting nobody
    // applies, so offering it would be offering a change that reaches no one.
    let run = paths.server_run(id);
    for row in crate::share::pack::settings_sent(paths, id).await?.files {
        if !row.sent || row.policy != Policy::Merge {
            continue;
        }
        let rel = row.rel;
        let shown = shown(&rel);
        let published = paths.server_published(id).join(&rel);
        let now = tokio::fs::read(run.join(&rel)).await?;
        // Named with the reason, and before anything else is tried: a file
        // Geode cannot read one setting at a time is one it can push nothing
        // from, and the admin has to be told which files those are rather than
        // left to wonder why an edit never appeared.
        if crate::settings::sniff(&now).is_none() {
            review.unreadable.push(Unreadable {
                path: shown,
                why: cannot_read(),
            });
            continue;
        }
        // A file that has appeared since the behaviour was chosen has nothing
        // to compare: players who do not have it are sent it whole, so what it
        // holds now is what they get, and that is the baseline from here.
        if tokio::fs::metadata(&published).await.is_err() {
            snapshot(paths, id, &rel).await?;
            continue;
        }
        match crate::settings::diff(&tokio::fs::read(&published).await?, &now) {
            Ok(moved) => review.moved.extend(moved.into_iter().map(|one| Moved {
                path: shown.clone(),
                key: one.key,
                was: one.was,
                now: one.now,
            })),
            Err(e) => review.unreadable.push(Unreadable {
                path: shown,
                why: format!("{e}"),
            }),
        }
    }
    Ok(review)
}

/// Send the confirmed settings: write them as this server's own, stamped with
/// one new push, and move the baseline on by exactly those settings.
///
/// One push number for the whole set, which is what makes a player's catch-up
/// a single pass. The number only ever goes up, and an entry's own number
/// never moves again — so a player who changes one of these settings back
/// keeps their change for ever, which is the sentence this whole mechanism
/// rests on.
///
/// Returns how many settings the server now owns in this push.
pub async fn push(paths: &Paths, id: &str, wanted: &[Wanted]) -> Result<u32> {
    let mut share = crate::server::share::load(paths, id)
        .await?
        .ok_or_else(|| Error::msg("this server has not been shared yet — share it first"))?;
    if wanted.is_empty() {
        return Err(Error::msg("nothing was chosen to send"));
    }

    let run = paths.server_run(id);
    // What this server's pack carries, worked out by the one function that
    // decides it rather than read off the list of picks — a setting inside a
    // file the pack leaves behind is one nobody could ever apply.
    let sent = crate::share::pack::settings_sent(paths, id).await?;
    let at = share.settings.at.saturating_add(1);
    for one in wanted {
        let rel = PathBuf::from(&one.path);
        // Only a file this server actually keeps in sync. Anything else would
        // be a setting nobody applies — a path players are sent whole, or one
        // the pack does not carry at all — written into a list that rides
        // every pack from here on.
        if share.settings.policy_for(&rel) != Policy::Merge {
            return Err(Error::msg(format!(
                "{} is not a file this server keeps in sync, so there is nothing to send for it",
                one.path
            )));
        }
        if !crate::paths::is_safe_relative(&rel)
            || !tokio::fs::symlink_metadata(run.join(&rel))
                .await
                .is_ok_and(|meta| meta.is_file())
        {
            return Err(Error::msg(format!(
                "{} is not a file in this server's folder",
                one.path
            )));
        }
        // And a file this server's pack carries, which is not the same
        // question: the admin picks what travels, so a file kept in sync that
        // nobody picked is one no player has a copy of to splice into.
        if !sent.files.iter().any(|row| row.sent && row.rel == rel) {
            return Err(Error::msg(format!(
                "{} is not a file this server sends its players, so a setting inside it would \
                 reach nobody",
                one.path
            )));
        }
        if one.key.iter().any(|segment| segment.trim().is_empty()) {
            return Err(Error::msg(format!(
                "a setting in {} has no name",
                one.path
            )));
        }
        let entry = Override {
            path: one.path.clone(),
            key: one.key.clone(),
            value: one.value.clone(),
            was: one.was.clone(),
            at,
        };
        match share
            .settings
            .overrides
            .iter_mut()
            .find(|held| held.same_key(&entry))
        {
            Some(held) => *held = entry,
            None => share.settings.overrides.push(entry),
        }
    }
    // Held to exactly what a pack off the network is held to, so a list Geode
    // would refuse to apply is one it refuses to make.
    share.settings.checked()?;
    share.settings.at = at;
    crate::server::share::save(paths, id, &share).await?;

    // Last, and only once the list is saved: the baseline is what the next
    // push is measured against, so moving it before the list was written
    // would lose a push that failed to save.
    for (nth, one) in wanted.iter().enumerate() {
        // One file published once, however many of its settings were sent.
        if wanted[..nth].iter().any(|before| before.path == one.path) {
            continue;
        }
        published(paths, id, Path::new(&one.path), wanted).await?;
    }
    Ok(wanted.len() as u32)
}

/// Move one file's baseline on by the settings that were sent, and by nothing
/// else.
///
/// Not a copy of the file, which is the whole of it: the admin sends the
/// settings they ticked, and removals arrive unticked by design. Copying the
/// file whole would make every other setting in it the new baseline, so a
/// setting they left for later — or one they changed while the Review screen
/// was open — would never be offered again and players would keep the old
/// value for ever, with nothing said to anybody. The same hazard
/// [`publish_all`] refuses to re-snapshot for, one file in.
///
/// So the settings that were sent are spliced into the baseline instead, which
/// is exactly the file players now hold for those keys and leaves every other
/// key reading what it read before.
async fn published(paths: &Paths, id: &str, rel: &Path, wanted: &[Wanted]) -> Result<()> {
    let to = paths.server_published(id).join(rel);
    // No baseline yet, so there is nothing to move it on from: players were
    // sent this file whole, and what it holds now is what they have.
    let Ok(bytes) = tokio::fs::read(&to).await else {
        return snapshot(paths, id, rel).await;
    };
    let sent: Vec<Override> = wanted
        .iter()
        .filter(|one| Path::new(&one.path) == rel)
        .map(|one| Override {
            path: one.path.clone(),
            key: one.key.clone(),
            value: one.value.clone(),
            was: one.was.clone(),
            at: 0,
        })
        .collect();
    let (next, _) = crate::settings::apply(&bytes, &sent)?;
    crate::server::write_atomic(&to, &next).await
}

/// Copy one file in the server's run directory to the published snapshot
/// beside it, through no link either side.
async fn snapshot(paths: &Paths, id: &str, rel: &Path) -> Result<()> {
    let from = paths.server_run(id).join(rel);
    if !tokio::fs::symlink_metadata(&from)
        .await
        .is_ok_and(|meta| meta.is_file())
    {
        return Ok(());
    }
    let to = paths.server_published(id).join(rel);
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let bytes = tokio::fs::read(&from).await?;
    crate::server::write_atomic(&to, &bytes).await
}

fn shown(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

fn cannot_read() -> String {
    "Geode cannot read this file one setting at a time, so there is nothing to push from it — \
     players were sent it once and it is theirs now."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;
    use crate::server::share::ServerShare;
    use crate::server::{Server, ServerKind};

    /// A shared server with a `config/` folder holding the files named, and
    /// the whole folder kept in sync.
    async fn kept_in_sync(tag: &str, files: &[(&str, &str)]) -> (Scratch, Server) {
        let scratch = Scratch::new(&format!("push-{tag}"));
        let paths = scratch.paths.clone();
        let mut server = Server::new("Survival".into(), ServerKind::Fabric, "1.21.1".into());
        server.id = format!("push-{tag}");
        let run = paths.server_run(&server.id);
        tokio::fs::create_dir_all(run.join("config")).await.unwrap();
        for (rel, text) in files {
            tokio::fs::write(run.join(rel), text).await.unwrap();
        }
        let mut share = ServerShare {
            code: crate::share::code::Code::generate(),
            include_config: true,
            settings: Default::default(),
            picked: Vec::new(),
            listing: None,
            shared: true,
            forbidden: Vec::new(),
        };
        share.settings.choose("config", Policy::Merge);
        crate::server::share::save(&paths, &server.id, &share)
            .await
            .unwrap();
        publish_all(&paths, &server.id).await.unwrap();
        (scratch, server)
    }

    /// Choosing to keep a folder in sync publishes what is in it, so the first
    /// Review after that has nothing to report — and an edit afterwards is the
    /// only thing it reports, by key.
    #[tokio::test]
    async fn review_reports_the_edit_and_not_the_file() {
        let (scratch, server) = kept_in_sync(
            "review",
            &[("config/a.properties", "renderDistance=8\nfov=70\n")],
        )
        .await;
        let paths = scratch.paths.clone();
        assert!(review(&paths, &server.id).await.unwrap().moved.is_empty());

        let run = paths.server_run(&server.id);
        tokio::fs::write(
            run.join("config/a.properties"),
            "#Mon Oct 19 11:02:55 EDT 2026\nrenderDistance = 12\nfov=70\n",
        )
        .await
        .unwrap();
        let review = review(&paths, &server.id).await.unwrap();
        assert_eq!(review.moved.len(), 1, "{:?}", review.moved);
        assert_eq!(review.moved[0].path, "config/a.properties");
        assert_eq!(review.moved[0].key, vec!["renderDistance".to_string()]);
        assert_eq!(review.moved[0].was.as_deref(), Some("8"));
        assert_eq!(review.moved[0].now.as_deref(), Some("12"));
    }

    /// A push writes the setting as the server's own, stamped with one new
    /// number, and moves the baseline — so the same edit is not offered twice.
    #[tokio::test]
    async fn a_push_owns_the_setting_and_moves_the_baseline() {
        let (scratch, server) =
            kept_in_sync("owns", &[("config/a.properties", "renderDistance=8\n")]).await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/a.properties"), "renderDistance=12\n")
            .await
            .unwrap();

        let before = crate::server::share::load(&paths, &server.id)
            .await
            .unwrap()
            .unwrap()
            .settings
            .at;
        let sent = push(
            &paths,
            &server.id,
            &[Wanted {
                path: "config/a.properties".into(),
                key: vec!["renderDistance".into()],
                value: Some("12".into()),
                was: Some("8".into()),
            }],
        )
        .await
        .unwrap();
        assert_eq!(sent, 1);

        let share = crate::server::share::load(&paths, &server.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(share.settings.at, before + 1);
        assert_eq!(share.settings.overrides.len(), 1);
        assert_eq!(share.settings.overrides[0].value.as_deref(), Some("12"));
        assert_eq!(share.settings.overrides[0].at, before + 1);
        assert!(review(&paths, &server.id).await.unwrap().moved.is_empty());

        // And a second push of the same setting replaces its entry rather
        // than adding to a log.
        tokio::fs::write(run.join("config/a.properties"), "renderDistance=16\n")
            .await
            .unwrap();
        push(
            &paths,
            &server.id,
            &[Wanted {
                path: "config/a.properties".into(),
                key: vec!["renderDistance".into()],
                value: Some("16".into()),
                was: Some("12".into()),
            }],
        )
        .await
        .unwrap();
        let share = crate::server::share::load(&paths, &server.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(share.settings.overrides.len(), 1);
        assert_eq!(share.settings.overrides[0].value.as_deref(), Some("16"));
        assert_eq!(share.settings.overrides[0].at, before + 2);
    }

    /// Sending one of two settings that moved leaves the other one still
    /// there to send. The removal row arrives unticked by design and the
    /// admin can untick any other, so a push is routinely a part of what
    /// moved — and a baseline taken from the whole file would make everything
    /// they did not tick the published value, so Review would never offer it
    /// again, players would keep the old value for ever, and nobody would be
    /// told.
    #[tokio::test]
    async fn a_push_of_one_setting_leaves_the_others_still_to_send() {
        let (scratch, server) = kept_in_sync(
            "partial",
            &[(
                "config/a.properties",
                "renderDistance=8\nfov=70\ngone=1\n",
            )],
        )
        .await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        tokio::fs::write(
            run.join("config/a.properties"),
            "renderDistance=12\nfov=90\n",
        )
        .await
        .unwrap();
        assert_eq!(review(&paths, &server.id).await.unwrap().moved.len(), 3);

        // Only the one setting is sent: the other change and the removal are
        // left for the admin to decide about later.
        push(
            &paths,
            &server.id,
            &[Wanted {
                path: "config/a.properties".into(),
                key: vec!["fov".into()],
                value: Some("90".into()),
                was: Some("70".into()),
            }],
        )
        .await
        .unwrap();

        let mut moved = review(&paths, &server.id).await.unwrap().moved;
        moved.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(moved.len(), 2, "{moved:?}");
        assert_eq!(moved[0].key, vec!["gone".to_string()]);
        assert_eq!(moved[0].was.as_deref(), Some("1"));
        assert_eq!(moved[0].now, None);
        assert_eq!(moved[1].key, vec!["renderDistance".to_string()]);
        assert_eq!(moved[1].was.as_deref(), Some("8"));
        assert_eq!(moved[1].now.as_deref(), Some("12"));
    }

    /// A file Geode cannot read one setting at a time is named with the
    /// reason, and is not quietly left out of the screen.
    ///
    /// Both of the real ones: Axiom's window layout, which is a `config/` file
    /// that is not configuration at all, and a `.json5` using syntax no
    /// `.json` needs — 8 of the 2208 JSON-family files measured here do, and
    /// nothing reads those per setting.
    #[tokio::test]
    async fn a_file_that_cannot_be_read_per_setting_is_named() {
        let (scratch, server) = kept_in_sync(
            "unreadable",
            &[
                ("config/imgui.ini", "[Window][###Tools]\nPos=0,22\n"),
                ("config/yacl.json5", "{\n  renderDistance: 8,\n}"),
                ("config/a.properties", "renderDistance=8\n"),
            ],
        )
        .await;
        let paths = scratch.paths.clone();
        let review = review(&paths, &server.id).await.unwrap();
        assert!(review.moved.is_empty());
        let mut named: Vec<&str> = review
            .unreadable
            .iter()
            .map(|one| one.path.as_str())
            .collect();
        named.sort();
        assert_eq!(named, vec!["config/imgui.ini", "config/yacl.json5"]);
        for one in &review.unreadable {
            assert!(one.why.contains("sent it once"), "{}: {}", one.path, one.why);
        }
    }

    /// A push may only name a file this server keeps in sync, and only a file
    /// that is in its folder — so nothing a UI bug or a hand-written call
    /// sends can put a setting into a list that rides every pack.
    #[tokio::test]
    async fn a_push_refuses_a_path_this_server_does_not_keep_in_sync() {
        let (scratch, server) =
            kept_in_sync("refuse", &[("config/a.properties", "a=1\n")]).await;
        let paths = scratch.paths.clone();
        let one = |path: &str| Wanted {
            path: path.into(),
            key: vec!["a".into()],
            value: Some("2".into()),
            was: None,
        };

        // Outside the folder the admin chose.
        let err = push(&paths, &server.id, &[one("options.txt")])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("keeps in sync"), "{err}");

        // Reaching out of the instance, and naming a jar.
        for path in ["config/../../etc/passwd", "mods/evil.jar"] {
            let err = push(&paths, &server.id, &[one(path)])
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains(path), "{err}");
        }

        // A path inside the folder that is not a file there.
        let err = push(&paths, &server.id, &[one("config/missing.properties")])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a file in this server's folder"), "{err}");
    }

    /// Pick which of this server's settings files travel, the way the admin's
    /// picker does.
    async fn pick(paths: &Paths, id: &str, picks: &[(&str, Policy)]) {
        let mut share = crate::server::share::load(paths, id)
            .await
            .unwrap()
            .expect("a shared server");
        share
            .pick(
                &picks
                    .iter()
                    .map(|(path, policy)| crate::share::pack::Pick {
                        path: (*path).to_string(),
                        policy: *policy,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        crate::server::share::save(paths, id, &share).await.unwrap();
    }

    /// A setting can only be pushed out of a file this server sends. The admin
    /// picks what travels, so a file kept in sync that they did not pick
    /// reaches nobody — and a push from it would be a change no player could
    /// ever apply, written into a list that rides every pack from then on.
    #[tokio::test]
    async fn nothing_is_pushed_from_a_file_this_server_does_not_send() {
        let (scratch, server) = kept_in_sync(
            "unsent",
            &[
                ("config/a.properties", "renderDistance=8\n"),
                ("config/b.properties", "fov=70\n"),
            ],
        )
        .await;
        let paths = scratch.paths.clone();
        pick(&paths, &server.id, &[("config/a.properties", Policy::Merge)]).await;

        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/a.properties"), "renderDistance=12\n")
            .await
            .unwrap();
        tokio::fs::write(run.join("config/b.properties"), "fov=90\n")
            .await
            .unwrap();

        // Only the file that travels is offered.
        let moved = review(&paths, &server.id).await.unwrap().moved;
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert_eq!(moved[0].path, "config/a.properties");

        let sending = |path: &str, key: &str, value: &str| Wanted {
            path: path.into(),
            key: vec![key.into()],
            value: Some(value.into()),
            was: None,
        };
        let err = push(
            &paths,
            &server.id,
            &[sending("config/b.properties", "fov", "90")],
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("sends its players"), "{err}");

        // And the one that travels pushes as it always did.
        push(
            &paths,
            &server.id,
            &[sending("config/a.properties", "renderDistance", "12")],
        )
        .await
        .unwrap();
        assert!(review(&paths, &server.id).await.unwrap().moved.is_empty());
    }

    /// Choosing a behaviour for one file takes that file's baseline and leaves
    /// every other baseline where it was. An edit the admin has not pushed yet
    /// is still an edit to push: a snapshot re-taken over it would make it the
    /// published value, so Review would never offer it again and players would
    /// keep the old value for ever with nothing said to anybody.
    #[tokio::test]
    async fn choosing_a_behaviour_for_one_file_keeps_anothers_unpushed_edit() {
        let (scratch, server) = kept_in_sync(
            "baseline",
            &[
                ("config/a.properties", "renderDistance=8\n"),
                ("config/c.properties", "fov=70\n"),
            ],
        )
        .await;
        let paths = scratch.paths.clone();
        let run = paths.server_run(&server.id);
        tokio::fs::write(run.join("config/a.properties"), "renderDistance=12\n")
            .await
            .unwrap();

        // A fresh choice for the second file, which moves the push counter —
        // and so is the moment every baseline just chosen is taken.
        pick(
            &paths,
            &server.id,
            &[
                ("config/a.properties", Policy::Merge),
                ("config/c.properties", Policy::Enforce),
            ],
        )
        .await;
        publish_all(&paths, &server.id).await.unwrap();

        let moved = review(&paths, &server.id).await.unwrap().moved;
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert_eq!(moved[0].path, "config/a.properties");
        assert_eq!(moved[0].was.as_deref(), Some("8"));
        assert_eq!(moved[0].now.as_deref(), Some("12"));
    }
}

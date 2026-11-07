//! An instance's singleplayer worlds: listing them, copying them, renaming
//! them, backing them up, importing one and deleting one.
//!
//! A world is a directory under [`Paths::instance_saves`] holding `level.dat`
//! and, beside it, the region files, `playerdata/`, `advancements/` and
//! whatever else the player's version and mods write. All of it is
//! irreplaceable, and three rules follow from that; each is load-bearing and
//! each has its reason written where it is enforced.
//!
//! **A running game owns its world.** Minecraft holds the world open and puts
//! `level.dat` on disk only at an autosave or a clean shutdown, so a copy
//! taken underneath it captures region files with no matching `level.dat` —
//! and restoring that hands the game a world it cannot identify, which it
//! answers by generating a new one beside the terrain already there.
//! `server/backup.rs` learned this the hard way for servers. A server can be
//! asked to `save-all flush`; a client cannot be asked anything, so there is
//! no equivalent of that escape here and the only correct answer is to refuse.
//! Every function below that reads or writes a world therefore goes through
//! [`only_when_stopped`] first, in this module rather than beside each caller,
//! for the reason `server::backup::create` keeps its flush here: a caller that
//! forgets produces a backup that reads as fine until the day it is needed.
//!
//! **A backup holds the whole world.** A server's leaves out its jar,
//! `libraries/` and `versions/` because Geode can ask for those again. A world
//! has no such parts. There is no upstream to re-fetch a region file from and
//! nothing that could promise to put one back, so the thinning rule that
//! governs `server/backup.rs` deliberately does not apply — see [`back_up`].
//!
//! **`level.dat` is the player's file.** It carries keys written by their
//! client and their mods: of the ten real worlds measured on this machine, the
//! Forge ones hold an `fml` compound beside `Data` at the root, in one case
//! *before* it. So nothing here is decoded that does not have to be. Renaming
//! rewrites one string and copies every other byte through untouched, the way
//! [`super::multiplayer`] does for `servers.dat` and over the same walk —
//! [`super::nbt`]. See [`with_level_name`].

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Context, Error, Result};
use crate::instance::nbt::{
    self, java_utf8, put_text, Reader, TAG_BYTE, TAG_COMPOUND, TAG_END, TAG_INT, TAG_LONG,
    TAG_STRING,
};
use crate::net::Progress;
use crate::paths::Paths;
use crate::state::GameRegistry;

/// What this file is called in an error a person reads.
const SUBJECT: &str = "this world's level.dat";

/// The one file that makes a directory a world rather than a folder.
const LEVEL_DAT: &str = "level.dat";

/// One world in an instance's `saves/`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct World {
    /// The directory under `saves/`. Also this world's identity for every
    /// operation here, because it is the only thing about a world that is
    /// unique — two worlds may share a `LevelName`.
    pub folder: String,
    /// `LevelName`, which is what Minecraft shows in its own world list. Falls
    /// back to the folder name when `level.dat` could not be read, so a row
    /// still says something.
    pub name: String,
    /// `LastPlayed`, milliseconds since the epoch — the game's own unit, left
    /// as it is found rather than rounded to seconds here.
    pub last_played: Option<i64>,
    /// `GameType`: 0 survival, 1 creative, 2 adventure, 3 spectator. Passed
    /// through as the number, because naming them is the screen's job.
    pub game_mode: Option<i32>,
    /// `hardcore`. Absent from a vanilla world's `level.dat` entirely — six of
    /// the ten measured here do not carry it — so a missing key is `false`
    /// rather than unreadable.
    pub hardcore: bool,
    /// `Version.Name`, the Minecraft version this world was last opened with.
    /// Worth the one nested read: a world last saved by a newer game than the
    /// instance runs is the thing a player most needs to see before pressing
    /// Play.
    pub version: Option<String>,
    /// Everything in the directory, in bytes.
    ///
    /// A recursive walk per row, which is the shape the mod-count work found
    /// worth measuring rather than assuming. No figure is quoted here, for
    /// either walk: the numbers that used to stand in this comment described
    /// worlds nobody counted, and the ones that replaced them were measured
    /// elsewhere and written down nowhere, so neither could be checked by
    /// anyone reading this. A figure in a comment reads as freshly measured
    /// whether it was or not. What is true of the shape is
    /// enough to keep it, and that is all that is said — this stats and
    /// never opens, so the cost is the file count and not the gigabytes, and a
    /// world runs from a few dozen files when it is made to tens of thousands
    /// once it has been played, which is what to measure if it ever matters.
    /// Kept at that price because a world's size is what somebody deciding
    /// whether to back one up is looking for.
    pub size: u64,
    /// Why this folder is not a world Geode can read, if it is not. A folder
    /// with no `level.dat`, or one whose `level.dat` does not parse, is
    /// reported here and still listed — it is sitting in the player's `saves/`
    /// and pretending it is not there is worse than saying what is wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trouble: Option<String>,
}

/// A zipped copy of one world.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorldBackup {
    /// The world this is a copy of, by folder name.
    pub world: String,
    /// The file name, which is also its timestamp.
    pub name: String,
    pub size: u64,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// Where a world is, and whether it may be touched
// ---------------------------------------------------------------------------

/// Refuse while the game has this instance open.
///
/// The one rule of this module, in the one place every operation passes
/// through. `doing` completes the sentence, so each refusal names the thing
/// that was asked for rather than making the player guess which button
/// complained.
///
/// Worth saying why this covers *every* operation and not only the writing
/// ones: reading a world out is the dangerous half. A backup or a copy taken
/// now is the one that silently captures region files with no matching
/// `level.dat` and reads as a success. Importing is the only genuinely harmless
/// case, and it is refused with the rest so the screen has one sentence to say
/// instead of a rule with an exception nobody remembers.
fn only_when_stopped(games: &GameRegistry, id: &str, doing: &str) -> Result<()> {
    if games.is_running(id) {
        return Err(Error::msg(format!(
            "Stop the game before {doing}. Minecraft keeps its world open while it runs and only \
             writes level.dat when it saves, so anything copied out now would be missing the part \
             that identifies it."
        )));
    }
    Ok(())
}

/// Reject anything that is not a plain directory name sitting in `saves/`.
///
/// Every world name on this path came in over IPC, so none of it is trusted to
/// be one component or to stay inside the folder.
fn resolve(paths: &Paths, id: &str, folder: &str) -> Result<PathBuf> {
    Ok(paths.instance_saves(id).join(one_component(folder)?))
}

/// `folder` as a single safe path component, or a readable refusal.
fn one_component(folder: &str) -> Result<&Path> {
    let rel = Path::new(folder);
    if folder.trim().is_empty()
        || !crate::paths::is_safe_relative(rel)
        || rel.components().count() != 1
    {
        return Err(Error::msg(format!("'{folder}' is not a world folder name")));
    }
    Ok(rel)
}

/// A directory name for a world called `name`.
///
/// Minecraft derives one the same way and then suffixes it when it is taken,
/// which is why a folder and a `LevelName` routinely disagree — two of the ten
/// real worlds measured here already do. Nothing is stripped that a filesystem
/// would accept, except the characters that no filesystem accepts everywhere:
/// a name chosen on Linux should still name a folder after the instance is
/// copied to Windows.
fn folder_for(name: &str) -> Result<String> {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            // Control characters are not names.
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Trailing dots and spaces are legal on Linux and silently dropped by
    // Windows, which would make a folder nothing here can find again.
    let trimmed = cleaned.trim().trim_end_matches('.').trim_end();
    if trimmed.is_empty() {
        return Err(Error::msg(
            "give the world a name that can also be a folder name",
        ));
    }
    Ok(trimmed.to_string())
}

/// Everything under `dir`, in bytes. A world is thousands of region files, so
/// this walks iteratively.
async fn size_of(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&current).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            match entry.file_type().await {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(t) if t.is_file() => {
                    total += entry.metadata().await.map(|m| m.len()).unwrap_or(0)
                }
                _ => {}
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// Every world in this instance's `saves/`, newest played first.
///
/// Reading only, so it does not refuse while the game runs: the list is how
/// somebody checks what is there, and `last_played` being a few minutes stale
/// is not a reason to show an empty screen.
pub async fn list(paths: &Paths, id: &str) -> Result<Vec<World>> {
    let saves = paths.instance_saves(id);
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(&saves).await {
        Ok(e) => e,
        // An instance that has never been played has no `saves/` yet, which is
        // no worlds rather than an error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(Error::from(e)).ctx("reading this instance's worlds"),
    };

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let Some(folder) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        out.push(read_world(&entry.path(), folder).await);
    }

    // Newest played first, the way the game's own list is ordered, with the
    // never-played after them by name.
    out.sort_by(|a, b| {
        b.last_played
            .unwrap_or(0)
            .cmp(&a.last_played.unwrap_or(0))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// One world's directory, read into a row for the screen.
///
/// Never an error: a folder that cannot be read is a row carrying what is
/// wrong with it. The listing is the one place a player finds out that one of
/// their worlds has a problem, so failing the whole call because of one of them
/// would hide the other nine.
async fn read_world(dir: &Path, folder: String) -> World {
    let size = size_of(dir).await;
    let blank = World {
        folder: folder.clone(),
        name: folder.clone(),
        last_played: None,
        game_mode: None,
        hardcore: false,
        version: None,
        size,
        trouble: None,
    };

    let bytes = match tokio::fs::read(dir.join(LEVEL_DAT)).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return World {
                trouble: Some(
                    "this folder has no level.dat, so it is not a world Minecraft can open".into(),
                ),
                ..blank
            }
        }
        Err(e) => {
            return World {
                trouble: Some(format!("its level.dat could not be read: {e}")),
                ..blank
            }
        }
    };

    match read_level(&bytes) {
        Ok(level) => World {
            name: level.name.unwrap_or(folder),
            last_played: level.last_played,
            game_mode: level.game_mode,
            hardcore: level.hardcore,
            version: level.version,
            ..blank
        },
        Err(e) => World {
            trouble: Some(e.to_string()),
            ..blank
        },
    }
}

// ---------------------------------------------------------------------------
// Reading level.dat
// ---------------------------------------------------------------------------

/// The handful of keys the Worlds screen shows. Nothing else in the file is
/// decoded, and nothing else is ever rewritten.
#[derive(Debug, Default, PartialEq)]
struct Level {
    name: Option<String>,
    last_played: Option<i64>,
    game_mode: Option<i32>,
    hardcore: bool,
    version: Option<String>,
}

/// Decompress `level.dat`.
///
/// It is gzip — `1f 8b` — which is the opposite answer from `servers.dat`
/// beside it, checked on this machine against all ten real worlds here and
/// recorded in [`super::multiplayer`]'s header for the same reason. Guessing
/// either way round loses the file.
fn gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;

    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Err(nbt::unreadable(
            SUBJECT,
            "it is not compressed, and a level.dat is",
        ));
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .map_err(|e| nbt::unreadable(SUBJECT, &format!("it is not readable gzip ({e})")))?;
    Ok(out)
}

/// What the screen shows, out of a `level.dat`'s bytes.
fn read_level(bytes: &[u8]) -> Result<Level> {
    let plain = gunzip(bytes)?;
    let (head, _) = find_data(&plain)?;
    let mut r = Reader::new(&plain, SUBJECT);
    r.at = head;
    read_data(&mut r)
}

/// `Data`'s keys, stopping at its end marker.
fn read_data(r: &mut Reader<'_>) -> Result<Level> {
    let mut level = Level::default();
    loop {
        let tag = r.u8()?;
        if tag == TAG_END {
            return Ok(level);
        }
        let key = r.text()?;
        match (key, tag) {
            (b"LevelName", TAG_STRING) => level.name = Some(text(r.text()?)),
            (b"LastPlayed", TAG_LONG) => level.last_played = Some(r.i64()?),
            (b"GameType", TAG_INT) => level.game_mode = Some(r.i32()?),
            (b"hardcore", TAG_BYTE) => level.hardcore = r.u8()? != 0,
            (b"Version", TAG_COMPOUND) => level.version = read_version(r)?,
            // Everything else — and there are 47 keys in a Forge world's
            // `Data` — is stepped over without being looked at.
            _ => r.skip(tag)?,
        }
    }
}

/// `Version.Name`, the version string the game last saved this world with.
fn read_version(r: &mut Reader<'_>) -> Result<Option<String>> {
    let mut name = None;
    loop {
        let tag = r.u8()?;
        if tag == TAG_END {
            return Ok(name);
        }
        let key = r.text()?;
        if key == b"Name" && tag == TAG_STRING {
            name = Some(text(r.text()?));
        } else {
            r.skip(tag)?;
        }
    }
}

/// An NBT string as text, out of Java's own UTF-8.
///
/// `servers.dat` reads its strings as they fall out, because an address is
/// ASCII and a name there is only ever compared with one Geode wrote. Neither
/// is true of a world's name: it is shown to the player and compared with what
/// they typed, and "Home 😀" holds that emoji as two surrogate halves that are
/// not valid UTF-8. Decoded properly, a rename of such a world also reads its
/// own work back as the name it wrote — see [`nbt::java_text`].
fn text(bytes: &[u8]) -> String {
    nbt::java_text(bytes)
}

/// Where `Data`'s payload starts and ends inside an uncompressed `level.dat`.
///
/// The root is not just `Data`. A Forge world carries an `fml` compound beside
/// it, and in one of the real files here `fml` comes *first* — so this finds
/// the key rather than assuming its position, and the bytes either side of what
/// it returns are what a rewrite copies through.
///
/// Whole is the point, as it is for `servers.dat`: a file that stops early, has
/// bytes left over, or is not NBT at all comes back as an error and is never
/// written to. All ten real worlds here parse whole with zero trailing bytes.
fn find_data(plain: &[u8]) -> Result<(usize, usize)> {
    let mut r = Reader::new(plain, SUBJECT);
    if r.u8()? != TAG_COMPOUND {
        return Err(nbt::unreadable(
            SUBJECT,
            "it does not start with an NBT compound",
        ));
    }
    r.text()?; // the root's own name, which the game leaves empty

    let mut found = None;
    loop {
        let tag = r.u8()?;
        if tag == TAG_END {
            break;
        }
        let key = r.text()?;
        // Only the first `Data` is read. A second would be invalid NBT, and
        // acting on one would be acting on a file we cannot explain.
        if key == b"Data" && tag == TAG_COMPOUND && found.is_none() {
            let start = r.at;
            r.skip(tag)?;
            found = Some((start, r.at));
        } else {
            r.skip(tag)?;
        }
    }
    if r.at != plain.len() {
        return Err(nbt::unreadable(
            SUBJECT,
            "it has bytes after the end of its contents",
        ));
    }
    found.ok_or_else(|| nbt::unreadable(SUBJECT, "it has no world data in it"))
}

// ---------------------------------------------------------------------------
// Renaming
// ---------------------------------------------------------------------------

/// Rename a world: write `LevelName`, and leave the folder alone.
///
/// Those are two different things, and this writes the one the player sees.
/// Minecraft shows `LevelName` in its world list and changes only that when a
/// world is renamed in-game; the folder it derived at creation stays as it is,
/// which is why two of the ten real worlds on this machine already have a
/// folder that does not match their name — one of them `New World (1)`, the
/// suffix the game itself added when the folder was taken.
///
/// So renaming the folder too would be the odd choice, not the thorough one.
/// It would also be the unsafe one: the folder name is the world's identity
/// here and in a `--quickPlaySingleplayer` argument, every backup already taken
/// sits under it, and a rename that moved the directory and then failed to
/// write `level.dat` would leave the two disagreeing with nothing to say which
/// was right. One write, one meaning. The screen shows the folder beside the
/// name so the difference is visible rather than a surprise for whoever reads
/// this next.
pub async fn rename(
    paths: &Paths,
    games: &GameRegistry,
    id: &str,
    folder: &str,
    name: &str,
) -> Result<World> {
    only_when_stopped(games, id, "renaming a world")?;
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::msg("give the world a name"));
    }
    let dir = resolve(paths, id, folder)?;
    let path = dir.join(LEVEL_DAT);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::msg(format!(
                "'{folder}' has no level.dat, so there is no name in it to change"
            )))
        }
        Err(e) => return Err(Error::from(e)).ctx("reading this world's level.dat"),
    };

    let next = rewrite_level_name(&bytes, name)?;
    // By rename, so a crash leaves the world's own `level.dat` rather than
    // half of a new one. This is the file the game refuses to open if it is
    // torn.
    crate::server::write_atomic(&path, &next).await?;
    Ok(read_world(&dir, folder.to_string()).await)
}

/// This `level.dat` with `LevelName` replaced and every other byte of it
/// carried through.
///
/// The whole reason nothing here goes through a value model. `fml`, the other
/// 46 keys in `Data`, their order and a mod's own additions all survive because
/// none of them is looked at — the bytes either side of `Data` are copied, and
/// inside it every key but one is copied as the slice it arrived as. It is
/// [`super::multiplayer::ensure`]'s rule applied to the other file.
///
/// A tag type the walk does not know is the one thing it will not carry: a
/// value is stepped over by measuring it, and an unknown tag cannot be
/// measured, so [`super::nbt`] calls the file unreadable and this returns that
/// — the world keeps the name it had. One world that cannot be renamed here is
/// a better answer than a `level.dat` rewritten around a guess at where the
/// next key starts.
///
/// What is about to be written is read back before it goes anywhere near the
/// world, because the cost of a bug in the walk is somebody's world and this
/// is the cheapest check that would catch one.
fn rewrite_level_name(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    use std::io::Write;

    let plain = gunzip(bytes)?;
    let (start, end) = find_data(&plain)?;

    let mut out = Vec::with_capacity(plain.len() + name.len() + 32);
    out.extend_from_slice(&plain[..start]);
    out.extend_from_slice(&with_level_name(&plain[start..end], name)?);
    out.extend_from_slice(&plain[end..]);

    // Read our own work back. Bytes that do not parse, or that parse to a name
    // other than the one asked for, never reach the file.
    let (head, _) = find_data(&out).map_err(|e| {
        Error::msg(format!(
            "Geode built a level.dat it could not read back, so this world was left alone: {e}"
        ))
    })?;
    let mut r = Reader::new(&out, SUBJECT);
    r.at = head;
    let read = read_data(&mut r)?;
    if read.name.as_deref() != Some(name) {
        return Err(Error::msg(
            "Geode built a level.dat that does not carry the new name, so this world was left alone",
        ));
    }

    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&out).map_err(Error::from)?;
    gz.finish().map_err(Error::from)
}

/// `Data`'s bytes — end marker included — with `LevelName` replaced and every
/// other key copied through exactly as it was, in the order it was.
fn with_level_name(data: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut r = Reader::new(data, SUBJECT);
    let mut out = Vec::with_capacity(data.len() + name.len() + 16);
    let mut named = false;

    loop {
        let tag = r.u8()?;
        if tag == TAG_END {
            break;
        }
        let key = r.text()?;
        let from = r.at;
        r.skip(tag)?;

        if key == b"LevelName" {
            if tag != TAG_STRING {
                return Err(nbt::unreadable(SUBJECT, "its world name is not text"));
            }
            out.push(TAG_STRING);
            put_text(&mut out, key);
            put_text(&mut out, &java_utf8(name));
            named = true;
            continue;
        }
        out.push(tag);
        put_text(&mut out, key);
        out.extend_from_slice(&data[from..r.at]);
    }

    // A `level.dat` with no name in it at all: every real one has one, and
    // adding it is the same answer `multiplayer::rewritten` gives for a row
    // missing a key it is being asked to set.
    if !named {
        nbt::put_string(&mut out, b"LevelName", name);
    }
    out.push(TAG_END);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Copying
// ---------------------------------------------------------------------------

/// Copy a world inside the same instance, under a new name.
///
/// The copy's `level.dat` is renamed to match, which is the one place copying
/// and renaming meet: a duplicate that kept the original's `LevelName` would
/// show in the game as two worlds with one name, and the player would have to
/// guess which is which. A name whose folder is already taken is refused
/// outright — never merged into and never written over, because the thing on
/// the other side of that collision is somebody's world.
pub async fn copy(
    paths: &Paths,
    games: &GameRegistry,
    id: &str,
    folder: &str,
    name: &str,
    progress: &Progress,
) -> Result<World> {
    only_when_stopped(games, id, "copying a world")?;
    let name = name.trim();
    let from = resolve(paths, id, folder)?;
    if tokio::fs::metadata(&from).await.is_err() {
        return Err(Error::msg(format!("there is no world called '{folder}'")));
    }

    let target = folder_for(name)?;
    let to = resolve(paths, id, &target)?;
    if tokio::fs::metadata(&to).await.is_ok() {
        return Err(Error::msg(format!(
            "there is already a world in a folder called '{target}' — pick another name"
        )));
    }

    progress.stage("Copying the world", 0).await;
    super::copy_dir(&from, &to, progress)
        .await
        .ctx("copying this world")?;

    // Only now, with every byte in place. A copy that was named first and then
    // failed to copy would leave a folder claiming to be a world it is not.
    let level = to.join(LEVEL_DAT);
    if let Ok(bytes) = tokio::fs::read(&level).await {
        let next = rewrite_level_name(&bytes, name)?;
        crate::server::write_atomic(&level, &next).await?;
    }
    Ok(read_world(&to, target).await)
}

// ---------------------------------------------------------------------------
// Backing up, and putting one back
// ---------------------------------------------------------------------------

/// Timestamped so the name sorts chronologically and survives any filesystem —
/// colons are fine on Linux and forbidden on Windows. The same name
/// `server/backup.rs` gives one, for the same reasons.
fn new_name() -> String {
    format!("{}.zip", chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S"))
}

/// Where this world's copies live: one directory per world, under the
/// instance's `world-backups/`.
///
/// A directory rather than the world's name mangled into the file name, so
/// nothing ever has to parse a folder back out of a timestamp — and so that
/// deleting a world can leave its copies standing.
fn backup_dir(paths: &Paths, id: &str, folder: &str) -> Result<PathBuf> {
    Ok(paths
        .instance_world_backups(id)
        .join(one_component(folder)?))
}

/// Reject anything that is not a plain backup file name.
fn backup_file(paths: &Paths, id: &str, folder: &str, name: &str) -> Result<PathBuf> {
    if !name.ends_with(".zip") {
        return Err(Error::msg(format!("'{name}' is not a backup")));
    }
    Ok(backup_dir(paths, id, folder)?.join(one_component(name)?))
}

/// Zip a whole world into a new backup beside the instance.
///
/// **The whole world.** `server/backup.rs` leaves a server's jar, `libraries/`
/// and `versions/` out because they are 156 MB of 165 MB and every byte of
/// them is something Geode can ask the same place for again. None of that is
/// true here. A world's region files, `playerdata/`, `advancements/` and
/// `stats/` exist on this disk and nowhere else, there is no upstream to
/// re-fetch any of it from, and nothing in Geode could promise to put a
/// missing one back — so a thinned world backup would restore to a world the
/// game cannot open, which is exactly the failure the server's pairing of
/// "thin the backup" with "reinstall on start" exists to avoid. There is no
/// equivalent of that reinstall for a world, so there is no thinning.
pub async fn back_up(
    paths: &Paths,
    games: &GameRegistry,
    id: &str,
    folder: &str,
    progress: &Progress,
) -> Result<WorldBackup> {
    only_when_stopped(games, id, "backing up a world")?;
    let world = resolve(paths, id, folder)?;
    if tokio::fs::metadata(&world).await.is_err() {
        return Err(Error::msg(format!("there is no world called '{folder}'")));
    }

    let dir = backup_dir(paths, id, folder)?;
    tokio::fs::create_dir_all(&dir).await?;
    let name = new_name();
    let dest = dir.join(&name);

    progress.stage("Backing up the world", 1).await;
    let dest_for_write = dest.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::create(&dest_for_write)?;
        let mut zip = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let mut stack = vec![world.clone()];
        while let Some(current) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(rel) = path.strip_prefix(&world) else {
                    continue;
                };
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                // Entries are named relative to the world, not to `saves/`, so
                // a restore can put the world back under whichever folder it
                // is asked for.
                zip.start_file(rel.to_string_lossy().replace('\\', "/"), options)?;
                // Strict, where `server::backup::create` skips a file it
                // cannot read. That tolerance is for a *running* server, whose
                // world is being written as it is archived — a torn read there
                // is expected and a whole abandoned backup would be worse. The
                // game is stopped here by the time anything gets this far, so
                // a file that will not read is something genuinely wrong, and
                // a world backup quietly missing a region file is the exact
                // archive that reads as fine until the day it is needed.
                let mut source = std::fs::File::open(&path)?;
                std::io::copy(&mut source, &mut zip)?;
            }
        }
        zip.finish()?;
        Ok(())
    })
    .await
    .map_err(|e| Error::msg(format!("backing up this world panicked: {e}")))?
    // A half-written archive is not a backup, and leaving one in the list
    // would offer it as though it were.
    .inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })
    .ctx("backing up this world")?;

    progress.advance(1, &name).await;
    let size = tokio::fs::metadata(&dest)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(WorldBackup {
        world: folder.to_string(),
        name,
        size,
        created_at: chrono::Utc::now().timestamp(),
    })
}

/// Every copy of every world in this instance, newest first.
pub async fn list_backups(paths: &Paths, id: &str) -> Result<Vec<WorldBackup>> {
    let root = paths.instance_world_backups(id);
    let mut out = Vec::new();
    let mut worlds = match tokio::fs::read_dir(&root).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(Error::from(e)).ctx("reading this instance's world backups"),
    };

    while let Some(world) = worlds.next_entry().await? {
        if !world.file_type().await?.is_dir() {
            continue;
        }
        let Some(folder) = world.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(mut files) = tokio::fs::read_dir(world.path()).await else {
            continue;
        };
        while let Some(file) = files.next_entry().await? {
            let name = file.file_name().to_string_lossy().to_string();
            if !name.ends_with(".zip") {
                continue;
            }
            let meta = file.metadata().await?;
            out.push(WorldBackup {
                world: folder.clone(),
                name,
                size: meta.len(),
                created_at: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or_default(),
            });
        }
    }

    // Newest first: the one you want is nearly always the last one taken.
    out.sort_by(|a, b| b.name.cmp(&a.name).then_with(|| a.world.cmp(&b.world)));
    Ok(out)
}

pub async fn delete_backup(paths: &Paths, id: &str, folder: &str, name: &str) -> Result<()> {
    let archive = backup_file(paths, id, folder, name)?;
    // Said in words rather than as errno, because an `Error` here serialises
    // straight into the frontend's copy and a second Delete is an ordinary
    // double click.
    if tokio::fs::metadata(&archive).await.is_err() {
        return Err(Error::msg(format!("no backup called {name}")));
    }
    tokio::fs::remove_file(&archive).await?;
    Ok(())
}

/// Unpack a backup back over its world.
///
/// Files the archive does not hold are left alone rather than the world being
/// wiped first, which is `server::backup::restore`'s choice and right for the
/// same reason: a restore should not also be a delete of whatever was added
/// since. It is the one operation here that writes over a world wholesale, so
/// it is also the one where refusing while the game runs matters most — a
/// world put back underneath a running game is overwritten again the moment it
/// autosaves.
pub async fn restore(
    paths: &Paths,
    games: &GameRegistry,
    id: &str,
    folder: &str,
    name: &str,
) -> Result<World> {
    only_when_stopped(games, id, "restoring a world")?;
    let archive = backup_file(paths, id, folder, name)?;
    if tokio::fs::metadata(&archive).await.is_err() {
        return Err(Error::msg(format!("no backup called {name}")));
    }
    let world = resolve(paths, id, folder)?;
    tokio::fs::create_dir_all(&world).await?;

    let into = world.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&archive)?;
        extract(zip::ZipArchive::new(file)?, &into, "")
    })
    .await
    .map_err(|e| Error::msg(format!("restoring this world panicked: {e}")))?
    .ctx("restoring this world")?;

    Ok(read_world(&world, folder.to_string()).await)
}

// ---------------------------------------------------------------------------
// Importing
// ---------------------------------------------------------------------------

/// Take a world from a zip or a folder somewhere else on this machine.
///
/// It has to actually be a world: a `level.dat` is looked for before anything
/// is written, and an archive without one is refused with a reason rather than
/// unpacked into `saves/` as a folder the game will not show. An archive is
/// somebody else's file, so every entry in it goes through
/// [`crate::paths::is_safe_relative`] and one that would escape fails the
/// import whole — half-unpacking a crafted archive and calling it a skipped
/// entry would leave its debris behind.
///
/// `LevelName` is left exactly as it arrived. The world keeps the name its
/// owner gave it; only the folder is Geode's to choose, and it is refused
/// rather than reused when it is taken.
pub async fn import(
    paths: &Paths,
    games: &GameRegistry,
    id: &str,
    source: &Path,
    progress: &Progress,
) -> Result<World> {
    only_when_stopped(games, id, "importing a world")?;
    let meta = tokio::fs::metadata(source)
        .await
        .map_err(|_| Error::msg(format!("{} is not there", source.display())))?;

    // The folder name the world arrives wanting: the directory's own name, or
    // the archive's without its extension.
    let suggested = if meta.is_dir() {
        source.file_name().and_then(|n| n.to_str()).unwrap_or("")
    } else {
        source.file_stem().and_then(|n| n.to_str()).unwrap_or("")
    };
    let target = folder_for(suggested)?;
    let to = resolve(paths, id, &target)?;
    if tokio::fs::metadata(&to).await.is_ok() {
        return Err(Error::msg(format!(
            "there is already a world in a folder called '{target}' — rename or move the one you \
             are importing, or rename the one already here"
        )));
    }
    tokio::fs::create_dir_all(paths.instance_saves(id)).await?;

    if meta.is_dir() {
        if tokio::fs::metadata(source.join(LEVEL_DAT)).await.is_err() {
            return Err(Error::msg(format!(
                "{} has no level.dat in it, so it is not a world",
                source.display()
            )));
        }
        progress.stage("Importing the world", 0).await;
        super::copy_dir(source, &to, progress)
            .await
            .ctx("importing this world")?;
        return Ok(read_world(&to, target).await);
    }

    progress.stage("Importing the world", 1).await;
    let archive = source.to_path_buf();
    let into = to.clone();
    let landed = tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&archive)?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|_| Error::msg("that file is not a zip Geode can open"))?;
        let prefix = world_root(&mut zip)?;
        std::fs::create_dir_all(&into)?;
        extract(zip, &into, &prefix)
    })
    .await
    .map_err(|e| Error::msg(format!("importing this world panicked: {e}")))?;

    if let Err(e) = landed {
        // Nothing half-imported is left sitting in `saves/` looking like a
        // world: an import either produced one or produced nothing.
        let _ = tokio::fs::remove_dir_all(&to).await;
        return Err(e);
    }
    progress.advance(1, &target).await;
    Ok(read_world(&to, target).await)
}

/// The entry prefix the world sits under inside an archive, and proof that it
/// is a world at all.
///
/// A world is exported both ways: `level.dat` at the top, or one folder down
/// with the world's name on it. The shallowest `level.dat` is the real one —
/// taking the first found would let a nested copy inside somebody's backup
/// decide where the root is.
fn world_root(zip: &mut zip::ZipArchive<std::fs::File>) -> Result<String> {
    let mut best: Option<String> = None;
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let Some(parent) = name.strip_suffix(LEVEL_DAT) else {
            continue;
        };
        // `level.dat` itself, or `<something>/level.dat` — not `mylevel.dat`.
        if !(parent.is_empty() || parent.ends_with('/')) {
            continue;
        }
        let shallower = match &best {
            Some(found) => parent.len() < found.len(),
            None => true,
        };
        if shallower {
            best = Some(parent.to_string());
        }
    }
    best.ok_or_else(|| {
        Error::msg("that archive has no level.dat in it, so it is not a world")
    })
}

/// Unpack `zip` into `into`, dropping `prefix` from each entry's path.
///
/// Every entry is checked before it is joined, and one that would land outside
/// `into` fails the whole call. Separators are normalised first: an archive
/// built on Windows names its entries with backslashes, and a path left
/// unnormalised would sail past a component check that never saw a separator.
fn extract(
    mut zip: zip::ZipArchive<std::fs::File>,
    into: &Path,
    prefix: &str,
) -> Result<()> {
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().replace('\\', "/");
        let Some(rel) = name.strip_prefix(prefix) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let rel = Path::new(rel);
        if !crate::paths::is_safe_relative(rel) {
            return Err(Error::msg(format!(
                "that archive holds a file that would be written outside this instance's saves \
                 folder ('{name}'), so nothing from it was imported"
            )));
        }
        let dest = into.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Deleting
// ---------------------------------------------------------------------------

/// Take a copy of a world, then remove it.
///
/// The backup is not optional. A world is hours of somebody's life and there
/// is no undo anywhere else in reach, so one press of Delete archives it first
/// and the copy stays under the instance's `world-backups/` — which is the
/// whole reason that folder is outside `.minecraft` and outside `saves/`.
/// Freeing the space is then a second, deliberate press against the backup
/// itself. An optional safety net is one somebody turns off and then regrets;
/// this one costs a zip.
///
/// That also settles how loud the confirmation has to be: the screen names the
/// world and says where the copy went, and does not ask anybody to type a name
/// to prove they meant it, because this is no longer the irreversible step it
/// would otherwise be.
pub async fn delete(
    paths: &Paths,
    games: &GameRegistry,
    id: &str,
    folder: &str,
    progress: &Progress,
) -> Result<WorldBackup> {
    only_when_stopped(games, id, "deleting a world")?;
    let dir = resolve(paths, id, folder)?;
    if tokio::fs::metadata(&dir).await.is_err() {
        return Err(Error::msg(format!("there is no world called '{folder}'")));
    }

    // Before anything is removed, and its failure is this call's failure: a
    // delete that could not take a copy does not go on to delete the world.
    let backup = back_up(paths, games, id, folder, progress)
        .await
        .map_err(|e| {
            Error::msg(format!(
                "this world was not deleted, because Geode could not back it up first: {e}"
            ))
        })?;

    tokio::fs::remove_dir_all(&dir)
        .await
        .ctx("deleting this world")?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::nbt::{put_string, TAG_INT_ARRAY, TAG_LIST};
    use crate::paths::Scratch;

    /// A `level.dat` in the shape the real ones are in: a root compound holding
    /// `Data` and, like every Forge world measured on this machine, an `fml`
    /// compound beside it.
    ///
    /// `extra` goes inside `Data`, so a test can put a mod's own keys where a
    /// rewrite has to carry them.
    fn level_dat(name: &str, last_played: i64, extra: &[u8]) -> Vec<u8> {
        use std::io::Write;

        let mut data = Vec::new();
        put_string(&mut data, b"LevelName", name);
        data.push(TAG_LONG);
        put_text(&mut data, b"LastPlayed");
        data.extend_from_slice(&last_played.to_be_bytes());
        data.push(TAG_INT);
        put_text(&mut data, b"GameType");
        data.extend_from_slice(&1i32.to_be_bytes());
        data.push(TAG_BYTE);
        put_text(&mut data, b"hardcore");
        data.push(1);
        // `Version` is a compound, and `Name` inside it is the one nested read.
        data.push(TAG_COMPOUND);
        put_text(&mut data, b"Version");
        put_string(&mut data, b"Name", "1.21.1");
        data.push(TAG_INT);
        put_text(&mut data, b"Id");
        data.extend_from_slice(&3953i32.to_be_bytes());
        data.push(TAG_END);
        data.extend_from_slice(extra);
        data.push(TAG_END);

        let mut plain = vec![TAG_COMPOUND, 0, 0];
        plain.push(TAG_COMPOUND);
        put_text(&mut plain, b"Data");
        plain.extend_from_slice(&data);
        // Beside `Data`, the way a Forge world carries it.
        plain.push(TAG_COMPOUND);
        put_text(&mut plain, b"fml");
        put_string(&mut plain, b"loaderVersion", "4.0.0");
        plain.push(TAG_END);
        plain.push(TAG_END);

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&plain).unwrap();
        gz.finish().unwrap()
    }

    /// Keys of the sort a mod writes: a nested compound with an int array in
    /// it, an empty list, and a long. The three shapes a walk gets wrong first.
    fn moddish_keys() -> Vec<u8> {
        let mut out = Vec::new();
        out.push(TAG_COMPOUND);
        put_text(&mut out, b"MyModData");
        out.push(TAG_INT_ARRAY);
        put_text(&mut out, b"positions");
        out.extend_from_slice(&2i32.to_be_bytes());
        out.extend_from_slice(&7i32.to_be_bytes());
        out.extend_from_slice(&8i32.to_be_bytes());
        out.push(TAG_END);
        out.push(TAG_LIST);
        put_text(&mut out, b"nothing");
        out.push(TAG_END);
        out.extend_from_slice(&0i32.to_be_bytes());
        out.push(TAG_LONG);
        put_text(&mut out, b"RandomSeed");
        out.extend_from_slice(&(-42i64).to_be_bytes());
        out
    }

    /// A world on disk: a `level.dat` and the files that sit beside one.
    async fn world(paths: &Paths, id: &str, folder: &str, name: &str, when: i64) {
        let dir = paths.instance_saves(id).join(folder);
        tokio::fs::create_dir_all(dir.join("region")).await.unwrap();
        tokio::fs::create_dir_all(dir.join("playerdata")).await.unwrap();
        tokio::fs::write(dir.join(LEVEL_DAT), level_dat(name, when, &moddish_keys()))
            .await
            .unwrap();
        tokio::fs::write(dir.join("region/r.0.0.mca"), b"REGION").await.unwrap();
        tokio::fs::write(dir.join("playerdata/uuid.dat"), b"PLAYER").await.unwrap();
    }

    fn stopped() -> GameRegistry {
        GameRegistry::default()
    }

    fn quiet() -> Progress {
        Progress::silent("test")
    }

    /// Reading a `saves/` folder: the name on screen is `LevelName`, not the
    /// folder, and the two are allowed to disagree — two of the ten real worlds
    /// on this machine already do.
    #[tokio::test]
    async fn a_saves_folder_is_listed_with_the_name_the_game_shows() {
        let scratch = Scratch::new("worlds-list");
        let paths = scratch.paths.clone();
        world(&paths, "survival", "New World (1)", "New World", 1_700_000_000_000).await;
        world(&paths, "survival", "hardcore", "Don't Die", 1_800_000_000_000).await;

        let listed = list(&paths, "survival").await.unwrap();
        assert_eq!(listed.len(), 2);
        // Newest played first.
        assert_eq!(listed[0].folder, "hardcore");
        assert_eq!(listed[0].name, "Don't Die");
        assert_eq!(listed[0].last_played, Some(1_800_000_000_000));
        assert_eq!(listed[0].game_mode, Some(1));
        assert!(listed[0].hardcore);
        assert_eq!(listed[0].version.as_deref(), Some("1.21.1"));
        assert!(listed[0].size > 0, "a world has files in it");
        assert!(listed[0].trouble.is_none());

        // The one whose folder is not its name.
        assert_eq!(listed[1].folder, "New World (1)");
        assert_eq!(
            listed[1].name, "New World",
            "the screen shows LevelName, which is not the folder"
        );
    }

    /// An instance nobody has played has no `saves/` at all, which is no
    /// worlds rather than an error.
    #[tokio::test]
    async fn an_instance_with_no_saves_folder_has_no_worlds() {
        let scratch = Scratch::new("worlds-none");
        assert!(list(&scratch.paths, "fresh").await.unwrap().is_empty());
    }

    /// A folder in `saves/` that is not a world is reported, not crashed on,
    /// and the worlds beside it still list.
    ///
    /// Break it by `?`-ing the read of `level.dat` and one stray folder hides
    /// every world the player has.
    #[tokio::test]
    async fn a_folder_with_no_level_dat_is_reported_rather_than_fatal() {
        let scratch = Scratch::new("worlds-trouble");
        let paths = scratch.paths.clone();
        world(&paths, "survival", "real", "Real", 1_700_000_000_000).await;
        tokio::fs::create_dir_all(paths.instance_saves("survival").join("not-a-world"))
            .await
            .unwrap();
        // And one whose `level.dat` is there but is not a level.dat.
        let broken = paths.instance_saves("survival").join("broken");
        tokio::fs::create_dir_all(&broken).await.unwrap();
        tokio::fs::write(broken.join(LEVEL_DAT), b"hello there").await.unwrap();

        let listed = list(&paths, "survival").await.unwrap();
        assert_eq!(listed.len(), 3, "all three folders are listed: {listed:?}");

        let stray = listed.iter().find(|w| w.folder == "not-a-world").unwrap();
        assert!(
            stray.trouble.as_deref().is_some_and(|t| t.contains("no level.dat")),
            "{:?}",
            stray.trouble
        );
        assert_eq!(stray.name, "not-a-world", "it falls back to the folder name");

        let broken = listed.iter().find(|w| w.folder == "broken").unwrap();
        assert!(broken.trouble.is_some(), "an unreadable level.dat is reported");

        // And the real one is unaffected.
        let real = listed.iter().find(|w| w.folder == "real").unwrap();
        assert_eq!(real.name, "Real");
        assert!(real.trouble.is_none());
    }

    /// **The test that protects the player's file.**
    ///
    /// A rename writes `LevelName` and nothing else. Every other key in `Data`
    /// comes back byte for byte, in the order it was in, and so does the `fml`
    /// compound beside `Data` that a Forge world carries — of the ten real
    /// worlds measured on this machine, the Forge ones all do, and in one of
    /// them `fml` comes *before* `Data`.
    ///
    /// Proved by comparing the uncompressed bytes either side of the one key
    /// that was allowed to change, rather than by checking that some list of
    /// keys is still present: a key can survive and still have been moved or
    /// re-encoded. Rebuild the file from a known-key model and this fails on
    /// the first splice.
    #[tokio::test]
    async fn renaming_changes_level_name_and_not_one_other_byte() {
        let scratch = Scratch::new("worlds-rename");
        let paths = scratch.paths.clone();
        world(&paths, "survival", "world", "Before", 1_700_000_000_000).await;
        let path = paths.instance_saves("survival").join("world").join(LEVEL_DAT);

        let before = gunzip(&tokio::fs::read(&path).await.unwrap()).unwrap();
        let renamed = rename(&paths, &stopped(), "survival", "world", "After \u{1f600}")
            .await
            .unwrap();
        assert_eq!(renamed.name, "After \u{1f600}");
        assert_eq!(renamed.folder, "world", "the folder is left alone");

        let after = gunzip(&tokio::fs::read(&path).await.unwrap()).unwrap();

        // The one key that changed, and the bytes it occupied in each file.
        let old_key = {
            let mut v = Vec::new();
            put_string(&mut v, b"LevelName", "Before");
            v
        };
        let new_key = {
            let mut v = Vec::new();
            put_string(&mut v, b"LevelName", "After \u{1f600}");
            v
        };
        let at = find(&before, &old_key).expect("the old name is in the original");
        assert_eq!(
            find(&after, &new_key),
            Some(at),
            "the new name is written exactly where the old one was"
        );

        // Everything before it, and everything after it, byte for byte.
        assert_eq!(
            &before[..at],
            &after[..at],
            "bytes before LevelName were rewritten"
        );
        assert_eq!(
            &before[at + old_key.len()..],
            &after[at + new_key.len()..],
            "bytes after LevelName were rewritten"
        );
        // Which, said the other way: the files differ by exactly the two
        // encodings of the name and nothing else.
        assert_eq!(
            after.len() - new_key.len(),
            before.len() - old_key.len(),
            "the rewrite changed the length of something other than the name"
        );

        // And the emoji went in as Java's UTF-8, not plain UTF-8, so the game
        // reads the name rather than mojibake.
        assert!(
            find(&after, &java_utf8("After \u{1f600}")).is_some(),
            "the name was not written in the encoding NBT holds"
        );

        // The file still reads, and still says what it should.
        let read = read_level(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(read.name.as_deref(), Some("After \u{1f600}"));
        assert_eq!(read.last_played, Some(1_700_000_000_000));
        assert_eq!(read.version.as_deref(), Some("1.21.1"));
        assert!(read.hardcore);
    }

    /// Where `needle` sits inside `haystack`.
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|w| w == needle)
    }

    /// A `level.dat` Geode cannot read is one Geode does not write. Each of
    /// these would be a different kind of bug to overwrite.
    #[tokio::test]
    async fn a_level_dat_that_cannot_be_read_is_left_exactly_as_it_was() {
        let good = level_dat("Home", 1, &[]);
        let mut truncated = good.clone();
        truncated.truncate(good.len() - 4);

        let mut plain = gunzip(&good).unwrap();
        plain.extend_from_slice(b"extra");
        let trailing = {
            use std::io::Write;
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(&plain).unwrap();
            gz.finish().unwrap()
        };
        let no_data = {
            use std::io::Write;
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(&[TAG_COMPOUND, 0, 0, TAG_END]).unwrap();
            gz.finish().unwrap()
        };

        // A key whose tag type this walk does not know. Every other value in
        // `Data` is stepped over by measuring it, and there is no length to be
        // worked out for one of these — so the file is refused rather than
        // rewritten around a guess at where `LevelName` ends up. The module's
        // own prose used to say an unknown tag "survives because none is ever
        // interpreted", which is the copying rule read one step too far.
        let unknown_tag = {
            let mut extra = vec![99u8];
            put_text(&mut extra, b"FromTheFuture");
            extra.extend_from_slice(&7i64.to_be_bytes());
            level_dat("Home", 1, &extra)
        };

        for (what, bytes) in [
            ("truncated gzip", truncated),
            ("trailing bytes", trailing),
            ("no Data compound", no_data),
            ("an unknown tag type", unknown_tag),
            ("not compressed at all", gunzip(&good).unwrap()),
            ("not NBT at all", b"hello there".to_vec()),
        ] {
            let scratch = Scratch::new("worlds-unreadable");
            let paths = scratch.paths.clone();
            let dir = paths.instance_saves("survival").join("world");
            tokio::fs::create_dir_all(&dir).await.unwrap();
            tokio::fs::write(dir.join(LEVEL_DAT), &bytes).await.unwrap();

            let err = rename(&paths, &stopped(), "survival", "world", "New")
                .await
                .expect_err(&format!("{what} was accepted"));
            assert!(err.to_string().contains("left alone"), "{what}: {err}");
            assert_eq!(
                tokio::fs::read(dir.join(LEVEL_DAT)).await.unwrap(),
                bytes,
                "{what} was written over"
            );
        }
    }

    /// A world nested deeper than the reader goes is refused like any other it
    /// cannot read — not recursed into.
    ///
    /// A Rust stack overflow is an abort, not an error, so without the ceiling
    /// this file takes the launcher with it instead of reaching "left alone".
    #[tokio::test]
    async fn a_level_dat_nested_deeper_than_the_reader_goes_is_left_alone() {
        use std::io::Write;
        const DEEP: usize = 50_000;

        let mut data = Vec::new();
        put_string(&mut data, b"LevelName", "Deep");
        for _ in 0..DEEP {
            data.push(TAG_COMPOUND);
            put_text(&mut data, b"deeper");
        }
        data.extend(std::iter::repeat_n(TAG_END, DEEP + 1));

        let mut plain = vec![TAG_COMPOUND, 0, 0, TAG_COMPOUND];
        put_text(&mut plain, b"Data");
        plain.extend_from_slice(&data);
        plain.push(TAG_END);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&plain).unwrap();
        let bytes = gz.finish().unwrap();

        let scratch = Scratch::new("worlds-deep");
        let paths = scratch.paths.clone();
        let dir = paths.instance_saves("survival").join("world");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join(LEVEL_DAT), &bytes).await.unwrap();

        let err = rename(&paths, &stopped(), "survival", "world", "New")
            .await
            .expect_err("a file nested past the ceiling was read anyway");
        assert!(err.to_string().contains("nest deeper"), "{err}");
        assert_eq!(tokio::fs::read(dir.join(LEVEL_DAT)).await.unwrap(), bytes);
    }

    /// A copy is its own world: its files are independent of the original's,
    /// and its `LevelName` is the new name rather than the original's.
    #[tokio::test]
    async fn a_copy_is_an_independent_world_under_its_own_name() {
        let scratch = Scratch::new("worlds-copy");
        let paths = scratch.paths.clone();
        world(&paths, "survival", "world", "Original", 1_700_000_000_000).await;

        let made = copy(&paths, &stopped(), "survival", "world", "Copy of Original", &quiet())
            .await
            .unwrap();
        assert_eq!(made.folder, "Copy of Original");
        assert_eq!(
            made.name, "Copy of Original",
            "a copy that kept the original's name would show as two worlds with one name"
        );
        // The keys a mod wrote came along, because a copy is a copy.
        assert_eq!(made.version.as_deref(), Some("1.21.1"));

        let saves = paths.instance_saves("survival");
        assert_eq!(
            tokio::fs::read(saves.join("Copy of Original/region/r.0.0.mca")).await.unwrap(),
            b"REGION"
        );

        // Independent: writing to one does not reach the other.
        tokio::fs::write(saves.join("Copy of Original/region/r.0.0.mca"), b"CHANGED")
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(saves.join("world/region/r.0.0.mca")).await.unwrap(),
            b"REGION",
            "the copy shares bytes with the original"
        );
        // And the original kept its own name.
        let listed = list(&paths, "survival").await.unwrap();
        let original = listed.iter().find(|w| w.folder == "world").unwrap();
        assert_eq!(original.name, "Original");
    }

    /// A name whose folder is already taken is refused. Never merged into and
    /// never written over — the thing on the other side is somebody's world.
    #[tokio::test]
    async fn copying_onto_an_existing_world_is_refused_rather_than_merged() {
        let scratch = Scratch::new("worlds-collide");
        let paths = scratch.paths.clone();
        world(&paths, "survival", "world", "Original", 1).await;
        world(&paths, "survival", "Taken", "Something Else", 2).await;

        let err = copy(&paths, &stopped(), "survival", "world", "Taken", &quiet())
            .await
            .expect_err("a collision was accepted");
        assert!(err.to_string().contains("already a world"), "{err}");

        // The world that was already there is untouched.
        let listed = list(&paths, "survival").await.unwrap();
        let taken = listed.iter().find(|w| w.folder == "Taken").unwrap();
        assert_eq!(taken.name, "Something Else");
        assert_eq!(
            tokio::fs::read(paths.instance_saves("survival").join("Taken/region/r.0.0.mca"))
                .await
                .unwrap(),
            b"REGION"
        );
    }

    /// A backup carries the whole world, and a restore puts it back.
    ///
    /// The whole world is the point: `server/backup.rs` thins a server's
    /// because Geode can fetch the thinned part again, and nothing here can.
    #[tokio::test]
    async fn a_backup_round_trips_the_whole_world() {
        let scratch = Scratch::new("worlds-backup");
        let paths = scratch.paths.clone();
        world(&paths, "survival", "world", "Home", 1_700_000_000_000).await;

        let info = back_up(&paths, &stopped(), "survival", "world", &quiet())
            .await
            .unwrap();
        assert_eq!(info.world, "world");
        assert!(info.size > 0, "an empty archive is not a backup");
        assert_eq!(list_backups(&paths, "survival").await.unwrap(), vec![info.clone()]);

        // The archive lives outside `.minecraft`, so the next backup cannot
        // contain the last one and an export cannot sweep it up.
        let dir = paths.instance_world_backups("survival").join("world");
        assert!(dir.join(&info.name).exists());
        assert!(
            !dir.starts_with(paths.game_dir("survival")),
            "a world backup is inside .minecraft"
        );

        // Break the world the way a bad afternoon would.
        let saves = paths.instance_saves("survival");
        tokio::fs::remove_file(saves.join("world/region/r.0.0.mca")).await.unwrap();
        tokio::fs::write(saves.join("world/playerdata/uuid.dat"), b"RUINED").await.unwrap();
        rename(&paths, &stopped(), "survival", "world", "Ruined").await.unwrap();

        let back = restore(&paths, &stopped(), "survival", "world", &info.name)
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(saves.join("world/region/r.0.0.mca")).await.unwrap(),
            b"REGION",
            "a deleted region file should come back"
        );
        assert_eq!(
            tokio::fs::read(saves.join("world/playerdata/uuid.dat")).await.unwrap(),
            b"PLAYER",
            "an overwritten player file should be put back"
        );
        assert_eq!(back.name, "Home", "level.dat came back with it");
        assert_eq!(back.last_played, Some(1_700_000_000_000));

        delete_backup(&paths, "survival", "world", &info.name).await.unwrap();
        assert!(list_backups(&paths, "survival").await.unwrap().is_empty());
        let err = delete_backup(&paths, "survival", "world", &info.name)
            .await
            .expect_err("a second delete");
        assert!(!err.to_string().contains("os error"), "{err}");
    }

    /// Deleting a world takes a copy first, and the copy outlives it. That is
    /// what makes one press of Delete something a player can come back from.
    #[tokio::test]
    async fn deleting_a_world_backs_it_up_first() {
        let scratch = Scratch::new("worlds-delete");
        let paths = scratch.paths.clone();
        world(&paths, "survival", "world", "Home", 1_700_000_000_000).await;

        let backup = delete(&paths, &stopped(), "survival", "world", &quiet()).await.unwrap();
        assert!(list(&paths, "survival").await.unwrap().is_empty(), "the world is gone");
        assert_eq!(
            list_backups(&paths, "survival").await.unwrap(),
            vec![backup.clone()],
            "its copy is not"
        );

        // And it restores from that copy, which is the whole claim.
        let back = restore(&paths, &stopped(), "survival", "world", &backup.name)
            .await
            .unwrap();
        assert_eq!(back.name, "Home");
        assert_eq!(
            tokio::fs::read(paths.instance_saves("survival").join("world/region/r.0.0.mca"))
                .await
                .unwrap(),
            b"REGION"
        );
    }

    /// Importing a zip: the world lands under its own folder, and `LevelName`
    /// is left exactly as its owner wrote it.
    #[tokio::test]
    async fn importing_a_zip_takes_the_world_and_leaves_its_name_alone() {
        let scratch = Scratch::new("worlds-import");
        let paths = scratch.paths.clone();
        let archive = scratch.paths.root.join("Shared World.zip");
        tokio::fs::create_dir_all(&scratch.paths.root).await.unwrap();

        // Nested one folder down, which is how a world usually travels.
        zip_up(
            &archive,
            &[
                ("Shared World/level.dat", &level_dat("Theirs", 99, &moddish_keys())),
                ("Shared World/region/r.0.0.mca", b"THEIRS"),
                ("Shared World/", b""),
            ],
        );

        let made = import(&paths, &stopped(), "survival", &archive, &quiet()).await.unwrap();
        assert_eq!(made.folder, "Shared World", "named after the archive");
        assert_eq!(
            made.name, "Theirs",
            "the world keeps the name its owner gave it"
        );
        assert_eq!(made.version.as_deref(), Some("1.21.1"));
        assert_eq!(
            tokio::fs::read(paths.instance_saves("survival").join("Shared World/region/r.0.0.mca"))
                .await
                .unwrap(),
            b"THEIRS"
        );

        // A second import of the same archive is refused, not merged.
        let err = import(&paths, &stopped(), "survival", &archive, &quiet())
            .await
            .expect_err("a collision was accepted");
        assert!(err.to_string().contains("already a world"), "{err}");
    }

    /// An archive with no `level.dat` is not a world, and is refused before
    /// anything is written.
    #[tokio::test]
    async fn importing_something_that_is_not_a_world_is_refused() {
        let scratch = Scratch::new("worlds-import-junk");
        let paths = scratch.paths.clone();
        tokio::fs::create_dir_all(&scratch.paths.root).await.unwrap();

        let archive = scratch.paths.root.join("Not A World.zip");
        zip_up(&archive, &[("mods/cool.jar", b"JAR"), ("README.txt", b"hi")]);
        let err = import(&paths, &stopped(), "survival", &archive, &quiet())
            .await
            .expect_err("a zip with no level.dat was accepted");
        assert!(err.to_string().contains("no level.dat"), "{err}");
        assert!(
            tokio::fs::metadata(paths.instance_saves("survival").join("Not A World"))
                .await
                .is_err(),
            "a refused import left a folder behind"
        );

        // A folder is held to the same rule.
        let loose = scratch.paths.root.join("Loose Folder");
        tokio::fs::create_dir_all(&loose).await.unwrap();
        tokio::fs::write(loose.join("notes.txt"), b"hi").await.unwrap();
        let err = import(&paths, &stopped(), "survival", &loose, &quiet())
            .await
            .expect_err("a folder with no level.dat was accepted");
        assert!(err.to_string().contains("no level.dat"), "{err}");

        // And a folder that is a world does import.
        tokio::fs::write(loose.join(LEVEL_DAT), level_dat("Loose", 5, &[])).await.unwrap();
        let made = import(&paths, &stopped(), "survival", &loose, &quiet()).await.unwrap();
        assert_eq!(made.name, "Loose");
        assert_eq!(made.folder, "Loose Folder");
    }

    /// **The zip-slip guard.** An archive is somebody else's file, and an entry
    /// that would be written outside `saves/` fails the import whole.
    ///
    /// Break [`crate::paths::is_safe_relative`]'s use here and a crafted world
    /// writes wherever it likes — into `mods/`, into `config/`, out of the
    /// instance altogether.
    #[tokio::test]
    async fn importing_an_archive_that_escapes_saves_is_refused() {
        let scratch = Scratch::new("worlds-slip");
        let paths = scratch.paths.clone();
        tokio::fs::create_dir_all(&scratch.paths.root).await.unwrap();
        // Something outside `saves/` for a crafted entry to aim at.
        let victim = paths.game_dir("survival").join("mods");
        tokio::fs::create_dir_all(&victim).await.unwrap();
        tokio::fs::write(victim.join("real.jar"), b"MINE").await.unwrap();

        for (what, entry) in [
            ("a parent-directory escape", "../../mods/real.jar"),
            ("an absolute path", "/etc/passwd"),
            ("a Windows-separated escape", "..\\..\\mods\\real.jar"),
        ] {
            let archive = scratch.paths.root.join("Crafted.zip");
            zip_up(
                &archive,
                &[
                    ("level.dat", &level_dat("Crafted", 1, &[])),
                    (entry, b"EVIL"),
                ],
            );

            let err = import(&paths, &stopped(), "survival", &archive, &quiet())
                .await
                .expect_err(&format!("{what} was accepted"));
            assert!(
                err.to_string().contains("outside this instance's saves"),
                "{what}: {err}"
            );
            // Nothing was written: not the file it aimed at, and not the
            // half-imported world either.
            assert_eq!(
                tokio::fs::read(victim.join("real.jar")).await.unwrap(),
                b"MINE",
                "{what} overwrote a file outside saves/"
            );
            assert!(
                tokio::fs::metadata(paths.instance_saves("survival").join("Crafted"))
                    .await
                    .is_err(),
                "{what} left a half-imported world behind"
            );
            tokio::fs::remove_file(&archive).await.unwrap();
        }
    }

    /// A world folder name arriving over IPC cannot escape `saves/` either.
    #[test]
    fn a_world_name_cannot_escape_its_folder() {
        let paths = Paths {
            root: PathBuf::from("/tmp/geode-test"),
        };
        for bad in ["../../etc", "nested/name", "/etc/passwd", "", "   "] {
            assert!(resolve(&paths, "survival", bad).is_err(), "{bad} was accepted");
        }
        assert!(resolve(&paths, "survival", "New World (1)").is_ok());
        // And the same for a backup's name.
        assert!(backup_file(&paths, "survival", "world", "../x.zip").is_err());
        assert!(backup_file(&paths, "survival", "world", "notazip.txt").is_err());
        assert!(backup_file(&paths, "survival", "world", "2026-01-01T00-00-00.zip").is_ok());
    }

    /// A folder name is derived from a display name the way Minecraft derives
    /// one, and only the characters no filesystem takes are replaced.
    #[test]
    fn a_folder_name_survives_being_carried_to_another_platform() {
        assert_eq!(folder_for("My World").unwrap(), "My World");
        assert_eq!(folder_for("  padded  ").unwrap(), "padded");
        assert_eq!(folder_for("a/b:c*d?e").unwrap(), "a_b_c_d_e");
        // Windows drops a trailing dot, which would leave a folder nothing
        // here could find again.
        assert_eq!(folder_for("Trailing...").unwrap(), "Trailing");
        // Nothing but separators is still a name once they are replaced, and
        // `___` is a folder like any other.
        assert_eq!(folder_for("///").unwrap(), "___");
        assert!(folder_for("").is_err());
        assert!(folder_for("   ").is_err());
    }

    /// **Every operation that touches a world refuses while the game has it
    /// open**, and says why on screen.
    ///
    /// A backup taken now holds region files with no matching `level.dat`, and
    /// a restore is overwritten again at the next autosave. A server can be
    /// asked to flush first; a client cannot be asked anything, so refusing is
    /// the only correct answer. Nothing is written by any of these.
    #[tokio::test]
    async fn every_operation_refuses_while_the_game_is_running() {
        let scratch = Scratch::new("worlds-running");
        let paths = scratch.paths.clone();
        world(&paths, "survival", "world", "Home", 1_700_000_000_000).await;
        let info = back_up(&paths, &stopped(), "survival", "world", &quiet())
            .await
            .unwrap();

        let archive = scratch.paths.root.join("Other.zip");
        zip_up(&archive, &[("level.dat", &level_dat("Other", 1, &[]))]);

        // A game holding this instance. The registry is what a real launch
        // writes to, so this is the state it leaves behind.
        let games = GameRegistry::default();
        games.insert("survival".into(), crate::launch::RunningGame::none_really());

        let path = paths.instance_saves("survival").join("world").join(LEVEL_DAT);
        let before = tokio::fs::read(&path).await.unwrap();

        let refusals: Vec<(&str, String)> = vec![
            (
                "rename",
                rename(&paths, &games, "survival", "world", "Renamed")
                    .await
                    .expect_err("rename while running")
                    .to_string(),
            ),
            (
                "copy",
                copy(&paths, &games, "survival", "world", "Copy", &quiet())
                    .await
                    .expect_err("copy while running")
                    .to_string(),
            ),
            (
                "back up",
                back_up(&paths, &games, "survival", "world", &quiet())
                    .await
                    .expect_err("back up while running")
                    .to_string(),
            ),
            (
                "restore",
                restore(&paths, &games, "survival", "world", &info.name)
                    .await
                    .expect_err("restore while running")
                    .to_string(),
            ),
            (
                "import",
                import(&paths, &games, "survival", &archive, &quiet())
                    .await
                    .expect_err("import while running")
                    .to_string(),
            ),
            (
                "delete",
                delete(&paths, &games, "survival", "world", &quiet())
                    .await
                    .expect_err("delete while running")
                    .to_string(),
            ),
        ];

        for (what, said) in &refusals {
            assert!(
                said.starts_with("Stop the game before"),
                "{what} refused without saying to stop the game: {said}"
            );
            assert!(
                said.contains("level.dat"),
                "{what} refused without saying why: {said}"
            );
        }

        // Nothing was written by any of them.
        assert_eq!(tokio::fs::read(&path).await.unwrap(), before);
        let listed = list(&paths, "survival").await.unwrap();
        assert_eq!(listed.len(), 1, "something was created: {listed:?}");
        assert_eq!(listed[0].name, "Home");
        assert_eq!(
            list_backups(&paths, "survival").await.unwrap(),
            vec![info],
            "a backup was taken while the game held the world"
        );

        // And once it stops, the same calls go through.
        games.remove("survival");
        assert!(rename(&paths, &games, "survival", "world", "Renamed").await.is_ok());
    }

    /// A zip with exactly these entries. Directory entries are the ones whose
    /// name ends in `/`.
    fn zip_up(at: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;

        let file = std::fs::File::create(at).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in entries {
            if let Some(dir) = name.strip_suffix('/') {
                zip.add_directory(dir, options).unwrap();
                continue;
            }
            zip.start_file(*name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
    }
}

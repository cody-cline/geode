//! What a shared pack says about itself, beyond what a `.mrpack` can.
//!
//! A Geode pack file **is** a `.mrpack` — `modrinth.index.json`, `overrides/`,
//! `client-overrides/`, `server-overrides/`, all at the root and all valid —
//! with one extra directory, `geode/`, alongside them. Nothing is nested and
//! nothing is wrapped.
//!
//! That shape is the whole design. The index *instructs*: it holds every byte
//! another launcher needs to rebuild a working instance, so Prism or the
//! Modrinth app open a Geode pack as an ordinary modpack and get it right. The
//! manifest here only *annotates*: the launcher settings, artwork and
//! provenance that `.mrpack` has nowhere to put. Delete `geode/` and the pack
//! must still install correctly elsewhere — there is a test for exactly that.
//!
//! The corollary is a rule worth stating: never write a download URL into this
//! half. The moment the same fact lives in both places they can disagree, and
//! the index is the one other software reads.

use serde::{Deserialize, Serialize};

/// The newest layout this build understands, and so the highest a pack may
/// claim before it is refused.
///
/// Bumped when the layout changes in a way an older reader would misread.
/// Format 2 is the `disabled` note on a file the index carries: see
/// [`format_for`].
pub const FORMAT: u32 = 2;

/// The layout a reader has to understand to read a pack with these notes,
/// which is not always the newest one.
///
/// Only one thing a note can say is *misread* rather than merely missed by a
/// format-1 Geode: `disabled` on a file with no `source`. Such a file is in
/// the index, so it is fetched; a format-1 reader applies the off state only
/// to files it fetched by id, so it switches nothing off and a mod the sharer
/// turned off — usually because it crashed — arrives switched on. Claiming 2
/// makes that reader refuse the pack and say to update, which is the honest
/// answer.
///
/// Every other pack keeps saying 1, because refusing one costs the recipient
/// every mod in it, and most packs have nothing turned off.
pub fn format_for(files: &[FileNote]) -> u32 {
    if files.iter().any(|n| n.disabled && n.source.is_none()) {
        2
    } else {
        1
    }
}

/// The file extension.
///
/// Not `.geode`: that is already the Geometry Dash mod SDK's extension, and it
/// is also a zip, so the collision would be a real one rather than cosmetic.
pub const EXTENSION: &str = "geodepack";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub geode_format: u32,
    /// What was exported. A server pack carries per-file sides an instance
    /// pack has no use for.
    pub kind: Kind,
    pub profile: Profile,
    /// Where the artwork lives inside the zip, if any was exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
    /// The modpack this was itself built from, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::instance::PackSource>,
    /// Per-file notes, keyed by the same path the index uses.
    ///
    /// Only files that need saying something about appear here; a pack where
    /// every mod runs on both sides carries an empty map.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileNote>,
    /// In a server's pack, the revision of what this very pack holds, so a
    /// player keeps the revision of what they received rather than of a
    /// description read before it was built — a server changed and changed
    /// back in between would otherwise never be noticed.
    ///
    /// An instance's pack has none, and so does a server's from a Geode older
    /// than this, which an older reader ignores in turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// In a server's pack, how the server should appear in its players'
    /// multiplayer list — so a player who imports it can join from the game's
    /// own screen rather than being handed a title screen and no address.
    ///
    /// Deliberately does **not** bump [`FORMAT`]: an older Geode that ignores
    /// this adds no row, and the instance it builds is right in every other
    /// respect. That is a miss, not a misread — see [`format_for`].
    ///
    /// An instance's pack never carries one. There is no server behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<Listing>,
    /// Which of the settings files this pack carries are the recipient's to
    /// keep their own changes to, and which the sender replaces every time.
    ///
    /// A top-level field of its own rather than a note on each file:
    /// [`FileNote`] is "keyed by the same path the index uses", and a settings
    /// file is in no index — making that comment a lie would cost more than
    /// the field does.
    ///
    /// Deliberately does **not** bump [`FORMAT`]. A reader that ignores this
    /// writes every settings file whole on every update, which is exactly what
    /// every reader did before the field existed: a miss, not a misread — see
    /// [`format_for`]. The admin should hear once that a player on an older
    /// Geode gets the whole file rather than only the settings that changed.
    ///
    /// `None` from a pack that syncs nothing, which is every pack but a
    /// server's whose admin has chosen a behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Settings>,
    /// In a server's pack, the mods it does not allow its players to run.
    ///
    /// A standing rule, not a change: it is applied on every update, so a
    /// player who turns one back on has it turned off again by the next update
    /// — not by their next Play, which downloads no pack while the server's
    /// revision has not moved. What it may name is an identity, never a file —
    /// see [`Forbidden`].
    ///
    /// Deliberately does **not** bump [`FORMAT`], for the same reason
    /// [`Manifest::listing`] does not: an older Geode ignores the field and
    /// turns nothing off, and the instance it builds is right in every other
    /// respect. Refusing the pack whole would cost the recipient every mod in
    /// it, which is a worse answer than a rule that goes unenforced on an old
    /// build — and the rule is not enforceable anyway, which is the one thing
    /// an admin has to be told about it.
    ///
    /// An instance's pack never carries any. Nobody is behind it to have a
    /// rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden: Vec<Forbidden>,
}

/// What a pack says about the settings files it carries: which paths the
/// recipient may keep their own changes to, and how far the sender's pushes
/// have got.
///
/// One type on both sides of the wire, because it is one fact. On the admin's
/// machine it lives in `servers/<id>/share.json` and `at` is the newest push
/// that server has made; inside a pack it is the copy that pack was built
/// from, and `at` is the push that pack holds — which is what a player's
/// `settings_at` becomes once the pack has landed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// One entry per path somebody has chosen a behaviour for. A folder covers
    /// everything under it, and the longest entry covering a file decides it —
    /// see [`Settings::covering`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<SyncedPath>,
    /// The settings the sender owns: one entry per key they have pushed, with
    /// the value they pushed and the push it went out at.
    ///
    /// Cumulative, and one entry per key rather than a log of pushes. A log
    /// would grow for ever and could never be pruned — nobody can know every
    /// player has applied an entry — where this is prunable by definition, and
    /// a player who skipped a year of pushes still ends up with exactly the
    /// current values in one pass, with no ordering to get right.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<Override>,
    /// The push these behaviours were last changed at. Monotonic and never
    /// reused: a number that went backwards would send a player a file they
    /// have since made their own.
    #[serde(default)]
    pub at: u32,
}

/// One setting the sender owns, inside one of the files it syncs.
///
/// Two things here are what make this safe to apply to a stranger's file, and
/// both are deliberate:
///
/// `value` is the **raw source text** of the value as it stands in the
/// sender's own file — `8`, `true`, `"right"`, `["vanilla","fabric"]`,
/// `minecraft\:normal`. Nothing has to understand it: the splice puts those
/// bytes where the old value's bytes were. That is what keeps TOML's types,
/// JSON's quoting and Java's escaping intact, and it is why no re-serialiser
/// is ever needed — any re-escaper would differ from the one that wrote the
/// file and churn values nobody touched.
///
/// `key` is a list of literal segments and never a dotted string. Real table
/// names hold dots, spaces and colons (`["Configs for Create: Tweaked
/// Controllers"]`), so a dotted path would need quoting rules — and quoting
/// rules applied to text a stranger's pack supplies are somewhere to hide an
/// injection. Segments matched literally have no syntax to abuse — as long as
/// a segment is a name rather than lines of a file, which is part of what
/// [`Settings::checked`] holds this to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Override {
    /// The file this setting is in, relative to the game directory. Held to
    /// the same rule as a [`SyncedPath`], and only applied at all when a
    /// [`Policy::Merge`] path covers it.
    pub path: String,
    /// The key, segment by segment. One segment for a key-and-value file.
    pub key: Vec<String>,
    /// The value to put there, as raw text. `None` removes the key — the one
    /// irreversible thing in this whole mechanism, which is why the sender has
    /// to ask for it rather than have it inferred from a diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// What the sender's own copy held before this push, as raw text. `None`
    /// when the sender's copy did not have the key either.
    ///
    /// Not a baseline — the recipient stores nothing and this is the same for
    /// everybody — and nothing is decided by it. It is only how the recipient
    /// can say whether *they* had changed this setting: a file holding what
    /// the sender last published is a setting they never touched, and one
    /// holding anything else is a setting of theirs that this push moved. Told
    /// apart, the second earns a line in their report and the first collapses
    /// into a count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
    /// The push this value went out at. The number that makes the whole
    /// mechanism work: a recipient applies every entry above the push they
    /// have caught up with, and because this never moves again, a recipient
    /// who changes the setting back keeps their change for ever.
    #[serde(default)]
    pub at: u32,
}

/// One path whose behaviour has been chosen, relative to the game directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyncedPath {
    /// `config`, or one file inside it. A folder covers everything under it.
    pub path: String,
    pub policy: Policy,
    /// The push this behaviour was chosen at. A player who has not caught up
    /// with it gets the file whole once, whatever the policy says, so a switch
    /// from one behaviour to another loses nothing the admin had already
    /// changed.
    #[serde(default)]
    pub from: u32,
}

/// What happens to a player's own changes to a synced file.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Policy {
    /// Sent when the player has no copy, and never touched again.
    ShipOnce,
    /// Sent whole the first time; afterwards only the settings the admin
    /// pushes, spliced into whatever the player has made of the file.
    Merge,
    /// The sender's copy replaces the player's on every update. What sharing a
    /// config folder has always done.
    Enforce,
}

impl Policy {
    /// The one spelling, used on the wire and in a server's revision alike, so
    /// a behaviour cannot be named two ways.
    pub fn as_str(self) -> &'static str {
        match self {
            Policy::ShipOnce => "ship-once",
            Policy::Merge => "merge",
            Policy::Enforce => "enforce",
        }
    }
}

impl Settings {
    /// The entry covering a file at this path inside the instance: the file
    /// itself, or the longest folder above it that somebody chose a behaviour
    /// for. `None` when nobody did.
    ///
    /// One matcher, because two things ask this and must never disagree. An
    /// import decides per file whether to write the pack's copy at all, and
    /// `share::pack::server_revision` decides per file whether its bytes are
    /// part of what players receive. A second matcher that drifted would hash
    /// a file nobody is sent, or send one nobody is ever told about.
    pub fn covering(&self, rel: &std::path::Path) -> Option<&SyncedPath> {
        self.paths
            .iter()
            .filter(|named| named.allowed() && covers(std::path::Path::new(&named.path), rel))
            .max_by_key(|named| std::path::Path::new(&named.path).components().count())
    }

    /// What happens to this file, for a pack that named no behaviour for it.
    ///
    /// [`Policy::Enforce`] is the answer, and it is the migration rule in one
    /// place: a share that sends its config folder and has chosen nothing
    /// behaves exactly as sharing a config folder always has, with every file
    /// replaced on every update.
    pub fn policy_for(&self, rel: &std::path::Path) -> Policy {
        self.covering(rel).map_or(Policy::Enforce, |named| named.policy)
    }

    /// This list as a pack carries it: with `path` spelled out as the
    /// behaviour already in force, when nothing in it covers that path.
    ///
    /// A pack states the behaviour of everything it sends, the choice nobody
    /// made included, so the Geode reading it has one rule to follow for every
    /// file rather than a rule and an exception. What gets spelled out is
    /// [`Policy::Enforce`] from before counting began — exactly what sending a
    /// folder of settings files has always done — so no player receives
    /// anything different for its being said out loud, and the revision says
    /// nothing about it, because nothing about it has changed.
    ///
    /// What it does buy, and the reason it is worth a field in every pack: the
    /// recipient can tell a file its sender syncs from a file out of any other
    /// pack. A settings file that cannot be written is then one line in a
    /// report rather than a failed update and a launch the player cannot have.
    pub fn spelling_out(&self, path: &str) -> Settings {
        let mut spelled = self.clone();
        spelled.spell_out(path, Policy::Enforce);
        spelled
    }

    /// Say what happens to one path, when nothing in this list covers it
    /// already.
    ///
    /// What [`Settings::spelling_out`] does for a whole folder, for a file
    /// that has no folder to be named under. A client file lands at the top of
    /// a player's game directory — `options.txt`, not `client-files/options.txt`
    /// — where the only thing that can name it is its own name, so the pack
    /// says what happens to it one file at a time.
    ///
    /// `from` is 0, the push before counting began, because this is the
    /// behaviour already in force rather than a new choice: a player who has
    /// the file keeps theirs, and one who does not is sent it.
    pub fn spell_out(&mut self, path: &str, policy: Policy) {
        if self.covering(std::path::Path::new(path)).is_some() {
            return;
        }
        self.paths.push(SyncedPath {
            path: path.to_string(),
            policy,
            from: 0,
        });
    }

    /// This list with every path spelled as the recipient's own.
    ///
    /// A server keeps the files that are its players' and not its own in
    /// `client-files/`, a folder no loader reads — which is the only way
    /// Minecraft's `options.txt` can travel at all, since a server has no such
    /// file. A player's game has no such folder either: there it is
    /// `options.txt`, at the top of the game directory. So the pack names the
    /// path the file lands at, through the one function that turns a server's
    /// path into a player's, because a behaviour chosen for a path nothing
    /// lands at would be a choice that reaches nobody.
    ///
    /// An entry whose new spelling is not a path a pack may name is dropped
    /// rather than carried: the recipient would refuse the whole half over it
    /// — see [`Settings::checked`] — and write every file it describes whole,
    /// which is the one outcome a list of what to keep exists to avoid.
    pub fn for_players(&self) -> Settings {
        let shown = |path: &str| {
            crate::instance::modpack::pack_rel(std::path::Path::new(path))
                .to_string_lossy()
                .replace('\\', "/")
        };
        Settings {
            paths: self
                .paths
                .iter()
                .map(|named| SyncedPath {
                    path: shown(&named.path),
                    ..named.clone()
                })
                .filter(|named| named.allowed())
                .collect(),
            overrides: self
                .overrides
                .iter()
                .map(|entry| Override {
                    path: shown(&entry.path),
                    ..entry.clone()
                })
                .filter(|entry| entry.allowed())
                .collect(),
            at: self.at,
        }
    }

    /// Choose what happens to a player's own changes to a path.
    ///
    /// Stamped with the next push, so a player who has not caught up gets the
    /// file whole once and keeps their own changes from then on — nothing the
    /// admin had already changed is lost at a switch. Choosing what is already
    /// in force writes nothing, so turning a folder off and on again does not
    /// send every player a pack over a choice that never moved.
    pub fn choose(&mut self, path: &str, policy: Policy) {
        if self.policy_for(std::path::Path::new(path)) == policy {
            return;
        }
        self.at = self.at.saturating_add(1);
        let from = self.at;
        match self.paths.iter_mut().find(|named| named.path == path) {
            Some(named) => {
                named.policy = policy;
                named.from = from;
            }
            None => self.paths.push(SyncedPath {
                path: path.to_string(),
                policy,
                from,
            }),
        }
    }
}

impl SyncedPath {
    /// Whether this is a path a pack may name at all — see [`nameable`].
    fn allowed(&self) -> bool {
        nameable(&self.path)
    }
}

impl Override {
    /// Whether this is a path a pack may name at all — see [`nameable`].
    pub fn allowed(&self) -> bool {
        nameable(&self.path)
    }

    /// Whether this entry names the same setting as another, which is what
    /// makes the list cumulative: a second push of one key replaces its entry
    /// rather than adding to a log.
    pub fn same_key(&self, other: &Override) -> bool {
        self.path == other.path && self.key == other.key
    }
}

/// Whether a pack may name this path in its settings half.
///
/// Safe-relative, so no entry reaches outside the instance, and never inside
/// `mods/` or `client-mods/`. What a settings entry decides is whether the
/// pack's own copy of a file is written, whether failing to write it stops the
/// import, and — for an override — which bytes inside it are replaced. None of
/// those is a decision a stranger's pack gets to make about a jar. Geode names
/// neither folder; a pack that does was written by something else.
fn nameable(path: &str) -> bool {
    let rel = std::path::Path::new(path);
    if path.trim().is_empty() || !crate::paths::is_safe_relative(rel) {
        return false;
    }
    !matches!(
        rel.components().next().and_then(|c| c.as_os_str().to_str()),
        Some("mods") | Some("client-mods")
    )
}

/// What a settings half may hold at most.
///
/// A pack is untrusted input — this is the one part of sharing where a
/// stranger names positions inside the recipient's own files — so every list
/// and every field here has a stated ceiling, and going past one is refused by
/// name rather than ignored. Each is far above anything an admin produces: the
/// largest real `config/` measured on a developer's machine is 96 files, and a
/// push names the handful of settings that moved.
mod ceiling {
    pub const PATHS: usize = 512;
    pub const OVERRIDES: usize = 1024;
    pub const DEPTH: usize = 8;
    pub const SEGMENT: usize = 128;
    pub const VALUE: usize = 8 * 1024;
    pub const TOTAL: usize = 256 * 1024;
}

impl Settings {
    /// Hold this half to every ceiling and every path rule, naming the first
    /// one it goes past.
    ///
    /// Called on the recipient's side before a single byte of it is acted on,
    /// and on the sender's before a push is written, so a list Geode would
    /// refuse to apply is one it also refuses to make.
    pub fn checked(&self) -> crate::error::Result<()> {
        use crate::error::Error;
        if self.paths.len() > ceiling::PATHS {
            return Err(Error::msg(format!(
                "this pack chooses a behaviour for {} settings paths, and Geode applies at most {}",
                self.paths.len(),
                ceiling::PATHS
            )));
        }
        if self.overrides.len() > ceiling::OVERRIDES {
            return Err(Error::msg(format!(
                "this pack carries {} settings, and Geode applies at most {}",
                self.overrides.len(),
                ceiling::OVERRIDES
            )));
        }
        let mut total = 0usize;
        for named in &self.paths {
            if !named.allowed() {
                return Err(Error::msg(format!(
                    "this pack chooses a behaviour for '{}', which is not a path Geode will \
                     write settings to",
                    named.path
                )));
            }
            total += named.path.len();
        }
        for entry in &self.overrides {
            if !entry.allowed() {
                return Err(Error::msg(format!(
                    "this pack sets a setting inside '{}', which is not a file Geode will write \
                     settings to",
                    entry.path
                )));
            }
            if entry.key.is_empty() || entry.key.len() > ceiling::DEPTH {
                return Err(Error::msg(format!(
                    "a setting in '{}' is named by {} parts, and Geode reads between 1 and {}",
                    entry.path,
                    entry.key.len(),
                    ceiling::DEPTH
                )));
            }
            for segment in &entry.key {
                if segment.len() > ceiling::SEGMENT {
                    return Err(Error::msg(format!(
                        "a setting name in '{}' is {} bytes long, and Geode reads at most {}",
                        entry.path,
                        segment.len(),
                        ceiling::SEGMENT
                    )));
                }
                // A segment is matched literally and written literally, which
                // is what this type's own doc rests on — so a segment holding
                // a line break or a control character is syntax to abuse
                // after all: it would write lines of its own into the
                // recipient's file, settings the push never named. No format
                // Geode reads names a setting with one, and a segment of
                // nothing at all would write a line with no name.
                if segment.trim().is_empty() || segment.chars().any(char::is_control) {
                    return Err(Error::msg(format!(
                        "a setting in '{}' is named by something Geode will not write into a \
                         settings file",
                        entry.path
                    )));
                }
            }
            for text in [entry.value.as_deref(), entry.was.as_deref()] {
                if text.unwrap_or_default().len() > ceiling::VALUE {
                    return Err(Error::msg(format!(
                        "a setting value in '{}' is {} bytes long, and Geode writes at most {}",
                        entry.path,
                        text.unwrap_or_default().len(),
                        ceiling::VALUE
                    )));
                }
            }
            total += entry.path.len()
                + entry.key.iter().map(String::len).sum::<usize>()
                + entry.value.as_deref().unwrap_or_default().len()
                + entry.was.as_deref().unwrap_or_default().len();
        }
        if total > ceiling::TOTAL {
            return Err(Error::msg(format!(
                "this pack's settings come to {total} bytes, and Geode applies at most {}",
                ceiling::TOTAL
            )));
        }
        Ok(())
    }
}

/// Whether `tree` is `rel` itself or a folder above it.
///
/// Compared component by component, never as text: `config` must not cover
/// `configs/x.toml`, and a prefix match on the string would say it does.
///
/// Shared with the walk that decides which of a server's settings files
/// travel, so the admin picking a folder and a behaviour chosen for one are
/// the same kind of claim on the same kind of path — one matcher, for the
/// reason [`Settings::covering`] gives.
pub(crate) fn covers(tree: &std::path::Path, rel: &std::path::Path) -> bool {
    if tree.as_os_str().is_empty() {
        return false;
    }
    let mut under = rel.components();
    tree.components().all(|part| under.next() == Some(part))
}

/// A mod a server does not allow its players to run.
///
/// Named by **identity**, never by file name, and that is the whole of what
/// keeps this safe. A list of file names would let any pack name any jar in a
/// player's folder; a provider and project, or the id a jar declares, names a
/// thing the player can look up — and one they do not have is simply
/// unreachable. Resolution runs against the player's own records and the ids
/// read from their own jars, so an entry holding a path, a `..` or a file name
/// matches nothing at all.
///
/// Either identity will do and both may be given. A player who installed the
/// mod through Geode is recognised by their record's provider and project; one
/// who dropped the jar in by hand has no record, and the id the jar declares is
/// what is left.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Forbidden {
    /// `modrinth` or `curseforge`, going with [`Forbidden::project_id`]. Empty
    /// when the admin named only a mod id.
    #[serde(default)]
    pub provider: String,
    /// The project's id on that provider. Empty with an empty provider.
    #[serde(default)]
    pub project_id: String,
    /// The id a jar declares for itself — `xaerominimap`, not `Xaeros Minimap
    /// 24.2.0.jar`. What recognises a jar nobody has a record of. Empty when
    /// the admin named only a project.
    #[serde(default)]
    pub mod_id: String,
    /// Why, in the admin's own words, shown to the player whose mod is turned
    /// off. Required: a rule with no reason is a bug report waiting to happen.
    pub reason: String,
}

/// The longest [`Forbidden::reason`] stored, in characters. A sentence, since
/// it is read inside a launch notice.
pub const MAX_FORBIDDEN_REASON: usize = 200;

/// The longest id or project id stored, in characters. Modrinth's are eight,
/// CurseForge's are numeric, and a Fabric mod id is a short slug.
const MAX_FORBIDDEN_ID: usize = 64;

impl Forbidden {
    /// This entry, fit to put in a pack — or why it is not.
    ///
    /// One validator for both sides, like [`Listing::checked`]: what the admin
    /// types and what arrives off the network are held to the same rules. An
    /// id is an identifier and nothing else — letters, digits, `-` and `_` —
    /// which is what refuses a path, a `..`, a file name and a wildcard in one
    /// rule, so no entry can even describe a file.
    pub fn checked(&self) -> crate::error::Result<Forbidden> {
        use crate::error::Error;

        let identifier = |what: &str, value: &str| -> crate::error::Result<String> {
            let value = value.trim();
            if value.chars().count() > MAX_FORBIDDEN_ID {
                return Err(Error::msg(format!(
                    "that {what} is longer than {MAX_FORBIDDEN_ID} characters, so it is not one"
                )));
            }
            if !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(Error::msg(format!(
                    "a {what} is letters, digits, '-' and '_' — '{value}' is not one. This list names mods, never files"
                )));
            }
            Ok(value.to_string())
        };

        let provider = self.provider.trim().to_ascii_lowercase();
        let project_id = identifier("project id", &self.project_id)?;
        let mod_id = identifier("mod id", &self.mod_id)?;

        match (provider.as_str(), project_id.is_empty()) {
            ("", false) => {
                return Err(Error::msg(
                    "say which site that project id is from, Modrinth or CurseForge",
                ))
            }
            ("modrinth" | "curseforge", _) | ("", true) => {}
            (other, _) => {
                return Err(Error::msg(format!(
                    "'{other}' is not a site Geode knows — use Modrinth or CurseForge"
                )))
            }
        }
        if !provider.is_empty() && project_id.is_empty() {
            return Err(Error::msg(
                "give the project's id on that site, or name the mod by its mod id instead",
            ));
        }
        if project_id.is_empty() && mod_id.is_empty() {
            return Err(Error::msg(
                "name the mod to turn off: its mod id, or its project id on Modrinth or CurseForge",
            ));
        }

        let reason = self.reason.trim();
        if reason.is_empty() {
            return Err(Error::msg(
                "say why this mod is not allowed — whoever has it is shown your reason",
            ));
        }
        // A newline in a reason reads as two sentences in a launch notice, and
        // a control character garbles it.
        if reason.chars().any(|c| c.is_control()) {
            return Err(Error::msg(
                "keep the reason to one line — a player reads it in a single notice",
            ));
        }
        let length = reason.chars().count();
        if length > MAX_FORBIDDEN_REASON {
            return Err(Error::msg(format!(
                "that reason is {length} characters — keep it to {MAX_FORBIDDEN_REASON} so it fits the notice the player reads"
            )));
        }

        Ok(Forbidden {
            provider,
            project_id,
            mod_id,
            reason: reason.to_string(),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Instance,
    Server,
}

/// How a server should appear in its players' multiplayer list.
///
/// Two things, both the admin's to choose: what players see it called, and the
/// address they connect to *for the game*. That second one is not the address
/// the pack came down — packs come down Geode's own port, the game connects to
/// the server's `server-port`, and the two may differ in host as well as port.
/// Confusing them writes a row that cannot connect, which is why the address
/// here goes through [`crate::share::link::game_address`] and never through
/// the share address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    /// Shown in the player's list. Not held to an address's printable-ASCII
    /// rule: a name legitimately holds emoji, `§` colour codes and non-Latin
    /// scripts, which is why the list writer encodes Java's own UTF-8.
    pub name: String,
    /// `play.example.com`, or `play.example.com:25566`. Minecraft's own port
    /// is left off so the string matches what a player would type by hand.
    pub address: String,
}

/// The longest [`Listing::name`] stored, in characters.
///
/// Ours, not a claim about Minecraft's own Add Server field, whose exact limit
/// is not something this build knows. It is here so a list stays readable, and
/// so a name fits an NBT string's own length whatever script it is written in.
pub const MAX_LISTING_NAME: usize = 64;

impl Listing {
    /// This listing, fit to write into somebody's multiplayer list — or why it
    /// is not.
    ///
    /// One validator for both sides, so the admin's typing and a pack off the
    /// network are held to the same rules. The admin is shown the reason; an
    /// import takes a refusal as "no row", which is the graceful degrade an
    /// older pack already gets.
    pub fn checked(&self) -> crate::error::Result<Listing> {
        use crate::error::Error;

        let name = self.name.trim();
        if name.is_empty() {
            return Err(Error::msg("give the server a name for players' server lists"));
        }
        // Control characters garble a list rather than appearing in it, and
        // a newline in an NBT string is a name that reads as two.
        if name.chars().any(|c| c.is_control()) {
            return Err(Error::msg(
                "that name has control characters in it, which would garble a player's server list",
            ));
        }
        let length = name.chars().count();
        if length > MAX_LISTING_NAME {
            return Err(Error::msg(format!(
                "that name is {length} characters — keep it to {MAX_LISTING_NAME} so it fits a player's server list"
            )));
        }
        Ok(Listing {
            name: name.to_string(),
            address: crate::share::link::game_address(&self.address)?,
        })
    }
}

/// The launcher-level settings a `.mrpack` has no field for.
///
/// All optional: a pack that does not carry a memory size should leave the
/// importer's own default alone rather than overwrite it with a zero.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_args: Option<String>,
    /// Deliberately **not** exported: a path on the author's machine means
    /// nothing on anybody else's, and a wrong one stops the game starting.
    /// Present only so the shape is obvious to a reader of the file.
    #[serde(skip)]
    pub java_path_never_shared: (),
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    /// Server software, for a server pack. Meaningless for an instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_kind: Option<String>,
}

/// Where a file came from, so a recipient can fetch it rather than be sent it.
///
/// This is the whole point of the format. A jar that exists on a provider is
/// carried as a reference and resolved by the recipient through their own
/// credentials — which keeps packs small, keeps mods current, and keeps Geode
/// out of redistributing other people's work.
///
/// Non-Modrinth references live here rather than in the index, and not because
/// the index forbids them: its four-host allowlist belongs to Modrinth's upload
/// validator, and readers do not enforce it. They live here because the index
/// can only hold a URL, and a URL baked into a pack goes on working after an
/// author turns third-party downloads off. An id resolved at import time gets
/// today's answer instead of the one that was true when somebody exported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
}

/// Something worth saying about one file in the index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileNote {
    /// The same path the index uses, so the two halves line up by key.
    pub path: String,
    /// Which side the file belongs on. In an instance's pack, what the jar
    /// declared about itself, carried across so an importer does not have to
    /// re-read every jar to find out. In a server's pack, how the server
    /// classified it — its folder, then the jar, then the listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<crate::instance::jar::Side>,
    /// Where to get it, when it is not something the index can point at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRef>,
    /// The sharer had this turned off.
    ///
    /// A file carried as bytes keeps its `.disabled` name on its own, but a
    /// reference is fetched fresh, so without this a mod somebody switched off
    /// because it crashed would arrive switched on and crash for the recipient
    /// too. Kept apart from `path`, which stays on the enabled name so the
    /// reference and the exporter's skip set go on matching.
    ///
    /// Set for a file the *index* carries as well as one this half sources,
    /// which is what [`format_for`] exists to version: a format-1 reader
    /// honours it only in the second case.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

impl Manifest {
    /// Where the manifest lives inside the zip.
    pub const PATH: &'static str = "geode/manifest.json";

    /// Whether this reader understands the file.
    ///
    /// A newer pack is refused rather than half-read: silently ignoring fields
    /// it does not know would import a pack that looks right and is not.
    pub fn readable(&self) -> bool {
        self.geode_format <= FORMAT
    }

    pub fn note(&self, path: &str) -> Option<&FileNote> {
        self.files.iter().find(|f| f.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn manifest() -> Manifest {
        Manifest {
            geode_format: FORMAT,
            kind: Kind::Instance,
            profile: Profile {
                name: "Test".into(),
                memory_mb: Some(4096),
                ..Default::default()
            },
            icon: None,
            banner: None,
            source: None,
            files: vec![FileNote {
                path: "mods/sodium.jar".into(),
                side: Some(crate::instance::jar::Side::Client),
                source: None,
                disabled: false,
            }],
            revision: None,
            listing: None,
            settings: None,
            forbidden: Vec::new(),
        }
    }

    #[test]
    fn a_manifest_round_trips() {
        let json = serde_json::to_string(&manifest()).unwrap();
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, manifest());
    }

    /// The absent things must stay absent rather than serialising as nulls: a
    /// pack that carries `"group": null` is asking an importer to clear a
    /// group the user set, which is not what "I did not export one" means.
    #[test]
    fn nothing_optional_is_written_when_it_is_absent() {
        let json = serde_json::to_string(&manifest()).unwrap();
        for absent in [
            "group",
            "banner",
            "source",
            "width",
            "serverKind",
            "listing",
            "settings",
            "forbidden",
        ] {
            assert!(!json.contains(absent), "{absent} should not be written: {json}");
        }
        // And a file with nothing to say about it carries no flag either.
        assert!(!json.contains("disabled"));
        assert!(!json.contains("source"));
    }

    /// A file a pack cannot carry travels as a reference instead, and the note
    /// is the whole of what the recipient has to go on: the provider, project
    /// and version it survives the round trip with are what they fetch it by.
    #[test]
    fn a_file_that_cannot_be_redistributed_is_referenced_not_embedded() {
        let note = FileNote {
            path: "mods/private.jar".into(),
            side: None,
            source: Some(SourceRef {
                provider: "curseforge".into(),
                project_id: "12345".into(),
                version_id: "67890".into(),
            }),
            disabled: false,
        };
        let json = serde_json::to_string(&note).unwrap();
        assert!(json.contains("curseforge"));
        let back: FileNote = serde_json::from_str(&json).unwrap();
        assert_eq!(back, note);
    }

    /// A path on the exporter's machine is meaningless on anyone else's, and a
    /// wrong one stops the game starting — so it is never written at all.
    #[test]
    fn a_java_path_is_never_exported() {
        let json = serde_json::to_string(&manifest()).unwrap();
        assert!(!json.to_lowercase().contains("javapath"));
    }

    /// A pack claims the oldest layout that reads it correctly, because the
    /// cost of claiming a newer one is that an older Geode refuses the pack
    /// whole and the recipient gets none of it.
    ///
    /// The one note an older reader gets *wrong* is `disabled` on a file it
    /// fetches from the index: it applies the off state only to files it
    /// resolved by id, so the mod arrives switched on. That pack says 2.
    #[test]
    fn only_a_pack_an_older_reader_would_misread_claims_the_newer_format() {
        let plain = FileNote {
            path: "mods/sodium.jar".into(),
            side: None,
            source: None,
            disabled: false,
        };
        assert_eq!(format_for(std::slice::from_ref(&plain)), 1);

        // Turned off and sourced here: an older reader writes a reference it
        // resolved straight to the off name, so nothing is misread.
        let sourced = FileNote {
            source: Some(SourceRef {
                provider: "curseforge".into(),
                project_id: "1".into(),
                version_id: "2".into(),
            }),
            disabled: true,
            ..plain.clone()
        };
        assert_eq!(format_for(&[sourced]), 1);

        // Turned off and carried by the index: only this layout switches it
        // back off.
        let indexed_off = FileNote {
            disabled: true,
            ..plain
        };
        assert_eq!(format_for(&[indexed_off]), 2);
    }

    /// A listing is an *addition*: an older Geode that does not know the field
    /// ignores it and adds no row, and the instance it builds is correct in
    /// every other respect. Refusing such a pack would cost its recipient
    /// every mod in it to save them a row in a list.
    ///
    /// Make `format_for` return 3 for a listing and this fails — which is the
    /// mistake worth a test, since the field is new and bumping looks tidy.
    #[test]
    fn a_listing_does_not_claim_a_newer_format() {
        let plain = FileNote {
            path: "mods/sodium.jar".into(),
            side: None,
            source: None,
            disabled: false,
        };
        // `format_for` reads the file notes, and a listing is not one of them.
        assert_eq!(format_for(std::slice::from_ref(&plain)), 1);

        let listed = Manifest {
            geode_format: format_for(std::slice::from_ref(&plain)),
            kind: Kind::Server,
            listing: Some(Listing {
                name: "Survival".into(),
                address: "play.example.com".into(),
            }),
            files: vec![plain],
            ..manifest()
        };
        assert_eq!(listed.geode_format, 1, "a listing claimed a newer format");
        assert!(listed.readable());

        // And a reader with no such field reads the rest of the pack, rather
        // than refusing it — which is what "an older reader" actually does,
        // since `Manifest` denies no unknown fields.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct OlderManifest {
            geode_format: u32,
            kind: Kind,
            profile: Profile,
        }
        let json = serde_json::to_string(&listed).unwrap();
        assert!(json.contains("\"listing\""), "{json}");
        let older: OlderManifest = serde_json::from_str(&json).unwrap();
        assert_eq!((older.geode_format, older.kind), (1, Kind::Server));
        assert_eq!(older.profile.name, "Test");
    }

    /// The two things the admin chooses, round-tripped, and the rules that
    /// keep a name out of a player's list when it would garble it.
    #[test]
    fn a_listing_round_trips_and_a_name_a_list_cannot_show_is_refused() {
        let listing = Listing {
            name: "  §aSurvival 🎮  ".into(),
            address: "PLAY.example.com:25565".into(),
        };
        let checked = listing.checked().unwrap();
        // Trimmed, and the address written the way a player would type it.
        assert_eq!(checked.name, "§aSurvival 🎮");
        assert_eq!(checked.address, "play.example.com");
        let back: Listing = serde_json::from_str(&serde_json::to_string(&checked).unwrap()).unwrap();
        assert_eq!(back, checked);

        let bad = |name: &str| Listing {
            name: name.into(),
            address: "play.example.com".into(),
        }
        .checked()
        .expect_err(&format!("{name:?} was accepted"))
        .to_string();

        assert!(bad("").contains("give the server a name"));
        assert!(bad("   ").contains("give the server a name"));
        assert!(bad("Survival\nNot really").contains("control characters"));
        assert!(bad("Survival\u{7f}").contains("control characters"));
        assert!(bad(&"x".repeat(MAX_LISTING_NAME + 1)).contains("characters"));
        // Right up to the cap is fine, counted in characters rather than
        // bytes so a non-Latin name is not refused for being itself.
        assert!(Listing {
            name: "あ".repeat(MAX_LISTING_NAME),
            address: "play.example.com".into()
        }
        .checked()
        .is_ok());

        // And the address goes through the one address validator.
        assert!(Listing {
            name: "Survival".into(),
            address: "https://pack.example.com".into()
        }
        .checked()
        .is_err());
    }

    /// A ban list names mods, never files — that is what keeps a pack from
    /// using it to reach a jar it was never allowed to name. One rule does it:
    /// an id is letters, digits, `-` and `_`, so a path, a `..`, a file name
    /// and a wildcard are all refused where the admin types them.
    #[test]
    fn a_forbidden_entry_names_a_mod_and_can_never_describe_a_file() {
        let banned = |entry: Forbidden| entry.checked();
        let by_mod_id = Forbidden {
            mod_id: "  XaeroMinimap  ".into(),
            reason: "  no x-ray on this server  ".into(),
            ..Default::default()
        };
        let checked = banned(by_mod_id).unwrap();
        assert_eq!(checked.mod_id, "XaeroMinimap");
        assert_eq!(checked.reason, "no x-ray on this server");
        assert!(checked.provider.is_empty() && checked.project_id.is_empty());
        let back: Forbidden =
            serde_json::from_str(&serde_json::to_string(&checked).unwrap()).unwrap();
        assert_eq!(back, checked);

        // By project, with the site written however the admin typed it.
        let by_project = banned(Forbidden {
            provider: "Modrinth".into(),
            project_id: "AANobbMI".into(),
            reason: "not on this server".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(by_project.provider, "modrinth");

        let refused = |entry: Forbidden| banned(entry).expect_err("accepted").to_string();
        let as_id = |value: &str| Forbidden {
            mod_id: value.into(),
            reason: "no".into(),
            ..Default::default()
        };
        for path in [
            "../../instances/x/mods/sodium.jar",
            "sodium.jar",
            "mods/sodium.jar",
            "..",
            "*",
            "sodium jar",
            "/etc/passwd",
            "C:\\mods\\x.jar",
        ] {
            assert!(
                refused(as_id(path)).contains("never files"),
                "{path:?} was read as a mod id"
            );
        }
        // And in the project field too, which is the other half of the pair.
        assert!(refused(Forbidden {
            provider: "modrinth".into(),
            project_id: "../x.jar".into(),
            reason: "no".into(),
            ..Default::default()
        })
        .contains("never files"));

        // Half a pair names nothing anybody can look up.
        assert!(refused(Forbidden {
            project_id: "AANobbMI".into(),
            reason: "no".into(),
            ..Default::default()
        })
        .contains("Modrinth or CurseForge"));
        assert!(refused(Forbidden {
            provider: "modrinth".into(),
            reason: "no".into(),
            ..Default::default()
        })
        .contains("mod id instead"));
        assert!(refused(Forbidden {
            provider: "mymodsite".into(),
            project_id: "1".into(),
            reason: "no".into(),
            ..Default::default()
        })
        .contains("not a site Geode knows"));
        assert!(refused(Forbidden {
            reason: "no".into(),
            ..Default::default()
        })
        .contains("name the mod to turn off"));

        // A rule with no reason is a bug report waiting to happen, and one the
        // notice cannot show is no better.
        assert!(refused(as_id_with_reason("x", "   ")).contains("say why"));
        assert!(refused(as_id_with_reason("x", "no\nx-ray")).contains("one line"));
        assert!(refused(as_id_with_reason(
            "x",
            &"y".repeat(MAX_FORBIDDEN_REASON + 1)
        ))
        .contains("characters"));
        assert!(banned(as_id_with_reason("x", &"y".repeat(MAX_FORBIDDEN_REASON))).is_ok());
    }

    fn as_id_with_reason(mod_id: &str, reason: &str) -> Forbidden {
        Forbidden {
            mod_id: mod_id.into(),
            reason: reason.into(),
            ..Default::default()
        }
    }

    /// The pair a mod id goes with is *no* site, because a mod id is not on
    /// one — and this is the rule the Share tab sends for the route its own
    /// hint recommends, so it is the one combination worth pinning rather than
    /// reaching through `..Default::default()`. The tab's site select always
    /// holds a site; `forbiddenAsTyped` is what drops it when there is no
    /// project id for it to belong to, and what is refused below is what used
    /// to be sent instead.
    #[test]
    fn a_mod_id_is_named_with_no_site_beside_it() {
        let as_the_tab_sends = Forbidden {
            provider: String::new(),
            project_id: String::new(),
            mod_id: "xaerominimap".into(),
            reason: "no x-ray on this server".into(),
        };
        assert_eq!(as_the_tab_sends.checked().unwrap(), as_the_tab_sends);

        let refused = Forbidden {
            provider: "modrinth".into(),
            ..as_the_tab_sends
        }
        .checked()
        .expect_err("a site with no project id beside it was accepted")
        .to_string();
        // The error tells them to name the mod by its mod id — which, with the
        // site still on the rule, is what they had already done.
        assert!(refused.contains("mod id instead"), "{refused}");
    }

    /// A ban list is an *addition*, like a listing: an older Geode ignores the
    /// field, turns nothing off, and builds a correct instance otherwise.
    /// Refusing the pack would cost its recipient every mod in it to enforce a
    /// rule that was never enforceable to begin with.
    #[test]
    fn a_ban_list_does_not_claim_a_newer_format() {
        let plain = FileNote {
            path: "mods/sodium.jar".into(),
            side: None,
            source: None,
            disabled: false,
        };
        let banned = Manifest {
            geode_format: format_for(std::slice::from_ref(&plain)),
            kind: Kind::Server,
            forbidden: vec![Forbidden {
                mod_id: "xaerominimap".into(),
                reason: "no x-ray".into(),
                ..Default::default()
            }],
            files: vec![plain],
            ..manifest()
        };
        assert_eq!(banned.geode_format, 1, "a ban list claimed a newer format");
        assert!(banned.readable());

        let json = serde_json::to_string(&banned).unwrap();
        assert!(json.contains("\"forbidden\""), "{json}");
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.forbidden, banned.forbidden);
    }

    #[test]
    fn a_pack_from_the_future_is_refused_rather_than_half_read() {
        let mut m = manifest();
        m.geode_format = FORMAT + 1;
        assert!(!m.readable());
        m.geode_format = FORMAT;
        assert!(m.readable());
    }

    /// The wire is where this spelling has to hold: a server's revision names
    /// a behaviour with the same string, a player's Geode reads it back by it,
    /// and `src/lib/types.ts` mirrors it by hand with no codegen to catch a
    /// rename. A pack from before any of this has no settings half at all and
    /// still reads, which is the whole reason the format is not bumped.
    #[test]
    fn the_behaviours_are_named_one_way_on_the_wire_and_an_older_pack_still_reads() {
        let settings = Settings {
            paths: vec![
                chosen("config", Policy::Merge, 4),
                chosen("config/keys", Policy::ShipOnce, 2),
                chosen("config/locked.toml", Policy::Enforce, 1),
            ],
            overrides: Vec::new(),
            at: 4,
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            json,
            r#"{"paths":[{"path":"config","policy":"merge","from":4},{"path":"config/keys","policy":"ship-once","from":2},{"path":"config/locked.toml","policy":"enforce","from":1}],"at":4}"#
        );
        assert_eq!(
            serde_json::from_str::<Settings>(&json).unwrap(),
            settings,
            "the wire form did not read back"
        );
        for named in &settings.paths {
            assert!(
                json.contains(&format!(r#""policy":"{}""#, named.policy.as_str())),
                "the revision and the wire name {:?} differently: {json}",
                named.policy
            );
        }

        let older: Manifest =
            serde_json::from_str(&serde_json::to_string(&manifest()).unwrap()).unwrap();
        assert_eq!(older.settings, None);
    }

    fn chosen(path: &str, policy: Policy, from: u32) -> SyncedPath {
        SyncedPath {
            path: path.into(),
            policy,
            from,
        }
    }

    /// The migration rule, and the one that keeps every share made before any
    /// of this working exactly as it did: a share that has chosen nothing
    /// replaces every file it sends, every time, which is what sending a
    /// config folder has always done.
    #[test]
    fn a_file_nobody_chose_a_behaviour_for_is_replaced_every_time() {
        let nothing = Settings::default();
        assert_eq!(nothing.covering(Path::new("config/sodium.json")), None);
        for file in ["config/sodium.json", "config", "options.txt", "mods/a.jar"] {
            assert_eq!(
                nothing.policy_for(Path::new(file)),
                Policy::Enforce,
                "{file}"
            );
        }
    }

    /// A folder covers what is under it, the longest choice wins, and `config`
    /// is not `configs` — a prefix match on the text would say it was, and a
    /// file in a folder nobody chose a behaviour for would quietly stop being
    /// replaced.
    #[test]
    fn the_longest_folder_above_a_file_is_what_decides_it() {
        let settings = Settings {
            paths: vec![
                chosen("config", Policy::Merge, 1),
                chosen("config/locked.toml", Policy::Enforce, 2),
                chosen("config/keys", Policy::ShipOnce, 3),
            ],
            overrides: Vec::new(),
            at: 3,
        };

        let policy = |file: &str| settings.policy_for(Path::new(file));
        assert_eq!(policy("config/sodium.json"), Policy::Merge);
        assert_eq!(policy("config/deep/down/here.toml"), Policy::Merge);
        assert_eq!(policy("config/locked.toml"), Policy::Enforce);
        assert_eq!(policy("config/keys/binds.txt"), Policy::ShipOnce);
        // Not under any of them, so nothing was chosen for it.
        assert_eq!(policy("configs/sodium.json"), Policy::Enforce);
        assert_eq!(policy("options.txt"), Policy::Enforce);
        assert_eq!(settings.covering(Path::new("options.txt")), None);
        // And the `from` travels with the choice, since that is what decides
        // whether a player is still behind it.
        assert_eq!(
            settings.covering(Path::new("config/keys/binds.txt")).map(|p| p.from),
            Some(3)
        );
    }

    /// What a stranger's pack may name. An entry decides whether the pack's own
    /// copy of a file is written and whether failing to write it stops the
    /// import, and neither is a decision anybody else's pack gets to make
    /// about a jar.
    #[test]
    fn a_pack_cannot_choose_a_behaviour_for_a_jar_or_for_anything_outside_the_instance() {
        for path in [
            "mods",
            "mods/evil.jar",
            "client-mods/evil.jar",
            "../elsewhere",
            "config/../../elsewhere",
            "/etc",
            "",
            "   ",
        ] {
            let settings = Settings {
                paths: vec![chosen(path, Policy::ShipOnce, 1)],
                overrides: Vec::new(),
                at: 1,
            };
            assert_eq!(
                settings.covering(Path::new("mods/evil.jar")),
                None,
                "{path} decided something"
            );
            assert_eq!(
                settings.policy_for(Path::new(path)),
                Policy::Enforce,
                "{path} decided something about itself"
            );
        }
    }

    /// Every ceiling a settings half is held to, and that going past one is
    /// refused by name rather than ignored. This is the one part of sharing
    /// where a stranger's pack names positions inside the recipient's own
    /// files, so a list Geode cannot vouch for is one it does not apply at
    /// all — and the reason says which rule it hit, because that line is what
    /// the recipient reads.
    #[test]
    fn a_settings_half_is_held_to_every_ceiling_and_says_which_one_it_hit() {
        let one = |path: &str, key: Vec<String>, value: Option<String>| Settings {
            paths: vec![chosen("config", Policy::Merge, 1)],
            overrides: vec![Override {
                path: path.to_string(),
                key,
                value,
                was: None,
                at: 1,
            }],
            at: 1,
        };
        let refusal = |settings: Settings| settings.checked().unwrap_err().to_string();

        // What a real push looks like: allowed.
        one("config/a.properties", vec!["fov".into()], Some("90".into()))
            .checked()
            .unwrap();

        // A setting inside a jar, or outside the instance altogether.
        for path in ["mods/evil.jar", "client-mods/evil.jar", "../evil", "/etc/passwd"] {
            let why = refusal(one(path, vec!["a".into()], Some("x".into())));
            assert!(why.contains(path), "{why}");
        }

        // A key that names nothing, and one nested past anything real.
        for key in [vec![], (0..9).map(|n| n.to_string()).collect::<Vec<_>>()] {
            let why = refusal(one("config/a.toml", key, Some("x".into())));
            assert!(why.contains("parts"), "{why}");
        }

        // A key segment and a value past their own ceilings.
        let why = refusal(one(
            "config/a.toml",
            vec!["k".repeat(129)],
            Some("x".into()),
        ));
        assert!(why.contains("129 bytes long"), "{why}");

        // A segment that is lines of a file rather than a name. A splice
        // writes a segment literally, so this is the one place the promise
        // that a segment has no syntax to abuse is kept: `evil\nhack=1` would
        // append two lines to a `.properties` file and leave a setting called
        // `hack` in it that no push ever named.
        for segment in ["evil\nhack=1", "a\rb", "a\tb", ""] {
            let why = refusal(one(
                "config/a.properties",
                vec![segment.into()],
                Some("x".into()),
            ));
            assert!(why.contains("will not write"), "{segment:?}: {why}");
        }
        let why = refusal(one(
            "config/a.toml",
            vec!["k".into()],
            Some("v".repeat(8 * 1024 + 1)),
        ));
        assert!(why.contains("8192"), "{why}");

        // Too many paths, too many settings, and too many bytes in total.
        let many = |n: usize| Settings {
            paths: vec![chosen("config", Policy::Merge, 1)],
            overrides: (0..n)
                .map(|at| Override {
                    path: "config/a.properties".into(),
                    key: vec![format!("k{at}")],
                    value: Some("v".repeat(256)),
                    was: None,
                    at: 1,
                })
                .collect(),
            at: 1,
        };
        let why = refusal(Settings {
            paths: (0..513).map(|n| chosen(&format!("config/{n}"), Policy::Merge, 1)).collect(),
            ..Settings::default()
        });
        assert!(why.contains("513 settings paths"), "{why}");
        let why = refusal(many(1025));
        assert!(why.contains("1025 settings"), "{why}");
        // Each of these is inside every other ceiling; together they are not.
        let why = refusal(many(1024));
        assert!(why.contains("bytes, and Geode applies at most"), "{why}");
    }

    /// A choice is dated so a player who has not caught up with it gets the
    /// file whole once and keeps their own changes from then on. Choosing what
    /// is already in force writes nothing, so turning a folder off and on
    /// again costs no player a download.
    #[test]
    fn choosing_a_behaviour_dates_it_and_choosing_it_again_changes_nothing() {
        let mut settings = Settings::default();

        // Already in force, because it is what nobody choosing anything means.
        settings.choose("config", Policy::Enforce);
        assert_eq!(settings, Settings::default());

        settings.choose("config", Policy::Merge);
        assert_eq!(settings.at, 1);
        assert_eq!(settings.paths, vec![chosen("config", Policy::Merge, 1)]);

        settings.choose("config", Policy::Merge);
        assert_eq!(settings.at, 1, "choosing the same thing twice dated it again");
        assert_eq!(settings.paths.len(), 1, "{:?}", settings.paths);

        // A switch is one entry with a newer date, never a second entry.
        settings.choose("config", Policy::ShipOnce);
        assert_eq!(settings.at, 2);
        assert_eq!(settings.paths, vec![chosen("config", Policy::ShipOnce, 2)]);

        // And back to what a share that had chosen nothing does, which is a
        // choice like any other rather than a return to silence: a player
        // below it has to be sent the file again.
        settings.choose("config", Policy::Enforce);
        assert_eq!(settings.at, 3);
        assert_eq!(settings.paths, vec![chosen("config", Policy::Enforce, 3)]);
    }
}

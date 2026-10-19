//! What local bytes are, on the platforms a pack can name.
//!
//! One catalogue, built by hashing each file once and asking each platform
//! what that file is, and every writer and reader of every pack format draws
//! on it. Nothing else in the codebase turns local bytes into a platform
//! reference — the alternative is a matcher inside each format, which is how
//! the hashing and the "which file matched" rule end up implemented three
//! different ways and disagreeing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::instance::content::{ContentKind, ContentRecord};
use crate::instance::jar::Side;
use crate::net::Progress;
use crate::paths::Paths;

/// The folders of an instance whose files a pack can name.
///
/// The same four the "Identify files" pass reads. `client-mods/` is a server's
/// folder, and `plugins/` belongs to no instance.
pub(crate) const INSTANCE_KINDS: [ContentKind; 4] = [
    ContentKind::Mod,
    ContentKind::ResourcePack,
    ContentKind::ShaderPack,
    ContentKind::DataPack,
];

/// One file a pack could carry, as the caller's own walk found it.
///
/// Callers build these from the walk they are already doing, so the matcher
/// never decides what is in scope: an export's `Walk` and an import's
/// extracted tree do.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub kind: ContentKind,
    /// Where the file is, from the root the walk started at, `.disabled` and
    /// all.
    pub disk_rel: PathBuf,
    /// Its enabled name, which is how a content record is keyed.
    pub file_name: String,
    pub disabled: bool,
}

/// Where a platform says one file's bytes came from.
#[derive(Debug, Clone)]
pub struct PlatformRef {
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
    /// The release this file belongs to, as people read it — `1.2.3`, not the
    /// id beside it.
    ///
    /// Nothing a pack writes needs it: a pack names a version by its id. It is
    /// here for the import that writes a content record, which shows a version
    /// in the mods list and has no other way to learn one — the id in a pack's
    /// own download URL is not a number anybody can read.
    pub version_number: String,
    /// The matched file's own download URL, when a lookup gave one.
    ///
    /// `None` for a reference a record claimed rather than a lookup proved,
    /// and for a platform whose packs name ids and resolve the URL at install
    /// time.
    pub url: Option<String>,
    /// Whether the author lets anyone but the platform's own app fetch this
    /// file. False only where a platform said so: CurseForge lets an author
    /// turn third-party downloads off, and Modrinth has no such switch.
    ///
    /// A reference nobody can fetch is worse than the bytes, so [`travel`] is
    /// where this is enforced and nothing else needs to remember it.
    pub distributable: bool,
}

/// One file's bytes, and everything that can name them.
#[derive(Debug, Clone)]
pub struct Identity {
    pub kind: ContentKind,
    /// The enabled name, whatever the file is called on disk.
    pub file_name: String,
    pub disk_rel: PathBuf,
    pub disabled: bool,
    pub size: u64,
    pub sha1: String,
    pub sha512: String,
    /// The only name CurseForge knows a file by. `None` whenever no CurseForge
    /// lookup was going to be made, because taking it costs a second read of
    /// the file.
    pub murmur2: Option<u32>,
    /// Which side someone other than the jar said this belongs on — the
    /// record's answer, carried through so a pack's `env` pair does not move
    /// the day a hash lookup starts answering as well.
    pub listed_side: Option<Side>,
    pub refs: Vec<PlatformRef>,
}

impl Identity {
    /// What one platform says about this file.
    pub fn reference(&self, provider: &str) -> Option<&PlatformRef> {
        self.refs.iter().find(|r| r.provider == provider)
    }
}

/// Which platforms to ask about a set of files.
pub enum Ask<'a> {
    /// Nobody. The records are the only answer, so there is no client and no
    /// network — which is what a pack that must come out the same every time
    /// needs, and why this is a shape rather than an empty list beside a
    /// client somebody could still use.
    Nobody,
    /// These platforms, through this client. A name nothing here knows is
    /// ignored rather than refused: it means a stage that has not landed.
    Platforms {
        client: &'a reqwest::Client,
        want: &'a [&'a str],
        /// Passed straight through to `providers::curseforge`, exactly as
        /// every other call site passes it. Nothing here inspects, keeps or
        /// logs it, and it never appears in a report. `None` means the
        /// CurseForge arm is skipped and the user is told so, rather than
        /// guessed around.
        ///
        /// It sits on this variant rather than beside it so there is no key to
        /// hand over when there is nobody to ask — the same reason
        /// [`Ask::Nobody`] is a shape and not an empty list.
        curseforge_key: Option<&'a str>,
        /// Whether a pack written from this catalogue will have an index
        /// ([`Carry::index`]), which is the whole of what a missing CurseForge
        /// key costs. With an index, a file Modrinth names travels as a link
        /// whatever CurseForge would have said, so the key is only missed for
        /// the files nothing else names. Without one — a CurseForge pack has
        /// no index — every file CurseForge might have named travels as bytes
        /// instead, so the key is missed for all of them.
        ///
        /// Read nowhere but that message: it decides which of the two the
        /// sharer is told, and telling them the wrong one points at the format
        /// when the thing they can act on is the key.
        index: bool,
        /// Whether a reference spares that pack the bytes
        /// ([`Carry::spares_bytes`]), which is what the same message promises
        /// a key would buy. False for a Prism instance zip, where it would be
        /// a promise the format cannot keep.
        spares_bytes: bool,
    },
}

impl<'a> Ask<'a> {
    /// What an export asks: Modrinth always, and CurseForge only where the
    /// pack being written has a channel that can carry a reference to it.
    ///
    /// Modrinth always, because its answer is what names a file in a pack
    /// index — and, for a format with no index, which of the two reasons the
    /// sharer is given for a jar that travelled as bytes. CurseForge only
    /// where the pack can name a file by its ids, because a CurseForge answer
    /// is the one thing here that costs a second read of every file in the
    /// instance ([`crate::net::digests`]), and for a plain `.mrpack` it cannot
    /// change what the pack does with a single file. The `carry` is the one
    /// the writer will use, which is what makes asking worth the reading.
    ///
    /// What a plain `.mrpack` gives up by not asking: a jar Geode has no
    /// record of and CurseForge happens to hold is reported as "nothing could
    /// name it" rather than "a format with a Geode half would link it". The
    /// pack is the same pack either way, and a jar Geode *did* install from
    /// CurseForge still reports the sharper reason, because its record says so
    /// without anybody being asked.
    pub fn exporting(
        client: &'a reqwest::Client,
        curseforge_key: Option<&'a str>,
        carry: Carry,
    ) -> Self {
        Self::Platforms {
            client,
            want: if carry.by_id == ById::Nothing {
                &["modrinth"]
            } else {
                &["modrinth", "curseforge"]
            },
            curseforge_key,
            index: carry.index,
            spares_bytes: carry.spares_bytes,
        }
    }

    /// What an import asks, to give the files that just landed a name.
    ///
    /// Modrinth only, and that is not a stage waiting to be finished. Every
    /// format says where a file came from in a way that needs no hash at all —
    /// a CurseForge manifest names ids, a packwiz index names a project — and
    /// an import that read one has already written those records down. A hash
    /// lookup is for the files whose own format said nothing about them: a jar
    /// out of an `overrides/` tree, an index entry served from somewhere Geode
    /// cannot read ids out of, a mod that arrived in a server's pack as bytes.
    /// Modrinth answers that question without anybody's credentials, which is
    /// what makes it worth asking on every import rather than only the ones
    /// where a key happens to be set.
    ///
    /// Asking CurseForge too would cost a key the importer may not have and a
    /// second read of every file that just landed ([`crate::net::digests`]),
    /// to fill in a name on the mods list. That is a trade for whoever wants
    /// it, not one to make on everybody's behalf mid-import.
    pub fn naming(client: &'a reqwest::Client) -> Self {
        Self::Platforms {
            client,
            want: &["modrinth"],
            curseforge_key: None,
            // Read only by the message about a missing CurseForge key, which
            // nothing here asks for.
            index: true,
            spares_bytes: true,
        }
    }
}

/// Everything known about the files a pack could carry.
#[derive(Debug, Default)]
pub struct Catalogue {
    files: HashMap<(ContentKind, String), Identity>,
    /// What could not be asked, in words the user can read. An export reports
    /// these rather than quietly shipping jars for a platform nobody reached.
    pub trouble: Vec<String>,
}

impl Catalogue {
    pub fn get(&self, kind: ContentKind, file_name: &str) -> Option<&Identity> {
        self.files.get(&(kind, file_name.to_string()))
    }

    pub fn identities(&self) -> impl Iterator<Item = &Identity> {
        self.files.values()
    }

    /// What a pack would do with one file, when it holds one at all.
    pub fn travel(&self, kind: ContentKind, file_name: &str, carry: Carry) -> Option<Travel<'_>> {
        self.get(kind, file_name).map(|i| travel(i, carry))
    }

    /// Pretend Modrinth was asked and said it holds these files.
    ///
    /// For the tests of everything downstream, which must not reach the
    /// network. Only the request is left out: the answer goes in through the
    /// same [`apply_modrinth`] a real one does, so a file left off this list
    /// gets the same treatment a real miss would — including having a record's
    /// claim about it proven stale.
    #[cfg(test)]
    pub fn pretend_modrinth_holds(&mut self, file_names: &[&str]) {
        let answer: HashMap<String, PlatformRef> = self
            .identities()
            .filter(|i| file_names.contains(&i.file_name.as_str()))
            .map(|i| {
                (
                    i.sha512.clone(),
                    PlatformRef {
                        provider: "modrinth".into(),
                        project_id: "mr-project".into(),
                        version_id: "mr-version".into(),
                        version_number: "mr-1.0".into(),
                        // Deliberately not the name on disk: that is the whole
                        // case for taking the matched file's own URL, since
                        // `link_untracked_in` records the on-disk base name
                        // and a renamed jar's constructed URL 404s.
                        url: Some(format!(
                            "https://cdn.modrinth.com/data/mr-project/versions/mr-version/uploaded-{}",
                            urlencoding::encode(&i.file_name)
                        )),
                        distributable: true,
                    },
                )
            })
            .collect();
        apply_modrinth(self, &answer, &[]);
    }

    /// Pretend CurseForge was asked and said it holds these files, each by its
    /// project id, its file id, and whether its author lets anything but
    /// CurseForge's own app fetch the bytes.
    ///
    /// The same shape as [`Self::pretend_modrinth_holds`], and for the same
    /// reason: only the request is left out, so the answer goes in through the
    /// [`apply_curseforge`] a real one does and a file left off this list has a
    /// stored CurseForge claim about it proven stale exactly as a real miss
    /// would. The fingerprints are made up — a catalogue built with
    /// [`Ask::Nobody`] never took one — and all they have to do is tell the
    /// files asked about apart.
    #[cfg(test)]
    pub fn pretend_curseforge_holds(&mut self, files: &[(&str, u32, u32, bool)]) {
        let asking: Vec<(ContentKind, String, u32)> = self
            .identities()
            .enumerate()
            .map(|(nth, i)| (i.kind, i.file_name.clone(), nth as u32))
            .collect();
        let answer: HashMap<u32, PlatformRef> = asking
            .iter()
            .filter_map(|(_, file_name, fingerprint)| {
                let held = files.iter().find(|f| f.0 == file_name.as_str())?;
                Some((
                    *fingerprint,
                    PlatformRef {
                        provider: "curseforge".into(),
                        project_id: held.1.to_string(),
                        version_id: held.2.to_string(),
                        version_number: format!("cf-{}", held.2),
                        // Never a URL, for the reason `curseforge_ref` gives.
                        url: None,
                        distributable: held.3,
                    },
                ))
            })
            .collect();
        apply_curseforge(self, &asking, &answer);
    }
}

/// Which files a pack's by-id channel can name.
///
/// The channel that carries ids rather than a URL, so the recipient resolves
/// the file with their own credentials and an author's wishes are read today
/// rather than on the day somebody exported. A `.geodepack`'s file notes are
/// one; a CurseForge manifest's `files[]` is the other, and they differ in what
/// they are able to say about a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ById {
    /// There is no such channel.
    Nothing,
    /// Any file, wherever it sits: a `.geodepack`'s note carries the path
    /// alongside the ids.
    AnyFile,
    /// Mods only. A CurseForge manifest names ids and nothing else — no path,
    /// no name, no hash — so the recipient picks a folder by sniffing the bytes
    /// and leaves what it cannot identify in `mods/`. A mod is the one kind
    /// that lands right either way; anything whose folder matters travels as
    /// bytes instead, where the pack says where it goes.
    ModsOnly,
}

/// What the pack being written is able to say, which is what decides whether a
/// file can travel as a reference instead of as bytes.
#[derive(Debug, Clone, Copy)]
pub struct Carry {
    /// The pack has a Modrinth index, which carries a file as a URL and a pair
    /// of hashes that need nobody's credentials at either end.
    pub index: bool,
    /// What the pack can name by id.
    pub by_id: ById,
    /// Whether a reference spares the pack the bytes.
    ///
    /// True of every format but a Prism instance zip, where the recipient's
    /// launcher fetches nothing and every file travels either way — so what a
    /// reference buys there is a name and an update path, not a smaller zip.
    /// Read where the sharer is told what a missing CurseForge key costs them,
    /// which is the one place the difference has to be said out loud.
    pub spares_bytes: bool,
    /// The pack can say a file is turned off, so a turned-off mod can be
    /// carried under its enabled name instead of travelling as bytes under its
    /// `.disabled` one.
    ///
    /// [`travel`] reads this on the index branch alone, because that is the
    /// only branch with a choice to make: an index entry is a URL the
    /// recipient fetches, and what arrives is switched on. A by-id channel
    /// carries the off state beside the id it names — a `.geodepack`'s note
    /// says so outright, a CurseForge manifest says it with `required: false`
    /// — so for a file named that way there is nothing to decide here, and
    /// writing the state is the writer's own job.
    pub off_state: bool,
}

impl Carry {
    /// A plain `.mrpack`: an index and an overrides tree, and nowhere to say
    /// anything else. A mod it cannot index travels as bytes, and so does a
    /// turned-off one, because the format has no way to say "off" and arriving
    /// switched on is worse than arriving as bytes.
    pub const MRPACK: Self = Self {
        index: true,
        spares_bytes: true,
        by_id: ById::Nothing,
        off_state: false,
    };
    /// A `.geodepack`: file notes carry both a source and the off state.
    pub const GEODEPACK: Self = Self {
        index: true,
        spares_bytes: true,
        by_id: ById::AnyFile,
        off_state: true,
    };
    /// A server's pack for its players. Its notes name sources, but a player's
    /// import reads a note's off state only for a file that note also sources
    /// (`arrivals`), so a mod the index carried would arrive switched on. A
    /// turned-off mod therefore still travels as bytes, which arrives off;
    /// changing that means changing what a player's import reads.
    pub const SERVER_PACK: Self = Self {
        index: true,
        spares_bytes: true,
        by_id: ById::AnyFile,
        off_state: false,
    };
    /// A Prism instance zip, whose packwiz index carries a Modrinth file as a
    /// URL and a CurseForge one as the ids Prism resolves at use time.
    ///
    /// The one `Carry` where a reference is not an alternative to the bytes:
    /// Prism's own importer takes the extracted tree as it finds it and
    /// downloads nothing, so every file travels either way and the index is
    /// provenance beside it. What this says is therefore which files the
    /// recipient's launcher will know the name and the update path of — and
    /// `off_state` is true because a turned-off jar travels under its
    /// `.disabled` name while its entry names the enabled one, which is how
    /// Prism matches a resource to its metadata.
    pub const PRISM: Self = Self {
        index: true,
        // The one format where a reference is not an alternative to the bytes.
        spares_bytes: false,
        by_id: ById::AnyFile,
        off_state: true,
    };
    /// A CurseForge pack: a manifest of `{projectID, fileID, required}` and an
    /// overrides tree, and no index at all — so a mod Modrinth alone can name
    /// travels as bytes however well it is known. `required: false` is the
    /// nearest the format comes to saying a mod is turned off, and is what the
    /// readers turn back into a `.disabled` file — which the manifest writer
    /// writes from the file's own off state, since with no index [`travel`]
    /// has no choice to make here.
    pub const CFPACK: Self = Self {
        index: false,
        spares_bytes: true,
        by_id: ById::ModsOnly,
        off_state: true,
    };
}

impl Default for Carry {
    /// The least a pack can say, because a writer that was told nothing has no
    /// business assuming it may leave bytes out.
    fn default() -> Self {
        Self::MRPACK
    }
}

/// How one file travels.
#[derive(Debug)]
pub enum Travel<'a> {
    /// An index entry: a URL and a pair of hashes the recipient fetches with
    /// nobody's credentials.
    Linked(&'a PlatformRef),
    /// Named by id in the pack's own by-id channel and fetched from there, so
    /// the recipient's own answer about an author's wishes is the one that
    /// counts.
    Named(&'a PlatformRef),
    /// Bytes under `overrides/`, for this reason.
    Copied(Copied),
}

/// Why a file's bytes had to travel.
///
/// Four different things to tell the sharer, and only one of them is "nobody
/// has this mod". Two are limits of the format they picked, which is something
/// they can act on by picking another, and the last is somebody else's
/// decision that no format gets around.
#[derive(Debug, PartialEq, Eq)]
pub enum Copied {
    /// Nothing could name these bytes.
    Unknown,
    /// A platform does name them, and this pack has no channel that can carry
    /// a reference to it — none that reaches that platform, or none that can
    /// say where this kind of file belongs.
    Elsewhere,
    /// Named, with a channel that could carry it, but this pack has nowhere to
    /// say the file is turned off.
    TurnedOff,
    /// Named, but by a platform whose copy the author has made undownloadable
    /// to anything but its own app.
    Blocked,
}

/// What a pack will do with one file, and why.
///
/// The one answer both a dry run and the writer take. A preview that worked
/// this out separately would eventually promise something the export then did
/// differently, and the only reason to show it is that it is true.
pub fn travel(ident: &Identity, carry: Carry) -> Travel<'_> {
    // The index first, wherever the pack has one: it names the bytes it points
    // at, and needs no credentials at either end.
    if carry.index {
        if let Some(reference) = ident.reference("modrinth") {
            // A turned-off file needs the pack to be able to say so, because a
            // reference arrives fresh from the platform and switched on. There
            // is nothing to fall through to: a content record names one
            // platform, so a file Modrinth knows has no second reference
            // waiting behind it.
            if ident.disabled && !carry.off_state {
                return Travel::Copied(Copied::TurnedOff);
            }
            return Travel::Linked(reference);
        }
    }
    // Then the by-id channel: ids rather than a URL, so the recipient's own
    // answer about an author's wishes is the one that counts. It carries the
    // off state alongside the source it names — a `.geodepack`'s note says so
    // outright and a CurseForge manifest says it with `required: false` — so
    // there is nothing more to ask.
    //
    // Modrinth is never named here. Where the pack has an index that is where
    // a Modrinth file belongs, and the format's rule is that the same fact
    // never lives in both halves; where it has none, neither channel can
    // resolve one anyway — a `.geodepack`'s import reports any provider but
    // CurseForge as unfetchable, and a CurseForge manifest has no slot for a
    // Modrinth file at all.
    let named = ident.refs.iter().find(|r| r.provider != "modrinth");
    match (carry.by_id, named) {
        // The one place the distribution rule is enforced, so no writer can
        // get it wrong. An author who turned third-party downloads off has
        // left the recipient a reference nobody can resolve, and the bytes are
        // better than that. This arm comes first so that a blocked reference
        // is never handed to a writer, whatever else is true of the file.
        //
        // Which of the two reasons the sharer hears depends on whether
        // anything else names the same bytes, because that is what decides
        // whether they can do something about it. Nothing else names them: the
        // author's decision is the whole story, and no other format would have
        // linked the file either. Modrinth names them: the author published
        // those bytes themselves, which a pack with an index has already
        // linked above — so what reaches here is an index-less format, whose
        // own limit is the honest reason and the one the sharer can act on by
        // exporting a format that can carry a Modrinth link.
        (_, Some(reference)) if !reference.distributable => {
            Travel::Copied(if ident.reference("modrinth").is_some() {
                Copied::Elsewhere
            } else {
                Copied::Blocked
            })
        }
        (ById::AnyFile, Some(reference)) => Travel::Named(reference),
        // A CurseForge manifest names ids and no path, so only a mod can be
        // trusted to land where it belongs. A resource pack, shader or data
        // pack named that way arrives wherever the recipient's own sniffing
        // puts it, which for a shader pack is `mods/` — so its bytes travel
        // instead, under a path the pack does state.
        //
        // And only CurseForge's own ids: this channel is a pair of integers
        // with no room to say which platform they belong to, so the
        // recipient's key resolves whatever is there as a CurseForge project
        // and file. Another platform's numeric id would parse and download an
        // unrelated mod, and the format carries no hash to catch it with.
        (ById::ModsOnly, Some(reference))
            if ident.kind == ContentKind::Mod && reference.provider == "curseforge" =>
        {
            Travel::Named(reference)
        }
        // A platform holds this mod and the pack has nowhere to say so. Worth
        // telling apart from a jar nothing knows: the remedy is a format with
        // a channel that fits, not hunting for where the mod came from.
        _ if named.is_some() || ident.reference("modrinth").is_some() => {
            Travel::Copied(Copied::Elsewhere)
        }
        _ => Travel::Copied(Copied::Unknown),
    }
}

/// Everything an instance's pack could name.
pub async fn for_instance(
    ask: Ask<'_>,
    paths: &Paths,
    instance: &crate::instance::Instance,
    progress: &Progress,
) -> Result<Catalogue> {
    let game_dir = paths.game_dir(&instance.id);
    let candidates = candidates_in(&game_dir, &INSTANCE_KINDS).await?;
    let records: Vec<&ContentRecord> = instance.content.iter().collect();
    identify(&game_dir, candidates, &records, ask, progress).await
}

/// The content files directly inside the given folders.
///
/// Links are followed, because an instance's own pack walk does: a mod the
/// player linked in is still theirs to share. A caller whose walk refuses
/// links — a server's — builds its candidates from that walk instead of
/// calling this.
pub(crate) async fn candidates_in(root: &Path, kinds: &[ContentKind]) -> Result<Vec<Candidate>> {
    use crate::instance::content::{is_content_file, strip_disabled};

    let mut out = Vec::new();
    for kind in kinds {
        let dir = root.join(kind.folder());
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            // A name that is not UTF-8 can be neither an index path nor a
            // record, so it is nothing a platform could be asked about. The
            // writer is the one place that reports it, once.
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !is_content_file(&name, *kind) {
                continue;
            }
            if !tokio::fs::metadata(entry.path())
                .await
                .is_ok_and(|m| m.is_file())
            {
                continue;
            }
            let (base, enabled) = strip_disabled(&name);
            out.push(Candidate {
                kind: *kind,
                disk_rel: PathBuf::from(kind.folder()).join(&name),
                file_name: base.to_string(),
                disabled: !enabled,
            });
        }
    }
    Ok(out)
}

/// Hash each file once and ask each platform what it is.
///
/// A record is a hint, a lookup is proof, and a miss is only a miss when a
/// lookup ran:
///
/// - A lookup that answered wins over the record, including where it disagrees
///   about which project or version the bytes are.
/// - A lookup that answered *nothing* about a file the record named proves the
///   record stale — the platform holds no file with those bytes — so the
///   reference goes, and the file travels as bytes. The index's hashes come
///   from the local file while its URL would come from the record, and every
///   reader would fail its checksum.
/// - No lookup at all leaves the record's reference standing, because the
///   alternative silently fattens an offline export. So does a lookup that was
///   made and refused: a chunk the platform turned away is not an answer.
pub async fn identify(
    root: &Path,
    candidates: Vec<Candidate>,
    records: &[&ContentRecord],
    ask: Ask<'_>,
    progress: &Progress,
) -> Result<Catalogue> {
    let mut catalogue = Catalogue::default();
    if candidates.is_empty() {
        return Ok(catalogue);
    }

    // Whether to take each file's murmur2, decided once for the whole run.
    //
    // It is all-or-nothing because the second read is the algorithm's and not
    // ours: CurseForge seeds its state with the number of bytes it is about to
    // hash, so that count has to be known before any mixing starts. Taking it
    // only for the files Modrinth turned out not to have would need a
    // murmur2-only pass after the Modrinth answer — cheaper in bytes read, at
    // the price of a second entry point into the only place a file's bytes are
    // read and a second hashing stage in a progress bar that has already
    // finished one. The trade bought here is one code path and one stage.
    //
    // What stops that costing anything for nothing is the caller: asking about
    // CurseForge at all is a choice about what the pack can say, which is what
    // [`Ask::exporting`] decides and what keeps a plain `.mrpack` reading
    // every file once.
    let want_murmur2 = matches!(
        ask,
        Ask::Platforms {
            want,
            curseforge_key: Some(_),
            ..
        } if want.contains(&"curseforge")
    );

    progress
        .stage("Identifying files", candidates.len() as u64)
        .await;
    for candidate in candidates {
        progress.check_cancelled()?;
        progress.advance(1, &candidate.file_name).await;

        let key = (candidate.kind, candidate.file_name.clone());
        // A mod on disk both on and off is one mod, and it is the copy that is
        // on — the same rule a server's pack states for its notes.
        if catalogue
            .files
            .get(&key)
            .is_some_and(|held| !held.disabled || candidate.disabled)
        {
            continue;
        }

        let Ok(digests) = crate::net::digests(&root.join(&candidate.disk_rel), want_murmur2).await
        else {
            // Bytes that cannot be read cannot be hashed, named or indexed.
            // The walk still carries the file if it is there to carry.
            continue;
        };

        let record = records
            .iter()
            .find(|r| r.kind == candidate.kind && r.file_name == candidate.file_name);
        let seeded = record.filter(|r| {
            !r.provider.is_empty() && !r.project_id.is_empty() && !r.version_id.is_empty()
        });

        catalogue.files.insert(
            key,
            Identity {
                kind: candidate.kind,
                file_name: candidate.file_name,
                disk_rel: candidate.disk_rel,
                disabled: candidate.disabled,
                size: digests.size,
                sha1: digests.sha1,
                sha512: digests.sha512,
                murmur2: digests.murmur2,
                listed_side: record.and_then(|r| r.side),
                refs: seeded
                    .map(|r| PlatformRef {
                        provider: r.provider.clone(),
                        project_id: r.project_id.clone(),
                        version_id: r.version_id.clone(),
                        version_number: r.version_number.clone(),
                        url: None,
                        // Geode installed this file from there once, so it was
                        // downloadable then and nothing here has heard
                        // otherwise. A lookup is what can say it changed.
                        distributable: true,
                    })
                    .into_iter()
                    .collect(),
            },
        );
    }

    let Ask::Platforms {
        client,
        want,
        curseforge_key,
        index,
        spares_bytes,
    } = ask
    else {
        return Ok(catalogue);
    };
    if want.contains(&"modrinth") {
        ask_modrinth(client, &mut catalogue).await;
    }
    if want.contains(&"curseforge") {
        match curseforge_key {
            Some(key) => ask_curseforge(client, key, &mut catalogue).await,
            // Nothing is guessed from no key, and nothing is quietly embedded
            // either: a mod only CurseForge hosts is one the sharer can link by
            // setting a key, and that is the only part of this they can act on.
            //
            // How many files that is depends on what the pack can carry, which
            // is what [`Ask::Platforms::index`] is for. With an index, a file
            // Modrinth named is linked from there whatever CurseForge holds, so
            // only the files nothing else named are the worse for the missing
            // key. Without one, a Modrinth answer buys the pack nothing and
            // every file CurseForge might have named is travelling as bytes —
            // which is the reason the sharer needs to hear, since blaming the
            // format points them away from the one thing they can change.
            None => {
                let unasked = catalogue
                    .identities()
                    .filter(|i| {
                        if index {
                            i.refs.is_empty()
                        } else {
                            i.reference("curseforge").is_none()
                        }
                    })
                    .count();
                if unasked > 0 && index && !spares_bytes {
                    // A Prism instance zip carries every jar whatever names
                    // it, so a key buys those files a name and an update path
                    // rather than a smaller zip. Offering the saving here
                    // would contradict the same export's own summary, which
                    // says in the next breath that nothing can be left out.
                    catalogue.trouble.push(format!(
                        "CurseForge was not asked about {unasked} files nothing else could name, \
                         because no API key is set — add one in Settings and the ones it has will \
                         arrive named instead of anonymous"
                    ));
                } else if unasked > 0 && index {
                    catalogue.trouble.push(format!(
                        "CurseForge was not asked about {unasked} files nothing else could name, \
                         because no API key is set — add one in Settings and the ones it has will \
                         be linked instead of bundled in"
                    ));
                } else if unasked > 0 {
                    catalogue.trouble.push(format!(
                        "CurseForge was not asked about {unasked} files, because no API key is \
                         set — this format links only what CurseForge names, so add one in \
                         Settings and the ones it has will be linked instead of bundled in"
                    ));
                }
            }
        }
    }
    Ok(catalogue)
}

/// Replace every Modrinth reference with what Modrinth itself says.
///
/// A failure is reported rather than read as "nothing matched": left
/// indistinguishable, a rate-limited or unreachable lookup would prove every
/// record stale at once and turn a three-hundred mod export into three hundred
/// jars.
async fn ask_modrinth(client: &reqwest::Client, catalogue: &mut Catalogue) {
    use crate::providers::modrinth::{files_by_hashes, HashAlgorithm};

    let hashes: Vec<String> = catalogue
        .identities()
        .map(|i| i.sha512.to_lowercase())
        .collect();
    // sha512 rather than sha1: the index needs it anyway, and it removes the
    // question of whether a hash hit is really the same bytes.
    let answered = match files_by_hashes(client, &hashes, HashAlgorithm::Sha512).await {
        Ok(answered) => answered,
        Err(e) => {
            catalogue.trouble.push(format!(
                "Modrinth could not be asked which of these files it has ({e}), \
                 so only content with a stored link travels as a reference"
            ));
            return;
        }
    };
    // Turned away part of the way through — rate-limited, most likely, since
    // Modrinth's budget is per address and shared with browsing. Those files
    // keep whatever their records claimed, because nothing was learned about
    // them.
    if !answered.unanswered.is_empty() {
        catalogue.trouble.push(format!(
            "Modrinth answered about only some of these files ({} were refused, \
             most likely for asking too often), so those travel on what was stored \
             about them",
            answered.unanswered.len()
        ));
    }

    let answer = answered
        .found
        .into_iter()
        .map(|(hash, (version, file))| {
            (
                hash,
                PlatformRef {
                    provider: "modrinth".into(),
                    project_id: version.project_id,
                    version_id: version.id,
                    version_number: version.version_number,
                    // The matched file's own URL, not one built from ids and
                    // the name on disk: a file renamed since it was installed
                    // has a constructed URL that 404s, and nothing checks it.
                    url: Some(file.url),
                    // Modrinth has no third-party-download switch for an
                    // author to turn off, so a match there is always one a
                    // recipient can fetch.
                    distributable: true,
                },
            )
        })
        .collect();
    apply_modrinth(catalogue, &answer, &answered.unanswered);
}

/// What an answer does to the catalogue, by sha512 of the bytes asked about.
///
/// Kept apart from the request so the rules can be read against a
/// hand-written answer, and so the one place that decides a record is stale is
/// not also the place that does HTTP.
///
/// `unanswered` holds the hashes nobody answered about. They are left exactly
/// as they were: a miss is only a miss when something answered, and a refused
/// chunk read as a miss would prove every record in it stale at once.
fn apply_modrinth(
    catalogue: &mut Catalogue,
    answer: &HashMap<String, PlatformRef>,
    unanswered: &[String],
) {
    let mut disowned = 0;
    for ident in catalogue.files.values_mut() {
        let hash = ident.sha512.to_lowercase();
        if unanswered.contains(&hash) {
            continue;
        }
        // Answered, so whatever the record claimed about Modrinth is no longer
        // the answer: either the lookup replaces it, or it is gone.
        let had = ident.reference("modrinth").is_some();
        ident.refs.retain(|r| r.provider != "modrinth");
        match answer.get(&hash) {
            Some(reference) => ident.refs.push(reference.clone()),
            // Only once nothing is left naming the file: a stale Modrinth
            // record on a jar CurseForge still names costs it an index entry,
            // not its reference, and telling the sharer it travels as a copy
            // would be the wrong sentence about the right file.
            None if had && ident.refs.is_empty() => disowned += 1,
            None => {}
        }
    }
    if disowned > 0 {
        catalogue.trouble.push(stale_links("Modrinth", disowned));
    }
}

/// What to tell the sharer about links a lookup disowned.
///
/// Embedding is the safe direction — a pack's hashes come from the local bytes
/// while the link would come from the record, so an index entry built on a
/// stale pair fails every reader's checksum and a CurseForge id pair quietly
/// installs other bytes — but it is not a thing to do silently. Without this
/// the sharer is told "nothing could name these mods" about the very files
/// Geode had written a link down for, which is the opposite of what happened.
fn stale_links(platform: &str, count: usize) -> String {
    format!(
        "{count} file(s) Geode had a {platform} link for are not ones {platform} says it holds \
         any more, so they travel as copies"
    )
}

/// Every file there is a fingerprint to ask about.
///
/// Not "every file Modrinth could not name": that is [`travel`]'s preference
/// order, and it belongs to the two formats that have a Modrinth index rather
/// than to the question. A CurseForge manifest has no slot for a Modrinth
/// download, so the files it most needs an answer for are exactly the ones a
/// `.geodepack` would have indexed instead. Which files are worth asking about
/// is the caller's choice, made once in [`Ask::exporting`] by not asking at
/// all, and the fingerprints are already in hand by the time this runs.
fn fingerprints_to_ask(catalogue: &Catalogue) -> Vec<(ContentKind, String, u32)> {
    catalogue
        .files
        .values()
        .filter_map(|i| {
            i.murmur2
                .map(|fingerprint| (i.kind, i.file_name.clone(), fingerprint))
        })
        .collect()
}

/// Ask CurseForge which of these files it has, by the murmur2 it knows them by.
async fn ask_curseforge(client: &reqwest::Client, api_key: &str, catalogue: &mut Catalogue) {
    use crate::providers::curseforge::match_fingerprints;

    let asking = fingerprints_to_ask(catalogue);
    if asking.is_empty() {
        return;
    }

    let fingerprints: Vec<u32> = asking.iter().map(|(_, _, f)| *f).collect();
    let answer = match match_fingerprints(client, &fingerprints, api_key).await {
        Ok(answer) => answer,
        // Reported rather than read as "CurseForge has none of these". A key
        // that has been revoked, a rate limit or no connection at all would
        // otherwise disown every stored CurseForge link at once and turn a
        // geodepack's references into a zip full of jars.
        //
        // The error is CurseForge's own sentence, and `providers::curseforge`
        // never puts the key in one.
        Err(e) => {
            catalogue.trouble.push(format!(
                "CurseForge could not be asked which of these files it has ({e}), \
                 so only content with a stored link travels as a reference"
            ));
            return;
        }
    };

    let answer: HashMap<u32, PlatformRef> = answer
        .into_iter()
        .map(|(fingerprint, version)| (fingerprint, curseforge_ref(version)))
        .collect();
    apply_curseforge(catalogue, &asking, &answer);
}

/// What CurseForge said about a file, as a reference.
///
/// Its own function so the distribution rule can be read — and tested —
/// without a request in the way.
fn curseforge_ref(version: crate::providers::Version) -> PlatformRef {
    PlatformRef {
        provider: "curseforge".into(),
        project_id: version.project_id,
        version_id: version.id,
        version_number: version.version_number,
        // Always `None`: a CurseForge pack names ids and nothing else, and the
        // recipient resolves the URL with their own key at install time, which
        // is what makes an author's decision today the one that counts rather
        // than the one that held when somebody else exported.
        url: None,
        // `blocked_reason` is set for exactly one thing: a file whose author
        // turned third-party downloads off, which CurseForge reports as a null
        // download URL.
        distributable: version.blocked_reason.is_none(),
    }
}

/// What an answer does to the catalogue, by the fingerprint asked about.
///
/// Kept apart from the request for the same reason [`apply_modrinth`] is: the
/// rules can then be read against a hand-written answer, and the one place
/// that decides a record is stale is not also the place that does HTTP.
///
/// `asking` is every file a question was asked about. A file not in it keeps
/// whatever its record claimed, because nothing was asked and so nothing was
/// learned. A file in it that no match came back for has had its record
/// disowned: CurseForge holds no file with those bytes, and a CurseForge pack
/// carries ids with no hash beside them, so a stale pair does not fail a
/// checksum on the way in — it quietly installs different bytes.
fn apply_curseforge(
    catalogue: &mut Catalogue,
    asking: &[(ContentKind, String, u32)],
    answer: &HashMap<u32, PlatformRef>,
) {
    let mut disowned = 0;
    for (kind, file_name, fingerprint) in asking {
        let Some(ident) = catalogue.files.get_mut(&(*kind, file_name.clone())) else {
            continue;
        };
        let had = ident.reference("curseforge").is_some();
        ident.refs.retain(|r| r.provider != "curseforge");
        match answer.get(fingerprint) {
            Some(reference) => ident.refs.push(reference.clone()),
            // As in [`apply_modrinth`]: only once nothing names the file. A
            // jar Modrinth matched travels as an index entry whatever becomes
            // of a CurseForge link Geode once wrote down for it.
            None if had && ident.refs.is_empty() => disowned += 1,
            None => {}
        }
    }
    if disowned > 0 {
        catalogue.trouble.push(stale_links("CurseForge", disowned));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;

    /// What Geode wrote down about a mod it installed. A blank provider is a
    /// jar it knows nothing about.
    fn a_record(provider: &str, file_name: &str) -> ContentRecord {
        ContentRecord {
            file_name: file_name.to_string(),
            kind: ContentKind::Mod,
            provider: provider.to_string(),
            project_id: "stored-project".into(),
            version_id: "stored-version".into(),
            name: file_name.to_string(),
            version_number: "1.0".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: None,
            side: None,
            server_off: false,
        }
    }

    /// A record for a file that is really there, for each named jar.
    async fn catalogue_of(tag: &str, jars: &[(&str, &str)]) -> (Scratch, Catalogue) {
        let scratch = Scratch::new(&format!("identity-{tag}"));
        let root = scratch.paths.root.join("game");
        tokio::fs::create_dir_all(root.join("mods")).await.unwrap();

        let mut candidates = Vec::new();
        let mut records = Vec::new();
        for (provider, name) in jars {
            tokio::fs::write(root.join("mods").join(name), name.as_bytes())
                .await
                .unwrap();
            candidates.push(Candidate {
                kind: ContentKind::Mod,
                disk_rel: PathBuf::from("mods").join(name),
                file_name: name.to_string(),
                disabled: false,
            });
            records.push(a_record(provider, name));
        }
        let borrowed: Vec<&ContentRecord> = records.iter().collect();
        let catalogue = identify(
            &root,
            candidates,
            &borrowed,
            Ask::Nobody,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        (scratch, catalogue)
    }

    /// A client whose every request fails, for proving what happens when a
    /// platform cannot be reached — and for proving a request was never
    /// attempted, since one that was would be reported.
    fn offline() -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap()
    }

    fn answer(hash: &str) -> HashMap<String, PlatformRef> {
        [(
            hash.to_string(),
            PlatformRef {
                provider: "modrinth".into(),
                project_id: "answered-project".into(),
                version_id: "answered-version".into(),
                version_number: "answered-1.0".into(),
                url: Some("https://cdn.modrinth.com/data/answered-project/versions/answered-version/real-name.jar".into()),
                distributable: true,
            },
        )]
        .into_iter()
        .collect()
    }

    /// Nothing asked, nothing concluded: the record stands, with no URL of its
    /// own, which is what keeps an offline export writing the pack it always
    /// wrote.
    #[tokio::test]
    async fn with_nobody_asked_the_record_is_the_answer() {
        let (_scratch, catalogue) = catalogue_of("seed", &[("modrinth", "sodium.jar")]).await;
        let ident = catalogue
            .get(ContentKind::Mod, "sodium.jar")
            .expect("hashed");
        let reference = ident.reference("modrinth").expect("the record's claim");
        assert_eq!(reference.project_id, "stored-project");
        assert!(reference.url.is_none());
        assert!(matches!(travel(ident, Carry::MRPACK), Travel::Linked(_)));
    }

    /// A lookup that answered wins over the record it disagrees with, and
    /// brings the matched file's own URL — the record's name on disk may not be
    /// the name the file was uploaded under.
    #[tokio::test]
    async fn a_lookup_wins_over_the_record_it_disagrees_with() {
        let (_scratch, mut catalogue) = catalogue_of("wins", &[("curseforge", "create.jar")]).await;
        let hash = catalogue
            .get(ContentKind::Mod, "create.jar")
            .unwrap()
            .sha512
            .clone();
        apply_modrinth(&mut catalogue, &answer(&hash), &[]);

        let ident = catalogue.get(ContentKind::Mod, "create.jar").unwrap();
        let reference = ident.reference("modrinth").expect("what Modrinth said");
        assert_eq!(reference.project_id, "answered-project");
        assert!(reference.url.as_deref().unwrap().ends_with("real-name.jar"));
        // The CurseForge record is untouched: it was never asked.
        assert!(ident.reference("curseforge").is_some());
    }

    /// A record the lookup disowned is proven stale and goes. Left standing it
    /// would put the local file's hashes beside a URL serving other bytes, and
    /// every launcher reading the pack would fail its checksum.
    #[tokio::test]
    async fn a_lookup_that_answered_nothing_proves_a_record_stale() {
        let (_scratch, mut catalogue) = catalogue_of("stale", &[("modrinth", "patched.jar")]).await;
        apply_modrinth(&mut catalogue, &HashMap::new(), &[]);

        let ident = catalogue.get(ContentKind::Mod, "patched.jar").unwrap();
        assert!(ident.reference("modrinth").is_none());
        assert!(matches!(
            travel(ident, Carry::GEODEPACK),
            Travel::Copied(Copied::Unknown)
        ));
    }

    /// A lookup nobody could make is reported, not read as "nothing matched".
    /// Indistinguishable, an unreachable Modrinth would prove every record
    /// stale at once and turn a three-hundred mod export into three hundred
    /// jars.
    #[tokio::test]
    async fn a_lookup_that_could_not_be_made_is_reported() {
        let (scratch, _) = catalogue_of("trouble", &[("modrinth", "sodium.jar")]).await;
        let root = scratch.paths.root.join("game");
        let client = offline();
        let catalogue = identify(
            &root,
            vec![Candidate {
                kind: ContentKind::Mod,
                disk_rel: PathBuf::from("mods/sodium.jar"),
                file_name: "sodium.jar".into(),
                disabled: false,
            }],
            &[&a_record("modrinth", "sodium.jar")],
            Ask::Platforms {
                client: &client,
                want: &["modrinth"],
                curseforge_key: None,
                index: true,
                spares_bytes: true,
            },
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert_eq!(catalogue.trouble.len(), 1, "{:?}", catalogue.trouble);
        assert!(
            catalogue
                .get(ContentKind::Mod, "sodium.jar")
                .unwrap()
                .reference("modrinth")
                .is_some(),
            "a lookup that never happened must not disown the record it never read"
        );
    }

    /// No key means no request — and no pretending the answer was "nobody has
    /// this". A mod only CurseForge hosts is one the sharer can link by
    /// setting a key, which is the one part of this they can act on, so they
    /// are told instead of quietly handed a fatter pack.
    ///
    /// Which files that covers is the format's question, not the file's. A
    /// pack with an index links a Modrinth file whatever CurseForge holds, so
    /// the key is only missed for a file nothing else named; a CurseForge pack
    /// has no index, so it is missed for every file CurseForge might have
    /// named — and saying nothing there leaves the sharer reading "this format
    /// cannot link to where they came from" about mods a key would have
    /// linked.
    ///
    /// The client here reaches a port nothing listens on, so a request that
    /// was attempted would fail and be reported in words of its own.
    #[tokio::test]
    async fn no_key_means_no_request_and_an_honest_report() {
        let (scratch, _) = catalogue_of("nokey", &[("modrinth", "sodium.jar")]).await;
        let root = scratch.paths.root.join("game");
        let candidate = Candidate {
            kind: ContentKind::Mod,
            disk_rel: PathBuf::from("mods/sodium.jar"),
            file_name: "sodium.jar".into(),
            disabled: false,
        };
        let client = offline();
        let keyless = |index: bool| Ask::Platforms {
            client: &client,
            want: &["curseforge"],
            curseforge_key: None,
            index,
            // A pack that leaves bytes out, which is every format but a Prism
            // instance zip; the one that does not has a test of its own.
            spares_bytes: true,
        };
        let about_the_key = |c: &Catalogue| {
            c.trouble
                .iter()
                .any(|line| line.contains("CurseForge") && line.contains("no API key"))
        };
        let record = a_record("modrinth", "sodium.jar");

        // A pack with an index has nothing to say about this file: Modrinth
        // names it, so it travels as a link and a key would not have changed
        // that.
        let indexed = identify(
            &root,
            vec![candidate.clone()],
            &[&record],
            keyless(true),
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(!about_the_key(&indexed), "{:?}", indexed.trouble);

        // The same file in a pack with no index travels as bytes, because the
        // one platform this format can link from was never asked.
        let by_id_only = identify(
            &root,
            vec![candidate.clone()],
            &[&record],
            keyless(false),
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(about_the_key(&by_id_only), "{:?}", by_id_only.trouble);

        // And with nothing naming it at all, the platform that might still
        // have had it was not asked, whatever the format.
        let nameless = identify(
            &root,
            vec![candidate],
            &[],
            keyless(true),
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(about_the_key(&nameless), "{:?}", nameless.trouble);
        // Nothing was asked, so nothing was hashed for it either: the second
        // read of every file is what the key pays for.
        assert!(nameless
            .get(ContentKind::Mod, "sodium.jar")
            .unwrap()
            .murmur2
            .is_none());
    }

    /// The same for a lookup that was answered for some files and refused for
    /// others, which is what a rate limit looks like: nothing is concluded
    /// about the files nobody answered for, and the sharer is told.
    #[tokio::test]
    async fn files_in_a_refused_chunk_keep_what_was_stored_about_them() {
        let (_scratch, mut catalogue) = catalogue_of(
            "refused",
            &[("modrinth", "answered.jar"), ("modrinth", "refused.jar")],
        )
        .await;
        let refused = catalogue
            .get(ContentKind::Mod, "refused.jar")
            .unwrap()
            .sha512
            .clone();

        // Answered for one file, refused for the other, and neither hash among
        // the hits — the ordinary shape of a rate-limited export.
        apply_modrinth(&mut catalogue, &HashMap::new(), &[refused]);

        assert!(
            catalogue
                .get(ContentKind::Mod, "refused.jar")
                .unwrap()
                .reference("modrinth")
                .is_some(),
            "a refused chunk proved a record stale"
        );
        assert!(
            catalogue
                .get(ContentKind::Mod, "answered.jar")
                .unwrap()
                .reference("modrinth")
                .is_none(),
            "an answered miss still has to disown its record"
        );
    }

    /// Which half of a pack a file belongs in, decided in one place. Only
    /// Modrinth can be an index entry; a platform whose downloads need a key
    /// goes in the Geode half, and nothing at all goes as bytes.
    #[tokio::test]
    async fn each_platform_travels_by_the_channel_that_can_carry_it() {
        let (_scratch, catalogue) = catalogue_of(
            "channels",
            &[
                ("modrinth", "sodium.jar"),
                ("curseforge", "create.jar"),
                ("", "homemade.jar"),
            ],
        )
        .await;

        let of = |name: &str, carry| catalogue.travel(ContentKind::Mod, name, carry);
        assert!(matches!(
            of("sodium.jar", Carry::GEODEPACK),
            Some(Travel::Linked(_))
        ));
        assert!(matches!(
            of("create.jar", Carry::GEODEPACK),
            Some(Travel::Named(_))
        ));
        assert!(matches!(
            of("homemade.jar", Carry::GEODEPACK),
            Some(Travel::Copied(Copied::Unknown))
        ));
        // A plain `.mrpack` has no second channel, so the CurseForge mod's
        // bytes have to travel — and for a different reason than the homemade
        // jar's, which the sharer is told apart.
        assert!(matches!(
            of("create.jar", Carry::MRPACK),
            Some(Travel::Copied(Copied::Elsewhere))
        ));
        assert!(matches!(
            of("homemade.jar", Carry::MRPACK),
            Some(Travel::Copied(Copied::Unknown))
        ));
        // And a CurseForge pack is the same thing the other way round: it can
        // name the CurseForge mod and has no index to carry the Modrinth one,
        // which is the format's limit rather than the mod being unknown.
        assert!(matches!(
            of("create.jar", Carry::CFPACK),
            Some(Travel::Named(_))
        ));
        assert!(matches!(
            of("sodium.jar", Carry::CFPACK),
            Some(Travel::Copied(Copied::Elsewhere))
        ));
        assert!(matches!(
            of("homemade.jar", Carry::CFPACK),
            Some(Travel::Copied(Copied::Unknown))
        ));
    }

    /// A CurseForge manifest names ids and no path, so only a mod can be
    /// trusted to land where it belongs: the recipient sniffs the bytes and
    /// leaves what it cannot identify in `mods/`, which is where a shader pack
    /// would end up doing nothing. Its bytes travel instead, under a path the
    /// pack does state.
    #[tokio::test]
    async fn only_a_mod_can_be_named_by_a_curseforge_manifest() {
        let scratch = Scratch::new("identity-modsonly");
        let root = scratch.paths.root.join("game");
        let mut candidates = Vec::new();
        for kind in [ContentKind::Mod, ContentKind::ShaderPack] {
            let dir = root.join(kind.folder());
            tokio::fs::create_dir_all(&dir).await.unwrap();
            tokio::fs::write(dir.join("thing.zip"), kind.folder())
                .await
                .unwrap();
            candidates.push(Candidate {
                kind,
                disk_rel: PathBuf::from(kind.folder()).join("thing.zip"),
                file_name: "thing.zip".into(),
                disabled: false,
            });
        }
        let mut catalogue = identify(&root, candidates, &[], Ask::Nobody, &Progress::silent("t"))
            .await
            .unwrap();
        catalogue.pretend_curseforge_holds(&[("thing.zip", 100, 200, true)]);

        assert!(matches!(
            catalogue.travel(ContentKind::Mod, "thing.zip", Carry::CFPACK),
            Some(Travel::Named(_))
        ));
        assert!(matches!(
            catalogue.travel(ContentKind::ShaderPack, "thing.zip", Carry::CFPACK),
            Some(Travel::Copied(Copied::Elsewhere))
        ));
        // The same file in a format whose channel carries a path is named.
        assert!(matches!(
            catalogue.travel(ContentKind::ShaderPack, "thing.zip", Carry::GEODEPACK),
            Some(Travel::Named(_))
        ));
    }

    /// A mod on disk both on and off is one mod, and it is the copy that is
    /// on: that is the one the pack names, whichever order the folder is read
    /// in.
    #[tokio::test]
    async fn a_mod_on_disk_both_on_and_off_is_the_copy_that_is_on() {
        let scratch = Scratch::new("identity-twin");
        let root = scratch.paths.root.join("game");
        tokio::fs::create_dir_all(root.join("mods")).await.unwrap();
        for name in ["twin.jar", "twin.jar.disabled"] {
            tokio::fs::write(root.join("mods").join(name), name.as_bytes())
                .await
                .unwrap();
        }

        for order in [
            ["twin.jar", "twin.jar.disabled"],
            ["twin.jar.disabled", "twin.jar"],
        ] {
            let candidates = order
                .iter()
                .map(|name| Candidate {
                    kind: ContentKind::Mod,
                    disk_rel: PathBuf::from("mods").join(name),
                    file_name: "twin.jar".into(),
                    disabled: name.ends_with(".disabled"),
                })
                .collect();
            let catalogue = identify(&root, candidates, &[], Ask::Nobody, &Progress::silent("t"))
                .await
                .unwrap();
            let ident = catalogue.get(ContentKind::Mod, "twin.jar").unwrap();
            assert!(!ident.disabled, "read in the order {order:?}");
            assert_eq!(ident.disk_rel, PathBuf::from("mods/twin.jar"));
        }
    }

    /// A turned-off mod travels per what the pack can say: indexed under its
    /// enabled name where a note can switch it back off, and as bytes under
    /// its `.disabled` name where nothing can.
    #[tokio::test]
    async fn a_turned_off_mod_needs_somewhere_to_say_so() {
        let scratch = Scratch::new("identity-off");
        let root = scratch.paths.root.join("game");
        tokio::fs::create_dir_all(root.join("mods")).await.unwrap();
        tokio::fs::write(root.join("mods/heavy.jar.disabled"), b"heavy")
            .await
            .unwrap();
        let record = a_record("modrinth", "heavy.jar");
        let catalogue = identify(
            &root,
            vec![Candidate {
                kind: ContentKind::Mod,
                disk_rel: PathBuf::from("mods/heavy.jar.disabled"),
                file_name: "heavy.jar".into(),
                disabled: true,
            }],
            &[&record],
            Ask::Nobody,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        let ident = catalogue.get(ContentKind::Mod, "heavy.jar").unwrap();
        assert!(matches!(travel(ident, Carry::GEODEPACK), Travel::Linked(_)));
        assert!(matches!(
            travel(ident, Carry::MRPACK),
            Travel::Copied(Copied::TurnedOff)
        ));
        // A server's pack has notes, but a player's import does not read a
        // note's off state for a file the index carried.
        assert!(matches!(
            travel(ident, Carry::SERVER_PACK),
            Travel::Copied(Copied::TurnedOff)
        ));
    }

    /// What CurseForge would have answered, without asking it.
    fn a_curseforge_file(file_id: &str, blocked: bool) -> crate::providers::Version {
        crate::providers::Version {
            provider: crate::providers::Provider::CurseForge,
            id: file_id.to_string(),
            project_id: "cf-project".into(),
            name: "Create".into(),
            version_number: "Create 0.5.1".into(),
            game_versions: vec!["1.20.1".into()],
            loaders: vec!["forge".into()],
            release_type: "release".into(),
            published: None,
            downloads: 0,
            files: Vec::new(),
            dependencies: Vec::new(),
            blocked_reason: blocked.then(|| "the author has disabled it".to_string()),
        }
    }

    /// The conversion the whole stage is for: a jar Geode wrote nothing down
    /// about, and which Modrinth does not have, becomes a pair of CurseForge
    /// ids the recipient resolves with their own key.
    #[tokio::test]
    async fn a_fingerprint_match_names_a_jar_no_record_ever_did() {
        let (_scratch, mut catalogue) = catalogue_of("cfhit", &[("", "create.jar")]).await;
        let asking = vec![(ContentKind::Mod, "create.jar".to_string(), 4242)];
        let answer = [(4242, curseforge_ref(a_curseforge_file("cf-file", false)))]
            .into_iter()
            .collect();
        apply_curseforge(&mut catalogue, &asking, &answer);

        let ident = catalogue.get(ContentKind::Mod, "create.jar").unwrap();
        let reference = ident.reference("curseforge").expect("what CurseForge said");
        assert_eq!(reference.project_id, "cf-project");
        assert_eq!(reference.version_id, "cf-file");
        // Never a URL: a CurseForge pack names ids, and the recipient's own key
        // turns them into a download at install time.
        assert!(reference.url.is_none());

        // The Geode half can name it; a plain `.mrpack` cannot, and says which
        // of the two reasons that is.
        assert!(matches!(travel(ident, Carry::GEODEPACK), Travel::Named(_)));
        assert!(matches!(
            travel(ident, Carry::MRPACK),
            Travel::Copied(Copied::Elsewhere)
        ));
    }

    /// The owner's distribution rule, in the one place it is enforced.
    ///
    /// A file whose author turned third-party downloads off is still a match —
    /// that is how the sharer gets told why — but no writer can be handed it as
    /// a reference, because the recipient would be left with ids nobody can
    /// resolve.
    #[tokio::test]
    async fn an_author_blocked_file_is_never_offered_as_a_reference() {
        let (_scratch, mut catalogue) = catalogue_of("cfblocked", &[("", "locked.jar")]).await;
        let asking = vec![(ContentKind::Mod, "locked.jar".to_string(), 7)];
        let answer = [(7, curseforge_ref(a_curseforge_file("cf-locked", true)))]
            .into_iter()
            .collect();
        apply_curseforge(&mut catalogue, &asking, &answer);

        let ident = catalogue.get(ContentKind::Mod, "locked.jar").unwrap();
        let reference = ident.reference("curseforge").expect("still a match");
        assert!(
            !reference.distributable,
            "a blocked match must be recorded as one, so the sharer can be told why"
        );
        // Every format, including the two that could have carried the ids.
        for carry in [
            Carry::GEODEPACK,
            Carry::MRPACK,
            Carry::SERVER_PACK,
            Carry::CFPACK,
        ] {
            assert!(
                matches!(travel(ident, carry), Travel::Copied(Copied::Blocked)),
                "{carry:?} offered a blocked file as a reference"
            );
        }
    }

    /// And the reason the sharer is given is the one they can act on.
    ///
    /// Where the author published the same bytes on Modrinth themselves, a
    /// pack with an index links them from there — their own release, not a way
    /// around them. A pack with no index cannot, so its jar travels for the
    /// format's reason and not the author's: "nobody can follow a link to
    /// this" would send the sharer looking for a remedy that does not exist,
    /// past the one that does.
    #[tokio::test]
    async fn a_blocked_file_the_author_also_published_on_modrinth_is_the_formats_limit() {
        let (_scratch, mut catalogue) = catalogue_of("cfblockedmr", &[("", "both.jar")]).await;
        catalogue.pretend_modrinth_holds(&["both.jar"]);
        let asking = vec![(ContentKind::Mod, "both.jar".to_string(), 9)];
        let answer = [(9, curseforge_ref(a_curseforge_file("cf-both", true)))]
            .into_iter()
            .collect();
        apply_curseforge(&mut catalogue, &asking, &answer);

        let ident = catalogue.get(ContentKind::Mod, "both.jar").unwrap();
        assert!(!ident.reference("curseforge").unwrap().distributable);
        for carry in [Carry::GEODEPACK, Carry::MRPACK, Carry::SERVER_PACK] {
            assert!(
                matches!(travel(ident, carry), Travel::Linked(_)),
                "{carry:?} passed over the author's own Modrinth release"
            );
        }
        assert!(
            matches!(
                travel(ident, Carry::CFPACK),
                Travel::Copied(Copied::Elsewhere)
            ),
            "a format that cannot carry the Modrinth link blamed the author instead"
        );
    }

    /// A by-id channel that names nothing but two integers can only carry the
    /// platform it belongs to.
    ///
    /// A CurseForge manifest has no field for the provider and no hash, so a
    /// numeric id from somewhere else would be resolved against CurseForge and
    /// install an unrelated mod with nothing to catch it. A `.geodepack`'s note
    /// carries the provider's name, so it can say what it means.
    #[tokio::test]
    async fn only_curseforge_ids_go_in_a_channel_that_names_no_platform() {
        let (_scratch, mut catalogue) = catalogue_of("otherprovider", &[("", "plugin.jar")]).await;
        let ident = catalogue.files.values_mut().next().unwrap();
        ident.refs.push(PlatformRef {
            provider: "spiget".into(),
            project_id: "28140".into(),
            version_id: "487564".into(),
            version_number: "1.0".into(),
            url: None,
            distributable: true,
        });

        let ident = catalogue.get(ContentKind::Mod, "plugin.jar").unwrap();
        assert!(matches!(travel(ident, Carry::GEODEPACK), Travel::Named(_)));
        assert!(
            matches!(
                travel(ident, Carry::CFPACK),
                Travel::Copied(Copied::Elsewhere)
            ),
            "another platform's ids were offered as a CurseForge pair"
        );
    }

    /// A lookup that answered nothing about a file the record named proves that
    /// record stale, the same way a Modrinth miss does — and for a sharper
    /// reason here, since a CurseForge pack carries ids with no hash beside
    /// them, so stale ids do not fail a checksum on the way in. They quietly
    /// install different bytes.
    ///
    /// A file nothing was asked about keeps what was stored, because nothing
    /// was learned.
    #[tokio::test]
    async fn a_fingerprint_miss_proves_a_stored_record_stale() {
        let (_scratch, mut catalogue) = catalogue_of(
            "cfmiss",
            &[("curseforge", "asked.jar"), ("curseforge", "unasked.jar")],
        )
        .await;
        let asking = vec![(ContentKind::Mod, "asked.jar".to_string(), 1)];
        apply_curseforge(&mut catalogue, &asking, &HashMap::new());

        let asked = catalogue.get(ContentKind::Mod, "asked.jar").unwrap();
        assert!(asked.reference("curseforge").is_none());
        assert!(matches!(
            travel(asked, Carry::GEODEPACK),
            Travel::Copied(Copied::Unknown)
        ));
        assert!(
            catalogue
                .get(ContentKind::Mod, "unasked.jar")
                .unwrap()
                .reference("curseforge")
                .is_some(),
            "a file nobody asked about had its record disowned"
        );
    }

    /// A lookup nobody could make is reported, never read as "CurseForge has
    /// none of these". Indistinguishable, a revoked key or no connection would
    /// disown every stored CurseForge link at once.
    ///
    /// Also what the murmur2 is for: it is taken in the hashing pass only when
    /// a key means there is somebody to send it to.
    #[tokio::test]
    async fn a_curseforge_lookup_that_could_not_be_made_is_reported() {
        let (scratch, _) = catalogue_of("cftrouble", &[("curseforge", "create.jar")]).await;
        let root = scratch.paths.root.join("game");
        let client = offline();
        let catalogue = identify(
            &root,
            vec![Candidate {
                kind: ContentKind::Mod,
                disk_rel: PathBuf::from("mods/create.jar"),
                file_name: "create.jar".into(),
                disabled: false,
            }],
            &[&a_record("curseforge", "create.jar")],
            Ask::Platforms {
                client: &client,
                want: &["curseforge"],
                // Shaped like a key and belonging to nobody. It never leaves
                // the process: the client cannot connect.
                curseforge_key: Some("$2a$10$not-a-real-key"),
                index: true,
                spares_bytes: true,
            },
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert_eq!(catalogue.trouble.len(), 1, "{:?}", catalogue.trouble);
        assert!(
            catalogue.trouble[0].contains("CurseForge"),
            "{:?}",
            catalogue.trouble
        );
        let ident = catalogue.get(ContentKind::Mod, "create.jar").unwrap();
        assert!(
            ident.reference("curseforge").is_some(),
            "a lookup that never happened must not disown the record it never read"
        );
        assert!(
            ident.murmur2.is_some(),
            "a key means the fingerprint is worth the second read"
        );
    }

    /// What a key would buy depends on what the format does with a file it
    /// cannot name, and for a Prism instance zip that is not "bundle it in":
    /// every jar travels inside one whatever names it, so the key buys a name
    /// and an update path. Offering the saving would contradict the summary
    /// the same export shows the sharer in the next breath.
    #[tokio::test]
    async fn the_missing_key_notice_promises_only_what_the_format_can_do() {
        let (scratch, _) = catalogue_of("prismkey", &[("", "create.jar")]).await;
        let root = scratch.paths.root.join("game");
        let candidate = Candidate {
            kind: ContentKind::Mod,
            disk_rel: PathBuf::from("mods/create.jar"),
            file_name: "create.jar".into(),
            disabled: false,
        };
        let client = offline();

        let notice = |catalogue: &Catalogue| {
            catalogue
                .trouble
                .iter()
                .find(|line| line.contains("no API key"))
                .cloned()
                .expect("the sharer is told the key was not there")
        };

        for (carry, expected, refused) in [
            (Carry::PRISM, "arrive named instead of anonymous", "bundled"),
            (
                Carry::GEODEPACK,
                "linked instead of bundled in",
                "anonymous",
            ),
        ] {
            let catalogue = identify(
                &root,
                vec![candidate.clone()],
                &[],
                Ask::exporting(&client, None, carry),
                &Progress::silent("t"),
            )
            .await
            .unwrap();

            let line = notice(&catalogue);
            assert!(line.contains(expected), "{line}");
            assert!(!line.contains(refused), "{line}");
        }
    }

    /// A file Modrinth has named is still asked about on CurseForge, because
    /// one format's preference order is not the question.
    ///
    /// [`travel`] reads a Modrinth reference first and never looks past it, so
    /// for the two formats with a Modrinth index the second answer changes
    /// nothing. A CurseForge manifest has no slot for a Modrinth download, so
    /// those are precisely the files it needs named — and skipping them here
    /// would send a handful of fingerprints and ship the rest of the instance
    /// as jars.
    #[tokio::test]
    async fn a_file_modrinth_named_is_still_asked_about_on_curseforge() {
        let (scratch, _) = catalogue_of("cfboth", &[("", "sodium.jar")]).await;
        let root = scratch.paths.root.join("game");
        tokio::fs::write(root.join("mods/create.jar"), b"create")
            .await
            .unwrap();
        let client = offline();
        // A key, so the fingerprints are taken; the client cannot reach
        // anything, so the lookup itself is the one that fails.
        let mut catalogue = identify(
            &root,
            ["sodium.jar", "create.jar"]
                .into_iter()
                .map(|name| Candidate {
                    kind: ContentKind::Mod,
                    disk_rel: PathBuf::from("mods").join(name),
                    file_name: name.into(),
                    disabled: false,
                })
                .collect(),
            &[],
            Ask::Platforms {
                client: &client,
                want: &["curseforge"],
                curseforge_key: Some("$2a$10$not-a-real-key"),
                index: true,
                spares_bytes: true,
            },
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        let hash = catalogue
            .get(ContentKind::Mod, "sodium.jar")
            .unwrap()
            .sha512
            .clone();
        apply_modrinth(&mut catalogue, &answer(&hash), &[]);
        assert!(catalogue
            .get(ContentKind::Mod, "sodium.jar")
            .unwrap()
            .reference("modrinth")
            .is_some());

        let asked: Vec<String> = fingerprints_to_ask(&catalogue)
            .into_iter()
            .map(|(_, name, _)| name)
            .collect();
        assert_eq!(
            asked.len(),
            2,
            "the file Modrinth named was left out of the question: {asked:?}"
        );
    }

    /// A `.mrpack` export reads every file once, because a CurseForge answer
    /// cannot change what that pack does with a single file.
    ///
    /// The fingerprint is the only thing in the hashing pass that costs a
    /// second read of every file in the instance, so asking for one is a
    /// choice about what the pack can say — and it is the pack's writer, not
    /// the matcher, that knows. The same run with a Geode half does ask, which
    /// is also what keeps the no-key and lookup-failed sentences true: they
    /// offer the sharer a link, and they are only ever shown for a pack that
    /// could carry one.
    #[tokio::test]
    async fn an_mrpack_export_asks_only_the_platform_it_can_link() {
        let (scratch, _) = catalogue_of("carry", &[("", "create.jar")]).await;
        let root = scratch.paths.root.join("game");
        let client = offline();
        let candidate = Candidate {
            kind: ContentKind::Mod,
            disk_rel: PathBuf::from("mods/create.jar"),
            file_name: "create.jar".into(),
            disabled: false,
        };
        let key = Some("$2a$10$not-a-real-key");

        let plain = identify(
            &root,
            vec![candidate.clone()],
            &[],
            Ask::exporting(&client, key, Carry::MRPACK),
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(
            plain
                .get(ContentKind::Mod, "create.jar")
                .unwrap()
                .murmur2
                .is_none(),
            "an .mrpack export read every file twice for an answer it cannot use"
        );
        // The offline client reports anything it was asked to do, so silence
        // here is proof no request was attempted — and that no sentence
        // offering a CurseForge link reached a pack that cannot carry one.
        assert!(
            !plain.trouble.iter().any(|line| line.contains("CurseForge")),
            "{:?}",
            plain.trouble
        );

        let with_notes = identify(
            &root,
            vec![candidate],
            &[],
            Ask::exporting(&client, key, Carry::GEODEPACK),
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(
            with_notes
                .get(ContentKind::Mod, "create.jar")
                .unwrap()
                .murmur2
                .is_some(),
            "a pack that can name a CurseForge file has to ask about one"
        );
        assert!(
            with_notes
                .trouble
                .iter()
                .any(|line| line.contains("CurseForge")),
            "{:?}",
            with_notes.trouble
        );
    }

    /// A link a lookup disowned is said out loud, not left to look like a mod
    /// nobody has.
    ///
    /// Embedding is right — the pack's hashes come from the local bytes while
    /// the link came from the record — but the sharer otherwise reads "nothing
    /// could name these" about the very files Geode had written a link down
    /// for, with nothing anywhere to say what happened. A file nothing was
    /// stored about is not news, so it is not counted.
    #[tokio::test]
    async fn a_link_a_lookup_disowned_is_reported_not_silent() {
        let (_scratch, mut modrinth) = catalogue_of(
            "disowned-mr",
            &[("modrinth", "stale.jar"), ("", "homemade.jar")],
        )
        .await;
        apply_modrinth(&mut modrinth, &HashMap::new(), &[]);
        assert_eq!(modrinth.trouble.len(), 1, "{:?}", modrinth.trouble);
        assert!(
            modrinth.trouble[0].contains("1 file(s) Geode had a Modrinth link for"),
            "{:?}",
            modrinth.trouble
        );

        let (_scratch, mut curseforge) = catalogue_of(
            "disowned-cf",
            &[
                ("curseforge", "stale.jar"),
                ("", "homemade.jar"),
                ("curseforge", "moved.jar"),
            ],
        )
        .await;
        // The third jar's bytes turn out to be on Modrinth, so losing the
        // CurseForge link Geode wrote down for it costs it nothing: it travels
        // as an index entry. Saying it travels as a copy would be a true count
        // of a false thing.
        let hash = curseforge
            .get(ContentKind::Mod, "moved.jar")
            .unwrap()
            .sha512
            .clone();
        apply_modrinth(&mut curseforge, &answer(&hash), &[]);
        let asking = vec![
            (ContentKind::Mod, "stale.jar".to_string(), 1),
            (ContentKind::Mod, "homemade.jar".to_string(), 2),
            (ContentKind::Mod, "moved.jar".to_string(), 3),
        ];
        apply_curseforge(&mut curseforge, &asking, &HashMap::new());
        assert_eq!(curseforge.trouble.len(), 1, "{:?}", curseforge.trouble);
        assert!(
            curseforge.trouble[0].contains("1 file(s) Geode had a CurseForge link for"),
            "{:?}",
            curseforge.trouble
        );
        assert!(matches!(
            curseforge.travel(ContentKind::Mod, "moved.jar", Carry::MRPACK),
            Some(Travel::Linked(_))
        ));
    }
}

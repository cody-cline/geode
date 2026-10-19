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
const INSTANCE_KINDS: [ContentKind; 4] = [
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
    },
}

impl<'a> Ask<'a> {
    /// What an export asks: Modrinth always, and CurseForge only where the
    /// pack being written has a channel that can carry a reference to it.
    ///
    /// Modrinth because its answer is what a pack index carries as a download.
    /// CurseForge because the Geode half can name a file by its ids — but only
    /// then, because a CurseForge answer is the one thing here that costs a
    /// second read of every file in the instance ([`crate::net::digests`]),
    /// and for a plain `.mrpack` it cannot change what the pack does with a
    /// single file. The `carry` is the one the writer will use, which is what
    /// makes asking worth the reading.
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
            want: if carry.notes {
                &["modrinth", "curseforge"]
            } else {
                &["modrinth"]
            },
            curseforge_key,
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
}

/// What the pack being written is able to say, which is what decides whether a
/// file can travel as a reference instead of as bytes.
#[derive(Debug, Clone, Copy)]
pub struct Carry {
    /// The Geode half can name a source, so a file on a platform the index
    /// cannot carry travels as ids the recipient resolves with their own
    /// credentials.
    pub notes: bool,
    /// Those notes are read back for the off state too, so a turned-off mod
    /// can be indexed under its enabled name instead of travelling as bytes
    /// under its `.disabled` one.
    pub off_state: bool,
}

impl Carry {
    /// A plain `.mrpack`: an index and an overrides tree, and nowhere to say
    /// anything else. A mod it cannot index travels as bytes, and so does a
    /// turned-off one, because the format has no way to say "off" and arriving
    /// switched on is worse than arriving as bytes.
    pub const MRPACK: Self = Self {
        notes: false,
        off_state: false,
    };
    /// A `.geodepack`: file notes carry both a source and the off state.
    pub const GEODEPACK: Self = Self {
        notes: true,
        off_state: true,
    };
    /// A server's pack for its players. Its notes name sources, but a player's
    /// import reads a note's off state only for a file that note also sources
    /// (`arrivals`), so a mod the index carried would arrive switched on. A
    /// turned-off mod therefore still travels as bytes, which arrives off;
    /// changing that means changing what a player's import reads.
    pub const SERVER_PACK: Self = Self {
        notes: true,
        off_state: false,
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
    /// Named in the Geode half and fetched by id, so the recipient's own
    /// answer about an author's wishes is the one that counts.
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
    /// a reference to that platform.
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
    // The index first, wherever it can carry the file: it names the bytes it
    // points at, and needs no credentials at either end.
    if let Some(reference) = ident.reference("modrinth") {
        // A turned-off file needs the pack to be able to say so, because a
        // reference arrives fresh from the platform and switched on. There is
        // nothing to fall through to: a content record names one platform, so
        // a file Modrinth knows has no second reference waiting behind it.
        if ident.disabled && !carry.off_state {
            return Travel::Copied(Copied::TurnedOff);
        }
        return Travel::Linked(reference);
    }
    // Then the Geode half: ids rather than a URL, so the recipient's own answer
    // about an author's wishes is the one that counts. A note carries the off
    // state alongside the source it names, so there is nothing more to ask.
    let named_elsewhere = ident.refs.iter().find(|r| r.provider != "modrinth");
    match (carry.notes, named_elsewhere) {
        // The one place the distribution rule is enforced, so no writer can
        // get it wrong. An author who turned third-party downloads off has
        // left the recipient a reference nobody can resolve, and the bytes are
        // better than that. Reported ahead of the format's own limits because
        // it is the stronger answer: picking another format would not help.
        //
        // Not a refusal to carry the mod. Where the author published the same
        // bytes on Modrinth themselves, the branch above has already linked it
        // from there, which is the author's own release and not a way around
        // them. What is left here is a file only the blocked copy names.
        (_, Some(reference)) if !reference.distributable => Travel::Copied(Copied::Blocked),
        (true, Some(reference)) => Travel::Named(reference),
        // A platform holds this mod and the pack has nowhere to say so. Worth
        // telling apart from a jar nothing knows: the remedy is a format with
        // a second channel, not hunting for where the mod came from.
        (false, Some(_)) => Travel::Copied(Copied::Elsewhere),
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
async fn candidates_in(root: &Path, kinds: &[ContentKind]) -> Result<Vec<Candidate>> {
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
            None => {
                let nameless = catalogue.identities().filter(|i| i.refs.is_empty()).count();
                if nameless > 0 {
                    catalogue.trouble.push(format!(
                        "CurseForge was not asked about {nameless} files nothing else could name, \
                         because no API key is set — add one in Settings and the ones it has will \
                         be linked instead of bundled in"
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
        let keyless = Ask::Platforms {
            client: &client,
            want: &["curseforge"],
            curseforge_key: None,
        };
        let about_the_key = |c: &Catalogue| {
            c.trouble
                .iter()
                .any(|line| line.contains("CurseForge") && line.contains("no API key"))
        };

        // With a record there is nothing to say: something does name this file,
        // whatever CurseForge would have answered.
        let named = identify(
            &root,
            vec![candidate.clone()],
            &[&a_record("modrinth", "sodium.jar")],
            keyless,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(!about_the_key(&named), "{:?}", named.trouble);

        // With none, the one platform that might still have had it was not
        // asked, and the sharer is the one who can change that.
        let nameless = identify(
            &root,
            vec![candidate],
            &[],
            Ask::Platforms {
                client: &client,
                want: &["curseforge"],
                curseforge_key: None,
            },
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
    /// resolve. Where the author published the same bytes on Modrinth
    /// themselves, the Modrinth branch has already linked it from there, which
    /// is their own release and not a way around them.
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
        // Every format, including the one that could have carried the ids.
        for carry in [Carry::GEODEPACK, Carry::MRPACK, Carry::SERVER_PACK] {
            assert!(
                matches!(travel(ident, carry), Travel::Copied(Copied::Blocked)),
                "{carry:?} offered a blocked file as a reference"
            );
        }
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

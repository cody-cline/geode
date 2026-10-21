//! A CurseForge file its author will not let anyone else serve, and the
//! author's own release of the identical bytes on Modrinth.
//!
//! CurseForge lets an author switch third-party downloads off. Geode's answer
//! was to refuse: name the file, and tell the user to fetch it by hand. This
//! is the one thing it does instead, and only when the hashes prove it is the
//! same file.
//!
//! **Why this is not routing around the author.** A sha1 match means the
//! author published those exact bytes on Modrinth themselves, where there is
//! no third-party opt-out to express. So what Geode installs is the author's
//! own publication, fetched from the author's own release — the standing
//! "prefer official distribution" rule, not an exception to it. A mirror, a
//! guessed CDN address or a copy from anyone but the author would be routing
//! around them, and none of those is reachable from here: the only input is
//! the sha1 CurseForge itself published about its own file, and the only place
//! asked is Modrinth's own hash lookup.
//!
//! **Never the reverse.** Modrinth has no third-party opt-out, so a Modrinth
//! file is never blocked and there is nothing to work around — and a blocked
//! file from anywhere else (a SpigotMC premium resource) is paid content whose
//! bytes nobody published for free. Only a CurseForge version is ever looked
//! up here, and [`instead_of`] asks nobody about anything else.
//!
//! **One place.** All three places a blocked file is met reach [`instead_of`]
//! and nothing else: a CurseForge pack import
//! ([`crate::instance::modpack::import_into_at`]), a shared pack's reference
//! ([`crate::share::pack`]) and an ordinary install
//! ([`crate::install::content::install_into`]). The sentence the user is told
//! is written here too, once, because all three have to say the same thing.

use crate::net::Progress;
use crate::providers::{modrinth, Provider, Version, VersionFile};

/// Where the author's own release is looked for, and where to say so when one
/// is found.
///
/// The base is a parameter for the reason every `_at` in this codebase is one:
/// what this does with Modrinth's answer decides whether a file installs or is
/// refused, and that is invisible from outside — so a test serves the answer
/// itself rather than reaching the real API with a hash nobody published.
///
/// The progress handle is here because the user is told *every* time a
/// substitution happens, and this is the one channel all three callers already
/// hold. Their own reports come back in three different shapes — a list of
/// problems, a filename, an install report — and none of them should be able
/// to forget the one sentence they all owe.
#[derive(Clone, Copy)]
pub struct Fallback<'a> {
    modrinth: &'a str,
    progress: &'a Progress,
}

impl<'a> Fallback<'a> {
    /// The real Modrinth.
    pub fn live(progress: &'a Progress) -> Self {
        Self {
            modrinth: modrinth::API,
            progress,
        }
    }

    /// Modrinth as this base serves it, which is how a test answers without
    /// reaching it.
    pub fn at(modrinth: &'a str, progress: &'a Progress) -> Self {
        Self { modrinth, progress }
    }
}

/// What to install in place of a file CurseForge will not serve.
// The Modrinth variant is the whole answer — a version and the file inside it
// — and the refusal is a sentence. One of those is bigger, and it is returned
// once per blocked file, which is rare and immediately matched.
#[allow(clippy::large_enum_variant)]
pub enum Instead {
    /// The author published these exact bytes on Modrinth themselves. Install
    /// `file` — the one in `version` whose sha1 is the one CurseForge
    /// published — and record the provenance from `version`, which says
    /// Modrinth, because that is where the bytes came from.
    ///
    /// The user has already been told; the caller owes them nothing more.
    Modrinth {
        version: Version,
        file: VersionFile,
    },
    /// Nothing to install, and this is why — the whole reason, ready to show
    /// after the file's name. Written here rather than at the three callers so
    /// a user meets the same explanation however they ran into the file.
    Refused(String),
}

/// Look for the author's own release of a blocked CurseForge file, and tell
/// the user when one is found.
///
/// Call this wherever a version turns out to carry a `blocked_reason`. A
/// version that carries none has a file to install and has no business here.
///
/// `by_hand` is what the user would have to do with the file themselves —
/// "drop it into the instance's resourcepacks folder" — because that is the
/// one part of a refusal only the caller knows. It is phrased as the
/// continuation of "download it from CurseForge and …".
pub async fn instead_of(
    client: &reqwest::Client,
    version: &Version,
    by_hand: &str,
    fallback: Fallback<'_>,
) -> Instead {
    // Only CurseForge has an opt-out to work around, so only a CurseForge file
    // has an author's own release elsewhere worth looking for. Nothing is
    // asked about anything else: a Modrinth file is never blocked, and a
    // SpigotMC premium resource is paid content, not bytes somebody published
    // for free under another name. Those keep the platform's own words, which
    // already say what to do about them.
    if version.provider != Provider::CurseForge {
        return Instead::Refused(
            version
                .blocked_reason
                .clone()
                .unwrap_or_else(|| format!("{} cannot be downloaded", version.name)),
        );
    }

    // The hash comes from CurseForge's own metadata about its own file
    // (`hashes`, algo 1 = sha1) and from nowhere else. There is no second
    // guess available and none would be acceptable: the file cannot be fetched
    // to hash, and a name or a size is not proof that two files are the same
    // bytes.
    let Some(sha1) = version.blocked_sha1.clone() else {
        return Instead::Refused(format!(
            "its author disabled third-party downloads, and CurseForge published no \
             hash for it — so Geode could not check whether they released the same \
             file on Modrinth. Download it from CurseForge and {by_hand}"
        ));
    };

    let answer = modrinth::files_by_hashes_at(
        client,
        fallback.modrinth,
        std::slice::from_ref(&sha1),
        modrinth::HashAlgorithm::Sha1,
    )
    .await;

    // A lookup that could not be made is not a no-match. Modrinth having a bad
    // minute says nothing at all about whether the author published there, and
    // reading it as "they did not" would turn a transient 429 into a refusal
    // that looks like the author's own decision.
    let answer = match answer {
        Ok(answer) if answer.unanswered.is_empty() => answer,
        Ok(_) => return Instead::Refused(lookup_failed("Modrinth turned the question away")),
        Err(e) => return Instead::Refused(lookup_failed(&e.to_string())),
    };

    let Some((mr_version, file)) = answer.found.get(&sha1.to_lowercase()).cloned() else {
        return Instead::Refused(format!(
            "its author disabled third-party downloads, and Modrinth holds no file \
             with the same bytes — so there is no release of their own to install \
             instead. Download it from CurseForge and {by_hand}"
        ));
    };

    // Said every time, in one place, in the plainest words there are for it.
    fallback.progress.notice(
        format!("{} came from Modrinth", mr_version.name),
        format!(
            "The CurseForge author's copy of {} is not distributable — they have \
             switched third-party downloads off — so Geode installed their own \
             Modrinth release of the identical file instead.",
            version.name
        ),
    );

    Instead::Modrinth {
        version: mr_version,
        file,
    }
}

/// The one wording for a lookup that could not be made, kept apart from the
/// wording for a lookup that answered nothing because those are different
/// facts and only one of them is final. A rate limit read as "the author
/// never published it there" would wear somebody else's decision.
///
/// No `by_hand` advice: nothing here is settled, and telling someone to go and
/// fetch a file by hand is the wrong instruction for a question that has not
/// been answered yet.
fn lookup_failed(detail: &str) -> String {
    format!(
        "its author disabled third-party downloads, and Modrinth could not be asked \
         whether they released the same file there ({detail}) — so this is not a no. \
         Try again shortly"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Recorder;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    /// A Modrinth of our own, answering one hash lookup with this body and
    /// counting what it was asked.
    ///
    /// Returns the base in the `.../v2` shape the real `API` constant has, so
    /// the path the call appends is the path under test.
    fn modrinth_answering(status: u16, body: &str) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let asked: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let body = body.to_string();

        let log = asked.clone();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut start_line = String::new();
            let _ = reader.read_line(&mut start_line);

            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut request = vec![0u8; length];
            let _ = reader.read_exact(&mut request);
            log.lock().expect("lock").push(format!(
                "{} {}",
                start_line.trim_end(),
                String::from_utf8_lossy(&request)
            ));

            let mut out = stream;
            let _ = out.write_all(
                format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        });

        (format!("http://{addr}/v2"), asked)
    }

    /// One version as the bulk hash endpoint hands it over, keyed by the hash
    /// that was asked about. The wanted jar is neither first nor primary, so a
    /// lookup that took `primary_file()` would install other bytes than the
    /// ones CurseForge named.
    fn version_holding(hash: &str) -> String {
        format!(
            r#"{{"{hash}":{{"id":"MRVER","project_id":"MRPROJ","name":"Cool Mod 1.2.3",
               "version_number":"1.2.3","version_type":"release","game_versions":["1.21.1"],
               "loaders":["fabric"],"downloads":7,"files":[
                 {{"url":"https://cdn.modrinth.com/sources.jar","filename":"sources.jar",
                   "hashes":{{"sha1":"0000000000000000000000000000000000000000"}},
                   "size":1,"primary":true}},
                 {{"url":"https://cdn.modrinth.com/cool-1.2.3.jar","filename":"cool-1.2.3.jar",
                   "hashes":{{"sha1":"{hash}","sha512":"ff"}},"size":42,"primary":false}}
               ]}}}}"#
        )
    }

    const SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

    /// What a caller says the user would have to do by hand. Only the caller
    /// knows the folder, so it is the one part of a refusal that comes in.
    const BY_HAND: &str = "drop it into the instance's mods folder";

    fn a_blocked_curseforge_file() -> Version {
        Version {
            provider: Provider::CurseForge,
            id: "6001".into(),
            project_id: "5001".into(),
            name: "Cool Mod".into(),
            version_number: "cool-1.2.3.jar".into(),
            game_versions: vec!["1.21.1".into()],
            loaders: vec!["fabric".into()],
            release_type: "release".into(),
            published: None,
            downloads: 0,
            files: Vec::new(),
            dependencies: Vec::new(),
            blocked_reason: Some(
                "The author has disabled third-party downloads for this file.".into(),
            ),
            blocked_sha1: Some(SHA1.into()),
        }
    }

    fn telling() -> (Progress, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        (Progress::new(recorder.clone(), "blocked"), recorder)
    }

    /// A client that cannot reach anything, for proving nothing was asked.
    fn offline() -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn a_blocked_file_modrinth_knows_by_hash_comes_from_the_authors_own_release() {
        let (base, asked) = modrinth_answering(200, &version_holding(SHA1));
        let (progress, told) = telling();

        let instead = instead_of(
            &crate::net::client(),
            &a_blocked_curseforge_file(),
            BY_HAND,
            Fallback::at(&base, &progress),
        )
        .await;

        let Instead::Modrinth { version, file } = instead else {
            panic!("Modrinth published the same bytes, so there is a release to install");
        };
        // Modrinth, because that is where the bytes came from. A record built
        // from this version says so without anybody remembering to.
        assert_eq!(version.provider, Provider::Modrinth);
        assert_eq!(version.project_id, "MRPROJ");
        assert_eq!(version.id, "MRVER");
        // The file whose hash matched, not the version's primary one.
        assert_eq!(file.filename, "cool-1.2.3.jar");
        assert_eq!(file.url, "https://cdn.modrinth.com/cool-1.2.3.jar");

        // Asked by the sha1 CurseForge published, and nothing else.
        let asked = asked.lock().expect("lock").clone();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(
            asked[0].starts_with("POST /v2/version_files "),
            "the hash lookup is the only thing asked: {asked:?}"
        );
        assert!(
            asked[0].contains(SHA1) && asked[0].contains("\"sha1\""),
            "the hash and the algorithm both have to be CurseForge's own: {asked:?}"
        );

        // Told, in plain words, that the author's CurseForge copy is not
        // distributable and their Modrinth release was used instead.
        let notices = told.events();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].0, "geode://notice");
        let body = notices[0].1["body"].as_str().expect("a body");
        assert!(body.contains("not distributable"), "{body}");
        assert!(body.contains("Modrinth release"), "{body}");
        assert!(body.contains("Cool Mod"), "{body}");
    }

    #[tokio::test]
    async fn a_blocked_file_modrinth_does_not_know_is_still_refused() {
        let (base, _asked) = modrinth_answering(200, "{}");
        let (progress, told) = telling();

        let instead = instead_of(
            &crate::net::client(),
            &a_blocked_curseforge_file(),
            BY_HAND,
            Fallback::at(&base, &progress),
        )
        .await;

        let Instead::Refused(because) = instead else {
            panic!("nobody else published these bytes, so there is nothing to install");
        };
        assert!(
            because.contains("disabled third-party downloads"),
            "the author's own reason still leads: {because}"
        );
        assert!(
            because.contains("no file with the same bytes"),
            "and the user is told why there was no substitute: {because}"
        );
        assert!(
            because.contains(BY_HAND),
            "a refusal has to end with what the user does about it: {because}"
        );
        assert!(
            told.events().is_empty(),
            "nothing happened, so nothing should have been announced: {:?}",
            told.events()
        );
    }

    /// The failure that would look like the author's decision: a rate limit
    /// read as "they never published it there".
    #[tokio::test]
    async fn a_lookup_that_could_not_be_made_is_not_a_no_match() {
        let (base, _asked) = modrinth_answering(429, r#"{"error":"slow down"}"#);
        let (progress, _told) = telling();

        let instead = instead_of(
            &crate::net::client(),
            &a_blocked_curseforge_file(),
            BY_HAND,
            Fallback::at(&base, &progress),
        )
        .await;

        let Instead::Refused(because) = instead else {
            panic!("a refused lookup is no reason to install anything");
        };
        assert!(
            because.contains("could not be asked"),
            "a lookup nobody answered has to read as one: {because}"
        );
        assert!(
            because.contains("not a no"),
            "and say it is not an answer, so trying again is worth it: {because}"
        );
        assert!(
            !because.contains("no file with the same bytes"),
            "a refused lookup was reported as Modrinth not having the file: {because}"
        );
    }

    /// Modrinth has no third-party opt-out, so a Modrinth file is never
    /// blocked and there is nothing to work around. A SpigotMC premium
    /// resource is blocked, and its bytes are paid content nobody published
    /// for free — looking for them under another name is exactly the mirror
    /// hunt this module exists not to do.
    #[tokio::test]
    async fn nothing_but_a_curseforge_file_is_ever_looked_up() {
        let (progress, told) = telling();

        for provider in [Provider::Modrinth, Provider::Spiget] {
            let mut version = a_blocked_curseforge_file();
            version.provider = provider;
            version.blocked_reason = Some("this resource is premium".into());

            // Pointed at a Modrinth that would answer with a match if it were
            // ever asked, so a request that is made cannot pass this test.
            let (base, asked) = modrinth_answering(200, &version_holding(SHA1));
            let instead =
                instead_of(&offline(), &version, BY_HAND, Fallback::at(&base, &progress)).await;

            let Instead::Refused(because) = instead else {
                panic!("{provider:?} has no author opt-out to work around");
            };
            assert_eq!(
                because, "this resource is premium",
                "the platform's own reason is the whole answer for {provider:?}"
            );
            assert!(
                !because.contains("Modrinth"),
                "{provider:?} was offered a Modrinth release: {because}"
            );
            assert!(
                asked.lock().expect("lock").is_empty(),
                "{provider:?} was looked up on Modrinth: {:?}",
                asked.lock().expect("lock")
            );
        }

        assert!(
            told.events().is_empty(),
            "a substitution was announced for a file nobody substituted"
        );
    }

    /// The hash is the whole proof, so without one there is nothing to prove
    /// and nothing is asked. Said apart from a miss: a user who sees this can
    /// act on it, and nobody should read it as the author having published
    /// nothing.
    #[tokio::test]
    async fn no_hash_from_curseforge_means_nothing_is_asked() {
        let (base, asked) = modrinth_answering(200, &version_holding(SHA1));
        let (progress, _told) = telling();
        let mut version = a_blocked_curseforge_file();
        version.blocked_sha1 = None;

        let instead = instead_of(&offline(), &version, BY_HAND, Fallback::at(&base, &progress)).await;

        let Instead::Refused(because) = instead else {
            panic!("with no hash there is no proof the bytes are the same file");
        };
        assert!(because.contains("published no"), "{because}");
        assert!(because.contains("hash"), "{because}");
        assert!(
            asked.lock().expect("lock").is_empty(),
            "Modrinth was asked with no hash to ask about"
        );
    }
}

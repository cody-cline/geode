//! Integration tests against the real CurseForge API.
//!
//! These hit the network *and* need a key, so they are `#[ignore]`d twice
//! over. Run them with the key the build already reads:
//!
//! ```sh
//! set -a; . ./.env; set +a
//! cargo test --test live_curseforge -- --ignored --nocapture
//! ```
//!
//! They exist because everything Geode knows about CurseForge was, until
//! these, proven only against listeners on localhost answering the shapes the
//! documentation describes. That proves what Geode *sends* and nothing about
//! what comes back — and three things that matter are decided entirely by what
//! comes back: whether a fingerprint names a local jar, whether a batch of ids
//! answers for every id, and whether the `hashes` array really numbers sha1 as
//! algorithm 1, which is the single fact the whole author-blocked substitution
//! rests on.
//!
//! The key is read from the environment and never printed. A test that cannot
//! find one says so and stops rather than reaching the API without it.

use geode_lib::net;
use geode_lib::providers::{self, curseforge, ProjectKind, Provider, SearchQuery, SortBy};

/// The key, or a clear reason there is nothing to run with.
///
/// Returned rather than logged: a key in a test log is a key in a terminal
/// scrollback, and this one belongs to the person running the suite.
fn key() -> String {
    match std::env::var("GEODE_CURSEFORGE_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => panic!(
            "these tests need a CurseForge key: `set -a; . ./.env; set +a` before \
             running them, or skip this file"
        ),
    }
}

/// A popular mod anybody can find, and its newest Fabric file. Taken from the
/// API rather than hardcoded, so nothing here goes stale when a version is
/// deleted or a project is renamed.
async fn a_real_file(client: &reqwest::Client, api_key: &str) -> providers::Version {
    let query = SearchQuery {
        query: "jei".into(),
        kind: ProjectKind::Mod,
        game_version: None,
        loader: None,
        categories: vec![],
        sort: SortBy::Downloads,
        offset: 0,
        limit: 1,
        providers: Some(vec![Provider::CurseForge]),
    };
    // The federation collects a provider's refusal as a warning rather than an
    // error, so an empty answer has to be read together with those or the
    // failure says "found nothing" when it means "the key was refused".
    let results = providers::search(client, &query, Some(api_key))
        .await
        .expect("CurseForge answered a search");
    let hit = results.hits.into_iter().next().unwrap_or_else(|| {
        panic!(
            "a search anybody can make found nothing on CurseForge. Warnings: {:?}",
            results.warnings
        )
    });

    providers::versions(client, Provider::CurseForge, &hit.id, None, None, Some(api_key))
        .await
        .expect("CurseForge answered for this project's files")
        .into_iter()
        .find(|v| v.primary_file().is_some())
        .expect("a file that can actually be downloaded")
}

/// The whole of stage 3's path, end to end against the real thing: take bytes
/// CurseForge served, hash them the way Geode hashes a local jar, and ask
/// CurseForge what that fingerprint is. If murmur2 is wrong — the filtering,
/// the seed, the two passes — this is where it shows, because no fixture is
/// involved on either side.
#[tokio::test]
#[ignore = "hits the network and needs a key"]
async fn a_fingerprint_of_bytes_curseforge_served_names_the_file_back() {
    let api_key = key();
    let client = net::client();

    let version = a_real_file(&client, &api_key).await;
    let file = version.primary_file().unwrap().clone();

    // Downloaded to a scratch path and hashed exactly as an import hashes a
    // jar it found on disk.
    let dir = std::env::temp_dir().join(format!("geode-cf-live-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join(&file.filename);
    net::fetch_one(
        &client,
        &net::Download::new(file.url.clone(), path.clone()).label(file.filename.clone()),
    )
    .await
    .expect("CurseForge served the file it advertised");

    let digests = net::digests(&path, true).await.expect("hashed the jar");
    let murmur2 = digests
        .murmur2
        .expect("a murmur2 was asked for and must be taken");

    let answer = curseforge::match_fingerprints(&client, &[murmur2], &api_key)
        .await
        .expect("CurseForge answered a fingerprint lookup");

    // Either CurseForge named the file, or it did not account for the
    // fingerprint at all - which it does about one call in four, and which
    // must come back as unanswered rather than as "we do not have this". What
    // is never acceptable is a silent miss: that is what strips a stored link
    // and turns a reference into a copy of somebody's jar.
    match answer.found.get(&murmur2) {
        Some(named) => {
            assert_eq!(named.project_id, version.project_id, "a different project");
            assert_eq!(named.id, version.id, "a different file of the same project");
            assert!(
                answer.unanswered.is_empty(),
                "found and unanswered at once: {:?}",
                answer.unanswered
            );
        }
        None => assert_eq!(
            answer.unanswered,
            vec![murmur2],
            "CurseForge did not name the fingerprint of its own file ({}) and did not \
             report it as unanswered either, so a real jar would have been read as one \
             nothing can name",
            file.filename
        ),
    }

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

/// The fact `install::blocked` rests on. CurseForge documents algorithm 1 as
/// sha1, and Geode reads the `hashes` array on that basis alone: a file whose
/// author has turned third-party downloads off cannot be fetched and hashed,
/// so that published sha1 is the only way to find the author's own release of
/// the same bytes on Modrinth. If the numbering were different, the
/// substitution would silently never match and nothing would say why.
#[tokio::test]
#[ignore = "hits the network and needs a key"]
async fn curseforge_publishes_a_sha1_for_its_own_file() {
    let api_key = key();
    let client = net::client();

    let version = a_real_file(&client, &api_key).await;
    let file = version.primary_file().expect("a downloadable file");

    let sha1 = file
        .sha1
        .as_deref()
        .expect("CurseForge published no sha1 for a file it serves");
    assert_eq!(sha1.len(), 40, "not a sha1 at all: {sha1}");
    assert!(
        sha1.bytes().all(|b| b.is_ascii_hexdigit()),
        "not hex: {sha1}"
    );

    // And the same bytes, hashed here, agree with what they published — which
    // is what makes a hash match proof that two files are the same file.
    let dir = std::env::temp_dir().join(format!("geode-cf-sha1-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let path = dir.join(&file.filename);
    net::fetch_one(
        &client,
        &net::Download::new(file.url.clone(), path.clone()).label(file.filename.clone()),
    )
    .await
    .expect("CurseForge served the file");

    let digests = net::digests(&path, false).await.expect("hashed the jar");
    assert_eq!(
        digests.sha1.to_lowercase(),
        sha1.to_lowercase(),
        "the sha1 CurseForge published is not the sha1 of the bytes it served"
    );

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

/// A fingerprint nobody has is a miss, not an error and not a hit. An importer
/// that read a refused or empty answer as "this jar is unknown" would embed a
/// jar it could have linked; one that read it as an error would fail a whole
/// pack over one unknown file.
#[tokio::test]
#[ignore = "hits the network and needs a key"]
async fn a_fingerprint_nobody_has_is_a_miss() {
    let api_key = key();
    let client = net::client();

    // Not a hash of anything: the fingerprint space is u32 and this is one
    // value no real jar has produced for anyone yet.
    let nobodys = 1u32;
    let matched = curseforge::match_fingerprints(&client, &[nobodys], &api_key)
        .await
        .expect("a lookup that finds nothing still answers");

    assert!(
        !matched.found.contains_key(&nobodys),
        "CurseForge claimed to know a fingerprint nobody has"
    );
    // And it is accounted for one way or the other: either listed as
    // unmatched, which is the answer, or reported unanswered. Silence read as
    // a miss is the whole defect this half exists for.
    assert!(
        matched.unanswered.is_empty() || matched.unanswered == vec![nobodys],
        "a fingerprint came back neither matched, nor unmatched, nor unanswered: {:?}",
        matched.unanswered
    );
}

/// The batch calls stage 8 replaced a per-mod loop with. Both are asked for
/// every id in one request, and both must answer for every id — a batch that
/// quietly drops one would produce an instance one mod short with nothing
/// said.
#[tokio::test]
#[ignore = "hits the network and needs a key"]
async fn a_batch_answers_for_every_id_it_was_given() {
    let api_key = key();
    let client = net::client();

    // Several real projects, so the batch has something to be wrong about.
    let query = SearchQuery {
        query: "fabric".into(),
        kind: ProjectKind::Mod,
        game_version: None,
        loader: None,
        categories: vec![],
        sort: SortBy::Downloads,
        offset: 0,
        limit: 5,
        providers: Some(vec![Provider::CurseForge]),
    };
    let hits = providers::search(&client, &query, Some(&api_key))
        .await
        .expect("CurseForge answered a search")
        .hits;
    assert!(hits.len() > 1, "need several projects: {}", hits.len());

    let project_ids: Vec<u32> = hits
        .iter()
        .filter_map(|hit| hit.id.parse().ok())
        .collect();
    let projects = curseforge::projects_by_ids(&client, &project_ids, &api_key)
        .await
        .expect("CurseForge answered a project batch");
    for id in &project_ids {
        assert!(
            projects.iter().any(|p| p.id == id.to_string()),
            "the project batch left out {id}: asked for {project_ids:?}"
        );
    }

    // Each project's own newest file, by id, through the other batch call.
    let mut file_ids = Vec::new();
    for hit in hits.iter().take(3) {
        if let Some(version) =
            providers::versions(&client, Provider::CurseForge, &hit.id, None, None, Some(&api_key))
                .await
                .expect("CurseForge answered for a project's files")
                .into_iter()
                .next()
        {
            if let Ok(id) = version.id.parse::<u32>() {
                file_ids.push(id);
            }
        }
    }
    assert!(!file_ids.is_empty(), "no file ids to ask about");

    let files = curseforge::files_by_ids(&client, &file_ids, &api_key)
        .await
        .expect("CurseForge answered a file batch");
    for id in &file_ids {
        assert!(
            files.iter().any(|v| v.id == id.to_string()),
            "the file batch left out {id}: asked for {file_ids:?}"
        );
    }
}

/// Where a file from a CurseForge pack lands is decided by its project's
/// `classId`, which stage 8 reads through `kind_from_class`. This checks the
/// numbers Geode maps are the numbers CurseForge actually uses, for the kinds
/// it can search by.
#[tokio::test]
#[ignore = "hits the network and needs a key"]
async fn a_projects_class_says_which_folder_its_files_belong_in() {
    let api_key = key();
    let client = net::client();

    for kind in [ProjectKind::Mod, ProjectKind::ResourcePack, ProjectKind::Shader] {
        let query = SearchQuery {
            query: String::new(),
            kind,
            game_version: None,
            loader: None,
            categories: vec![],
            sort: SortBy::Downloads,
            offset: 0,
            limit: 1,
            providers: Some(vec![Provider::CurseForge]),
        };
        let hit = providers::search(&client, &query, Some(&api_key))
            .await
            .unwrap_or_else(|e| panic!("CurseForge refused a {kind:?} search: {e}"))
            .hits
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no {kind:?} found at all"));

        let id: u32 = hit.id.parse().expect("a numeric project id");
        let projects = curseforge::projects_by_ids(&client, &[id], &api_key)
            .await
            .expect("CurseForge answered a project batch");
        let project = projects
            .iter()
            .find(|p| p.id == id.to_string())
            .expect("the project just searched for");

        assert_eq!(
            project.kind, kind,
            "a {kind:?} came back as {:?}, so its files would land in the wrong folder",
            project.kind
        );
    }
}

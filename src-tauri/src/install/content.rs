//! Installing mods and other content into an instance, with dependency
//! resolution and update checking.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::instance::content::{disabled_name, ContentKind, ContentRecord};
use crate::instance::Instance;
use crate::install::blocked;
use crate::net::{Download, Progress};
use crate::paths::Paths;
use crate::providers::{self, DependencyKind, Project, Provider, Version};

/// Install one version's primary file into the instance.
///
/// Replaces any earlier file from the same project so updating never leaves two
/// copies of a mod in the folder — the single most common way a modded instance
/// breaks.
#[allow(clippy::too_many_arguments)] // provenance fields travel together
pub async fn install(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    version: &Version,
    kind: ContentKind,
    display_name: &str,
    icon_url: Option<String>,
    slug: Option<String>,
    as_dependency: bool,
    fallback: blocked::Fallback<'_>,
) -> Result<String> {
    let dir = crate::instance::content::content_dir(paths, &instance.id, kind);
    install_into(
        client,
        &dir,
        &mut instance.content,
        version,
        kind,
        display_name,
        icon_url,
        slug,
        as_dependency,
        None,
        fallback,
    )
    .await
}

/// The same install, into any content folder with any set of records.
///
/// A server's plugins folder is not an instance's mods folder, but nothing
/// about downloading a file and recording where it came from cares which.
#[allow(clippy::too_many_arguments)]
pub async fn install_into(
    client: &reqwest::Client,
    dir: &std::path::Path,
    records: &mut Vec<ContentRecord>,
    version: &Version,
    kind: ContentKind,
    display_name: &str,
    icon_url: Option<String>,
    slug: Option<String>,
    as_dependency: bool,
    side: Option<crate::instance::jar::Side>,
    fallback: blocked::Fallback<'_>,
) -> Result<String> {
    // Asked of the mod the user asked for, before anything is downloaded and
    // before Modrinth is asked anything: a refusal arriving after the notice
    // that says the file came from the author's own Modrinth release would
    // contradict it.
    refuse_a_servers_mod(
        records,
        kind,
        version.provider,
        &version.project_id,
        version.primary_file().map(|f| f.filename.as_str()),
    )?;

    // What the user asked for, kept because a substitution answers in another
    // platform's names and two places below have to know both: the copy
    // already on disk was recorded under the name the user asked by.
    let asked = (version.provider, version.project_id.clone());

    // A CurseForge author who switched third-party downloads off used to end
    // this here. It still does unless the hashes prove that same author
    // published the identical bytes on Modrinth, in which case their own
    // release is what installs — provenance and all, because every field
    // below is read off the version actually being installed. The file comes
    // with it rather than from `primary_file`: a version answered by a hash
    // lookup can list a sources jar as its primary one, and the file that
    // matters is the one whose hash CurseForge published. See
    // `install::blocked`.
    let substituted;
    let (version, file) = if version.blocked_reason.is_some() {
        // Where the file would go if the user fetched it themselves. The
        // folder, not the full path: `dir` can be inside a server's run tree
        // or an instance, and the folder is the part that tells them where to
        // look.
        let by_hand = format!("drop it into the {} folder", kind.folder());
        match blocked::instead_of(client, version, &by_hand, fallback).await {
            blocked::Instead::Modrinth { version, file } => {
                substituted = (version, file);
                (&substituted.0, &substituted.1)
            }
            blocked::Instead::Refused(because) => return Err(Error::msg(because)),
        }
    } else {
        let file = version
            .primary_file()
            .ok_or_else(|| Error::msg(format!("{display_name} has no downloadable file")))?;
        (version, file)
    };

    // And asked again about the name and the provenance that would actually be
    // written. A substitution answers with Modrinth's ids and Modrinth's name
    // for the same mod, and the server's own record holds whichever of the two
    // its pack resolved — so one of the two asks catches it and neither alone
    // does.
    refuse_a_servers_mod(
        records,
        kind,
        version.provider,
        &version.project_id,
        Some(&file.filename),
    )?;

    tokio::fs::create_dir_all(dir).await?;

    // Remove the previous file for this project before writing the new one —
    // under either name this mod goes by. A substitution writes Modrinth's
    // ids, while the copy the user already had was recorded from CurseForge,
    // and matching only the new ones leaves both jars in the folder: two
    // builds of one mod, which plenty of mods refuse to load and no player
    // asked for.
    let previous: Vec<String> = records
        .iter()
        .filter(|r| {
            r.kind == kind
                && ((r.provider == version.provider.as_str()
                    && r.project_id == version.project_id)
                    || (r.provider == asked.0.as_str() && r.project_id == asked.1))
        })
        .map(|r| r.file_name.clone())
        .collect();

    crate::net::fetch_one(
        client,
        &Download::new(file.url.clone(), dir.join(&file.filename))
            .sha1(file.sha1.clone())
            .size(file.size)
            .label(display_name.to_string()),
    )
    .await?;

    for old in previous {
        if old == file.filename {
            continue;
        }
        let _ = tokio::fs::remove_file(dir.join(&old)).await;
        let _ = tokio::fs::remove_file(dir.join(disabled_name(&old))).await;
        records.retain(|r| !(r.file_name == old && r.kind == kind));
    }

    records.retain(|r| !(r.file_name == file.filename && r.kind == kind));
    records.push(ContentRecord {
        file_name: file.filename.clone(),
        kind,
        provider: version.provider.as_str().to_string(),
        project_id: version.project_id.clone(),
        version_id: version.id.clone(),
        name: display_name.to_string(),
        version_number: version.version_number.clone(),
        slug,
        icon_url,
        dependency: as_dependency,
        origin: Default::default(),
        requires: Some(required_ids(version)),
        side,
        server_off: false,
    });

    Ok(file.filename.clone())
}

/// Refuse an install that would replace a mod the server's pack owns.
///
/// Installing over a server's mod replaces it — a new version, or a second copy
/// under the same name — and the version a server's mod runs at is the server's
/// to choose. A player on a different build of a mod both sides load cannot
/// join, and sharing runs one way, so the next sync would only put the server's
/// back.
///
/// The mod is named twice over, by file name and by provenance, because a
/// substitution makes those two different answers: `file_name` is `None` for a
/// blocked version, which carries no file of its own to name.
fn refuse_a_servers_mod(
    records: &[ContentRecord],
    kind: ContentKind,
    provider: Provider,
    project_id: &str,
    file_name: Option<&str>,
) -> Result<()> {
    let owned = records.iter().find(|r| {
        r.origin == crate::instance::content::Origin::Server
            && r.kind == kind
            && (file_name.is_some_and(|name| r.file_name == name)
                || (r.provider == provider.as_str() && r.project_id == project_id))
    });
    match owned {
        Some(owned) => Err(Error::msg(format!(
            "{} came from the server's pack, so the server decides which version it runs",
            owned.name
        ))),
        None => Ok(()),
    }
}

/// The provider's view of what a version requires, kept as a fallback for jars
/// whose own metadata we cannot read.
fn required_ids(version: &Version) -> Vec<String> {
    version
        .dependencies
        .iter()
        .filter(|d| d.kind == DependencyKind::Required)
        .filter_map(|d| d.project_id.clone())
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub installed: Vec<String>,
    /// Dependencies we could not resolve or download, with the reason.
    pub skipped: Vec<String>,
    /// The folder it went into. A server may send a mod asked for as a mod to
    /// its Client mods instead, and the UI says so.
    pub kind: ContentKind,
}

/// Install a version plus everything it requires.
///
/// Walks required dependencies breadth-first, picking the newest compatible
/// version of each. Optional dependencies are deliberately left alone: they are
/// the author saying "this works better with X", not "this needs X", and
/// pulling them in silently is how instances bloat.
///
/// `dep_game_version` and `dep_loader` say what the dependencies should be
/// resolved against, which is not always the instance's own pair: a Fabric mod
/// installed through Connector needs Fabric dependencies, and a mod taken from
/// a neighbouring patch version wants that version's.
#[allow(clippy::too_many_arguments)]
pub async fn install_with_dependencies(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    version: &Version,
    project: &Project,
    dep_game_version: &str,
    dep_loader: &str,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<InstallReport> {
    let kind = project.kind.install_kind().ok_or_else(|| {
        Error::msg(format!(
            "{} is a modpack — it creates its own instance rather than \
             installing into one",
            project.name
        ))
    })?;
    let dir = crate::instance::content::content_dir(paths, &instance.id, kind);
    install_with_dependencies_into(
        client,
        &dir,
        &mut instance.content,
        version,
        project,
        kind,
        dep_game_version,
        dep_loader,
        curseforge_key,
        progress,
    )
    .await
}

/// The same, into a folder and a record list rather than an instance.
///
/// A server keeps identical provenance records and wants identical behaviour —
/// installing a mod that needs three others should bring the three — so it runs
/// the same walk rather than a second copy of it. The caller says which folder
/// and which kind, because a server decides that differently: the same project
/// is a mod in one folder and a plugin in another.
#[allow(clippy::too_many_arguments)] // one install's coordinates, travelling together
pub async fn install_with_dependencies_into(
    client: &reqwest::Client,
    dir: &Path,
    records: &mut Vec<ContentRecord>,
    version: &Version,
    project: &Project,
    kind: ContentKind,
    dep_game_version: &str,
    dep_loader: &str,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<InstallReport> {
    let mut report = InstallReport {
        installed: Vec::new(),
        skipped: Vec::new(),
        kind,
    };

    progress.stage("Installing", 1).await;
    let file = install_into(
        client,
        dir,
        records,
        version,
        kind,
        &project.name,
        project.icon_url.clone(),
        Some(project.slug.clone()),
        false,
        project.listed_side(),
        blocked::Fallback::live(progress),
    )
    .await?;
    report.installed.push(file);
    progress.advance(1, &project.name).await;

    // Only things that load other things have dependencies worth chasing. A
    // plugin's are plugins, and they belong in the same folder as their parent,
    // which is why the kind travels down rather than being assumed to be a mod.
    if !matches!(
        kind,
        ContentKind::Mod | ContentKind::ClientMod | ContentKind::Plugin
    ) {
        return Ok(report);
    }

    let mut seen = installed_projects(records, version.provider.as_str(), kind);

    let mut queue: Vec<String> = version
        .dependencies
        .iter()
        .filter(|d| d.kind == DependencyKind::Required)
        .filter_map(|d| d.project_id.clone())
        .collect();

    // Bounded so a cyclic or pathological dependency graph cannot spin forever.
    let mut budget = 32;
    while let Some(project_id) = queue.pop() {
        if budget == 0 {
            report
                .skipped
                .push("dependency chain too deep — stopped resolving".into());
            break;
        }
        budget -= 1;

        if !seen.insert(project_id.clone()) {
            continue;
        }
        progress.check_cancelled()?;

        match resolve_dependency(
            client,
            dir,
            records,
            version.provider,
            &project_id,
            kind,
            dep_game_version,
            dep_loader,
            curseforge_key,
            blocked::Fallback::live(progress),
        )
        .await
        {
            Ok(Some((filename, dep_version))) => {
                progress.advance(1, &filename).await;
                report.installed.push(filename);
                queue.extend(
                    dep_version
                        .dependencies
                        .iter()
                        .filter(|d| d.kind == DependencyKind::Required)
                        .filter_map(|d| d.project_id.clone()),
                );
            }
            Ok(None) => report
                .skipped
                .push(format!("no compatible version of dependency {project_id}")),
            Err(e) => report.skipped.push(format!("dependency {project_id}: {e}")),
        }
    }

    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn resolve_dependency(
    client: &reqwest::Client,
    dir: &Path,
    records: &mut Vec<ContentRecord>,
    provider: Provider,
    project_id: &str,
    kind: ContentKind,
    game_version: &str,
    loader: &str,
    curseforge_key: Option<&str>,
    fallback: blocked::Fallback<'_>,
) -> Result<Option<(String, Version)>> {
    let project = providers::project(client, provider, project_id, curseforge_key).await?;
    let versions = providers::versions(
        client,
        provider,
        project_id,
        Some(game_version),
        Some(loader),
        curseforge_key,
    )
    .await?;

    let Some(best) = pick_best(&versions) else {
        return Ok(None);
    };

    let filename = install_into(
        client,
        dir,
        records,
        best,
        kind,
        &project.name,
        project.icon_url.clone(),
        Some(project.slug.clone()),
        true,
        project.listed_side(),
        fallback,
    )
    .await?;
    Ok(Some((filename, best.clone())))
}

/// The projects of one provider already installed, as far as a dependency walk
/// into `kind` is concerned.
///
/// A library a server has only in Client mods is not installed for the server:
/// the server never loads that folder, so a mod needing the library would not
/// start. So a walk into Mods looks past client mods, while a walk into Client
/// mods counts both folders — a library the server loads is one its players
/// get with the pack anyway.
fn installed_projects(
    records: &[ContentRecord],
    provider: &str,
    kind: ContentKind,
) -> HashSet<String> {
    records
        .iter()
        .filter(|r| r.provider == provider)
        .filter(|r| !(kind == ContentKind::Mod && r.kind == ContentKind::ClientMod))
        .map(|r| r.project_id.clone())
        .collect()
}

/// Prefer a stable release; fall back to the newest prerelease when that is all
/// the author has published for this game version.
pub fn pick_best(versions: &[Version]) -> Option<&Version> {
    versions
        .iter()
        .find(|v| v.release_type == "release" && v.primary_file().is_some())
        .or_else(|| versions.iter().find(|v| v.primary_file().is_some()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCandidate {
    pub file_name: String,
    pub name: String,
    pub provider: String,
    pub project_id: String,
    pub current_version: String,
    pub new_version_id: String,
    pub new_version_number: String,
    pub kind: ContentKind,
}

/// Newer versions of whatever is installed.
///
/// Takes the records and the target rather than an instance: a server keeps
/// exactly the same provenance and asks exactly the same question of it, and
/// there is no sense in two copies of this.
pub async fn check_updates(
    client: &reqwest::Client,
    records: &[ContentRecord],
    game_version: &str,
    mod_loader: Option<&str>,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<UpdateCandidate>> {
    // A server's mods update when the server's do, never from here — offering
    // one would only lead to a refusal, or "Update all" stopping at it.
    let tracked: Vec<ContentRecord> = records
        .iter()
        .filter(|r| r.origin != crate::instance::content::Origin::Server)
        .cloned()
        .collect();
    progress
        .stage("Checking for updates", tracked.len() as u64)
        .await;

    let mut out = Vec::new();
    for record in tracked {
        progress.check_cancelled()?;
        progress.advance(1, &record.name).await;

        let Some(provider) = Provider::parse(&record.provider) else {
            continue;
        };
        if provider == Provider::CurseForge && curseforge_key.is_none() {
            continue;
        }

        // Resource packs, shaders and datapacks are not loader-specific, and
        // neither is a plugin: its platform is not a mod loader, so asking for
        // one would match nothing. A client mod is built for the same loader
        // as the server's mods.
        let loader = matches!(record.kind, ContentKind::Mod | ContentKind::ClientMod)
            .then_some(mod_loader)
            .flatten();
        let versions = match providers::versions(
            client,
            provider,
            &record.project_id,
            Some(game_version),
            loader,
            curseforge_key,
        )
        .await
        {
            Ok(v) => v,
            // One dead project should not abort the whole check.
            Err(_) => continue,
        };

        let Some(best) = pick_best(&versions) else {
            continue;
        };
        if best.id == record.version_id {
            continue;
        }

        out.push(UpdateCandidate {
            file_name: record.file_name.clone(),
            name: record.name.clone(),
            provider: record.provider.clone(),
            project_id: record.project_id.clone(),
            current_version: record.version_number.clone(),
            new_version_id: best.id.clone(),
            new_version_number: best.version_number.clone(),
            kind: record.kind,
        });
    }

    progress.finish().await;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Scratch;
    use crate::providers::VersionFile;

    fn version(id: &str, release_type: &str, has_file: bool) -> Version {
        Version {
            provider: Provider::Modrinth,
            id: id.into(),
            project_id: "p".into(),
            name: id.into(),
            version_number: id.into(),
            game_versions: vec![],
            loaders: vec![],
            release_type: release_type.into(),
            published: None,
            downloads: 0,
            files: if has_file {
                vec![VersionFile {
                    url: "https://example.invalid/a.jar".into(),
                    filename: "a.jar".into(),
                    sha1: None,
                    sha512: None,
                    size: None,
                    primary: true,
                }]
            } else {
                vec![]
            },
            dependencies: vec![],
            blocked_reason: None,
            blocked_sha1: None,
        }
    }

    fn server_record(file_name: &str, project_id: &str) -> ContentRecord {
        ContentRecord {
            file_name: file_name.into(),
            kind: ContentKind::Mod,
            provider: "modrinth".into(),
            project_id: project_id.into(),
            version_id: "old".into(),
            name: "Server Anticheat".into(),
            version_number: "1.0".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: crate::instance::content::Origin::Server,
            requires: None,
            side: None,
            server_off: false,
        }
    }

    /// Updating a server's mod was a way around the rule that it cannot be
    /// removed: the update deleted the server's file — even a disabled copy —
    /// and wrote the new record as the player's own, one click from deletable.
    /// It is refused before anything is downloaded or deleted.
    #[tokio::test]
    async fn a_server_mod_cannot_be_replaced_by_an_install_or_update() {
        let scratch = Scratch::new("srv-update");
        let dir = scratch.paths.root.clone();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("anticheat-1.0.jar.disabled"), b"x").await.unwrap();

        // A newer version of the same project: an update.
        let mut update = version("2.0", "release", true);
        update.project_id = "AC".into();
        let mut records = vec![server_record("anticheat-1.0.jar", "AC")];

        let err = install_into(
            &reqwest::Client::new(),
            &dir,
            &mut records,
            &update,
            ContentKind::Mod,
            "Server Anticheat",
            None,
            None,
            false,
            None,
            blocked::Fallback::live(&Progress::silent("t")),
        )
        .await
        .expect_err("an update must not replace the server's mod");
        assert!(err.to_string().contains("server decides"), "{err}");
        assert!(dir.join("anticheat-1.0.jar.disabled").is_file(), "the server's file was deleted");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].origin, crate::instance::content::Origin::Server);

        // A different project that happens to ship the same file name would
        // overwrite it in place, which is the same replacement by another road.
        let mut clash = version("x", "release", true);
        clash.project_id = "SOMETHING-ELSE".into();
        clash.files[0].filename = "anticheat-1.0.jar".into();
        install_into(
            &reqwest::Client::new(),
            &dir,
            &mut records,
            &clash,
            ContentKind::Mod,
            "Imposter",
            None,
            None,
            false,
            None,
            blocked::Fallback::live(&Progress::silent("t")),
        )
        .await
        .expect_err("a same-named file must not overwrite the server's");
    }

    /// The bytes every stub below serves, and the sha1 CurseForge publishes
    /// about its own copy of them. Real, because the download checks it: a
    /// made-up hash would fail the fetch rather than the thing under test.
    const JAR: &[u8] = b"JAR";
    const JAR_SHA1: &str = "a7a941dea93a9fd0d1a0a8980df11d40b2cf576c";

    /// A CurseForge file whose author switched third-party downloads off: no
    /// `VersionFile` at all, because there is no URL to put in one, and the
    /// sha1 CurseForge published for it instead.
    fn a_blocked_curseforge_file() -> Version {
        Version {
            provider: Provider::CurseForge,
            project_id: "CF-5001".into(),
            blocked_reason: Some(
                "The author has disabled third-party downloads for this file.".into(),
            ),
            blocked_sha1: Some(JAR_SHA1.into()),
            ..version("6001", "release", false)
        }
    }

    /// A Modrinth that answers the hash lookup with the author's own release of
    /// those bytes, and serves the jar it names.
    ///
    /// The matching file is listed second and not primary, so an install that
    /// took the version's primary file would write a sources jar instead of the
    /// bytes CurseForge named. Returned in the `.../v2` shape the real `API`
    /// constant has.
    async fn modrinth_holding(filename: &str) -> String {
        use http_body_util::Full;
        use hyper::body::{Bytes, Incoming};
        use hyper::service::service_fn;
        use hyper::{Request, Response};
        use hyper_util::rt::TokioIo;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v2", listener.local_addr().unwrap());
        let answer = format!(
            r#"{{"{JAR_SHA1}":{{"id":"MRVER","project_id":"MRPROJ","name":"Cool Mod on Modrinth",
               "version_number":"1.2.3","version_type":"release","game_versions":["1.21.1"],
               "loaders":["fabric"],"downloads":9,"files":[
                 {{"url":"{base}/sources.jar","filename":"sources.jar",
                   "hashes":{{"sha1":"0000000000000000000000000000000000000000"}},
                   "size":1,"primary":true}},
                 {{"url":"{base}/{filename}","filename":"{filename}",
                   "hashes":{{"sha1":"{JAR_SHA1}"}},"size":3,"primary":false}}
               ]}}}}"#
        );

        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let answer = answer.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |r: Request<Incoming>| {
                        let answer = answer.clone();
                        async move {
                            let body = if r.uri().path().ends_with("/version_files") {
                                Bytes::from(answer)
                            } else {
                                Bytes::from_static(JAR)
                            };
                            Ok::<_, std::convert::Infallible>(Response::new(Full::new(body)))
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        base
    }

    /// The ordinary install's end of the rule, which a pack import and a shared
    /// pack's reference have their own tests for: a file CurseForge will not
    /// serve arrives from its author's own Modrinth release, under Modrinth's
    /// name for it, recorded as having come from there — because that is where
    /// the bytes came from.
    #[tokio::test]
    async fn an_ordinary_install_takes_a_blocked_file_from_the_authors_own_release() {
        let scratch = Scratch::new("blocked-install");
        let dir = scratch.paths.root.join("mods");
        let base = modrinth_holding("mr-cool-1.2.3.jar").await;
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let progress = Progress::new(recorder.clone(), "t");

        let mut records = Vec::new();
        let landed = install_into(
            &crate::net::client(),
            &dir,
            &mut records,
            &a_blocked_curseforge_file(),
            ContentKind::Mod,
            "Cool Mod",
            None,
            None,
            false,
            None,
            blocked::Fallback::at(&base, &progress),
        )
        .await
        .expect("the author published the same bytes on Modrinth themselves");

        assert_eq!(landed, "mr-cool-1.2.3.jar");
        assert_eq!(tokio::fs::read(dir.join(&landed)).await.unwrap(), JAR);
        assert!(
            !dir.join("sources.jar").exists(),
            "the version's primary file was installed instead of the one whose hash matched"
        );

        // Modrinth, because that is where the bytes came from. Nobody has to
        // remember to write it: every field is read off the version installed.
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].provider, "modrinth");
        assert_eq!(records[0].project_id, "MRPROJ");
        assert_eq!(records[0].version_id, "MRVER");

        // And the sentence that explains the swap left through the handle this
        // caller passed in.
        assert_eq!(recorder.topics(), vec!["geode://notice"]);
    }

    /// A user who already had this mod from CurseForge, updating to a build
    /// whose author has since switched third-party downloads off. The jar that
    /// replaces it is Modrinth's, so the record written names Modrinth — and
    /// the copy being replaced was recorded from CurseForge, which is the one
    /// name the lookup for "the previous file for this project" would miss.
    /// Missing it leaves both jars in the folder, and two builds of one mod is
    /// a game that often will not start.
    #[tokio::test]
    async fn a_substitution_still_replaces_the_copy_the_user_already_had() {
        let scratch = Scratch::new("blocked-replaces");
        let dir = scratch.paths.root.join("mods");
        let base = modrinth_holding("mr-cool-1.2.3.jar").await;
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let progress = Progress::new(recorder.clone(), "t");

        // What the user has now: the older build, fetched from CurseForge back
        // when its author still allowed it.
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("cool-1.2.2.jar"), b"the older build")
            .await
            .unwrap();
        let mut had = server_record("cool-1.2.2.jar", "CF-5001");
        had.provider = "curseforge".into();
        had.origin = crate::instance::content::Origin::User;
        let mut records = vec![had];

        let landed = install_into(
            &crate::net::client(),
            &dir,
            &mut records,
            &a_blocked_curseforge_file(),
            ContentKind::Mod,
            "Cool Mod",
            None,
            None,
            false,
            None,
            blocked::Fallback::at(&base, &progress),
        )
        .await
        .expect("the author published the same bytes on Modrinth themselves");

        assert_eq!(landed, "mr-cool-1.2.3.jar");
        assert!(
            !dir.join("cool-1.2.2.jar").exists(),
            "the build being replaced is still in the folder beside its replacement"
        );
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].file_name, "mr-cool-1.2.3.jar");
    }

    /// The server's mod stays the server's through a substitution, which names
    /// the same mod twice over: Modrinth's ids and Modrinth's file name for a
    /// mod the server's pack recorded from CurseForge, or the other way round
    /// once the server's own pack has been substituted too. A rule that knew
    /// only one of those names let a second copy of the server's mod in beside
    /// it, and a player on two builds of one mod cannot join.
    #[tokio::test]
    async fn a_servers_mod_is_refused_whichever_name_a_substitution_knows_it_by() {
        let scratch = Scratch::new("srv-blocked");
        let dir = scratch.paths.root.clone();

        // The pack resolved this mod while it could still be fetched from
        // CurseForge, so that is the provenance on the server's record.
        let mut from_curseforge = server_record("cool-1.2.2.jar", "CF-5001");
        from_curseforge.provider = "curseforge".into();
        let mut records = vec![from_curseforge];

        // Nothing here can reach Modrinth, so a refusal phrased as the
        // server's rule can only have been decided before Modrinth was asked —
        // which is the point: the user is not told the file came from the
        // author's Modrinth release and then refused it.
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let progress = Progress::new(recorder.clone(), "t");
        let offline = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap();
        let err = install_into(
            &offline,
            &dir,
            &mut records,
            &a_blocked_curseforge_file(),
            ContentKind::Mod,
            "Cool Mod",
            None,
            None,
            false,
            None,
            blocked::Fallback::at("http://127.0.0.1:9/v2", &progress),
        )
        .await
        .expect_err("the server decides which version of its own mod it runs");
        assert!(err.to_string().contains("server decides"), "{err}");
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(
            recorder.events().is_empty(),
            "a swap was announced for a file that was then refused: {:?}",
            recorder.events()
        );

        // The other way round: the server's own pack met the same blocked file
        // and recorded the author's Modrinth release, so the ids the user asked
        // for match nothing and only the substituted ones do.
        let base = modrinth_holding("mr-cool-1.2.3.jar").await;
        let mut from_modrinth = server_record("mr-cool-1.2.3.jar", "MRPROJ");
        from_modrinth.provider = "modrinth".into();
        let mut records = vec![from_modrinth];
        let err = install_into(
            &crate::net::client(),
            &dir,
            &mut records,
            &a_blocked_curseforge_file(),
            ContentKind::Mod,
            "Cool Mod",
            None,
            None,
            false,
            None,
            blocked::Fallback::at(&base, &Progress::silent("t")),
        )
        .await
        .expect_err("the server's own mod, under the name its pack recorded");
        assert!(err.to_string().contains("server decides"), "{err}");
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(
            !dir.join("mr-cool-1.2.3.jar").exists(),
            "the server's file was overwritten by the substitute"
        );
    }

    /// The listing's side is kept on the record at install time, because it is
    /// the only word there is for a jar that declares nothing, and nothing
    /// asks the provider again later.
    #[tokio::test]
    async fn install_into_records_the_side_it_is_given() {
        use crate::instance::jar::Side;

        let scratch = Scratch::new("install-side");
        let dir = scratch.paths.root.clone();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // Already on disk with the right hash, so nothing is downloaded, and
        // the client is pointed at a port nothing listens on in case it is.
        tokio::fs::write(dir.join("a.jar"), b"zoom").await.unwrap();
        let mut zoom = version("1.0", "release", true);
        zoom.files[0].sha1 = Some(crate::net::sha1_file(&dir.join("a.jar")).await.unwrap());
        let offline = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap();

        let mut records = Vec::new();
        install_into(
            &offline,
            &dir,
            &mut records,
            &zoom,
            ContentKind::Mod,
            "Zoom",
            None,
            None,
            false,
            Some(Side::Client),
            blocked::Fallback::live(&Progress::silent("t")),
        )
        .await
        .unwrap();

        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].side, Some(Side::Client));
    }

    /// A walk into Mods installs a library the server has only in Client mods,
    /// since the server never loads that folder. A walk into Client mods
    /// counts both folders, since players get the server's mods anyway.
    #[test]
    fn a_library_only_in_client_mods_is_still_installed_for_the_server() {
        let mut in_mods = server_record("api.jar", "API");
        in_mods.origin = Default::default();
        let in_client_mods = ContentRecord {
            kind: ContentKind::ClientMod,
            ..server_record("cloth.jar", "CLOTH")
        };
        let elsewhere = ContentRecord {
            provider: "curseforge".into(),
            ..server_record("other.jar", "OTHER")
        };
        let records = vec![in_mods, in_client_mods, elsewhere];

        let sorted = |set: HashSet<String>| {
            let mut ids: Vec<String> = set.into_iter().collect();
            ids.sort();
            ids
        };
        assert_eq!(
            sorted(installed_projects(&records, "modrinth", ContentKind::Mod)),
            vec!["API"]
        );
        assert_eq!(
            sorted(installed_projects(
                &records,
                "modrinth",
                ContentKind::ClientMod
            )),
            vec!["API", "CLOTH"]
        );
    }

    /// Not offered either. "Update all" would otherwise stop at the first
    /// server mod it reached.
    ///
    /// Asserted on what the check sets out to look at, not on its result: a
    /// lookup that fails is skipped rather than reported, so "found nothing"
    /// would pass whether or not the server's mod had been excluded.
    #[tokio::test]
    async fn a_server_mod_is_never_offered_as_an_update() {
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let progress = Progress::new(recorder.clone(), "t");
        let records = vec![server_record("anticheat-1.0.jar", "AC")];

        let found = check_updates(
            &reqwest::Client::new(),
            &records,
            "1.21.1",
            Some("fabric"),
            None,
            &progress,
        )
        .await
        .unwrap();
        assert!(found.is_empty(), "{found:?}");

        let planned = recorder
            .events()
            .into_iter()
            .find(|(_, e)| e["stage"] == "Checking for updates")
            .map(|(_, e)| e["total"].as_u64())
            .expect("the check reports how much it will look at");
        assert_eq!(planned, Some(0), "the server's mod was queued for an update check");
    }

    #[test]
    fn parses_ids_from_a_modrinth_cdn_url() {
        let url = "https://cdn.modrinth.com/data/AANobbMI/versions/S3DUMfBo/sodium.jar";
        assert_eq!(
            ids_from_modrinth_url(url),
            Some(("AANobbMI".into(), "S3DUMfBo".into()))
        );
    }

    #[test]
    fn ignores_urls_from_elsewhere() {
        assert!(ids_from_modrinth_url("https://edge.forgecdn.net/files/1/2/x.jar").is_none());
        assert!(ids_from_modrinth_url("https://cdn.modrinth.com/data/OnlyProject").is_none());
        assert!(ids_from_modrinth_url("not a url").is_none());
    }

    #[test]
    fn prefers_stable_release() {
        let versions = vec![
            version("beta-2", "beta", true),
            version("release-1", "release", true),
        ];
        assert_eq!(pick_best(&versions).unwrap().id, "release-1");
    }

    #[test]
    fn falls_back_to_prerelease() {
        let versions = vec![version("beta-2", "beta", true)];
        assert_eq!(pick_best(&versions).unwrap().id, "beta-2");
    }

    #[test]
    fn skips_versions_without_files() {
        let versions = vec![
            version("blocked", "release", false),
            version("ok", "beta", true),
        ];
        assert_eq!(pick_best(&versions).unwrap().id, "ok");
    }

    #[test]
    fn none_when_nothing_installable() {
        let versions = vec![version("blocked", "release", false)];
        assert!(pick_best(&versions).is_none());
    }
}

// ---------------------------------------------------------------------------
// Linking files back to their source
// ---------------------------------------------------------------------------

/// A Modrinth CDN download URL encodes the ids we want:
/// `https://cdn.modrinth.com/data/<PROJECT>/versions/<VERSION>/<file>`
///
/// A `.mrpack` index lists only URLs and hashes, so without this every mod a
/// pack installs would arrive with no provenance — unnamed, untracked and
/// invisible to update checks.
pub fn ids_from_modrinth_url(url: &str) -> Option<(String, String)> {
    let rest = url.split_once("cdn.modrinth.com/data/").map(|(_, r)| r)?;
    let mut parts = rest.split('/');
    let project = parts.next()?;
    if parts.next()? != "versions" {
        return None;
    }
    let version = parts.next()?;
    if project.is_empty() || version.is_empty() {
        return None;
    }
    Some((project.to_string(), version.to_string()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkReport {
    pub identified: usize,
    pub unknown: usize,
}

/// Give provenance to files that have none.
///
/// Works from the file bytes: SHA-1 each untracked file and ask Modrinth which
/// version it is. That covers hand-dropped jars and anything a pack installed
/// by URL alone, and it is exact — a hash match is the same file, not a guess
/// from the filename.
///
/// Modrinth only. CurseForge's equivalent takes a bespoke murmur2 fingerprint
/// rather than a standard hash, so files that exist only there stay untracked.
pub async fn link_untracked(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    jars: &crate::instance::jar::MetaCache,
    progress: &Progress,
) -> Result<LinkReport> {
    use crate::instance::content::{self as content, ContentKind};

    let folders: Vec<(ContentKind, std::path::PathBuf)> = [
        ContentKind::Mod,
        ContentKind::ResourcePack,
        ContentKind::ShaderPack,
        ContentKind::DataPack,
    ]
    .into_iter()
    .map(|kind| (kind, content::content_dir(paths, &instance.id, kind)))
    .collect();

    link_untracked_in(client, &folders, &mut instance.content, jars, progress).await
}

/// The same, over whatever folders the caller names.
///
/// A server's are not an instance's: it holds plugins, it has no shaders at
/// all, and its datapacks live inside its world rather than beside it. So the
/// folders are given rather than assumed from an id.
pub async fn link_untracked_in(
    client: &reqwest::Client,
    folders: &[(crate::instance::content::ContentKind, std::path::PathBuf)],
    records: &mut Vec<ContentRecord>,
    jars: &crate::instance::jar::MetaCache,
    progress: &Progress,
) -> Result<LinkReport> {
    use crate::instance::content::{self as content, ContentKind};

    // Collect the untracked files first so the hash lookup is one request.
    let mut pending: Vec<(ContentKind, String, String)> = Vec::new();
    for (kind, dir) in folders {
        let items = content::list_in(dir, records, *kind, jars).await?;
        for item in items.into_iter().filter(|i| i.from_disk) {
            let path = dir.join(&item.file_name);
            if let Ok(sha1) = crate::net::sha1_file(&path).await {
                pending.push((*kind, item.file_name, sha1.to_lowercase()));
            }
        }
    }

    if pending.is_empty() {
        progress.finish().await;
        return Ok(LinkReport {
            identified: 0,
            unknown: 0,
        });
    }

    progress
        .stage("Identifying files", pending.len() as u64)
        .await;

    let hashes: Vec<String> = pending.iter().map(|(_, _, h)| h.clone()).collect();
    // Only the hits: here a file nobody answered about and a file Modrinth
    // does not have come to the same thing, which is that it stays untracked
    // until the next time this runs.
    let found = crate::providers::modrinth::files_by_hashes(
        client,
        &hashes,
        crate::providers::modrinth::HashAlgorithm::Sha1,
    )
    .await?
    .found;

    // One more call turns ids into names and icons.
    let project_ids: Vec<String> = {
        let mut ids: Vec<String> = found.values().map(|(v, _)| v.project_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    };
    let projects = crate::providers::modrinth::projects_by_ids(client, &project_ids)
        .await
        .unwrap_or_default();

    let mut identified = 0;
    for (kind, file_name, sha1) in pending {
        progress.advance(1, &file_name).await;
        let Some((version, _)) = found.get(&sha1) else {
            continue;
        };
        let project = projects.iter().find(|p| p.id == version.project_id);

        // Records key off the plain name, so toggling does not orphan them.
        let base = content::strip_disabled(&file_name).0.to_string();

        records.retain(|r| !(r.file_name == base && r.kind == kind));
        records.push(ContentRecord {
            file_name: base,
            kind,
            provider: Provider::Modrinth.as_str().to_string(),
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            name: project
                .map(|p| p.name.clone())
                .unwrap_or_else(|| version.name.clone()),
            version_number: version.version_number.clone(),
            slug: project.map(|p| p.slug.clone()),
            icon_url: project.and_then(|p| p.icon_url.clone()),
            dependency: false,
            origin: Default::default(),
            // A hash lookup gives provenance, not a dependency list.
            requires: None,
            side: project.and_then(|p| p.listed_side()),
            server_off: false,
        });
        identified += 1;
    }

    progress.finish().await;
    Ok(LinkReport {
        identified,
        unknown: hashes.len() - identified,
    })
}

// ---------------------------------------------------------------------------
// Naming what a pack brought
// ---------------------------------------------------------------------------

/// Give the files an import just brought in a name, where the pack itself gave
/// none.
///
/// Every format says where a mod came from in a way that needs no hash: a
/// CurseForge manifest names ids, a packwiz index names a project, an
/// `.mrpack` names a URL with the ids inside it. An import that read one has
/// already written those records. This is for everything it could not — a jar
/// out of an `overrides/` tree, an index entry served from a host Geode can
/// read no ids out of (github and gitlab are both legal there, and Prism's own
/// export emits them), a file in a folder the index reader passes over, a mod
/// that arrived in a server's pack as bytes. Without this they sit in the mods
/// list as untracked rows with a jar name for a name and no update path, until
/// somebody presses Identify files by hand.
///
/// Best-effort from end to end, and deliberately: a pack is perfectly usable
/// with imperfect labels, so a lookup that could not be made leaves the labels
/// as they were rather than failing an import that has already finished.
///
/// What this never does is overwrite provenance. A record that already names a
/// release — another platform's, or another version of this one — is somebody
/// else's answer about where the file came from, and a hash lookup is not here
/// to overrule it; `origin`, `server_off` and a side the server stated survive
/// untouched, which is what makes this safe to run on a joined instance.
/// [`link_untracked_in`] cannot stand in for it for exactly those two reasons:
/// it only looks at files with no record at all — and a jar that arrived in a
/// server's pack *has* a record, the provenance-less one `settle_server_files`
/// writes it — and it would then write `origin: User` and a fresh side over
/// what the server said.
pub async fn name_what_arrived(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    progress: &Progress,
) {
    use crate::install::identity::{candidates_in, identify, Ask, INSTANCE_KINDS};

    let game_dir = paths.game_dir(&instance.id);
    let Ok(candidates) = candidates_in(&game_dir, &INSTANCE_KINDS).await else {
        return;
    };
    // Only the files something is still missing about, which is the whole
    // reason this builds its own candidate list rather than taking the one an
    // export takes. Listing a folder is free; reading every jar in it is not,
    // and an update from a server runs on the way to Play. After the first
    // time, an instance whose records are all filled in has nothing here to
    // hash and this costs a `read_dir`.
    let worth: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| {
            match instance
                .content
                .iter()
                .find(|r| r.kind == candidate.kind && r.file_name == candidate.file_name)
            {
                Some(record) => has_blanks(record),
                None => true,
            }
        })
        .collect();
    if worth.is_empty() {
        return;
    }

    let records: Vec<&ContentRecord> = instance.content.iter().collect();
    let Ok(catalogue) = identify(
        &game_dir,
        worth,
        &records,
        Ask::naming(client),
        progress,
    )
    .await
    else {
        return;
    };
    name_from_catalogue(client, instance, &catalogue).await;
}

/// The same, from a catalogue already built.
///
/// The seam: everything above this line is one hash-and-lookup pass that lives
/// in [`crate::install::identity`] and is shared with every pack writer, and
/// everything below it is what an import does with the answer. Which also
/// makes the rules below readable against a catalogue a test built by hand,
/// without a request in the way.
async fn name_from_catalogue(
    client: &reqwest::Client,
    instance: &mut Instance,
    catalogue: &crate::install::identity::Catalogue,
) {
    use crate::install::identity::PlatformRef;

    let mut pending: Vec<(ContentKind, String, PlatformRef)> = Vec::new();
    for ident in catalogue.identities() {
        let Some(reference) = ident.reference(Provider::Modrinth.as_str()) else {
            continue;
        };
        let held = instance
            .content
            .iter()
            .find(|r| r.kind == ident.kind && r.file_name == ident.file_name);
        if held.is_some_and(|record| !has_blanks(record) || !may_fill(record, reference)) {
            continue;
        }
        pending.push((ident.kind, ident.file_name.clone(), reference.clone()));
    }
    if pending.is_empty() {
        return;
    }

    // One batched call turns ids into names, slugs and icons — the same one
    // every other import makes, for the same reason.
    let ids: Vec<String> = {
        let mut ids: Vec<String> = pending.iter().map(|(_, _, r)| r.project_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    };
    let projects = providers::modrinth::projects_by_ids(client, &ids)
        .await
        .unwrap_or_default();

    for (kind, file_name, reference) in pending {
        let project = projects.iter().find(|p| p.id == reference.project_id);
        match instance
            .content
            .iter_mut()
            .find(|r| r.kind == kind && r.file_name == file_name)
        {
            Some(record) => fill_blanks(record, &reference, project),
            None => instance.content.push(ContentRecord {
                name: project
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| file_name.clone()),
                file_name,
                kind,
                provider: reference.provider.clone(),
                project_id: reference.project_id.clone(),
                version_id: reference.version_id.clone(),
                version_number: reference.version_number.clone(),
                slug: project.map(|p| p.slug.clone()),
                icon_url: project.and_then(|p| p.icon_url.clone()),
                dependency: false,
                origin: Default::default(),
                // A hash lookup gives provenance, not a dependency list.
                requires: None,
                side: project.and_then(|p| p.listed_side()),
                server_off: false,
            }),
        }
    }
}

/// Whether anything about this file is still unsaid.
///
/// A record with every field filled is what an ordinary install leaves behind,
/// and there is nothing a lookup could add to it — so this is both what decides
/// whether a file is worth hashing and what decides whether an answer is worth
/// reading. A name that is the file's own name counts as unsaid: see
/// [`fill_blanks`].
fn has_blanks(record: &ContentRecord) -> bool {
    // A record that names another platform's release has nothing here anybody
    // could fill: the only lookup is Modrinth's, and `may_fill` will not let
    // its answer touch such a record. Counting its unsaid fields — a side,
    // most of all, which a CurseForge import never writes — would have every
    // jar in a three-hundred-mod CurseForge pack re-hashed on that import and
    // on every update after it, to learn nothing each time.
    if !is_blank(record) && record.provider != Provider::Modrinth.as_str() {
        return false;
    }
    is_blank(record)
        || record.name.is_empty()
        || record.name == record.file_name
        || record.version_number.is_empty()
        || record.side.is_none()
}

/// Whether this answer is allowed to fill them.
///
/// Only when the record is blank, or when it names the very release the lookup
/// found. A record naming a release this is not — another platform's, or
/// another version of this one — is somebody's own account of where the file
/// came from, and filling fields in from a second account would mix the two.
fn may_fill(record: &ContentRecord, reference: &crate::install::identity::PlatformRef) -> bool {
    is_blank(record) || names(record, reference)
}

/// A record nobody has said anything about yet.
///
/// The three fields travel together — a provider with no ids names nothing —
/// so one missing makes the whole claim blank. This is what
/// `settle_server_files` writes for a jar that arrived in a server's pack with
/// no download to its name.
fn is_blank(record: &ContentRecord) -> bool {
    record.provider.is_empty() || record.project_id.is_empty() || record.version_id.is_empty()
}

/// Whether a record and a lookup are talking about the same release.
fn names(record: &ContentRecord, reference: &crate::install::identity::PlatformRef) -> bool {
    record.provider == reference.provider
        && record.project_id == reference.project_id
        && record.version_id == reference.version_id
}

/// Fill in what nobody had said, and nothing else.
///
/// `origin`, `server_off`, `dependency` and `requires` are not this function's
/// to touch: the first two are the server's word about a joined instance's
/// files and the others were settled when the file was installed. A side that
/// is already set stays — on a joined instance it is how the server classified
/// the file, which outranks a project listing.
fn fill_blanks(
    record: &mut ContentRecord,
    reference: &crate::install::identity::PlatformRef,
    project: Option<&Project>,
) {
    if is_blank(record) {
        record.provider = reference.provider.clone();
        record.project_id = reference.project_id.clone();
        record.version_id = reference.version_id.clone();
    }
    if record.version_number.is_empty() {
        record.version_number = reference.version_number.clone();
    }
    let Some(project) = project else {
        return;
    };
    // A name that is the file's own name is the placeholder the importers
    // write when they have nothing better — `settle_server_files` for a jar
    // that arrived as bytes, `record_mrpack_provenance` for a project it could
    // not look up. It is a blank with a jar name in it, not an answer.
    if record.name.is_empty() || record.name == record.file_name {
        record.name = project.name.clone();
    }
    if record.slug.is_none() {
        record.slug = Some(project.slug.clone());
    }
    if record.icon_url.is_none() {
        record.icon_url = project.icon_url.clone();
    }
    if record.side.is_none() {
        record.side = project.listed_side();
    }
}

#[cfg(test)]
mod naming_tests {
    use super::*;
    use crate::install::identity::{for_instance, Ask, Catalogue};
    use crate::instance::content::Origin;
    use crate::instance::jar::Side;
    use crate::meta::Loader;
    use crate::paths::Scratch;

    /// A client that cannot reach anything. Every test here names its files
    /// through a catalogue built by hand, so a client that fails is what
    /// proves no answer was fetched — a project lookup that cannot be made
    /// leaves the names as they were, which is the best-effort rule working.
    fn offline() -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap()
    }

    fn fixture(tag: &str) -> (Scratch, Instance) {
        let scratch = Scratch::new(&format!("naming-{tag}"));
        let mut inst = Instance::new("P".into(), "1.21.1".into(), Loader::Fabric, String::new());
        inst.id = tag.to_string();
        (scratch, inst)
    }

    /// What the files on disk are, with nobody asked about them — then told
    /// that Modrinth holds the named ones, through the same code a real answer
    /// goes through.
    async fn catalogue_claiming(
        paths: &Paths,
        inst: &Instance,
        held: &[&str],
    ) -> Catalogue {
        let mut catalogue = for_instance(Ask::Nobody, paths, inst, &Progress::silent("t"))
            .await
            .unwrap();
        catalogue.pretend_modrinth_holds(held);
        catalogue
    }

    /// A pack's `overrides/` tree is bytes with nothing said about them, so
    /// every jar in one landed as an untracked row with a file name for a name
    /// — and the only thing that ever fixed it was the player pressing
    /// Identify files by hand.
    #[tokio::test]
    async fn a_jar_that_arrived_in_overrides_gets_a_name() {
        let (scratch, mut inst) = fixture("overrides");
        let paths = &scratch.paths;
        let archive = paths.root.join("pack.mrpack");
        std::fs::create_dir_all(&paths.root).unwrap();
        write_pack(
            &archive,
            serde_json::json!([]),
            &[("overrides/mods/carried.jar", b"carried bytes")],
        );

        crate::instance::modpack::import_into(
            &offline(),
            paths,
            &mut inst,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(
            inst.content.is_empty(),
            "a pack says nothing about its overrides: {:?}",
            inst.content
        );

        let catalogue = catalogue_claiming(paths, &inst, &["carried.jar"]).await;
        name_from_catalogue(&offline(), &mut inst, &catalogue).await;

        let record = named(&inst, "carried.jar");
        assert_eq!(
            (
                record.provider.as_str(),
                record.project_id.as_str(),
                record.version_id.as_str()
            ),
            ("modrinth", "mr-project", "mr-version")
        );
    }

    /// Provenance used to depend on the shape of a download URL: the ids were
    /// read out of `cdn.modrinth.com/data/<p>/versions/<v>/`, and the format
    /// allows github, gitlab and raw.githubusercontent too — which Prism's own
    /// export emits. Those files arrived nameless. A hash does not care who
    /// served the bytes.
    #[tokio::test]
    async fn an_index_file_from_github_is_identified_by_hash() {
        let (scratch, mut inst) = fixture("github");
        let paths = &scratch.paths;
        let archive = paths.root.join("pack.mrpack");
        std::fs::create_dir_all(&paths.root).unwrap();
        write_pack(
            &archive,
            serde_json::json!([{
                "path": "mods/elsewhere.jar",
                "hashes": {},
                "fileSize": 9,
                "downloads": ["https://raw.githubusercontent.com/someone/repo/main/elsewhere.jar"],
            }]),
            &[],
        );

        // Already there at the size the index states, so nothing is fetched
        // and the URL only has to be read, not reached.
        let game = paths.game_dir(&inst.id);
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/elsewhere.jar"), b"from repo").unwrap();

        crate::instance::modpack::import_into(
            &offline(),
            paths,
            &mut inst,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(
            inst.content.is_empty(),
            "no ids can be read out of that URL: {:?}",
            inst.content
        );

        let catalogue = catalogue_claiming(paths, &inst, &["elsewhere.jar"]).await;
        name_from_catalogue(&offline(), &mut inst, &catalogue).await;

        let record = named(&inst, "elsewhere.jar");
        assert_eq!(record.project_id, "mr-project");
        assert_eq!(record.version_id, "mr-version");
    }

    /// Two blanks the importer left on every file a pack installed: no version
    /// at all, and no side although the pack's own `env` pair had said one.
    #[tokio::test]
    async fn an_imported_pack_knows_its_versions_and_sides() {
        let (scratch, mut inst) = fixture("blanks");
        let paths = &scratch.paths;
        let archive = paths.root.join("pack.mrpack");
        std::fs::create_dir_all(&paths.root).unwrap();
        write_pack(
            &archive,
            serde_json::json!([{
                "path": "mods/clientonly.jar",
                "hashes": {},
                "fileSize": 7,
                "env": { "client": "required", "server": "unsupported" },
                "downloads": [
                    "https://cdn.modrinth.com/data/mr-project/versions/mr-version/clientonly.jar"
                ],
            }]),
            &[],
        );

        let game = paths.game_dir(&inst.id);
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/clientonly.jar"), b"client!").unwrap();

        crate::instance::modpack::import_into(
            &offline(),
            paths,
            &mut inst,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        // The pack said this one is a client mod, and the pack is the only one
        // here who knows.
        assert_eq!(named(&inst, "clientonly.jar").side, Some(Side::Client));

        let catalogue = catalogue_claiming(paths, &inst, &["clientonly.jar"]).await;
        name_from_catalogue(&offline(), &mut inst, &catalogue).await;

        let record = named(&inst, "clientonly.jar");
        assert_eq!(
            record.version_number, "mr-1.0",
            "the matched version's own number"
        );
        assert_eq!(
            record.side,
            Some(Side::Client),
            "the pack's side was overwritten by a lookup's"
        );
    }

    /// The trap that makes `link_untracked_in` unsafe to reuse here. A jar
    /// that arrived in a server's pack as bytes has a record already — the
    /// provenance-less one `settle_server_files` writes it — saying whose it
    /// is, which side the server put it on and whether the server had it off.
    /// Naming it must fill the blanks and touch nothing else, or a player
    /// could delete the server's anti-cheat.
    #[tokio::test]
    async fn enriching_a_record_keeps_what_the_server_said() {
        let (scratch, mut inst) = fixture("joined");
        let paths = &scratch.paths;
        let game = paths.game_dir(&inst.id);
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/theirs.jar"), b"the server's").unwrap();

        inst.content.push(ContentRecord {
            file_name: "theirs.jar".into(),
            kind: ContentKind::Mod,
            provider: String::new(),
            project_id: String::new(),
            version_id: String::new(),
            name: "theirs.jar".into(),
            version_number: String::new(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Origin::Server,
            requires: None,
            side: Some(Side::Server),
            server_off: true,
        });

        let catalogue = catalogue_claiming(paths, &inst, &["theirs.jar"]).await;
        name_from_catalogue(&offline(), &mut inst, &catalogue).await;

        let record = named(&inst, "theirs.jar");
        assert_eq!(record.provider, "modrinth", "the blank was not filled");
        assert_eq!(record.project_id, "mr-project");
        assert_eq!(record.version_number, "mr-1.0");
        assert_eq!(record.origin, Origin::Server, "whose file this is");
        assert!(record.server_off, "the server had it off");
        assert_eq!(
            record.side,
            Some(Side::Server),
            "the server's own classification"
        );
    }

    /// The other half of the same rule, and the one that can corrupt an
    /// instance rather than merely fail to improve one: a record that already
    /// names a release is somebody's answer about where this file came from,
    /// and a hash lookup does not get to replace it.
    #[tokio::test]
    async fn a_record_that_names_another_release_is_left_alone() {
        let (scratch, mut inst) = fixture("stands");
        let paths = &scratch.paths;
        let game = paths.game_dir(&inst.id);
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/known.jar"), b"known bytes").unwrap();

        inst.content.push(ContentRecord {
            file_name: "known.jar".into(),
            kind: ContentKind::Mod,
            provider: "curseforge".into(),
            project_id: "238222".into(),
            version_id: "4712345".into(),
            name: "Just Enough Items".into(),
            version_number: String::new(),
            slug: None,
            icon_url: None,
            dependency: true,
            origin: Default::default(),
            requires: None,
            side: None,
            server_off: false,
        });

        let catalogue = catalogue_claiming(paths, &inst, &["known.jar"]).await;
        name_from_catalogue(&offline(), &mut inst, &catalogue).await;

        let record = named(&inst, "known.jar");
        assert_eq!(record.provider, "curseforge");
        assert_eq!(record.project_id, "238222");
        assert_eq!(record.version_id, "4712345");
        assert_eq!(record.name, "Just Enough Items");
        assert!(
            record.version_number.is_empty(),
            "a number from another platform's release was written in"
        );
        assert!(record.dependency, "how it was installed");
    }

    /// The hashing pass is the expensive half, and what makes it worth paying
    /// is that something could be filled in afterwards. A record naming
    /// another platform's release is one `may_fill` will not let a Modrinth
    /// answer touch, so counting its unsaid side as a blank would re-read
    /// every jar of an imported CurseForge pack on that import and on every
    /// update after it, to learn nothing each time.
    #[test]
    fn a_record_naming_another_platform_is_not_worth_hashing() {
        let mut curseforge = a_record("curseforge", "328085", "4593548");
        // What a CurseForge pack import writes, every time: the manifest named
        // the project and the file and said nothing about sides.
        curseforge.side = None;
        assert!(!has_blanks(&curseforge));

        // The two that are still worth it: a Modrinth record a lookup may fill
        // in, and a jar nobody has said anything about at all.
        let mut modrinth = a_record("modrinth", "AANobbMI", "Hu34AbCd");
        modrinth.side = None;
        assert!(has_blanks(&modrinth));
        let mut blank = curseforge.clone();
        blank.project_id = String::new();
        assert!(has_blanks(&blank));
    }

    /// The half of [`fill_blanks`] a project listing fills. Every other test
    /// here reaches it through a lookup that cannot be made, so the listing is
    /// always `None` and this half is never run — the guards are handed the
    /// answer directly instead.
    #[test]
    fn a_project_listing_fills_only_what_nobody_had_said() {
        let reference = crate::install::identity::PlatformRef {
            provider: "modrinth".into(),
            project_id: "AANobbMI".into(),
            version_id: "Hu34AbCd".into(),
            version_number: "mr-1.0".into(),
            url: None,
            distributable: true,
        };
        // Listed as a client mod, which is what a listing has to say for the
        // server's own answer to be worth keeping over it.
        let project: Project = serde_json::from_value(serde_json::json!({
            "provider": "modrinth", "id": "AANobbMI", "slug": "sodium",
            "name": "Sodium", "summary": "", "downloads": 0, "follows": 0,
            "author": "", "kind": "mod", "pageUrl": "",
            "iconUrl": "https://cdn.modrinth.com/sodium.png",
            "clientSide": "required", "serverSide": "unsupported",
        }))
        .unwrap();

        // A jar that arrived in a server's pack as bytes: a file name for a
        // name, and the side the server itself put it on.
        let mut theirs = a_record("", "", "");
        theirs.name = theirs.file_name.clone();
        theirs.slug = None;
        theirs.icon_url = None;
        theirs.side = Some(Side::Server);
        fill_blanks(&mut theirs, &reference, Some(&project));
        assert_eq!(theirs.name, "Sodium", "a jar name is a blank, not an answer");
        assert_eq!(theirs.slug.as_deref(), Some("sodium"));
        assert_eq!(
            theirs.icon_url.as_deref(),
            Some("https://cdn.modrinth.com/sodium.png")
        );
        assert_eq!(
            theirs.side,
            Some(Side::Server),
            "the server classified this file and outranks a listing"
        );

        // The same release, installed here, with its own answers already.
        let mut ours = a_record("modrinth", "AANobbMI", "Hu34AbCd");
        ours.name = "Sodium (renamed)".into();
        ours.slug = Some("sodium-fork".into());
        ours.icon_url = Some("file:///mine.png".into());
        fill_blanks(&mut ours, &reference, Some(&project));
        assert_eq!(ours.name, "Sodium (renamed)");
        assert_eq!(ours.slug.as_deref(), Some("sodium-fork"));
        assert_eq!(ours.icon_url.as_deref(), Some("file:///mine.png"));
    }

    /// One record for the predicates, in the shape an install leaves behind.
    fn a_record(provider: &str, project_id: &str, version_id: &str) -> ContentRecord {
        ContentRecord {
            file_name: "mod.jar".into(),
            kind: ContentKind::Mod,
            provider: provider.into(),
            project_id: project_id.into(),
            version_id: version_id.into(),
            name: "A Mod".into(),
            version_number: "1.0".into(),
            slug: Some("a-mod".into()),
            icon_url: Some("https://example.invalid/a.png".into()),
            dependency: false,
            origin: Default::default(),
            requires: None,
            side: Some(Side::Both),
            server_off: false,
        }
    }

    fn named<'a>(inst: &'a Instance, file_name: &str) -> &'a ContentRecord {
        inst.content
            .iter()
            .find(|r| r.file_name == file_name)
            .unwrap_or_else(|| panic!("no record for {file_name}: {:?}", inst.content))
    }

    /// An `.mrpack` with the index entries and the extra zip entries given.
    fn write_pack(dest: &Path, files: serde_json::Value, extra: &[(&str, &[u8])]) {
        use std::io::Write;
        let index = serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "1",
            "name": "P",
            "files": files,
            "dependencies": { "minecraft": "1.21.1" },
        }))
        .unwrap();

        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("modrinth.index.json", options).unwrap();
        zip.write_all(&index).unwrap();
        for (name, bytes) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
}

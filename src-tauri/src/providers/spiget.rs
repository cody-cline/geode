//! SpigotMC, through the Spiget API (api.spiget.org).
//!
//! Spigot itself publishes no API; Spiget is a long-running third-party index
//! of the resource pages, and it is the only way to search SpigotMC
//! programmatically. Keyless, unlike CurseForge.
//!
//! Two things here are unlike the mod platforms:
//!
//! * Many resources are *external* — the page links to GitHub or the author's
//!   own site rather than hosting a file — and premium ones are paid. Neither
//!   can be downloaded on the user's behalf, so both are marked undistributable
//!   and the UI sends them to the page instead.
//! * Only the current file is downloadable. Spiget lists past version names but
//!   serves the latest, so the version list here is one entry rather than a
//!   history that would mostly 404 if anyone clicked it.

use serde::Deserialize;

use crate::error::Result;
use crate::net::get_json;

use super::{Project, ProjectKind, Provider, SearchQuery, Version, VersionFile};

const API: &str = "https://api.spiget.org/v2";
const SITE: &str = "https://www.spigotmc.org";

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resource {
    id: u64,
    #[serde(default)]
    name: String,
    /// Spiget's name for the one-line blurb.
    #[serde(default)]
    tag: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(default)]
    tested_versions: Vec<String>,
    #[serde(default)]
    external: bool,
    #[serde(default)]
    premium: bool,
    #[serde(default)]
    icon: Option<ResourceIcon>,
    #[serde(default)]
    version: Option<ResourceVersionRef>,
    #[serde(default)]
    update_date: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct ResourceIcon {
    /// Relative to the site root, e.g. `data/resource_icons/28/28140.jpg`.
    #[serde(default)]
    url: String,
}

#[derive(Debug, Default, Deserialize)]
struct ResourceVersionRef {
    #[serde(default)]
    id: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceVersion {
    #[serde(default)]
    id: u64,
    /// The author's version string, e.g. `5.5.71`.
    #[serde(default)]
    name: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    release_date: Option<i64>,
}

fn page_url(id: u64) -> String {
    format!("{SITE}/resources/{id}/")
}

fn to_project(resource: Resource) -> Project {
    let icon_url = resource
        .icon
        .as_ref()
        .map(|i| i.url.trim())
        .filter(|u| !u.is_empty())
        .map(|u| format!("{SITE}/{u}"));

    Project {
        provider: Provider::Spiget,
        page_url: page_url(resource.id),
        id: resource.id.to_string(),
        // Deliberately empty. Spigot resources have no slug, and putting the
        // numeric id here would make cross-platform matching key on "28140" —
        // which matches nothing, so LuckPerms would appear twice rather than as
        // one row carrying both sources. Empty falls back to the name, which is
        // what actually matches.
        slug: String::new(),
        name: resource.name,
        summary: resource.tag,
        icon_url,
        downloads: resource.downloads,
        follows: resource.likes,
        // Only an author id is published here, and resolving it would cost a
        // request per row for a byline.
        author: String::new(),
        kind: ProjectKind::Plugin,
        categories: Vec::new(),
        // What it is published for. A Paper server is offered Spigot plugins
        // because Paper implements the Spigot API, and that rule lives with the
        // server rather than being asserted here.
        loaders: vec!["spigot".into()],
        game_versions: resource.tested_versions,
        updated: resource
            .update_date
            .map(|t| chrono::DateTime::from_timestamp(t, 0).unwrap_or_default().to_rfc3339()),
        banner_url: None,
        // An external resource is hosted elsewhere and a premium one is paid;
        // neither is ours to fetch.
        distribution_allowed: !resource.external && !resource.premium,
        // A Bukkit plugin is server software by definition — there is no
        // client half of it to install.
        client_side: Some("unsupported".into()),
        server_side: Some("required".into()),
        also_on: Vec::new(),
    }
}

pub async fn search(client: &reqwest::Client, query: &SearchQuery) -> Result<Vec<Project>> {
    let size = query.limit.clamp(1, 100);
    let url = if query.query.trim().is_empty() {
        format!(
            "{API}/resources?size={size}&page={}&sort=-downloads",
            query.offset / size.max(1) + 1
        )
    } else {
        // `page` matters as much here as on the browse endpoint: without it
        // every "load more" asks for page one again, and the results dedupe
        // down to nothing new.
        format!(
            "{API}/search/resources/{}?field=name&size={size}&page={}&sort=-downloads",
            urlencoding::encode(query.query.trim()),
            query.offset / size.max(1) + 1
        )
    };

    let resources: Vec<Resource> = get_json(client, &url).await?;
    let mut hits: Vec<Project> = resources.into_iter().map(to_project).collect();

    // Spiget has no game-version facet, so the filter is applied here rather
    // than pretending the query did it.
    if let Some(want) = &query.game_version {
        hits.retain(|p| p.game_versions.iter().any(|v| v == want));
    }
    Ok(hits)
}

pub async fn project(client: &reqwest::Client, id: &str) -> Result<Project> {
    let resource: Resource = get_json(client, &format!("{API}/resources/{id}")).await?;
    Ok(to_project(resource))
}

/// The one downloadable file.
///
/// Spiget serves the current version at a fixed endpoint that redirects to the
/// CDN; older versions are listed but not reliably served, so offering them
/// would be offering links that fail.
pub async fn versions(client: &reqwest::Client, id: &str) -> Result<Vec<Version>> {
    versions_at(client, API, id).await
}

/// The same, against a given base URL. Injectable so a test can serve the two
/// answers itself and look at the file name they compose into.
pub async fn versions_at(client: &reqwest::Client, base: &str, id: &str) -> Result<Vec<Version>> {
    let resource: Resource = get_json(client, &format!("{base}/resources/{id}")).await?;

    let latest: Vec<ResourceVersion> = get_json(
        client,
        &format!("{base}/resources/{id}/versions?size=1&sort=-releaseDate"),
    )
    .await
    .unwrap_or_default();

    let version_name = latest
        .first()
        .map(|v| v.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "latest".to_string());
    let version_id = latest
        .first()
        .map(|v| v.id)
        .or(resource.version.as_ref().map(|v| v.id))
        .unwrap_or(0);

    let blocked = if resource.premium {
        Some("This is a premium resource — buy it on SpigotMC and add the jar by hand.".to_string())
    } else if resource.external {
        Some(
            "The author hosts this file elsewhere, so it cannot be downloaded automatically. \
             Open the resource page to get it."
                .to_string(),
        )
    } else {
        None
    };

    let files = if blocked.is_some() {
        Vec::new()
    } else {
        vec![VersionFile {
            url: format!("{base}/resources/{id}/download"),
            // Spiget serves the jar behind a redirect and names it by id, so
            // the name is built rather than read from a header we cannot see
            // before downloading. Both halves go through `sanitise` together:
            // the version name is as much Spiget's JSON as the resource name
            // is, and sanitising only one of them left the other free to put a
            // `/` or a `..` in the name an install then joins onto a folder.
            filename: format!("{}.jar", sanitise(&format!("{}-{version_name}", resource.name))),
            sha1: None,
            sha512: None,
            size: None,
            primary: true,
        }]
    };

    Ok(vec![Version {
        provider: Provider::Spiget,
        id: version_id.to_string(),
        project_id: id.to_string(),
        name: version_name.clone(),
        version_number: version_name,
        game_versions: resource.tested_versions,
        loaders: vec!["spigot".into()],
        release_type: "release".into(),
        published: latest
            .first()
            .and_then(|v| v.release_date)
            .map(|t| chrono::DateTime::from_timestamp(t, 0).unwrap_or_default().to_rfc3339()),
        downloads: latest.first().map(|v| v.downloads).unwrap_or_default(),
        files,
        dependencies: Vec::new(),
        blocked_reason: blocked,
        // Nothing to look up: a premium resource is paid content, not bytes
        // somebody published for free somewhere else.
        blocked_sha1: None,
    }])
}

/// A file name that will not fight the filesystem.
///
/// A dot survives so a version number still reads as one, which is the whole
/// point of putting it in the name — but never a leading or trailing one, so
/// what comes back can be joined onto a folder and cannot be `..`.
fn sanitise(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '-' || c == '.').to_string();
    if trimmed.is_empty() {
        "plugin".into()
    } else {
        trimmed
    }
}

/// Spiget exposes a description endpoint, but it returns the forum post's HTML
/// with SpigotMC's own markup and inline styling. Rather than render that, the
/// panel shows the blurb and a link to the page.
pub async fn description(_client: &reqwest::Client, _id: &str) -> Result<String> {
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource() -> Resource {
        Resource {
            id: 28140,
            name: "LuckPerms".into(),
            tag: "A permissions plugin".into(),
            downloads: 100,
            tested_versions: vec!["1.21".into(), "26.2".into()],
            ..Default::default()
        }
    }

    #[test]
    fn a_hosted_resource_can_be_installed() {
        let project = to_project(resource());
        assert!(project.distribution_allowed);
        assert_eq!(project.id, "28140");
        assert_eq!(project.kind, ProjectKind::Plugin);
        assert_eq!(project.loaders, vec!["spigot"]);
        assert_eq!(project.page_url, "https://www.spigotmc.org/resources/28140/");
    }

    #[test]
    fn the_slug_is_left_empty_so_the_name_can_match_across_platforms() {
        // A numeric slug would key on "28140" and match nothing, leaving the
        // same plugin listed once per platform.
        let project = to_project(resource());
        assert!(project.slug.is_empty());
        assert_eq!(
            super::super::identity_key(&project.slug, &project.name),
            super::super::identity_key("luckperms", "LuckPerms"),
        );
    }

    #[test]
    fn an_externally_hosted_resource_is_not_ours_to_fetch() {
        // ViaVersion and SkinsRestorer are both like this: listed on SpigotMC,
        // downloaded from somewhere else.
        let mut r = resource();
        r.external = true;
        assert!(!to_project(r).distribution_allowed);
    }

    #[test]
    fn a_premium_resource_is_not_ours_to_fetch_either() {
        let mut r = resource();
        r.premium = true;
        assert!(!to_project(r).distribution_allowed);
    }

    #[test]
    fn the_icon_becomes_an_absolute_url() {
        let mut r = resource();
        r.icon = Some(ResourceIcon {
            url: "data/resource_icons/28/28140.jpg".into(),
        });
        assert_eq!(
            to_project(r).icon_url.as_deref(),
            Some("https://www.spigotmc.org/data/resource_icons/28/28140.jpg")
        );
    }

    #[test]
    fn a_missing_icon_stays_missing() {
        let mut r = resource();
        r.icon = Some(ResourceIcon { url: String::new() });
        assert!(to_project(r).icon_url.is_none());
    }

    #[test]
    fn file_names_survive_awkward_resource_names() {
        assert_eq!(sanitise("LuckPerms"), "LuckPerms");
        assert_eq!(sanitise("Chat / Format++"), "Chat---Format");
        assert_eq!(sanitise("???"), "plugin");
    }

    /// Spiget, serving the two answers `versions` reads.
    ///
    /// Both are `#[serde(default)]` strings straight out of somebody else's
    /// JSON; what they compose into is a name an install joins onto a folder.
    async fn spiget_answering(resource_name: &str, version_name: &str) -> String {
        use http_body_util::Full;
        use hyper::body::{Bytes, Incoming};
        use hyper::service::service_fn;
        use hyper::{Request, Response};
        use hyper_util::rt::TokioIo;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v2", listener.local_addr().unwrap());
        let resource = format!(r#"{{"id":28140,"name":{}}}"#, json_string(resource_name));
        let versions = format!(
            r#"[{{"id":7,"name":{},"downloads":1}}]"#,
            json_string(version_name)
        );

        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (resource, versions) = (resource.clone(), versions.clone());
                tokio::spawn(async move {
                    let service = service_fn(move |r: Request<Incoming>| {
                        let (resource, versions) = (resource.clone(), versions.clone());
                        async move {
                            let body = if r.uri().path().ends_with("/versions") {
                                Bytes::from(versions)
                            } else {
                                Bytes::from(resource)
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

    fn json_string(s: &str) -> String {
        serde_json::to_string(s).unwrap()
    }

    /// The name a version actually arrives with, which is the only thing that
    /// becomes a path.
    ///
    /// Asserting on `sanitise` alone — which is all this file used to do —
    /// could not see that it was being applied to one half of a composed name:
    /// the version string went in verbatim, so a resource whose latest version
    /// is called `../../../../tools/geode-probe.jar` named the file that every
    /// server JVM is started with under `-javaagent:`.
    #[tokio::test]
    async fn the_name_a_version_arrives_with_is_one_ordinary_file_name() {
        let client = crate::net::client();

        // The ordinary case, unchanged: the version number still reads as one.
        let base = spiget_answering("LuckPerms", "5.5.71").await;
        let versions = versions_at(&client, &base, "28140").await.unwrap();
        assert_eq!(
            versions[0].files[0].filename, "LuckPerms-5.5.71.jar",
            "the everyday name changed shape"
        );

        // And the version name as a path, which is where it came from.
        let base = spiget_answering("LuckPerms", "../../../../tools/geode-probe.jar").await;
        let versions = versions_at(&client, &base, "28140").await.unwrap();
        let name = &versions[0].files[0].filename;
        let rel = std::path::Path::new(name);
        assert!(
            crate::paths::is_safe_relative(rel) && rel.components().count() == 1,
            "a version name composed into a path: {name}"
        );
    }
}

//! Managing the files inside an instance: mods, resource packs, shader packs.
//!
//! Disk is the source of truth. `instance.json` only records *provenance* —
//! which project and version a file came from — so that a jar dragged in by
//! hand still shows up, and deleting a file outside the launcher does not leave
//! a phantom entry.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    #[default]
    Mod,
    ResourcePack,
    ShaderPack,
    DataPack,
    /// Server plugins. Only servers have a `plugins/` folder; an instance that
    /// somehow has one lists nothing, which is the honest answer.
    Plugin,
    /// A mod a server keeps for its players and never loads.
    ///
    /// Only servers have these, in `client-mods/`, a folder no loader reads —
    /// which is the point: a renderer or a minimap in `mods/` is at best dead
    /// weight on a server and at worst a crash on boot. A player receives each
    /// one as an ordinary mod in `mods/`.
    ClientMod,
}

impl ContentKind {
    pub fn folder(self) -> &'static str {
        match self {
            ContentKind::Mod => "mods",
            ContentKind::ResourcePack => "resourcepacks",
            ContentKind::ShaderPack => "shaderpacks",
            ContentKind::DataPack => "datapacks",
            ContentKind::Plugin => "plugins",
            ContentKind::ClientMod => "client-mods",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mod" => Some(ContentKind::Mod),
            "resourcepack" => Some(ContentKind::ResourcePack),
            "shaderpack" | "shader" => Some(ContentKind::ShaderPack),
            "datapack" => Some(ContentKind::DataPack),
            "plugin" => Some(ContentKind::Plugin),
            "clientmod" => Some(ContentKind::ClientMod),
            _ => None,
        }
    }
}

/// Who put a file here.
///
/// A client that has joined a server's pack holds two kinds of thing at once:
/// the server's mods, which are the server's to decide on, and its own, which
/// are nobody else's business. Keeping them in one list with a tag — rather
/// than two lists — is what lets a player add a minimap to a server pack
/// without being able to delete the server's anti-cheat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Installed here, by whoever uses this machine.
    #[default]
    User,
    /// Arrived with a server's shared pack.
    ///
    /// Can be turned off — deciding a mod is not for you is a local choice —
    /// but not deleted. The list belongs to the server, and sharing runs one
    /// way, so removing an entry would leave the next sync to put it back.
    Server,
}

impl Origin {
    pub fn is_default(&self) -> bool {
        *self == Self::User
    }
}

/// Provenance for one installed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentRecord {
    pub file_name: String,
    pub kind: ContentKind,
    /// `modrinth` or `curseforge`.
    pub provider: String,
    pub project_id: String,
    pub version_id: String,
    pub name: String,
    pub version_number: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
    /// Installed as a dependency rather than chosen directly. Shown dimmed and
    /// offered for cleanup when the parent goes away.
    #[serde(default)]
    pub dependency: bool,
    /// Whether this arrived with a server's pack or was installed here.
    #[serde(default, skip_serializing_if = "Origin::is_default")]
    pub origin: Origin,
    /// Project ids this version requires, from the same provider.
    ///
    /// `None` means nobody has looked yet — records written before this was
    /// tracked, and anything that arrived through a modpack. The distinction
    /// matters: an empty list is a promise that this needs nothing, and
    /// treating "unknown" as that promise is how you end up telling someone to
    /// delete a library half their mods depend on.
    #[serde(default)]
    pub requires: Option<Vec<String>>,
    /// Which side this belongs on, as told by someone other than the jar.
    ///
    /// On a server or a player's own install, that is Modrinth's listing,
    /// captured when it was installed. On a joined instance's server files, it
    /// is how the server classified the file. `None` when nobody has said —
    /// CurseForge lists nothing, and records written before this was kept are
    /// not filled in after the fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<super::jar::Side>,
}

/// Where a mod belongs, from everything known about it.
///
/// The folder comes first, because it is the admin's own decision: a mod in
/// Client mods is a client mod whatever it says. Then what the jar declares,
/// because that is what the loader enforces. Then the listing, which is an
/// author's tick-box and the only word there is for a jar that says nothing —
/// most of NeoForge. Nothing said anywhere is both, never a guess at one side:
/// a wrong guess leaves a server missing a mod, or a player without one.
pub fn side_of(
    kind: ContentKind,
    declared: Option<super::jar::Side>,
    listed: Option<super::jar::Side>,
) -> super::jar::Side {
    use super::jar::Side;
    if kind == ContentKind::ClientMod {
        return Side::Client;
    }
    match (declared, listed) {
        (Some(side @ (Side::Client | Side::Server)), _) => side,
        (_, Some(side @ (Side::Client | Side::Server))) => side,
        _ => Side::Both,
    }
}

/// One row in the instance's content list.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentItem {
    pub file_name: String,
    pub kind: ContentKind,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub dependency: bool,
    /// Read from the jar itself when we have no provenance record.
    pub from_disk: bool,
    /// Where this came from. A server's own mods can be turned off here but
    /// not deleted, and the UI has to say so rather than offer a button that
    /// fails.
    #[serde(default, skip_serializing_if = "Origin::is_default")]
    pub origin: Origin,
    /// File names of installed mods this one needs.
    #[serde(default)]
    pub requires: Vec<String>,
    /// File names of installed mods that need this one.
    #[serde(default)]
    pub required_by: Vec<String>,
    /// Pulled in as a dependency, and nothing installed still needs it.
    #[serde(default)]
    pub unused: bool,
    /// The jar told us what it needs. When false, its edges are guesswork from
    /// the provider's list, or absent — so it cannot be used to argue that
    /// something else is unused.
    #[serde(default)]
    pub declares_deps: bool,
    /// The mod ids this file answers to. Display names vary — Fabric API is
    /// "Forgified Fabric API" on NeoForge — but the ids do not, so this is what
    /// to match on when a specific mod needs recognising.
    #[serde(default)]
    pub mod_ids: Vec<String>,
    /// Which side a mod belongs on, for mods and client mods only.
    ///
    /// Worked out with [`side_of`], except for a file that arrived with a
    /// server's pack: that one carries the server's own answer, or none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<super::jar::Side>,
}

pub(crate) const DISABLED_SUFFIX: &str = ".disabled";

/// A disabled file is the same file with `.disabled` appended — the convention
/// every other launcher uses, so toggling here is visible to them too.
pub(crate) fn strip_disabled(name: &str) -> (&str, bool) {
    match name.strip_suffix(DISABLED_SUFFIX) {
        Some(base) => (base, false),
        None => (name, true),
    }
}

pub(crate) fn is_content_file(name: &str, kind: ContentKind) -> bool {
    let (base, _) = strip_disabled(name);
    let lower = base.to_ascii_lowercase();
    match kind {
        // A plugin is a jar as much as a mod is. What differs is the folder it
        // belongs in and what loads it, neither of which the name can show.
        ContentKind::Mod | ContentKind::ClientMod | ContentKind::Plugin => lower.ends_with(".jar"),
        ContentKind::ResourcePack | ContentKind::DataPack => lower.ends_with(".zip"),
        ContentKind::ShaderPack => lower.ends_with(".zip") || lower.ends_with(".zip.txt"),
    }
}

/// List everything of one kind in an instance, joining disk state with any
/// provenance we recorded at install time.
pub async fn list(
    paths: &Paths,
    instance: &super::Instance,
    kind: ContentKind,
    jars: &super::jar::MetaCache,
) -> Result<Vec<ContentItem>> {
    list_in(
        &paths.game_dir(&instance.id).join(kind.folder()),
        &instance.content,
        kind,
        jars,
    )
    .await
}

/// The same listing, for any folder holding content and any records describing
/// it.
///
/// Servers keep both somewhere else, and nothing about reading a folder
/// depends on which kind of thing owns it.
pub async fn list_in(
    dir: &Path,
    records: &[ContentRecord],
    kind: ContentKind,
    jars: &super::jar::MetaCache,
) -> Result<Vec<ContentItem>> {
    let mut items = Vec::new();
    // Kept alongside `items` by index; only mods have any.
    let mut metas: Vec<Option<super::jar::JarMeta>> = Vec::new();

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(items),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !is_content_file(&file_name, kind) {
            continue;
        }
        let (base_name, enabled) = strip_disabled(&file_name);
        let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);

        // Records are keyed by the enabled filename so toggling does not orphan
        // the provenance.
        let record = records
            .iter()
            .find(|r| r.file_name == base_name && r.kind == kind);

        // Every mod is read, not just the untracked ones: the jar is where the
        // dependency graph comes from. Plugins declare themselves too, in a
        // different file, and reading it is what gives a plugins folder real
        // edges rather than only the ones recorded at install time.
        let meta = match kind {
            ContentKind::Mod | ContentKind::ClientMod | ContentKind::Plugin => {
                jars.get(&entry.path()).await
            }
            _ => None,
        };

        let mut item = match record {
            // A record that knows only whose a file is — a server's override
            // jar — has no name or version of its own to offer, so it reads
            // like any untracked file, and keeps the one thing it does know.
            Some(r) if r.provider.is_empty() => ContentItem {
                file_name: file_name.clone(),
                kind,
                name: meta
                    .as_ref()
                    .and_then(|m| m.name.clone())
                    .unwrap_or_else(|| base_name.to_string()),
                version: meta
                    .as_ref()
                    .and_then(|m| m.version.clone())
                    .unwrap_or_default(),
                enabled,
                size,
                dependency: false,
                from_disk: false,
                origin: r.origin,
                ..Default::default()
            },
            Some(r) => ContentItem {
                file_name: file_name.clone(),
                kind,
                name: r.name.clone(),
                version: r.version_number.clone(),
                enabled,
                size,
                provider: Some(r.provider.clone()),
                project_id: Some(r.project_id.clone()),
                version_id: Some(r.version_id.clone()),
                icon_url: r.icon_url.clone(),
                slug: r.slug.clone(),
                dependency: r.dependency,
                from_disk: false,
                origin: r.origin,
                ..Default::default()
            },
            None => ContentItem {
                file_name: file_name.clone(),
                kind,
                name: meta
                    .as_ref()
                    .and_then(|m| m.name.clone())
                    .unwrap_or_else(|| base_name.to_string()),
                version: meta
                    .as_ref()
                    .and_then(|m| m.version.clone())
                    .unwrap_or_default(),
                enabled,
                size,
                dependency: false,
                from_disk: true,
                ..Default::default()
            },
        };
        item.mod_ids = meta.as_ref().map(|m| m.primary.clone()).unwrap_or_default();
        if matches!(kind, ContentKind::Mod | ContentKind::ClientMod) {
            // A server's file is the server's to classify, and a joined
            // instance takes its word rather than second-guessing it here.
            item.side = match record {
                Some(r) if r.origin == Origin::Server => r.side,
                _ => Some(side_of(
                    kind,
                    meta.as_ref().and_then(|m| m.side),
                    record.and_then(|r| r.side),
                )),
            };
        }
        items.push(item);
        metas.push(meta);
    }

    // Plugins get edges too, now that servers draw the same dependency field.
    // Their jars declare nothing we can read — a Bukkit `plugin.yml` is not a
    // mod manifest — so `metas` is all `None` and only the project ids recorded
    // at install time contribute. That is still worth drawing, and it stays
    // honest either way: `declares_deps` is false, saying the jar did not tell
    // us, and `unused` only ever marks something installed *as* a dependency,
    // so a plugin dropped in by hand is never called unneeded.
    if matches!(
        kind,
        ContentKind::Mod | ContentKind::ClientMod | ContentKind::Plugin
    ) {
        link(&mut items, &metas, records);
    }

    // Sorting last keeps `items` and `metas` aligned while the graph is built.
    items.sort_by_key(|i| i.name.to_lowercase());
    Ok(items)
}

/// Mod ids vary in punctuation across loaders — `cloth-config` on Fabric,
/// `cloth_config` on NeoForge — and the same mod keeps its meaning across both.
fn id_key(id: &str) -> String {
    id.to_ascii_lowercase().replace('-', "_")
}

/// Fill in each item's dependency edges, and mark libraries nothing needs.
///
/// Edges come from two places and are unioned rather than chosen between: what
/// the jars declare, and the required project ids recorded at install time.
/// Both are incomplete in different ways, and for deciding that something is
/// unused, a missed edge is the expensive mistake — it invites you to delete a
/// library that is holding the instance up.
fn link(
    items: &mut [ContentItem],
    metas: &[Option<super::jar::JarMeta>],
    records: &[ContentRecord],
) {
    // Which files answer to a given mod id, and which to a given project. Kept
    // as lists because several mods can carry a copy of the same library.
    let mut is_id: HashMap<String, Vec<usize>> = HashMap::new();
    let mut carries_id: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_project: HashMap<(String, String), usize> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        if let Some(meta) = &metas[i] {
            for id in &meta.primary {
                is_id.entry(id_key(id)).or_default().push(i);
            }
            for id in &meta.bundled {
                carries_id.entry(id_key(id)).or_default().push(i);
            }
        }
        if let (Some(provider), Some(project)) = (&item.provider, &item.project_id) {
            by_project.insert((provider.clone(), project.clone()), i);
        }
    }

    // Who answers for an id, best candidate first.
    //
    // Being the library beats shipping a copy of it, and not needing the id
    // beats needing it — a mod that asks for fabric-resource-loader-v0 while
    // also carrying one is plainly not where that module comes from, even
    // though Fabric API only carries it too. Nothing ever answers for itself,
    // or a mod bundling its own dependency would satisfy its own requirement
    // and the edge would disappear.
    let wants = |i: usize, key: &str| {
        metas[i]
            .as_ref()
            .is_some_and(|m| m.requires.iter().any(|r| id_key(r) == key))
    };
    let resolve = |from: usize, id: &str| -> Option<usize> {
        let key = id_key(id);
        let rank = |m: &HashMap<String, Vec<usize>>, needy: bool| {
            m.get(&key).and_then(|found| {
                found
                    .iter()
                    .find(|&&i| i != from && wants(i, &key) == needy)
                    .copied()
            })
        };
        rank(&is_id, false)
            .or_else(|| rank(&is_id, true))
            .or_else(|| rank(&carries_id, false))
            .or_else(|| rank(&carries_id, true))
    };

    let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); items.len()];
    for (i, item) in items.iter().enumerate() {
        if let Some(meta) = &metas[i] {
            for id in &meta.requires {
                if let Some(target) = resolve(i, id) {
                    edges[i].insert(target);
                }
            }
        }

        let base = strip_disabled(&item.file_name).0;
        let recorded = records
            .iter()
            .find(|r| r.file_name == base && r.kind == item.kind)
            .and_then(|r| r.requires.as_ref().map(|ids| (r.provider.clone(), ids)));
        if let Some((provider, ids)) = recorded {
            for id in ids {
                if let Some(&target) = by_project.get(&(provider.clone(), id.clone())) {
                    edges[i].insert(target);
                }
            }
        }

        edges[i].remove(&i);
    }

    let names: Vec<String> = items.iter().map(|i| i.file_name.clone()).collect();
    for (i, targets) in edges.iter().enumerate() {
        items[i].requires = targets.iter().map(|&t| names[t].clone()).collect();
        items[i].declares_deps = metas[i].is_some();
        for &t in targets {
            items[t].required_by.push(names[i].clone());
        }
    }
    for item in items.iter_mut() {
        item.unused = item.dependency && item.required_by.is_empty();
    }
}

pub fn content_dir(paths: &Paths, instance_id: &str, kind: ContentKind) -> PathBuf {
    paths.game_dir(instance_id).join(kind.folder())
}

/// Resolve a user-supplied filename inside a content folder, rejecting anything
/// that tries to escape it.
fn resolve_in(dir: &Path, file: &str) -> Result<PathBuf> {
    let rel = Path::new(file);
    if !crate::paths::is_safe_relative(rel) || rel.components().count() != 1 {
        return Err(Error::msg("invalid filename"));
    }
    Ok(dir.join(rel))
}

pub async fn set_enabled_in(dir: &Path, file: &str, enabled: bool) -> Result<String> {
    let current = resolve_in(dir, file)?;
    let (base, currently_enabled) = strip_disabled(file);
    if currently_enabled == enabled {
        return Ok(file.to_string());
    }

    let new_name = if enabled {
        base.to_string()
    } else {
        format!("{base}{DISABLED_SUFFIX}")
    };
    let target = resolve_in(dir, &new_name)?;
    tokio::fs::rename(&current, &target).await?;
    Ok(new_name)
}

pub async fn remove_in(
    dir: &Path,
    records: &mut Vec<ContentRecord>,
    kind: ContentKind,
    file: &str,
) -> Result<()> {
    let (base, _) = strip_disabled(file);

    // A server's mods are not the player's to delete. Turning one off is fine —
    // deciding a mod is not for you is a local choice, and the file stays where
    // the next sync expects it — but removing the entry would only leave the
    // server to put it back, because sharing runs one way.
    if records
        .iter()
        .any(|r| r.file_name == base && r.kind == kind && r.origin == Origin::Server)
    {
        return Err(Error::msg(format!(
            "{base} came from the server's pack, so it cannot be removed here. \
             Turn it off instead if you do not want it."
        )));
    }

    let path = resolve_in(dir, file)?;
    tokio::fs::remove_file(&path).await?;
    records.retain(|r| !(r.file_name == base && r.kind == kind));
    Ok(())
}

/// Mark everything in an instance as having arrived with a server's pack.
///
/// Tagging records alone is not enough, and was the first version's mistake: a
/// jar with no provenance travels as an override, lands with no record at all,
/// and so could never be tagged — which left exactly the mods a server most
/// needs to keep, the ones with no public download, as the only ones a player
/// could delete. So every content file on disk gets a record here, and a file
/// that had none gets one that says nothing except whose it is.
///
/// Meant for a freshly imported instance, where everything present came from
/// the pack. Safe to run on a failed import too, and should be: a server pack
/// that stopped halfway is still the server's.
pub async fn claim_for_server(paths: &Paths, instance: &mut super::Instance) -> Result<()> {
    for record in &mut instance.content {
        record.origin = Origin::Server;
    }

    for kind in [
        ContentKind::Mod,
        ContentKind::ResourcePack,
        ContentKind::ShaderPack,
        ContentKind::DataPack,
    ] {
        let dir = content_dir(paths, &instance.id, kind);
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_content_file(&name, kind) {
                continue;
            }
            let (base, _) = strip_disabled(&name);
            if instance
                .content
                .iter()
                .any(|r| r.file_name == base && r.kind == kind)
            {
                continue;
            }
            instance.content.push(ContentRecord {
                file_name: base.to_string(),
                kind,
                // Nothing is known about where it came from, and nothing is
                // pretended: no provider means no update check and no source.
                provider: String::new(),
                project_id: String::new(),
                version_id: String::new(),
                name: base.to_string(),
                version_number: String::new(),
                slug: None,
                icon_url: None,
                dependency: false,
                origin: Origin::Server,
                requires: None,
                side: None,
            });
        }
    }
    Ok(())
}

pub async fn set_enabled(
    paths: &Paths,
    instance_id: &str,
    kind: ContentKind,
    file: &str,
    enabled: bool,
) -> Result<String> {
    set_enabled_in(&content_dir(paths, instance_id, kind), file, enabled).await
}

pub async fn remove(
    paths: &Paths,
    instance: &mut super::Instance,
    kind: ContentKind,
    file: &str,
) -> Result<()> {
    let dir = content_dir(paths, &instance.id, kind);
    remove_in(&dir, &mut instance.content, kind, file).await
}

#[cfg(test)]
mod tests {

    /// A record written before origins existed is the player's own. Reading it
    /// back as the server's would lock somebody out of deleting their own mods.
    #[test]
    fn an_older_record_belongs_to_the_user() {
        let json = r#"{"fileName":"jei.jar","kind":"mod","provider":"modrinth",
            "projectId":"p","versionId":"v","name":"JEI","versionNumber":"1.0"}"#;
        let record: ContentRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.origin, Origin::User);
    }

    /// The rule the server share rests on: a player may turn the server's mods
    /// off, but the list is the server's. Deleting one would only leave the
    /// next sync to put it back, so it is refused where it is attempted rather
    /// than silently undone later.
    #[tokio::test]
    async fn a_server_mod_can_be_turned_off_but_not_removed() {
        let dir = std::env::temp_dir().join(format!("geode-origin-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("anticheat.jar"), b"x").await.unwrap();
        tokio::fs::write(dir.join("minimap.jar"), b"x").await.unwrap();

        let mut records = vec![
            ContentRecord {
                origin: Origin::Server,
                ..record("anticheat.jar", ContentKind::Mod, "AC", &[])
            },
            ContentRecord {
                origin: Origin::User,
                ..record("minimap.jar", ContentKind::Mod, "MM", &[])
            },
        ];

        let err = remove_in(&dir, &mut records, ContentKind::Mod, "anticheat.jar")
            .await
            .expect_err("the server's mod must not be removable");
        assert!(err.to_string().contains("cannot be removed"), "{err}");
        assert!(dir.join("anticheat.jar").is_file(), "the file was deleted anyway");
        assert_eq!(records.len(), 2, "the record was dropped anyway");

        // Turning it off is still allowed, and is the offered alternative.
        let renamed = set_enabled_in(&dir, "anticheat.jar", false).await.unwrap();
        assert_eq!(renamed, "anticheat.jar.disabled");

        // Even disabled, it is still the server's.
        let err = remove_in(&dir, &mut records, ContentKind::Mod, "anticheat.jar.disabled")
            .await
            .expect_err("disabling must not become a way around the rule");
        assert!(err.to_string().contains("cannot be removed"), "{err}");

        // The player's own mod is theirs to delete.
        remove_in(&dir, &mut records, ContentKind::Mod, "minimap.jar")
            .await
            .unwrap();
        assert!(!dir.join("minimap.jar").exists());
        assert_eq!(records.len(), 1);
    }

    /// A server.json written while mods could be held back from players still
    /// reads, and the choice it carried is gone on the next save rather than
    /// written back.
    #[test]
    fn an_old_record_with_a_sharing_choice_still_reads() {
        let json = r#"{"fileName":"jei.jar","kind":"mod","provider":"modrinth",
            "projectId":"p","versionId":"v","name":"JEI","versionNumber":"1.0",
            "sharing":{"excluded":true,"serverOnly":true}}"#;
        let record: ContentRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.file_name, "jei.jar");
        let written = serde_json::to_string(&record).unwrap();
        assert!(!written.contains("sharing"), "{written}");
    }

    /// The default is not written out, so adding this field does not rewrite
    /// every existing server.json with noise.
    #[test]
    fn the_default_is_not_written_to_disk() {
        let record = ContentRecord {
            file_name: "jei.jar".into(),
            kind: ContentKind::Mod,
            provider: "modrinth".into(),
            project_id: "p".into(),
            version_id: "v".into(),
            name: "JEI".into(),
            version_number: "1.0".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: None,
            side: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("origin"), "{json}");

        let servers = ContentRecord {
            origin: Origin::Server,
            ..record
        };
        let json = serde_json::to_string(&servers).unwrap();
        assert!(
            json.contains(r#""origin":"server""#),
            "an actual origin is written: {json}"
        );
    }
    use super::*;
    use crate::instance::jar::JarMeta;

    fn item(file: &str, dependency: bool) -> ContentItem {
        ContentItem {
            file_name: file.into(),
            kind: ContentKind::Mod,
            name: file.into(),
            dependency,
            ..Default::default()
        }
    }

    fn plugin(file: &str, dependency: bool) -> ContentItem {
        ContentItem {
            file_name: file.into(),
            kind: ContentKind::Plugin,
            name: file.into(),
            dependency,
            ..Default::default()
        }
    }

    fn record(file: &str, kind: ContentKind, project: &str, requires: &[&str]) -> ContentRecord {
        ContentRecord {
            file_name: file.into(),
            kind,
            provider: "modrinth".into(),
            project_id: project.into(),
            version_id: "1".into(),
            name: file.into(),
            version_number: "1".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: Some(requires.iter().map(|s| s.to_string()).collect()),
            side: None,
        }
    }

    fn meta(primary: &[&str], requires: &[&str]) -> Option<JarMeta> {
        Some(JarMeta {
            primary: primary.iter().map(|s| s.to_string()).collect(),
            requires: requires.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        })
    }

    fn bundling(primary: &[&str], bundled: &[&str], requires: &[&str]) -> Option<JarMeta> {
        let mut m = meta(primary, requires).unwrap();
        m.bundled = bundled.iter().map(|s| s.to_string()).collect();
        Some(m)
    }

    #[test]
    fn a_library_two_mods_share_is_not_unused_until_both_go() {
        let mut items = vec![
            item("a.jar", false),
            item("b.jar", false),
            item("lib.jar", true),
        ];
        let metas = vec![
            meta(&["a"], &["lib"]),
            meta(&["b"], &["lib"]),
            meta(&["lib"], &[]),
        ];
        link(&mut items, &metas, &[]);

        assert_eq!(items[2].required_by, vec!["a.jar", "b.jar"]);
        assert_eq!(items[0].requires, vec!["lib.jar"]);
        assert!(!items[2].unused);

        // Drop one dependent: the other still holds it up.
        let mut items = vec![item("b.jar", false), item("lib.jar", true)];
        link(
            &mut items,
            &[meta(&["b"], &["lib"]), meta(&["lib"], &[])],
            &[],
        );
        assert!(!items[1].unused);

        // Drop the last one and it is finally loose.
        let mut items = vec![item("lib.jar", true)];
        link(&mut items, &[meta(&["lib"], &[])], &[]);
        assert!(items[0].unused);
    }

    #[test]
    fn a_mod_that_carries_a_copy_still_points_at_the_real_library() {
        // Mod Menu ships its own fabric-resource-loader-v0, but an installed
        // Fabric API is what the loader will actually use for it.
        let mut items = vec![item("modmenu.jar", false), item("fabric-api.jar", false)];
        let metas = vec![
            bundling(
                &["modmenu"],
                &["fabric-resource-loader-v0"],
                &["fabric-resource-loader-v0"],
            ),
            bundling(&["fabric-api"], &["fabric-resource-loader-v0"], &[]),
        ];
        link(&mut items, &metas, &[]);
        assert_eq!(items[0].requires, vec!["fabric-api.jar"]);
        assert_eq!(items[1].required_by, vec!["modmenu.jar"]);
    }

    #[test]
    fn the_library_itself_beats_a_mod_carrying_a_copy_of_it() {
        let mut items = vec![
            item("needs-cloth.jar", false),
            item("also-bundles-cloth.jar", false),
            item("cloth-config.jar", true),
        ];
        let metas = vec![
            meta(&["needy"], &["cloth-config"]),
            bundling(&["other"], &["cloth-config"], &[]),
            meta(&["cloth-config"], &[]),
        ];
        link(&mut items, &metas, &[]);
        assert_eq!(items[0].requires, vec!["cloth-config.jar"]);
    }

    #[test]
    fn a_mod_that_needs_an_id_is_not_treated_as_its_source() {
        // Both carry a copy of the module; only Fabric API is not asking for
        // one, so it is the one that answers.
        let mut items = vec![
            item("modmenu.jar", false),
            item("fabric-api.jar", false),
            item("sodium.jar", false),
        ];
        let metas = vec![
            bundling(
                &["modmenu"],
                &["fabric-resource-loader-v0"],
                &["fabric-resource-loader-v0"],
            ),
            bundling(&["fabric-api"], &["fabric-resource-loader-v0"], &[]),
            meta(&["sodium"], &["fabric-resource-loader-v0"]),
        ];
        link(&mut items, &metas, &[]);
        assert_eq!(items[0].requires, vec!["fabric-api.jar"]);
        assert_eq!(items[2].requires, vec!["fabric-api.jar"]);
        assert!(items[1].requires.is_empty());
    }

    #[test]
    fn a_mod_you_chose_is_never_reported_unused() {
        let mut items = vec![item("chosen.jar", false)];
        link(&mut items, &[meta(&["chosen"], &[])], &[]);
        assert!(!items[0].unused);
    }

    #[test]
    fn ids_match_across_the_loaders_punctuation() {
        // Fabric writes cloth-config, NeoForge writes cloth_config, and the
        // same jar can be asked for either way.
        let mut items = vec![item("mod.jar", false), item("cloth.jar", true)];
        let metas = vec![
            meta(&["mod"], &["cloth-config"]),
            meta(&["cloth_config"], &[]),
        ];
        link(&mut items, &metas, &[]);
        assert_eq!(items[1].required_by, vec!["mod.jar"]);
        assert!(!items[1].unused);
    }

    #[test]
    fn falls_back_to_the_recorded_project_ids() {
        // Neither jar declares anything readable, so the only edge available is
        // the one the provider gave us at install time.
        let mut items = vec![item("mod.jar", false), item("lib.jar", true)];
        items[0].provider = Some("modrinth".into());
        items[0].project_id = Some("AAA".into());
        items[1].provider = Some("modrinth".into());
        items[1].project_id = Some("BBB".into());

        let records = vec![ContentRecord {
            file_name: "mod.jar".into(),
            kind: ContentKind::Mod,
            provider: "modrinth".into(),
            project_id: "AAA".into(),
            version_id: "1".into(),
            name: "Mod".into(),
            version_number: "1".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: Some(vec!["BBB".into()]),
            side: None,
        }];
        link(&mut items, &[None, None], &records);

        assert_eq!(items[1].required_by, vec!["mod.jar"]);
        assert!(!items[1].unused);
        assert!(!items[0].declares_deps);
    }

    #[test]
    fn plugins_link_through_their_records_too() {
        // A Bukkit jar declares nothing we can read — `plugin.yml` is not a mod
        // manifest — so `metas` is all None and the ids recorded at install
        // time are the only edges available. This is the path a server's
        // dependency field depends on entirely.
        let mut items = vec![plugin("essentials.jar", false), plugin("vault.jar", true)];
        items[0].provider = Some("modrinth".into());
        items[0].project_id = Some("AAA".into());
        items[1].provider = Some("modrinth".into());
        items[1].project_id = Some("BBB".into());

        let records = vec![record("essentials.jar", ContentKind::Plugin, "AAA", &["BBB"])];
        link(&mut items, &[None, None], &records);

        assert_eq!(items[0].requires, vec!["vault.jar"]);
        assert_eq!(items[1].required_by, vec!["essentials.jar"]);
        // The jar told us nothing; only the install record did, and saying so
        // is how the UI knows not to trust this as a complete picture.
        assert!(!items[0].declares_deps);
        // It arrived as a dependency and something still needs it.
        assert!(!items[1].unused);
    }

    /// The gate this covers lives in `list_in`, not in `link`. Linking was
    /// always kind-agnostic; plugins were simply never handed to it, so every
    /// plugin came back with empty edges and a server's dependency field drew
    /// a row of disconnected discs. A `link`-level test would pass either way,
    /// which is why this one goes through the folder.
    #[tokio::test]
    async fn a_plugins_folder_comes_back_with_its_edges() {
        let dir = std::env::temp_dir()
            .join(format!("geode-plugin-link-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.expect("create");

        // The bytes do not matter: a plugin jar is never opened, because there
        // is no reader for a Bukkit manifest.
        for name in ["essentials.jar", "vault.jar"] {
            tokio::fs::write(dir.join(name), b"PK").await.expect("write");
        }

        let records = vec![
            record("essentials.jar", ContentKind::Plugin, "AAA", &["BBB"]),
            record("vault.jar", ContentKind::Plugin, "BBB", &[]),
        ];

        let items = list_in(
            &dir,
            &records,
            ContentKind::Plugin,
            &crate::instance::jar::MetaCache::new(),
        )
        .await
        .expect("the folder should list");

        let essentials = items
            .iter()
            .find(|i| i.file_name == "essentials.jar")
            .expect("essentials");
        let vault = items
            .iter()
            .find(|i| i.file_name == "vault.jar")
            .expect("vault");

        assert_eq!(
            essentials.requires,
            vec!["vault.jar"],
            "a plugin's recorded dependency should become an edge"
        );
        assert_eq!(vault.required_by, vec!["essentials.jar"]);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// The folder outranks the jar, and the jar outranks the listing. A
    /// declaration of both sides says nothing a listing cannot sharpen, and
    /// silence everywhere is both rather than a guess.
    #[test]
    fn side_of_prefers_the_folder_then_the_jar_then_the_listing() {
        use crate::instance::jar::Side;
        use ContentKind::{ClientMod, Mod};

        let table = [
            (
                ClientMod,
                Some(Side::Server),
                Some(Side::Server),
                Side::Client,
            ),
            (Mod, Some(Side::Client), Some(Side::Server), Side::Client),
            (Mod, Some(Side::Server), Some(Side::Client), Side::Server),
            (Mod, Some(Side::Both), Some(Side::Client), Side::Client),
            (Mod, None, Some(Side::Server), Side::Server),
            (Mod, Some(Side::Both), None, Side::Both),
            (Mod, None, None, Side::Both),
        ];
        for (kind, declared, listed, want) in table {
            assert_eq!(
                side_of(kind, declared, listed),
                want,
                "{kind:?} {declared:?} {listed:?}"
            );
        }
    }

    /// A jar with a real `fabric.mod.json`, so reading it finds a side.
    fn fabric_jar(id: &str, environment: &str) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(
            format!(
                r#"{{"schemaVersion":1,"id":"{id}","version":"1.0","environment":"{environment}"}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap().into_inner()
    }

    /// Client mods are read like mods — the jar opened, the record found — and
    /// never borrow the record of a mod in the other folder with the same file
    /// name.
    #[tokio::test]
    async fn a_client_mods_folder_lists_like_mods() {
        let dir = std::env::temp_dir()
            .join(format!("geode-client-mods-{}", std::process::id()))
            .join("client-mods");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // Declares both sides, so only the folder can make it a client mod.
        tokio::fs::write(dir.join("minimap.jar"), fabric_jar("minimap", "*"))
            .await
            .unwrap();

        let records = vec![
            record("minimap.jar", ContentKind::Mod, "IN-MODS", &[]),
            record("minimap.jar", ContentKind::ClientMod, "IN-CLIENT-MODS", &[]),
        ];
        let items = list_in(
            &dir,
            &records,
            ContentKind::ClientMod,
            &crate::instance::jar::MetaCache::new(),
        )
        .await
        .unwrap();

        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].kind, ContentKind::ClientMod);
        assert_eq!(items[0].project_id.as_deref(), Some("IN-CLIENT-MODS"));
        assert_eq!(items[0].mod_ids, vec!["minimap"], "the jar was not read");
        assert_eq!(items[0].side, Some(crate::instance::jar::Side::Client));

        let _ = tokio::fs::remove_dir_all(dir.parent().unwrap()).await;
    }

    /// Each mod row says where it belongs. A file that came from a server
    /// carries the server's word or none, never what the jar says on its own.
    #[tokio::test]
    async fn the_list_says_which_side_each_mod_is_on() {
        use crate::instance::jar::Side;

        let dir = std::env::temp_dir().join(format!("geode-list-sides-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        for (file, environment) in [
            ("tick-profiler.jar", "server"),
            ("zoom.jar", "*"),
            ("from-server.jar", "client"),
        ] {
            tokio::fs::write(
                dir.join(file),
                fabric_jar(&file.replace(['.', '-'], ""), environment),
            )
            .await
            .unwrap();
        }
        let records = vec![
            ContentRecord {
                side: Some(Side::Client),
                ..record("zoom.jar", ContentKind::Mod, "ZOOM", &[])
            },
            ContentRecord {
                origin: Origin::Server,
                ..record("from-server.jar", ContentKind::Mod, "SRV", &[])
            },
        ];

        let items = list_in(
            &dir,
            &records,
            ContentKind::Mod,
            &crate::instance::jar::MetaCache::new(),
        )
        .await
        .unwrap();
        let side = |file: &str| {
            items
                .iter()
                .find(|i| i.file_name == file)
                .unwrap_or_else(|| panic!("{file} is not listed: {items:?}"))
                .side
        };
        assert_eq!(side("tick-profiler.jar"), Some(Side::Server));
        assert_eq!(side("zoom.jar"), Some(Side::Client));
        assert_eq!(side("from-server.jar"), None);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn a_record_of_another_kind_is_never_borrowed() {
        // The same file name can sit in mods/ and in plugins/. Matching on the
        // name alone would hang one folder's dependencies off the other's file.
        let mut items = vec![plugin("shared.jar", false), plugin("dep.jar", true)];
        items[0].provider = Some("modrinth".into());
        items[0].project_id = Some("AAA".into());
        items[1].provider = Some("modrinth".into());
        items[1].project_id = Some("BBB".into());

        // A mod's record, for a plugin of the same name.
        let records = vec![record("shared.jar", ContentKind::Mod, "AAA", &["BBB"])];
        link(&mut items, &[None, None], &records);

        assert!(items[0].requires.is_empty());
        assert!(items[1].required_by.is_empty());
    }

    #[test]
    fn a_disabled_file_still_matches_its_record() {
        let mut items = vec![item("mod.jar.disabled", false), item("lib.jar", true)];
        items[0].provider = Some("modrinth".into());
        items[0].project_id = Some("AAA".into());
        items[1].provider = Some("modrinth".into());
        items[1].project_id = Some("BBB".into());

        let records = vec![ContentRecord {
            // Records key off the enabled name.
            file_name: "mod.jar".into(),
            kind: ContentKind::Mod,
            provider: "modrinth".into(),
            project_id: "AAA".into(),
            version_id: "1".into(),
            name: "Mod".into(),
            version_number: "1".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            origin: Default::default(),
            requires: Some(vec!["BBB".into()]),
            side: None,
        }];
        link(&mut items, &[None, None], &records);
        assert_eq!(items[1].required_by, vec!["mod.jar.disabled"]);
    }
}

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl ContentKind {
    pub fn folder(self) -> &'static str {
        match self {
            ContentKind::Mod => "mods",
            ContentKind::ResourcePack => "resourcepacks",
            ContentKind::ShaderPack => "shaderpacks",
            ContentKind::DataPack => "datapacks",
            ContentKind::Plugin => "plugins",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mod" => Some(ContentKind::Mod),
            "resourcepack" => Some(ContentKind::ResourcePack),
            "shaderpack" | "shader" => Some(ContentKind::ShaderPack),
            "datapack" => Some(ContentKind::DataPack),
            "plugin" => Some(ContentKind::Plugin),
            _ => None,
        }
    }
}

/// What a server does with one installed file when it shares its setup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sharing {
    /// The server needs it; a player does not.
    ///
    /// Still shared, because a client that wants to match the server exactly
    /// should be able to — but offered as something to drop on import. A
    /// performance mod or an anti-cheat is the usual case.
    pub server_only: bool,
    /// Never leaves this machine, whatever anybody asks.
    ///
    /// For the admin's own tooling, anything with a licence that forbids
    /// passing it on, and anything with a secret in its config.
    pub excluded: bool,
}

impl Sharing {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether this file goes to a client at all.
    pub fn shareable(&self) -> bool {
        !self.excluded
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
    /// How this file is treated when a server shares its setup with players.
    ///
    /// A server keeps one list, not two — a mod is installed or it is not —
    /// and these say what happens to each entry on the way out. Defaults mean
    /// "share it, and a player needs it", which is true of most things.
    #[serde(default, skip_serializing_if = "Sharing::is_default")]
    pub sharing: Sharing,
    /// Project ids this version requires, from the same provider.
    ///
    /// `None` means nobody has looked yet — records written before this was
    /// tracked, and anything that arrived through a modpack. The distinction
    /// matters: an empty list is a promise that this needs nothing, and
    /// treating "unknown" as that promise is how you end up telling someone to
    /// delete a library half their mods depend on.
    #[serde(default)]
    pub requires: Option<Vec<String>>,
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

fn is_content_file(name: &str, kind: ContentKind) -> bool {
    let (base, _) = strip_disabled(name);
    let lower = base.to_ascii_lowercase();
    match kind {
        // A plugin is a jar as much as a mod is. What differs is the folder it
        // belongs in and what loads it, neither of which the name can show.
        ContentKind::Mod | ContentKind::Plugin => lower.ends_with(".jar"),
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
            ContentKind::Mod | ContentKind::Plugin => jars.get(&entry.path()).await,
            _ => None,
        };

        let mut item = match record {
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
    if matches!(kind, ContentKind::Mod | ContentKind::Plugin) {
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
    let path = resolve_in(dir, file)?;
    tokio::fs::remove_file(&path).await?;

    let (base, _) = strip_disabled(file);
    records.retain(|r| !(r.file_name == base && r.kind == kind));
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

    /// A record written before sharing existed must read back as "share it,
    /// and a player needs it" — the safe default — rather than as excluded.
    #[test]
    fn an_older_record_defaults_to_being_shareable() {
        let json = r#"{"fileName":"jei.jar","kind":"mod","provider":"modrinth",
            "projectId":"p","versionId":"v","name":"JEI","versionNumber":"1.0"}"#;
        let record: ContentRecord = serde_json::from_str(json).unwrap();
        assert!(record.sharing.shareable());
        assert!(!record.sharing.server_only);
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
            sharing: Sharing::default(),
            requires: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("sharing"), "{json}");

        let excluded = ContentRecord {
            sharing: Sharing { server_only: false, excluded: true },
            ..record
        };
        let json = serde_json::to_string(&excluded).unwrap();
        assert!(json.contains("excluded"), "an actual choice is written: {json}");
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
            sharing: Default::default(),
            requires: Some(requires.iter().map(|s| s.to_string()).collect()),
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
            sharing: Default::default(),
            requires: Some(vec!["BBB".into()]),
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
            sharing: Default::default(),
            requires: Some(vec!["BBB".into()]),
        }];
        link(&mut items, &[None, None], &records);
        assert_eq!(items[1].required_by, vec!["mod.jar.disabled"]);
    }
}

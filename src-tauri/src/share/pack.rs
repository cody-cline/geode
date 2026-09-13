//! Writing and reading a `.geodepack`.
//!
//! The file is an ordinary `.mrpack` with a `geode/` directory beside the
//! index. Writing it is the mrpack writer plus a few more entries; reading it
//! is the mrpack reader plus a pass that applies what the index had nowhere to
//! record. Neither half duplicates the other — see [`super::manifest`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::instance::{self, Instance};
use crate::net::Progress;
use crate::paths::Paths;

use super::manifest::{FileNote, Kind, Manifest, Profile, SourceRef, EXTENSION, FORMAT};

/// Build the Geode half of a pack for an instance.
pub async fn manifest_for(paths: &Paths, instance: &Instance) -> Manifest {
    let media = paths.instance_media(&instance.id);

    Manifest {
        geode_format: FORMAT,
        kind: Kind::Instance,
        profile: Profile {
            name: instance.name.clone(),
            group: instance.group.clone(),
            notes: instance.notes.clone(),
            memory_mb: instance.overrides.memory_mb,
            java_args: instance.overrides.java_args.clone(),
            java_path_never_shared: (),
            width: instance.overrides.width,
            height: instance.overrides.height,
            fullscreen: instance.overrides.fullscreen,
            server_kind: None,
        },
        icon: instance
            .icon
            .as_ref()
            .filter(|name| media.join(name).is_file())
            .map(|name| media_entry(name)),
        banner: instance
            .banner
            .as_ref()
            .filter(|name| media.join(name).is_file())
            .map(|name| media_entry(name)),
        source: instance.pack.clone(),
        files: notes_for(paths, instance).await,
    }
}

fn media_entry(file_name: &str) -> String {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png");
    format!("geode/media/{}", sanitise(file_name, ext))
}

/// Keep the extension and throw away everything else about the name.
///
/// The name inside the zip need only be stable and safe; the original is a
/// local file name that means nothing to a reader.
fn sanitise(file_name: &str, ext: &str) -> String {
    let stem = if file_name.starts_with("banner") {
        "banner"
    } else {
        "icon"
    };
    format!("{stem}.{ext}")
}

/// What each file declared about itself, and where a recipient can get it.
///
/// Two things earn a file a note. The **side** is what the jar said about where
/// it belongs, carried across so an importer need not re-read every jar. The
/// **source** is how a file the index cannot point at gets fetched anyway — by
/// project and version id, resolved through the recipient's own credentials,
/// rather than by us putting the jar in the zip.
async fn notes_for(paths: &Paths, instance: &Instance) -> Vec<FileNote> {
    let game_dir = paths.game_dir(&instance.id);
    let mut out = Vec::new();

    for record in &instance.content {
        let rel = PathBuf::from(record.kind.folder()).join(&record.file_name);

        // A record is provenance, not proof. Deleting a file outside the
        // launcher leaves its record behind forever — the module says as much,
        // and nothing prunes it — so a reference written from the record alone
        // would promise the recipient a mod the sharer does not actually have.
        let Some(full) = on_disk(&game_dir, &rel).await else {
            continue;
        };

        let side = tokio::task::spawn_blocking(move || instance::jar::read(&full))
            .await
            .ok()
            .flatten()
            .and_then(|meta| meta.side);
        let source = source_for(record);

        // Only files with something to say are recorded. A pack whose mods all
        // come from Modrinth and all run on both sides carries no notes at all,
        // which is the difference between a manifest that annotates and one
        // that repeats.
        if side.is_none() && source.is_none() {
            continue;
        }
        out.push(FileNote {
            path: rel.to_string_lossy().replace('\\', "/"),
            side,
            server_only: record.sharing.server_only,
            source,
            manual: false,
        });
    }
    out
}

/// The file a record refers to, enabled or not.
///
/// Records are keyed by the enabled name, so a mod that has been turned off is
/// `foo.jar` in the record and `foo.jar.disabled` on disk. Both count as having
/// the file.
async fn on_disk(game_dir: &Path, rel: &Path) -> Option<PathBuf> {
    let enabled = game_dir.join(rel);
    if tokio::fs::metadata(&enabled).await.is_ok() {
        return Some(enabled);
    }

    let mut name = rel.file_name()?.to_os_string();
    name.push(instance::content::DISABLED_SUFFIX);
    let disabled = game_dir.join(rel.with_file_name(name));
    tokio::fs::metadata(&disabled)
        .await
        .is_ok()
        .then_some(disabled)
}

/// Where a recipient should fetch a file, when the index cannot say.
///
/// Modrinth content is deliberately absent: the index already carries it as a
/// URL and a pair of hashes, and the rule this format rests on is that the same
/// fact never lives in both halves. A record with no provider or no ids — a jar
/// dragged in by hand — gets nothing, and so stays an embedded override, which
/// is the one case where there is genuinely no link to give.
fn source_for(record: &instance::content::ContentRecord) -> Option<SourceRef> {
    if record.provider.is_empty()
        || record.provider == "modrinth"
        || record.project_id.is_empty()
        || record.version_id.is_empty()
    {
        return None;
    }
    Some(SourceRef {
        provider: record.provider.clone(),
        project_id: record.project_id.clone(),
        version_id: record.version_id.clone(),
    })
}

/// Everything that goes in `geode/`, ready for the mrpack writer.
pub async fn extras_for(
    paths: &Paths,
    instance: &Instance,
    manifest: &Manifest,
) -> Result<crate::instance::modpack::Extras> {
    let mut extras = vec![(
        Manifest::PATH.to_string(),
        serde_json::to_vec_pretty(manifest)?,
    )];

    let media = paths.instance_media(&instance.id);
    for (entry, source) in [
        (manifest.icon.as_ref(), instance.icon.as_ref()),
        (manifest.banner.as_ref(), instance.banner.as_ref()),
    ] {
        let (Some(entry), Some(source)) = (entry, source) else {
            continue;
        };
        // Artwork is the one thing that is always embedded rather than linked.
        // It is small, it is the author's own, and a pack that arrives without
        // its picture looks broken.
        if let Ok(bytes) = tokio::fs::read(media.join(source)).await {
            extras.push((entry.clone(), bytes));
        }
    }
    Ok(extras)
}

/// Write a pack that another launcher can open and Geode can open exactly.
pub async fn export(
    paths: &Paths,
    instance: &Instance,
    dest: PathBuf,
    include_saves: bool,
    progress: &Progress,
) -> Result<()> {
    let manifest = manifest_for(paths, instance).await;
    let extras = extras_for(paths, instance, &manifest).await?;

    // Read off the manifest rather than worked out a second time. A file is
    // left out of `overrides/` if and only if the manifest actually carries a
    // reference for it, so the two halves cannot come to disagree about which
    // files the recipient is expected to fetch — which is the same rule the
    // format applies to download URLs, for the same reason.
    let referenced: HashSet<PathBuf> = manifest
        .files
        .iter()
        .filter(|note| note.source.is_some())
        // Split on the separator the manifest uses rather than handing the
        // string to `PathBuf`, so a path written on one platform still matches
        // the walk's own on another.
        .map(|note| note.path.split('/').collect::<PathBuf>())
        .collect();

    instance::modpack::export_mrpack(
        paths,
        instance,
        dest,
        include_saves,
        extras,
        &referenced,
        progress,
    )
    .await
}

/// Read the Geode half out of a pack, if it has one.
///
/// `Ok(None)` means a plain `.mrpack`, which is a perfectly good thing to be
/// handed and is imported as one.
pub fn read_manifest(archive: &Path) -> Result<Option<Manifest>> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let Ok(mut entry) = zip.by_name(Manifest::PATH) else {
        return Ok(None);
    };

    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text)?;
    let manifest: Manifest = serde_json::from_str(&text)
        .map_err(|e| Error::msg(format!("this pack's Geode manifest could not be read: {e}")))?;

    if !manifest.readable() {
        return Err(Error::msg(format!(
            "this pack was written by a newer Geode (format {}, this build reads {FORMAT}) \
             — update before importing it",
            manifest.geode_format
        )));
    }
    Ok(Some(manifest))
}

/// Apply the annotations to an instance the mrpack half already created.
///
/// Only ever fills in what the pack actually carried: a manifest that recorded
/// no memory size leaves the importer's default alone rather than writing a
/// zero over it.
pub fn apply(manifest: &Manifest, instance: &mut Instance) {
    let p = &manifest.profile;
    if !p.name.trim().is_empty() {
        instance.name = p.name.clone();
    }
    if p.group.is_some() {
        instance.group = p.group.clone();
    }
    if !p.notes.is_empty() {
        instance.notes = p.notes.clone();
    }
    if p.memory_mb.is_some() {
        instance.overrides.memory_mb = p.memory_mb;
    }
    if p.java_args.is_some() {
        instance.overrides.java_args = p.java_args.clone();
    }
    if p.width.is_some() {
        instance.overrides.width = p.width;
    }
    if p.height.is_some() {
        instance.overrides.height = p.height;
    }
    if p.fullscreen.is_some() {
        instance.overrides.fullscreen = p.fullscreen;
    }
    if manifest.source.is_some() {
        instance.pack = manifest.source.clone();
    }
    // A Java path is never carried, so there is nothing to apply and nothing
    // that could point at a directory on somebody else's machine.
}

/// Apply the Geode half of a pack to the instance the mrpack half just built.
///
/// `Ok(vec![])` means everything the pack named is now on disk. Anything in the
/// returned list is a file the manifest pointed at that the recipient has to
/// fetch themselves — which is the design working rather than a shortfall: the
/// pack said where each mod lives instead of carrying somebody else's jar, and
/// a named list is more actionable than an instance that is quietly incomplete.
///
/// A plain `.mrpack` has no Geode half, so this does nothing and says so.
pub async fn apply_pack(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    let probe = archive.to_path_buf();
    let manifest = tokio::task::spawn_blocking(move || read_manifest(&probe))
        .await
        .map_err(|e| Error::msg(format!("reading the pack's manifest panicked: {e}")))??;
    let Some(manifest) = manifest else {
        return Ok(Vec::new());
    };

    apply(&manifest, instance);
    extract_media(paths, instance, archive, &manifest).await?;

    let missing =
        resolve_references(client, paths, instance, &manifest, curseforge_key, progress).await?;

    // Everything that arrived with a server's pack is the server's, however it
    // arrived — through the index, through a reference, or as an override. A
    // player may turn any of it off; the list itself is not theirs to edit.
    if manifest.kind == Kind::Server {
        for record in &mut instance.content {
            record.origin = instance::content::Origin::Server;
        }
    }

    Ok(missing)
}

/// Copy the pack's artwork out of `geode/` and point the instance at it.
///
/// Overrides are extracted by path prefix and `geode/` matches none of them, so
/// without this the artwork is not merely unused — it never lands on disk.
async fn extract_media(
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    manifest: &Manifest,
) -> Result<()> {
    let wanted: Vec<(String, bool)> = [(manifest.icon.as_ref(), true), (manifest.banner.as_ref(), false)]
        .into_iter()
        .filter_map(|(entry, is_icon)| entry.map(|e| (e.clone(), is_icon)))
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }

    let media = paths.instance_media(&instance.id);
    tokio::fs::create_dir_all(&media).await?;

    let archive = archive.to_path_buf();
    let media_dir = media.clone();
    let written = tokio::task::spawn_blocking(move || -> Result<Vec<(String, bool)>> {
        let file = std::fs::File::open(&archive)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let mut out = Vec::new();
        for (entry, is_icon) in wanted {
            // Only ever from inside `geode/`, and only ever a bare file name on
            // the way out, so a crafted entry cannot write outside the folder.
            if !entry.starts_with("geode/media/") {
                continue;
            }
            let Some(name) = Path::new(&entry).file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(mut source) = zip.by_name(&entry) else {
                continue;
            };
            let mut dest = std::fs::File::create(media_dir.join(name))?;
            std::io::copy(&mut source, &mut dest)?;
            out.push((name.to_string(), is_icon));
        }
        Ok(out)
    })
    .await
    .map_err(|e| Error::msg(format!("extracting pack artwork panicked: {e}")))??;

    for (name, is_icon) in written {
        if is_icon {
            instance.icon = Some(name);
        } else {
            instance.banner = Some(name);
        }
    }
    Ok(())
}

/// Fetch everything the manifest pointed at rather than carried.
///
/// Each reference is resolved live through the recipient's own credentials, so
/// an author who has since turned third-party downloads off is respected rather
/// than worked around — which is the whole reason these are ids and not URLs.
async fn resolve_references(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    manifest: &Manifest,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    let refs: Vec<(&FileNote, &SourceRef)> = manifest
        .files
        .iter()
        .filter_map(|note| note.source.as_ref().map(|source| (note, source)))
        .collect();
    if refs.is_empty() {
        return Ok(Vec::new());
    }

    let key = curseforge_key.filter(|k| !k.trim().is_empty());
    let game_dir = paths.game_dir(&instance.id);
    let mut downloads = Vec::new();
    let mut records = Vec::new();
    let mut missing = Vec::new();

    progress.stage("Resolving pack references", refs.len() as u64).await;

    for (note, source) in refs {
        progress.check_cancelled()?;
        progress.advance(1, &note.path).await;

        let rel = Path::new(&note.path);
        if !crate::paths::is_safe_relative(rel) {
            continue;
        }
        let Some(kind) = kind_for(rel) else {
            missing.push(format!("{} (unrecognised folder)", note.path));
            continue;
        };

        // Only CurseForge is ever written as a reference today — Modrinth goes
        // in the index, and nothing else is installed by id.
        if source.provider != "curseforge" {
            missing.push(format!("{} (from {}, which Geode cannot fetch by id)", note.path, source.provider));
            continue;
        }
        let Some(key) = key else {
            missing.push(format!("{} (needs your CurseForge API key)", note.path));
            continue;
        };
        let (Ok(project), Ok(file_id)) = (
            source.project_id.parse::<u32>(),
            source.version_id.parse::<u32>(),
        ) else {
            missing.push(format!("{} (unreadable CurseForge ids)", note.path));
            continue;
        };

        let version = match crate::providers::curseforge::file(client, project, file_id, key).await {
            Ok(v) => v,
            Err(e) => {
                missing.push(format!("{} ({e})", note.path));
                continue;
            }
        };
        let Some(file) = version.primary_file().filter(|_| version.blocked_reason.is_none()) else {
            // The author turned third-party downloads off. That is a decision
            // to respect and report, not to route around.
            missing.push(format!("{} — {} (the author disabled third-party downloads)", note.path, version.name));
            continue;
        };

        downloads.push(
            crate::net::Download::new(file.url.clone(), game_dir.join(rel))
                .sha1(file.sha1.clone())
                .size(file.size)
                .label(note.path.clone()),
        );
        records.push(instance::content::ContentRecord {
            file_name: rel
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| note.path.clone()),
            kind,
            provider: "curseforge".into(),
            project_id: version.project_id.clone(),
            version_id: version.id.clone(),
            name: version.name.clone(),
            version_number: version.version_number.clone(),
            slug: None,
            icon_url: None,
            dependency: false,
            sharing: Default::default(),
            origin: Default::default(),
            requires: None,
        });
    }

    progress.stage("Downloading referenced content", downloads.len() as u64).await;
    crate::net::fetch_all(client, downloads, progress, 8).await?;
    instance.content.extend(records);

    Ok(missing)
}

/// Which content folder a manifest path names, if any.
fn kind_for(rel: &Path) -> Option<instance::content::ContentKind> {
    let folder = rel.parent()?.file_name()?.to_str()?;
    [
        instance::content::ContentKind::Mod,
        instance::content::ContentKind::ResourcePack,
        instance::content::ContentKind::ShaderPack,
        instance::content::ContentKind::DataPack,
        instance::content::ContentKind::Plugin,
    ]
    .into_iter()
    .find(|k| k.folder() == folder)
}

/// `My Pack 1.2` becomes `My Pack 1.2.geodepack`.
pub fn file_name(stem: &str) -> String {
    let cleaned: String = stem
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '-' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    let stem = if trimmed.is_empty() { "pack" } else { trimmed };
    format!("{stem}.{EXTENSION}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::InstanceOverrides;

    fn manifest() -> Manifest {
        Manifest {
            geode_format: FORMAT,
            kind: Kind::Instance,
            profile: Profile {
                name: "Shared Pack".into(),
                group: Some("Friends".into()),
                notes: "read me".into(),
                memory_mb: Some(6144),
                java_args: Some("-XX:+UseG1GC".into()),
                java_path_never_shared: (),
                width: Some(1600),
                height: Some(900),
                fullscreen: Some(false),
                server_kind: None,
            },
            icon: None,
            banner: None,
            source: None,
            files: Vec::new(),
        }
    }

    fn instance() -> Instance {
        let mut i = Instance::new(
            "Local name".into(),
            "1.21.1".into(),
            crate::meta::Loader::Fabric,
            "0.16.0".into(),
        );
        i.overrides = InstanceOverrides {
            memory_mb: Some(2048),
            java_path: Some("/usr/lib/jvm/java-21/bin/java".into()),
            ..Default::default()
        };
        i
    }

    #[test]
    fn importing_applies_what_the_pack_carried() {
        let mut got = instance();
        apply(&manifest(), &mut got);
        assert_eq!(got.name, "Shared Pack");
        assert_eq!(got.group.as_deref(), Some("Friends"));
        assert_eq!(got.notes, "read me");
        assert_eq!(got.overrides.memory_mb, Some(6144));
        assert_eq!(got.overrides.java_args.as_deref(), Some("-XX:+UseG1GC"));
        assert_eq!(got.overrides.width, Some(1600));
    }

    /// The other half of the same rule: what the pack did not carry must be
    /// left alone. Writing a `None` over a setting the importer chose would
    /// make "this pack says nothing about memory" and "this pack wants the
    /// default" the same thing, and they are not.
    #[test]
    fn importing_leaves_alone_what_the_pack_did_not_carry() {
        let mut m = manifest();
        m.profile.memory_mb = None;
        m.profile.java_args = None;
        m.profile.group = None;
        m.profile.notes = String::new();

        let mut got = instance();
        got.notes = "my own notes".into();
        apply(&m, &mut got);

        assert_eq!(got.overrides.memory_mb, Some(2048), "kept the local size");
        assert_eq!(got.notes, "my own notes");
        assert!(got.group.is_none());
    }

    /// A Java path is the author's machine, not the pack's. It is never
    /// exported, so importing can never point somebody's launcher at a
    /// directory that does not exist on their disk.
    #[test]
    fn importing_never_touches_the_java_path() {
        let mut got = instance();
        let before = got.overrides.java_path.clone();
        apply(&manifest(), &mut got);
        assert_eq!(got.overrides.java_path, before);
    }

    /// The invariant the whole format rests on: everything Geode adds lives
    /// under `geode/`, so deleting that directory leaves a `.mrpack` other
    /// launchers still open correctly. If the Geode half could write anywhere
    /// else it could overwrite the index, and the pack would stop being a
    /// modpack the moment it gained a Geode feature.
    #[tokio::test]
    async fn everything_geode_adds_stays_inside_its_own_directory() {
        let paths = Paths {
            root: std::env::temp_dir().join(format!("geode-extras-{}", std::process::id())),
        };
        let mut inst = instance();
        inst.icon = Some("icon.png".into());
        inst.banner = Some("banner.jpg".into());

        let mut m = manifest();
        m.icon = Some(media_entry("icon.png"));
        m.banner = Some(media_entry("banner.jpg"));

        let extras = extras_for(&paths, &inst, &m).await.unwrap();
        assert!(!extras.is_empty());
        for (name, _) in &extras {
            assert!(
                name.starts_with("geode/"),
                "{name} is written outside geode/"
            );
        }
        assert!(extras.iter().all(|(n, _)| n != "modrinth.index.json"));
    }

    #[test]
    fn artwork_keeps_its_own_format_rather_than_being_renamed_to_png() {
        // A jpg banner stays a jpg; calling it .png would leave a file no
        // decoder opens by name.
        assert_eq!(media_entry("banner-93ab.jpg"), "geode/media/banner.jpg");
        assert_eq!(media_entry("icon-12cd.png"), "geode/media/icon.png");
    }

    // -----------------------------------------------------------------------
    // Referencing rather than copying
    // -----------------------------------------------------------------------

    use crate::instance::content::{ContentKind, ContentRecord};

    fn record(provider: &str, file_name: &str, kind: ContentKind) -> ContentRecord {
        ContentRecord {
            file_name: file_name.into(),
            kind,
            provider: provider.into(),
            project_id: if provider.is_empty() { String::new() } else { "proj-1".into() },
            version_id: if provider.is_empty() { String::new() } else { "ver-1".into() },
            name: file_name.into(),
            version_number: "1.0".into(),
            slug: None,
            icon_url: None,
            dependency: false,
            sharing: Default::default(),
            origin: Default::default(),
            requires: None,
        }
    }

    /// An instance on disk, with each named file actually written.
    async fn fixture(tag: &str, files: &[(&str, &str)]) -> (Paths, Instance) {
        let paths = Paths {
            root: std::env::temp_dir().join(format!("geode-ref-{tag}-{}", std::process::id())),
        };
        let mut inst = instance();
        inst.id = format!("inst-{tag}");
        let game = paths.game_dir(&inst.id);
        let _ = tokio::fs::remove_dir_all(&game).await;
        for (dir, name) in files {
            let d = game.join(dir);
            tokio::fs::create_dir_all(&d).await.unwrap();
            // Real zip bytes would let `jar::read` find a side; empty is fine
            // here, because every assertion below is about paths, not sides.
            tokio::fs::write(d.join(name), b"not really a jar").await.unwrap();
        }
        (paths, inst)
    }

    /// Everything inside an exported pack, by entry name.
    fn entries(archive: &Path) -> Vec<String> {
        let file = std::fs::File::open(archive).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect()
    }

    /// The point of the whole format: a jar that came from somewhere is named,
    /// not copied. Sending the bytes is what makes a shared pack an act of
    /// redistribution rather than a description of one.
    #[tokio::test]
    async fn a_provider_backed_jar_is_referenced_instead_of_embedded() {
        let (paths, mut inst) =
            fixture("cf", &[("mods", "create.jar"), ("config", "create.toml")]).await;
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        let manifest = manifest_for(&paths, &inst).await;
        let note = manifest.note("mods/create.jar").expect("a note was written");
        assert_eq!(
            note.source,
            Some(SourceRef {
                provider: "curseforge".into(),
                project_id: "proj-1".into(),
                version_id: "ver-1".into(),
            })
        );

        let dest = paths.root.join("out.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t")).await.unwrap();
        let names = entries(&dest);

        assert!(
            !names.iter().any(|n| n.contains("create.jar")),
            "the jar was embedded anyway: {names:?}"
        );
        // The config is the author's own work and has no link to give, so it
        // still travels as bytes. That is what `overrides/` is for.
        assert!(names.contains(&"overrides/config/create.toml".to_string()), "{names:?}");
    }

    /// The same jar, turned off.
    ///
    /// Records are keyed by the enabled name while disk carries `.disabled`, so
    /// a naive path comparison misses every disabled file — and misses it in
    /// the worst direction, writing the jar into `overrides/` while the
    /// manifest says it was left out. Both halves wrong at once.
    #[tokio::test]
    async fn a_disabled_provider_backed_jar_is_not_embedded_either() {
        let (paths, mut inst) = fixture("cfoff", &[("mods", "create.jar.disabled")]).await;
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        let manifest = manifest_for(&paths, &inst).await;
        assert!(
            manifest.note("mods/create.jar").is_some_and(|n| n.source.is_some()),
            "a disabled file still has a source: {:?}",
            manifest.files
        );

        let dest = paths.root.join("off.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t")).await.unwrap();
        let names = entries(&dest);
        assert!(
            !names.iter().any(|n| n.contains("create.jar")),
            "the disabled jar was embedded: {names:?}"
        );
    }

    /// A jar somebody dragged in has no link to give, so it is still carried.
    ///
    /// This is the case the format's own rule names, and the tempting mistake
    /// is to drop it for symmetry — which would silently lose a mod the author
    /// wrote themselves, with nothing anywhere saying where it went.
    #[tokio::test]
    async fn a_jar_with_no_provenance_is_still_embedded() {
        let (paths, mut inst) = fixture(
            "hand",
            &[("mods", "homemade.jar"), ("mods", "no-record-at-all.jar")],
        )
        .await;
        // Two ways a jar has no link to give: a record with nothing in it, and
        // — the ordinary case — a file somebody dropped in, which Geode never
        // writes a record for at all.
        inst.content = vec![record("", "homemade.jar", ContentKind::Mod)];

        let manifest = manifest_for(&paths, &inst).await;
        for path in ["mods/homemade.jar", "mods/no-record-at-all.jar"] {
            assert!(
                manifest.note(path).is_none_or(|n| n.source.is_none()),
                "{path} must not claim a source"
            );
        }

        let dest = paths.root.join("hand.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t")).await.unwrap();
        let names = entries(&dest);
        for path in ["mods/homemade.jar", "mods/no-record-at-all.jar"] {
            assert!(
                names.contains(&format!("overrides/{path}")),
                "{path} was dropped from the pack: {names:?}"
            );
        }
    }

    /// Disk is the source of truth; a record is only provenance. Deleting a
    /// file outside the launcher leaves its record behind forever, and a
    /// reference written from that record alone would promise the recipient a
    /// mod the sharer does not have.
    #[tokio::test]
    async fn a_record_whose_file_is_gone_is_not_referenced() {
        let (paths, mut inst) = fixture("stale", &[("mods", "present.jar")]).await;
        inst.content = vec![
            record("curseforge", "present.jar", ContentKind::Mod),
            record("curseforge", "deleted-by-hand.jar", ContentKind::Mod),
        ];

        let manifest = manifest_for(&paths, &inst).await;
        assert!(manifest.note("mods/present.jar").is_some());
        assert!(
            manifest.note("mods/deleted-by-hand.jar").is_none(),
            "referenced a file that is not there: {:?}",
            manifest.files
        );
    }

    /// The index already says where a Modrinth file comes from. Saying it again
    /// here is how the two halves come to disagree.
    #[tokio::test]
    async fn modrinth_content_is_never_given_a_source() {
        let (paths, mut inst) = fixture("mr", &[("mods", "sodium.jar")]).await;
        inst.content = vec![record("modrinth", "sodium.jar", ContentKind::Mod)];

        let manifest = manifest_for(&paths, &inst).await;
        assert!(manifest.files.iter().all(|n| n.source.is_none()), "{:?}", manifest.files);
    }

    /// The invariant that keeps a pack honest: a file is left out of
    /// `overrides/` exactly when the manifest carries a reference for it. If
    /// these two could drift, a pack would either redistribute a jar it claimed
    /// not to, or omit one nothing tells the recipient how to get.
    #[tokio::test]
    async fn nothing_is_omitted_without_the_manifest_saying_where_to_get_it() {
        let (paths, mut inst) = fixture(
            "inv",
            &[
                ("mods", "from-cf.jar"),
                ("mods", "from-mr.jar"),
                ("mods", "homemade.jar"),
                ("config", "settings.toml"),
            ],
        )
        .await;
        inst.content = vec![
            record("curseforge", "from-cf.jar", ContentKind::Mod),
            record("modrinth", "from-mr.jar", ContentKind::Mod),
            record("", "homemade.jar", ContentKind::Mod),
        ];

        let manifest = manifest_for(&paths, &inst).await;
        let dest = paths.root.join("inv.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t")).await.unwrap();
        let names = entries(&dest);

        for (dir, file) in [
            ("mods", "from-cf.jar"),
            ("mods", "from-mr.jar"),
            ("mods", "homemade.jar"),
            ("config", "settings.toml"),
        ] {
            let path = format!("{dir}/{file}");
            let embedded = names.contains(&format!("overrides/{path}"));
            let referenced = manifest.note(&path).is_some_and(|n| n.source.is_some());
            // A Modrinth file is in neither: the index carries it. Everything
            // else must be in exactly one of the two.
            let indexed = path == "mods/from-mr.jar";
            assert!(
                embedded ^ referenced || indexed,
                "{path}: embedded={embedded} referenced={referenced}"
            );
            assert!(!(embedded && referenced), "{path} is in the pack twice over");
        }
    }

    /// A plain `.mrpack` has nowhere to record a reference, so it must keep
    /// carrying the bytes — leaving a file out of one loses the mod with
    /// nothing anywhere to say so.
    #[tokio::test]
    async fn a_plain_mrpack_still_embeds_what_it_cannot_reference() {
        let (paths, mut inst) = fixture("plain", &[("mods", "create.jar")]).await;
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        let dest = paths.root.join("plain.mrpack");
        crate::instance::modpack::export_mrpack(
            &paths,
            &inst,
            dest.clone(),
            false,
            Vec::new(),
            &Default::default(),
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert!(
            entries(&dest).contains(&"overrides/mods/create.jar".to_string()),
            "{:?}",
            entries(&dest)
        );
    }

    // -----------------------------------------------------------------------
    // The round trip
    // -----------------------------------------------------------------------

    /// The whole point of the format: what goes out comes back. Before this,
    /// Geode could write a `.geodepack` and then silently drop its own name,
    /// notes, memory and artwork on the way back in.
    #[tokio::test]
    async fn a_pack_carries_the_instance_back_to_another_machine() {
        let (paths, mut inst) =
            fixture("round", &[("mods", "create.jar"), ("config", "x.toml")]).await;
        inst.name = "My Pack".into();
        inst.notes = "read me".into();
        inst.overrides.memory_mb = Some(6144);
        inst.content = vec![record("curseforge", "create.jar", ContentKind::Mod)];

        // Artwork lives beside the instance rather than in the game directory.
        let media = paths.instance_media(&inst.id);
        tokio::fs::create_dir_all(&media).await.unwrap();
        tokio::fs::write(media.join("icon.png"), b"PNG").await.unwrap();
        inst.icon = Some("icon.png".into());

        let dest = paths.root.join("round.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t"))
            .await
            .unwrap();

        // A fresh instance, standing in for somebody else's machine.
        let mut arrived = instance();
        arrived.id = "arrived".into();
        let missing = apply_pack(
            &reqwest::Client::new(),
            &paths,
            &mut arrived,
            &dest,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert_eq!(arrived.name, "My Pack");
        assert_eq!(arrived.notes, "read me");
        assert_eq!(arrived.overrides.memory_mb, Some(6144));
        assert_eq!(arrived.icon.as_deref(), Some("icon.png"));
        assert!(
            paths.instance_media("arrived").join("icon.png").is_file(),
            "the artwork never landed on disk"
        );

        // The CurseForge mod was named rather than carried, so with no key
        // configured the recipient gets a list to act on — not an instance
        // that is quietly one mod short.
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("create.jar"), "{missing:?}");
        assert!(missing[0].contains("CurseForge API key"), "{missing:?}");
    }

    /// Everything that arrives with a server's pack belongs to the server,
    /// however it arrived. A player may turn any of it off; the list is not
    /// theirs to edit, because sharing runs one way.
    #[tokio::test]
    async fn a_server_pack_marks_everything_it_brought_as_the_servers() {
        let (paths, mut inst) = fixture("srv", &[("mods", "anticheat.jar")]).await;

        let manifest = Manifest {
            geode_format: FORMAT,
            kind: Kind::Server,
            profile: Profile { name: "Survival".into(), ..Default::default() },
            icon: None,
            banner: None,
            source: None,
            files: Vec::new(),
        };
        let dest = paths.root.join("server.geodepack");
        write_geode_only_pack(&dest, &manifest);

        // As if the mrpack half had just installed these.
        inst.content = vec![
            record("modrinth", "anticheat.jar", ContentKind::Mod),
            record("modrinth", "sodium.jar", ContentKind::Mod),
        ];

        apply_pack(
            &reqwest::Client::new(),
            &paths,
            &mut inst,
            &dest,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert_eq!(inst.name, "Survival");
        assert!(
            inst.content
                .iter()
                .all(|r| r.origin == crate::instance::content::Origin::Server),
            "a server pack left something marked as the player's own"
        );
    }

    /// A pack from a person, not a server, leaves the recipient in charge of
    /// their own mods — they can delete any of them.
    #[tokio::test]
    async fn a_personal_pack_leaves_its_content_the_recipients_own() {
        let (paths, mut inst) = fixture("pers", &[("mods", "sodium.jar")]).await;

        let manifest = Manifest {
            geode_format: FORMAT,
            kind: Kind::Instance,
            profile: Profile { name: "Mine".into(), ..Default::default() },
            icon: None,
            banner: None,
            source: None,
            files: Vec::new(),
        };
        let dest = paths.root.join("personal.geodepack");
        write_geode_only_pack(&dest, &manifest);

        inst.content = vec![record("modrinth", "sodium.jar", ContentKind::Mod)];
        apply_pack(
            &reqwest::Client::new(),
            &paths,
            &mut inst,
            &dest,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();

        assert!(
            inst.content
                .iter()
                .all(|r| r.origin == crate::instance::content::Origin::User),
            "a personal pack must not lock the recipient out of their own mods"
        );
    }

    /// Both halves together, in the order the app runs them: the mrpack reader
    /// first, then the Geode half. This is what proves `geode/` entries do not
    /// confuse the override extractor, that nothing Geode adds leaks into the
    /// game directory, and that a config the author wrote actually arrives.
    #[tokio::test]
    async fn a_pack_imports_through_the_same_path_the_app_uses() {
        let (paths, mut inst) = fixture(
            "full",
            &[("mods", "homemade.jar"), ("config", "author.toml")],
        )
        .await;
        inst.name = "Full Round".into();
        inst.overrides.java_args = Some("-XX:+UseG1GC".into());
        inst.content = vec![record("", "homemade.jar", ContentKind::Mod)];

        let dest = paths.root.join("full.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t"))
            .await
            .unwrap();

        let mut arrived = instance();
        arrived.id = "full-arrived".into();
        let client = reqwest::Client::new();

        crate::instance::modpack::import_into(
            &client,
            &paths,
            &mut arrived,
            &dest,
            None,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        let missing = apply_pack(&client, &paths, &mut arrived, &dest, None, &Progress::silent("t"))
            .await
            .unwrap();

        assert!(missing.is_empty(), "nothing here needed fetching: {missing:?}");
        assert_eq!(arrived.name, "Full Round");
        assert_eq!(arrived.overrides.java_args.as_deref(), Some("-XX:+UseG1GC"));

        let game = paths.game_dir("full-arrived");
        assert!(
            game.join("config").join("author.toml").is_file(),
            "the author's own config did not arrive"
        );
        assert!(
            game.join("mods").join("homemade.jar").is_file(),
            "the jar with no link to give did not arrive"
        );
        assert!(
            !game.join("geode").exists(),
            "the Geode half was unpacked into the game directory"
        );
    }

    /// A pack with only the Geode half, for exercising import on its own.
    fn write_geode_only_pack(dest: &Path, manifest: &Manifest) {
        use std::io::Write;
        let file = std::fs::File::create(dest).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file(Manifest::PATH, options).unwrap();
        zip.write_all(&serde_json::to_vec_pretty(manifest).unwrap()).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn a_name_becomes_a_safe_file_name() {
        assert_eq!(file_name("All the Mods 10"), "All the Mods 10.geodepack");
        assert_eq!(file_name("bad/name:here"), "bad-name-here.geodepack");
        assert_eq!(file_name("   "), "pack.geodepack");
    }
}

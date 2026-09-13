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
        let Some((full, disabled)) = on_disk(&game_dir, &rel).await else {
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
            // Only a reference needs telling. A file carried as bytes keeps its
            // `.disabled` name on the way through, and a note written for its
            // side alone would be saying it twice.
            disabled: disabled && source.is_some(),
            source,
            manual: false,
        });
    }
    out
}

/// The file a record refers to, and whether it is turned off.
///
/// Records are keyed by the enabled name, so a mod that has been turned off is
/// `foo.jar` in the record and `foo.jar.disabled` on disk. Both count as having
/// the file.
async fn on_disk(game_dir: &Path, rel: &Path) -> Option<(PathBuf, bool)> {
    let enabled = game_dir.join(rel);
    if tokio::fs::metadata(&enabled).await.is_ok() {
        return Some((enabled, false));
    }

    let disabled = game_dir.join(disabled_name(rel)?);
    tokio::fs::metadata(&disabled)
        .await
        .is_ok()
        .then_some((disabled, true))
}

/// `mods/foo.jar` → `mods/foo.jar.disabled`.
fn disabled_name(rel: &Path) -> Option<PathBuf> {
    let mut name = rel.file_name()?.to_os_string();
    name.push(instance::content::DISABLED_SUFFIX);
    Some(rel.with_file_name(name))
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

/// Import a pack into a freshly created instance — both halves, in order.
///
/// The mrpack half runs first and builds a working instance any launcher would
/// recognise; the Geode half then applies what `.mrpack` has nowhere to put.
/// This is the one sequence the app runs, so it is also the one the tests run.
///
/// `Ok(vec![])` means everything the pack named is now on disk. Anything in the
/// returned list is a file the manifest pointed at that the recipient has to
/// fetch themselves — which is the design working rather than a shortfall: the
/// pack said where each mod lives instead of carrying somebody else's jar, and
/// a named list is more actionable than an instance that is quietly incomplete.
///
/// On any exit, including an error or a cancel, whatever landed from a server's
/// pack has already been claimed for the server. The caller saves a partial
/// import rather than discarding it, so the ownership has to be right before
/// the failure is reported, not only on success.
pub async fn import(
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

    let outcome = async {
        instance::modpack::import_into(client, paths, instance, archive, curseforge_key, progress)
            .await?;
        match &manifest {
            Some(manifest) => {
                apply_pack(client, paths, instance, archive, manifest, curseforge_key, progress)
                    .await
            }
            // A plain `.mrpack` has no Geode half.
            None => Ok(Vec::new()),
        }
    }
    .await;

    if manifest.as_ref().is_some_and(|m| m.kind == Kind::Server) {
        instance::content::claim_for_server(paths, instance).await?;
    }
    outcome
}

/// Apply the Geode half to the instance the mrpack half just built.
async fn apply_pack(
    client: &reqwest::Client,
    paths: &Paths,
    instance: &mut Instance,
    archive: &Path,
    manifest: &Manifest,
    curseforge_key: Option<&str>,
    progress: &Progress,
) -> Result<Vec<String>> {
    apply(manifest, instance);
    extract_media(paths, instance, archive, manifest).await?;
    resolve_references(client, paths, instance, manifest, curseforge_key, progress).await
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
        use std::io::Read;
        let file = std::fs::File::open(&archive)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let mut out = Vec::new();
        for (entry, is_icon) in wanted {
            let Ok(mut source) = zip.by_name(&entry) else {
                continue;
            };
            // The name on disk is ours, never the pack's. Taking the entry's
            // file name looks safe and is not: on Windows `C:evil.dll` is a
            // single component that `join` treats as a drive-relative path and
            // writes wherever the process happens to be running. Only the
            // extension is taken, and only one the app would accept from its
            // own image picker.
            let Some(ext) = Path::new(&entry)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif"))
            else {
                continue;
            };
            let name = format!("{}.{ext}", if is_icon { "icon" } else { "banner" });

            // The same ceiling as a picture chosen by hand, and enforced on the
            // bytes rather than the header, which a crafted zip can understate.
            const MAX_BYTES: u64 = 8 * 1024 * 1024;
            let mut dest = std::fs::File::create(media_dir.join(&name))?;
            let copied = std::io::copy(&mut (&mut source).take(MAX_BYTES + 1), &mut dest)?;
            if copied > MAX_BYTES {
                drop(dest);
                let _ = std::fs::remove_file(media_dir.join(&name));
                continue;
            }
            out.push((name, is_icon));
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
            // Refused, and said so. An empty list has to mean everything the
            // pack named arrived — and a pack naming `../../` is exactly the
            // one somebody most needs to hear about.
            missing.push(format!("{} (not a safe path, so it was not fetched)", note.path));
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

        // Arrives in whichever state the sharer had it. The record stays on the
        // enabled name either way, which is how every record is keyed.
        let dest = if note.disabled {
            match disabled_name(rel) {
                Some(off) => game_dir.join(off),
                None => game_dir.join(rel),
            }
        } else {
            game_dir.join(rel)
        };
        downloads.push(
            crate::net::Download::new(file.url.clone(), dest.clone())
                .sha1(file.sha1.clone())
                .size(file.size)
                .label(note.path.clone()),
        );
        records.push((dest, instance::content::ContentRecord {
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
        }));
    }

    progress.stage("Downloading referenced content", downloads.len() as u64).await;
    let fetched = crate::net::fetch_all(client, downloads, progress, 8).await;

    // Record what arrived even if something else did not. A download writes to
    // a `.part` file and renames only on success, so a destination that exists
    // is one that completed. Dropping every record because one of five failed
    // would leave four working jars as untracked files — and a CurseForge jar
    // cannot be re-identified by hash later, so they would stay that way.
    instance
        .content
        .extend(records.into_iter().filter(|(dest, _)| dest.is_file()).map(|(_, r)| r));
    fetched?;

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

    use crate::instance::content::Origin;

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
        let mut arrived = fresh("arrived");
        let missing = import_offline(&paths, &mut arrived, &dest).await.unwrap();

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

    /// Both halves together, in the order the app runs them. This is what
    /// proves `geode/` entries do not confuse the override extractor, that
    /// nothing Geode adds leaks into the game directory, and that a config the
    /// author wrote actually arrives.
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

        let mut arrived = fresh("full-arrived");
        let missing = import_offline(&paths, &mut arrived, &dest).await.unwrap();

        assert!(missing.is_empty(), "nothing here needed fetching: {missing:?}");
        assert_eq!(arrived.name, "Full Round");
        assert_eq!(arrived.overrides.java_args.as_deref(), Some("-XX:+UseG1GC"));

        let game = paths.game_dir("full-arrived");
        assert!(game.join("config").join("author.toml").is_file(), "the author's config did not arrive");
        assert!(game.join("mods").join("homemade.jar").is_file(), "the jar with no link did not arrive");
        assert!(!game.join("geode").exists(), "the Geode half was unpacked into the game directory");
    }

    /// The bug the first version shipped. A jar with no public download travels
    /// as an override and lands with no record — so tagging records alone left
    /// exactly the mods a server most needs to keep, its private ones, as the
    /// only ones a player could delete.
    #[tokio::test]
    async fn a_server_pack_owns_even_the_jars_that_arrive_with_no_record() {
        let (paths, mut inst) = fixture("srv", &[("mods", "private-anticheat.jar")]).await;
        inst.content = vec![record("", "private-anticheat.jar", ContentKind::Mod)];

        let dest = paths.root.join("server.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(&dest, |m| m.kind = Kind::Server, &[]);

        let mut arrived = fresh("srv-arrived");
        import_offline(&paths, &mut arrived, &dest).await.unwrap();

        let owned = arrived
            .content
            .iter()
            .find(|r| r.file_name == "private-anticheat.jar")
            .expect("the override-carried jar was given no record");
        assert_eq!(owned.origin, Origin::Server);

        let mods = paths.game_dir("srv-arrived").join("mods");
        let err = crate::instance::content::remove_in(
            &mods,
            &mut arrived.content,
            ContentKind::Mod,
            "private-anticheat.jar",
        )
        .await
        .expect_err("a server's private jar must not be deletable");
        assert!(err.to_string().contains("cannot be removed"), "{err}");
        assert!(mods.join("private-anticheat.jar").is_file());
    }

    /// A server pack that stops halfway is still the server's. The caller saves
    /// a partial import rather than throwing it away, so ownership has to be
    /// settled before the failure is reported — not only when all went well.
    #[tokio::test]
    async fn a_cancelled_server_import_still_belongs_to_the_server() {
        let paths = Paths {
            root: std::env::temp_dir().join(format!("geode-cancel-{}", std::process::id())),
        };
        let _ = tokio::fs::remove_dir_all(&paths.root).await;
        tokio::fs::create_dir_all(&paths.root).await.unwrap();

        // One index file already present on the recipient's disk, so the
        // download step completes without the network and then notices the
        // cancel — deterministic, and offline.
        let mut arrived = fresh("cancel-arrived");
        let mods = paths.game_dir("cancel-arrived").join("mods");
        tokio::fs::create_dir_all(&mods).await.unwrap();
        tokio::fs::write(mods.join("indexed.jar"), b"indexed").await.unwrap();
        let sha1 = crate::net::sha1_file(&mods.join("indexed.jar")).await.unwrap();

        let index = serde_json::json!({
            "formatVersion": 1, "game": "minecraft", "versionId": "1", "name": "S",
            "files": [{
                "path": "mods/indexed.jar",
                "hashes": { "sha1": sha1, "sha512": "unused" },
                "env": { "client": "required", "server": "required" },
                "downloads": ["http://127.0.0.1:9/never-fetched.jar"],
                "fileSize": 7
            }],
            "dependencies": { "minecraft": "1.21.1" }
        });
        let manifest = Manifest {
            geode_format: FORMAT,
            kind: Kind::Server,
            profile: Profile { name: "Survival".into(), ..Default::default() },
            icon: None,
            banner: None,
            source: None,
            files: Vec::new(),
        };
        let dest = paths.root.join("cancel.geodepack");
        write_pack(&dest, &index, &manifest, &[("overrides/mods/private.jar", b"private")]);

        let progress = Progress::silent("t");
        progress.cancel_flag().store(true, std::sync::atomic::Ordering::Relaxed);
        let result = import(&reqwest::Client::new(), &paths, &mut arrived, &dest, None, &progress).await;

        assert!(result.is_err(), "the import was meant to be cancelled");
        for file in ["indexed.jar", "private.jar"] {
            let record = arrived
                .content
                .iter()
                .find(|r| r.file_name == file)
                .unwrap_or_else(|| panic!("{file} landed but was left unowned: {:?}", arrived.content));
            assert_eq!(record.origin, Origin::Server, "{file}");
        }
    }

    /// A pack from a person, not a server, leaves the recipient in charge of
    /// their own mods — they can delete any of them.
    #[tokio::test]
    async fn a_personal_pack_leaves_its_content_the_recipients_own() {
        let (paths, mut inst) = fixture("pers", &[("mods", "homemade.jar")]).await;
        inst.content = vec![record("", "homemade.jar", ContentKind::Mod)];

        let dest = paths.root.join("personal.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t"))
            .await
            .unwrap();

        let mut arrived = fresh("pers-arrived");
        import_offline(&paths, &mut arrived, &dest).await.unwrap();

        assert!(
            arrived.content.iter().all(|r| r.origin == Origin::User),
            "a personal pack must not lock the recipient out of their own mods"
        );
        let mods = paths.game_dir("pers-arrived").join("mods");
        crate::instance::content::remove_in(&mods, &mut arrived.content, ContentKind::Mod, "homemade.jar")
            .await
            .unwrap();
    }

    /// A reference is fetched fresh, so a mod the sharer turned off has to say
    /// so or it arrives switched on — the one they turned off because it
    /// crashed, now crashing for somebody else.
    #[tokio::test]
    async fn a_disabled_reference_carries_its_state() {
        let (paths, mut inst) = fixture(
            "offref",
            &[("mods", "crashes.jar.disabled"), ("mods", "fine.jar")],
        )
        .await;
        inst.content = vec![
            record("curseforge", "crashes.jar", ContentKind::Mod),
            record("curseforge", "fine.jar", ContentKind::Mod),
        ];

        let manifest = manifest_for(&paths, &inst).await;
        assert!(manifest.note("mods/crashes.jar").is_some_and(|n| n.disabled), "{:?}", manifest.files);
        assert!(manifest.note("mods/fine.jar").is_some_and(|n| !n.disabled), "{:?}", manifest.files);
        assert_eq!(disabled_name(Path::new("mods/crashes.jar")), Some(PathBuf::from("mods/crashes.jar.disabled")));
    }

    /// The name artwork is written under is Geode's, never the pack's. On
    /// Windows a name like `C:evil.png` joins as a drive-relative path and lands
    /// outside the media folder entirely; this cannot reproduce that on Linux,
    /// but it does pin that the pack's chosen name never reaches the disk.
    #[tokio::test]
    async fn a_pack_cannot_choose_where_its_artwork_is_written() {
        let (paths, inst) = fixture("art", &[("config", "x.toml")]).await;
        let dest = paths.root.join("art.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(
            &dest,
            |m| {
                m.icon = Some("geode/media/C:evil.png".into());
                m.banner = Some("geode/media/payload.dll".into());
            },
            &[
                ("geode/media/C:evil.png", b"PNG".to_vec()),
                ("geode/media/payload.dll", b"MZ".to_vec()),
            ],
        );

        let mut arrived = fresh("art-arrived");
        import_offline(&paths, &mut arrived, &dest).await.unwrap();

        assert_eq!(arrived.icon.as_deref(), Some("icon.png"));
        assert_eq!(arrived.banner, None, "a non-image banner must be refused");
        let names: Vec<String> = std::fs::read_dir(paths.instance_media("art-arrived"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["icon.png".to_string()], "{names:?}");
    }

    /// An "icon" is not a way to fill somebody's disk. Held to the same eight
    /// megabytes as a picture chosen by hand.
    #[tokio::test]
    async fn oversized_artwork_is_refused() {
        let (paths, inst) = fixture("big", &[("config", "x.toml")]).await;
        let dest = paths.root.join("big.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(
            &dest,
            |m| m.icon = Some("geode/media/icon.png".into()),
            &[("geode/media/icon.png", vec![0u8; 9 * 1024 * 1024])],
        );

        let mut arrived = fresh("big-arrived");
        import_offline(&paths, &mut arrived, &dest).await.unwrap();
        assert_eq!(arrived.icon, None);
        assert!(!paths.instance_media("big-arrived").join("icon.png").exists());
    }

    /// Refusing a path is not enough on its own; the refusal has to be said.
    /// An empty list means everything the pack named arrived, and a pack that
    /// names `../../` is exactly the one somebody most needs to hear about.
    #[tokio::test]
    async fn an_unsafe_reference_is_reported_rather_than_skipped() {
        let (paths, inst) = fixture("unsafe", &[("config", "x.toml")]).await;
        let dest = paths.root.join("unsafe.geodepack");
        export(&paths, &inst, dest.clone(), false, &Progress::silent("t"))
            .await
            .unwrap();
        rewrite_pack(
            &dest,
            |m| {
                m.files.push(FileNote {
                    path: "../../mods/escape.jar".into(),
                    side: None,
                    server_only: false,
                    source: Some(SourceRef {
                        provider: "curseforge".into(),
                        project_id: "1".into(),
                        version_id: "2".into(),
                    }),
                    disabled: false,
                    manual: false,
                })
            },
            &[],
        );

        let mut arrived = fresh("unsafe-arrived");
        let missing = import_offline(&paths, &mut arrived, &dest).await.unwrap();
        assert!(missing.iter().any(|m| m.contains("not a safe path")), "{missing:?}");
    }

    // -----------------------------------------------------------------------
    // Helpers for building and bending packs
    // -----------------------------------------------------------------------

    fn fresh(id: &str) -> Instance {
        let mut i = instance();
        i.id = id.into();
        i
    }

    /// The app's own import sequence, with no key and a client that is never
    /// used — none of these fixtures download anything.
    async fn import_offline(paths: &Paths, inst: &mut Instance, archive: &Path) -> Result<Vec<String>> {
        import(&reqwest::Client::new(), paths, inst, archive, None, &Progress::silent("t")).await
    }

    /// Rewrite an exported pack in place: edit its manifest, add entries.
    ///
    /// For building packs Geode would never write itself — a server pack, a
    /// hostile one — out of one it did, so the rest stays realistic.
    fn rewrite_pack(archive: &Path, edit: impl FnOnce(&mut Manifest), extra: &[(&str, Vec<u8>)]) {
        use std::io::{Read, Write};
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
            for i in 0..zip.len() {
                let mut e = zip.by_index(i).unwrap();
                let mut bytes = Vec::new();
                e.read_to_end(&mut bytes).unwrap();
                entries.push((e.name().to_string(), bytes));
            }
        }
        let mut manifest: Manifest = entries
            .iter()
            .find(|(n, _)| n == Manifest::PATH)
            .map(|(_, b)| serde_json::from_slice(b).unwrap())
            .expect("an exported pack has a manifest");
        edit(&mut manifest);

        let mut zip = zip::ZipWriter::new(std::fs::File::create(archive).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for (name, bytes) in entries.iter().filter(|(n, _)| n != Manifest::PATH) {
            zip.start_file(name.as_str(), options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.start_file(Manifest::PATH, options).unwrap();
        zip.write_all(&serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        for (name, bytes) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A pack built from parts, for the cases no export produces.
    fn write_pack(dest: &Path, index: &serde_json::Value, manifest: &Manifest, extra: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        zip.start_file("modrinth.index.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(index).unwrap()).unwrap();
        zip.start_file(Manifest::PATH, options).unwrap();
        zip.write_all(&serde_json::to_vec_pretty(manifest).unwrap()).unwrap();
        for (name, bytes) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_name_becomes_a_safe_file_name() {
        assert_eq!(file_name("All the Mods 10"), "All the Mods 10.geodepack");
        assert_eq!(file_name("bad/name:here"), "bad-name-here.geodepack");
        assert_eq!(file_name("   "), "pack.geodepack");
    }
}

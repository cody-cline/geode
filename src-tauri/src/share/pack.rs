//! Writing and reading a `.geodepack`.
//!
//! The file is an ordinary `.mrpack` with a `geode/` directory beside the
//! index. Writing it is the mrpack writer plus a few more entries; reading it
//! is the mrpack reader plus a pass that applies what the index had nowhere to
//! record. Neither half duplicates the other — see [`super::manifest`].

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::instance::{self, Instance};
use crate::net::Progress;
use crate::paths::Paths;

use super::manifest::{FileNote, Kind, Manifest, Profile, EXTENSION, FORMAT};

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

/// What each mod declared about itself, so an importer need not re-read jars.
async fn notes_for(paths: &Paths, instance: &Instance) -> Vec<FileNote> {
    let game_dir = paths.game_dir(&instance.id);
    let mut out = Vec::new();

    for record in &instance.content {
        let rel = PathBuf::from(record.kind.folder()).join(&record.file_name);
        let full = game_dir.join(&rel);
        let probe = full.clone();
        let side = tokio::task::spawn_blocking(move || instance::jar::read(&probe))
            .await
            .ok()
            .flatten()
            .and_then(|meta| meta.side);

        // Only files with something to say are recorded. A pack where every
        // mod runs on both sides carries no notes at all, which is the
        // difference between a manifest that annotates and one that repeats.
        if side.is_none() {
            continue;
        }
        out.push(FileNote {
            path: rel.to_string_lossy().replace('\\', "/"),
            side,
            server_only: false,
        });
    }
    out
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
    instance::modpack::export_mrpack(paths, instance, dest, include_saves, extras, progress).await
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

    #[test]
    fn a_name_becomes_a_safe_file_name() {
        assert_eq!(file_name("All the Mods 10"), "All the Mods 10.geodepack");
        assert_eq!(file_name("bad/name:here"), "bad-name-here.geodepack");
        assert_eq!(file_name("   "), "pack.geodepack");
    }
}

//! The IPC surface. Most commands validate, delegate and return; the install
//! commands additionally own the policy that decides which file goes where, and
//! launch owns the order its steps run in, because neither rule can be enforced
//! anywhere a caller could go round it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::auth::store::{Account, AccountView, AccountsFile};
use crate::error::{Error, Result};
use crate::instance::content::{ContentItem, ContentKind};
use crate::instance::{self, Instance};
use crate::meta::Loader;
use crate::net::Progress;
use crate::providers::{self, Project, Provider, SearchQuery, SearchResults, Version};
use crate::share;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<serde_json::Value> {
    Ok(state.config().await.redacted())
}

/// Saved settings. The CurseForge key travels separately so it is never part of
/// a routine settings round-trip.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPatch {
    pub memory_mb: Option<u32>,
    pub java_args: Option<String>,
    pub java_path: Option<Option<String>>,
    pub client_id: Option<Option<String>>,
    pub theme: Option<String>,
    pub accent: Option<String>,
    pub hide_on_launch: Option<bool>,
    pub concurrent_downloads: Option<usize>,
    pub show_snapshots: Option<bool>,
}

#[tauri::command]
pub async fn set_config(
    state: State<'_, AppState>,
    patch: ConfigPatch,
) -> Result<serde_json::Value> {
    let mut config = state.config().await;
    if let Some(v) = patch.memory_mb {
        config.memory_mb = v.clamp(512, 65536);
    }
    if let Some(v) = patch.java_args {
        config.java_args = v;
    }
    if let Some(v) = patch.java_path {
        config.java_path = v.filter(|s| !s.trim().is_empty());
    }
    if let Some(v) = patch.client_id {
        config.client_id = v.filter(|s| !s.trim().is_empty());
    }
    if let Some(v) = patch.theme {
        config.theme = v;
    }
    if let Some(v) = patch.accent {
        config.accent = v;
    }
    if let Some(v) = patch.hide_on_launch {
        config.hide_on_launch = v;
    }
    if let Some(v) = patch.concurrent_downloads {
        config.concurrent_downloads = v.clamp(1, 64);
    }
    if let Some(v) = patch.show_snapshots {
        config.show_snapshots = v;
    }

    config.save(&state.paths).await?;
    state.set_config(config.clone()).await;
    Ok(config.redacted())
}

#[tauri::command]
pub async fn set_curseforge_key(state: State<'_, AppState>, key: Option<String>) -> Result<bool> {
    let mut config = state.config().await;
    config.curseforge_api_key = key
        .filter(|k| !k.trim().is_empty())
        .map(|k| k.trim().to_string());
    config.save(&state.paths).await?;
    let has_key = config.effective_curseforge_key().is_some();
    state.set_config(config).await;
    Ok(has_key)
}

/// Try a key against CurseForge and report exactly what came back.
///
/// Takes the key to test so Settings can verify before saving; omit it to test
/// whatever is currently in effect.
#[tauri::command]
pub async fn test_curseforge_key(
    state: State<'_, AppState>,
    key: Option<String>,
) -> Result<providers::curseforge::KeyCheck> {
    let typed = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    let config = state.config().await;

    let (candidate, source) = match typed {
        Some(k) => (Some(k), "the key you typed"),
        None => {
            let saved = config
                .curseforge_api_key
                .as_ref()
                .map(|k| k.trim())
                .filter(|k| !k.is_empty())
                .map(str::to_string);
            match saved {
                Some(k) => (Some(k), "the key saved in Settings"),
                None => (
                    crate::config::default_curseforge_key(),
                    "the key compiled into this build",
                ),
            }
        }
    };

    let Some(candidate) = candidate else {
        return Ok(providers::curseforge::KeyCheck {
            ok: false,
            status: 0,
            message: "No CurseForge key is set.".into(),
            source: "nothing".into(),
            fingerprint: String::new(),
        });
    };

    Ok(providers::curseforge::check_key(&state.http, &candidate, source).await)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
    pub platform: String,
    pub has_client_id: bool,
}

/// Total physical RAM, so the memory slider has a meaningful upper bound
/// instead of an arbitrary one.
#[tauri::command]
pub async fn system_memory_mb() -> Result<u64> {
    use sysinfo::System;
    let mut system = System::new();
    system.refresh_memory();
    Ok((system.total_memory() / (1024 * 1024)).max(2048))
}

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo> {
    let config = state.config().await;
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        data_dir: state.paths.root.to_string_lossy().to_string(),
        platform: crate::meta::piston::current_os().to_string(),
        has_client_id: config.client_id.is_some(),
    })
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameVersion {
    pub id: String,
    pub kind: String,
    pub release_time: String,
}

#[tauri::command]
pub async fn list_game_versions(
    state: State<'_, AppState>,
    include_snapshots: bool,
) -> Result<Vec<GameVersion>> {
    let manifest = state.manifests.get(&state.http, &state.paths).await?;
    Ok(manifest
        .versions
        .into_iter()
        .filter(|v| include_snapshots || v.kind == "release")
        .map(|v| GameVersion {
            id: v.id,
            kind: v.kind,
            release_time: v.release_time,
        })
        .collect())
}

#[tauri::command]
pub async fn list_loader_versions(
    state: State<'_, AppState>,
    loader: String,
    game_version: String,
) -> Result<Vec<String>> {
    let loader = Loader::parse(&loader).ok_or_else(|| Error::msg("unknown loader"))?;
    crate::meta::loader_versions(&state.http, loader, &game_version).await
}

// ---------------------------------------------------------------------------
// Instances
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceView {
    #[serde(flatten)]
    pub instance: Instance,
    pub running: bool,
    pub mod_count: usize,
    /// Absolute paths to the uploaded images, or `None` when none is set or the
    /// file has since gone. Resolved here so the UI never builds a path or
    /// needs to know where the data directory is.
    pub icon_path: Option<String>,
    pub banner_path: Option<String>,
}

/// An uploaded image's full path, if it is still there.
fn media_path(dir: &std::path::Path, name: &Option<String>) -> Option<String> {
    let name = name.as_ref()?;
    let path = dir.join(name);
    path.is_file().then(|| path.to_string_lossy().to_string())
}

fn view(instance: Instance, state: &AppState) -> InstanceView {
    let media = state.paths.instance_media(&instance.id);
    InstanceView {
        running: state.games.is_running(&instance.id),
        mod_count: instance
            .content
            .iter()
            .filter(|c| c.kind == ContentKind::Mod)
            .count(),
        icon_path: media_path(&media, &instance.icon),
        banner_path: media_path(&media, &instance.banner),
        instance,
    }
}

/// Which picture is being set.
fn parse_slot(slot: &str) -> Result<&'static str> {
    match slot {
        "icon" => Ok("icon"),
        "banner" => Ok("banner"),
        _ => Err(Error::msg("unknown image slot")),
    }
}

/// Copy a chosen image into an owner's `media` folder and hand back its name.
///
/// The name carries a timestamp rather than being just `icon.png`: the webview
/// caches by URL, and reusing a name leaves it showing the picture you replaced.
async fn store_image(dir: &std::path::Path, slot: &str, source: &str) -> Result<String> {
    /// Generous for artwork, small enough that a mistaken video is refused.
    const MAX_BYTES: u64 = 8 * 1024 * 1024;

    let source = PathBuf::from(source);
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif"))
        .ok_or_else(|| Error::msg("pick a PNG, JPEG, WebP or GIF"))?;

    let size = tokio::fs::metadata(&source)
        .await
        .map_err(|_| Error::msg("that file could not be read"))?
        .len();
    if size > MAX_BYTES {
        return Err(Error::msg("that image is larger than 8 MB"));
    }

    tokio::fs::create_dir_all(dir).await?;
    let name = format!("{slot}-{}.{ext}", chrono::Utc::now().timestamp_millis());
    tokio::fs::copy(&source, dir.join(&name)).await?;
    Ok(name)
}

#[tauri::command]
pub async fn list_instances(state: State<'_, AppState>) -> Result<Vec<InstanceView>> {
    Ok(instance::list(&state.paths)
        .await?
        .into_iter()
        .map(|i| view(i, &state))
        .collect())
}

#[tauri::command]
pub async fn get_instance(state: State<'_, AppState>, id: String) -> Result<InstanceView> {
    Ok(view(instance::load(&state.paths, &id).await?, &state))
}

#[tauri::command]
pub async fn create_instance(
    state: State<'_, AppState>,
    name: String,
    game_version: String,
    loader: String,
    loader_version: String,
) -> Result<InstanceView> {
    let loader = Loader::parse(&loader).ok_or_else(|| Error::msg("unknown loader"))?;
    if loader != Loader::Vanilla && loader_version.trim().is_empty() {
        return Err(Error::msg(format!(
            "pick a {} version",
            loader.display_name()
        )));
    }
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::msg("give the instance a name"));
    }

    let created =
        instance::create(&state.paths, name, game_version, loader, loader_version).await?;
    Ok(view(created, &state))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstancePatch {
    pub name: Option<String>,
    pub icon: Option<Option<String>>,
    pub group: Option<Option<String>>,
    pub notes: Option<String>,
    pub game_version: Option<String>,
    pub loader: Option<String>,
    pub loader_version: Option<String>,
    pub overrides: Option<instance::InstanceOverrides>,
}

#[tauri::command]
pub async fn update_instance(
    state: State<'_, AppState>,
    id: String,
    patch: InstancePatch,
) -> Result<InstanceView> {
    let mut inst = instance::load(&state.paths, &id).await?;

    if let Some(name) = patch.name {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(Error::msg("the instance needs a name"));
        }
        inst.name = name;
    }
    if let Some(group) = patch.group {
        inst.group = group.filter(|g| !g.trim().is_empty());
    }
    if let Some(notes) = patch.notes {
        inst.notes = notes;
    }
    if let Some(v) = patch.game_version {
        inst.mc_version = v;
    }
    if let Some(l) = patch.loader {
        inst.loader = Loader::parse(&l).ok_or_else(|| Error::msg("unknown loader"))?;
    }
    if let Some(v) = patch.loader_version {
        inst.loader_version = v;
    }
    if let Some(o) = patch.overrides {
        inst.overrides = o;
    }

    inst.save(&state.paths).await?;
    Ok(view(inst, &state))
}

/// Set or clear an instance's icon or banner.
///
/// Its own command rather than part of the patch, because it has to touch the
/// filesystem: a picture is a file to copy, not a string to store.
#[tauri::command]
pub async fn set_instance_image(
    state: State<'_, AppState>,
    id: String,
    slot: String,
    source: Option<String>,
) -> Result<InstanceView> {
    let slot = parse_slot(&slot)?;
    let mut inst = instance::load(&state.paths, &id).await?;
    let dir = state.paths.instance_media(&id);

    let previous = if slot == "icon" {
        inst.icon.clone()
    } else {
        inst.banner.clone()
    };
    // The new file lands before the old one goes, so a failed copy leaves the
    // instance with the picture it already had.
    let next = match source {
        Some(path) => Some(store_image(&dir, slot, &path).await?),
        None => None,
    };
    if let Some(old) = previous {
        if Some(&old) != next.as_ref() {
            let _ = tokio::fs::remove_file(dir.join(old)).await;
        }
    }

    if slot == "icon" {
        inst.icon = next;
    } else {
        inst.banner = next;
    }
    inst.save(&state.paths).await?;
    Ok(view(inst, &state))
}

/// The same for a server.
#[tauri::command]
pub async fn set_server_image(
    state: State<'_, AppState>,
    id: String,
    slot: String,
    source: Option<String>,
) -> Result<ServerView> {
    let slot = parse_slot(&slot)?;
    let mut server = crate::server::load(&state.paths, &id).await?;
    let dir = state.paths.server_media(&id);

    let previous = if slot == "icon" {
        server.icon.clone()
    } else {
        server.banner.clone()
    };
    let next = match source {
        Some(path) => Some(store_image(&dir, slot, &path).await?),
        None => None,
    };
    if let Some(old) = previous {
        if Some(&old) != next.as_ref() {
            let _ = tokio::fs::remove_file(dir.join(old)).await;
        }
    }

    if slot == "icon" {
        server.icon = next;
    } else {
        server.banner = next;
    }
    server.save(&state.paths).await?;
    Ok(server_view(server, &state))
}

#[tauri::command]
pub async fn delete_instance(state: State<'_, AppState>, id: String) -> Result<()> {
    if state.games.is_running(&id) {
        return Err(Error::msg("stop the game before deleting this instance"));
    }
    instance::delete(&state.paths, &id).await
}

#[tauri::command]
pub async fn duplicate_instance(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<InstanceView> {
    let copy = instance::duplicate(&state.paths, &id, name.trim().to_string()).await?;
    Ok(view(copy, &state))
}

#[tauri::command]
pub async fn instance_folder(state: State<'_, AppState>, id: String) -> Result<String> {
    Ok(state.paths.game_dir(&id).to_string_lossy().to_string())
}

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

fn parse_kind(s: &str) -> Result<ContentKind> {
    ContentKind::parse(s).ok_or_else(|| Error::msg(format!("unknown content type '{s}'")))
}

#[tauri::command]
pub async fn list_content(
    state: State<'_, AppState>,
    id: String,
    kind: String,
) -> Result<Vec<ContentItem>> {
    let kind = parse_kind(&kind)?;
    let inst = instance::load(&state.paths, &id).await?;
    instance::content::list(&state.paths, &inst, kind, &state.jars).await
}

#[tauri::command]
pub async fn set_content_enabled(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    file: String,
    enabled: bool,
) -> Result<String> {
    let kind = parse_kind(&kind)?;
    instance::content::set_enabled(&state.paths, &id, kind, &file, enabled).await
}

/// Turn a joined instance's server-side mods on or off, and say how many
/// changed.
#[tauri::command]
pub async fn set_server_side_mods(
    state: State<'_, AppState>,
    id: String,
    on: bool,
) -> Result<usize> {
    // The game has its mods open, and a jar renamed under it would not stop it
    // loading, only mislead the player about what is running.
    if state.games.is_running(&id) {
        return Err(Error::msg("Stop the game before changing server-side mods"));
    }
    // A launch preparing may be updating from the server, renaming the very
    // jars this would rename, and it settles them from the setting as it saw it.
    if state.is_launching(&id) {
        return Err(Error::msg("Wait for the game to finish starting"));
    }
    let mut inst = instance::load(&state.paths, &id).await?;
    if inst.joined.is_none() {
        return Err(Error::msg(
            "only an instance joined from a server has server-side mods",
        ));
    }
    let changed = instance::content::set_server_side_mods(&state.paths, &mut inst, on).await?;
    inst.save(&state.paths).await?;
    Ok(changed)
}

#[tauri::command]
pub async fn remove_content(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    file: String,
) -> Result<()> {
    let kind = parse_kind(&kind)?;
    let mut inst = instance::load(&state.paths, &id).await?;
    instance::content::remove(&state.paths, &mut inst, kind, &file).await?;
    inst.save(&state.paths).await
}

/// Copy files the user picked (or dropped) into the right content folder.
#[tauri::command]
pub async fn add_local_content(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    files: Vec<String>,
) -> Result<usize> {
    let kind = parse_kind(&kind)?;
    add_local_files(&state.paths, &id, kind, &files).await
}

/// What [`add_local_content`] does, without an IPC handle in the way.
async fn add_local_files(
    paths: &crate::paths::Paths,
    id: &str,
    kind: ContentKind,
    files: &[String],
) -> Result<usize> {
    let inst = instance::load(paths, id).await?;

    // Copying over a server's mod replaces the jar while the record — and the
    // server badge on it — stays, which is the one thing every other way into
    // this folder refuses. Whichever name the file goes by on disk: a pack's
    // mod turned off is the same mod. Refused before anything is copied, so a
    // pick holding one leaves the folder exactly as it was.
    //
    // Case is folded here and nowhere else, because this is the one route whose
    // name comes off the player's own disk rather than from a record or a
    // provider: on macOS and Windows a `Sodium.jar` the player picked lands on
    // the server's `sodium.jar`, and on Linux it lands beside it, which is a
    // second copy of the same mod the loader will not start on.
    let refused = files
        .iter()
        .filter_map(|file| std::path::Path::new(file).file_name()?.to_str())
        .map(|name| instance::content::strip_disabled(name).0)
        .find(|base| {
            inst.content.iter().any(|r| {
                r.file_name.eq_ignore_ascii_case(base)
                    && r.kind == kind
                    && r.origin == instance::content::Origin::Server
            })
        });
    if let Some(base) = refused {
        return Err(Error::msg(format!(
            "{base} came from the server's pack, so it cannot be replaced here. \
             Turn it off instead if you do not want it."
        )));
    }

    let dir = instance::content::content_dir(paths, id, kind);
    tokio::fs::create_dir_all(&dir).await?;

    let mut added = 0;
    for file in files {
        let source = PathBuf::from(file);
        let Some(name) = source.file_name() else {
            continue;
        };
        if tokio::fs::copy(&source, dir.join(name)).await.is_ok() {
            added += 1;
        }
    }
    Ok(added)
}

/// Whether Fabric mods can be offered for this instance via Sinytra Connector.
///
/// Answered from the files Connector publishes, so it stays correct as it gains
/// or drops versions without anything here changing.
#[tauri::command]
pub async fn connector_support(
    state: State<'_, AppState>,
    game_version: String,
    loader: String,
) -> Result<crate::connector::ConnectorSupport> {
    crate::connector::support_for(&state.http, &state.connector, &game_version, &loader).await
}

#[tauri::command]
pub async fn install_content(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    provider: String,
    project_id: String,
    version_id: Option<String>,
    // Install the mod's Fabric build and bring in the Connector runtime.
    via_connector: Option<bool>,
) -> Result<crate::install::content::InstallReport> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let cf_key = state.cf_key().await;

    let mut inst = instance::load(&state.paths, &id).await?;
    let project = providers::project(&state.http, provider, &project_id, cf_key.as_deref()).await?;

    let via_connector = via_connector.unwrap_or(false);
    if via_connector {
        // Re-check rather than trusting the caller: the UI decides whether to
        // offer this, but the rule belongs with the thing that enforces it.
        let support = crate::connector::support_for(
            &state.http,
            &state.connector,
            &inst.mc_version,
            inst.loader.as_str(),
        )
        .await?;
        if !support.supported {
            return Err(Error::msg(format!(
                "Sinytra Connector has no build for Minecraft {} on {}, so Fabric mods \
                 cannot run there",
                inst.mc_version,
                inst.loader.display_name()
            )));
        }
    }

    // A Connector install wants the mod's Fabric build, not one for the
    // instance's own loader — that is the whole point of the route.
    let loader = if via_connector {
        Some("fabric")
    } else {
        (project.kind.install_kind() == Some(ContentKind::Mod)).then(|| inst.loader.as_str())
    };
    let versions = providers::versions(
        &state.http,
        provider,
        &project_id,
        Some(&inst.mc_version),
        loader,
        cf_key.as_deref(),
    )
    .await?;

    let version = match &version_id {
        Some(want) => versions
            .iter()
            .find(|v| &v.id == want)
            .ok_or_else(|| Error::msg("that version is no longer available"))?,
        None => crate::install::content::pick_best(&versions).ok_or_else(|| {
            Error::msg(format!(
                "{} has no release for Minecraft {} on {}",
                project.name,
                inst.mc_version,
                inst.loader.display_name()
            ))
        })?,
    };

    let progress = Progress::new(crate::desktop::sink(app), format!("install:{id}:{project_id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    // The runtime goes in first: a Fabric jar in a (Neo)Forge mods folder does
    // nothing on its own, and a failure here should stop before that happens.
    let mut connector_added = Vec::new();
    if via_connector {
        match crate::connector::ensure_stack(&state.http, &state.paths, &mut inst, &progress).await
        {
            Ok(added) => connector_added = added,
            Err(e) => {
                state.clear_cancel(progress.id());
                let _ = inst.save(&state.paths).await;
                progress.fail(&e.to_string()).await;
                return Err(e);
            }
        }
    }

    // A Connector install resolves the mod's dependencies as Fabric ones —
    // they run through the same layer, and asking for (Neo)Forge builds of them
    // would find nothing.
    let dep_game_version = inst.mc_version.clone();
    let dep_loader = if via_connector {
        "fabric"
    } else {
        inst.loader.as_str()
    };

    let result = crate::install::content::install_with_dependencies(
        &state.http,
        &state.paths,
        &mut inst,
        version,
        &project,
        &dep_game_version,
        dep_loader,
        cf_key.as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    match result {
        Ok(mut report) => {
            // Report the runtime alongside the mod so the toast can say what
            // else arrived and why.
            for name in connector_added {
                report.installed.push(name);
            }
            inst.save(&state.paths).await?;
            progress.finish().await;
            Ok(report)
        }
        Err(e) => {
            // Persist whatever did land so the folder and the manifest agree.
            let _ = inst.save(&state.paths).await;
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Install a modpack as a brand-new instance.
///
/// A pack is not content you add to an instance — it defines one, including its
/// Minecraft version and loader. So this downloads the pack, creates the
/// instance its manifest describes, and imports into that.
#[tauri::command]
pub async fn install_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: String,
    project_id: String,
    version_id: Option<String>,
    name: Option<String>,
) -> Result<InstanceView> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let cf_key = state.cf_key().await;

    let project = providers::project(&state.http, provider, &project_id, cf_key.as_deref()).await?;
    if project.kind != providers::ProjectKind::Modpack {
        return Err(Error::msg(format!("{} is not a modpack", project.name)));
    }

    // Packs are not filtered by loader or game version: the pack chooses those.
    let versions = providers::versions(
        &state.http,
        provider,
        &project_id,
        None,
        None,
        cf_key.as_deref(),
    )
    .await?;
    let version = match &version_id {
        Some(want) => versions
            .iter()
            .find(|v| &v.id == want)
            .ok_or_else(|| Error::msg("that pack version is no longer available"))?,
        None => crate::install::content::pick_best(&versions)
            .ok_or_else(|| Error::msg(format!("{} has no downloadable release", project.name)))?,
    };

    if let Some(reason) = &version.blocked_reason {
        return Err(Error::msg(reason.clone()));
    }
    let file = version
        .primary_file()
        .ok_or_else(|| Error::msg(format!("{} has no downloadable file", project.name)))?;

    let progress = Progress::new(crate::desktop::sink(app), format!("pack:{}", project.id));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let archive = state.paths.cache().join("packs").join(&file.filename);
    progress.stage("Downloading pack", 1).await;
    let downloaded = crate::net::fetch_one(
        &state.http,
        &crate::net::Download::new(file.url.clone(), archive.clone())
            .sha1(file.sha1.clone())
            .size(file.size)
            .label(project.name.clone()),
    )
    .await;

    if let Err(e) = downloaded {
        state.clear_cancel(progress.id());
        progress.fail(&e.to_string()).await;
        return Err(e);
    }
    progress.advance(1, &file.filename).await;

    let preview = instance::modpack::preview(archive.clone()).await?;
    if preview.needs_curseforge_key && cf_key.is_none() {
        state.clear_cancel(progress.id());
        let e = Error::msg(
            "this CurseForge pack needs your CurseForge API key to resolve its \
             contents — add one in Settings",
        );
        progress.fail(&e.to_string()).await;
        return Err(e);
    }

    let display_name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| project.name.clone());

    let mut inst = instance::create(
        &state.paths,
        display_name,
        preview.mc_version,
        preview.loader,
        preview.loader_version,
    )
    .await?;

    let result = instance::modpack::import_into(
        &state.http,
        &state.paths,
        &mut inst,
        &archive,
        cf_key.as_deref(),
        &progress,
    )
    .await;

    // Record where it came from so updates and re-imports have provenance.
    inst.pack = Some(instance::PackSource::from_project(
        &project,
        version.version_number.clone(),
        version.id.clone(),
    ));

    state.clear_cancel(progress.id());
    // Save regardless: a partly imported pack is more useful than a directory
    // the launcher refuses to acknowledge.
    inst.save(&state.paths).await?;

    match result {
        Ok(()) => {
            progress.finish().await;
            Ok(view(inst, &state))
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Identify files that arrived without provenance and link them to their
/// project, so they get a real name and join update checks.
#[tauri::command]
pub async fn link_untracked_content(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<crate::install::content::LinkReport> {
    let mut inst = instance::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app), format!("link:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let result = crate::install::content::link_untracked(
        &state.http,
        &state.paths,
        &mut inst,
        &state.jars,
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    match result {
        Ok(report) => {
            inst.save(&state.paths).await?;
            Ok(report)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn check_content_updates(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::install::content::UpdateCandidate>> {
    let config = state.config().await;
    let inst = instance::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app), format!("updates:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let result = crate::install::content::check_updates(
        &state.http,
        &inst.content,
        &inst.mc_version,
        Some(inst.loader.as_str()),
        config.effective_curseforge_key().as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    if let Err(e) = &result {
        progress.fail(&e.to_string()).await;
    }
    result
}

#[tauri::command]
pub async fn apply_content_update(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    provider: String,
    project_id: String,
    version_id: String,
) -> Result<crate::install::content::InstallReport> {
    install_content(app, state, id, provider, project_id, Some(version_id), None).await
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn search_projects(
    state: State<'_, AppState>,
    query: SearchQuery,
) -> Result<SearchResults> {
    let config = state.config().await;
    providers::search(
        &state.http,
        &query,
        config.effective_curseforge_key().as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn get_project(
    state: State<'_, AppState>,
    provider: String,
    id: String,
) -> Result<Project> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let config = state.config().await;
    providers::project(
        &state.http,
        provider,
        &id,
        config.effective_curseforge_key().as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn get_project_description(
    state: State<'_, AppState>,
    provider: String,
    id: String,
) -> Result<String> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let config = state.config().await;
    providers::description(
        &state.http,
        provider,
        &id,
        config.effective_curseforge_key().as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn get_project_versions(
    state: State<'_, AppState>,
    provider: String,
    id: String,
    game_version: Option<String>,
    loader: Option<String>,
) -> Result<Vec<Version>> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let config = state.config().await;
    providers::versions(
        &state.http,
        provider,
        &id,
        game_version.as_deref(),
        loader.as_deref(),
        config.effective_curseforge_key().as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn get_categories(state: State<'_, AppState>, kind: String) -> Result<Vec<String>> {
    let kind =
        providers::ProjectKind::parse(&kind).ok_or_else(|| Error::msg("unknown project type"))?;
    providers::modrinth::categories(&state.http, kind).await
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_plans(state: State<'_, AppState>) -> Result<Vec<crate::plan::Plan>> {
    crate::plan::list(&state.paths).await
}

#[tauri::command]
pub async fn get_plan(state: State<'_, AppState>, id: String) -> Result<crate::plan::Plan> {
    crate::plan::load(&state.paths, &id).await
}

#[tauri::command]
pub async fn create_plan(state: State<'_, AppState>, name: String) -> Result<crate::plan::Plan> {
    crate::plan::create(&state.paths, name).await
}

/// Save a whole plan. The list of mods is small and edited as a unit, so the
/// frontend owns it and writes it back rather than issuing an add/remove call
/// per row.
///
/// Every mod enters a plan through here, whatever route it came by, which makes
/// this the one place worth asking whether a CurseForge entry is really a
/// Modrinth mod wearing the wrong name.
#[tauri::command]
pub async fn save_plan(
    state: State<'_, AppState>,
    mut plan: crate::plan::Plan,
) -> Result<crate::plan::Plan> {
    crate::plan::prefer::prefer_modrinth(&state.http, &mut plan.mods, &state.plan_twins).await;
    // Rewriting identities can turn two entries into one, so de-duplication —
    // which `save` does — has to happen after, not before.
    crate::plan::save(&state.paths, plan).await
}

#[tauri::command]
pub async fn delete_plan(state: State<'_, AppState>, id: String) -> Result<()> {
    crate::plan::delete(&state.paths, &id).await
}

/// Work out which loader and game version can run this plan.
#[tauri::command]
pub async fn resolve_plan(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<crate::plan::resolve::Resolution> {
    let plan = crate::plan::load(&state.paths, &id).await?;
    let config = state.config().await;
    let progress = Progress::new(crate::desktop::sink(app), format!("plan:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    // Being unable to read Connector's matrix costs us the Connector option,
    // not the answer — `resolve` says so in its warnings.
    let matrix = state.connector.get(&state.http).await.unwrap_or_default();

    let result = crate::plan::resolve::resolve(
        &state.http,
        &plan,
        &matrix,
        &state.plan_support,
        config.effective_curseforge_key().as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    match result {
        Ok(resolution) => {
            progress.finish().await;
            Ok(resolution)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Resolve a plan and find the target that was asked for, as its index into
/// the resolution.
///
/// Re-resolved rather than trusting a target round-tripped through the
/// frontend: the per-mod verdicts decide which file each mod gets, and they
/// are cheap to recompute against the session cache.
///
/// The index rather than the target itself, because a `PlanTarget` borrows from
/// the resolution it was found in.
///
/// Both callers have already made a progress job and registered its cancel flag
/// by the time they get here, so a failure is ended here too: the dock drops a
/// job only when it hears one is done, and a resolve that Modrinth cannot answer
/// would otherwise leave a bar and a live flag behind for the rest of the
/// session.
async fn resolved_target(
    state: &AppState,
    plan: &crate::plan::Plan,
    loader: &str,
    game_version: &str,
    cf_key: Option<&str>,
    progress: &Progress,
) -> Result<(crate::plan::resolve::Resolution, usize)> {
    let found: Result<(crate::plan::resolve::Resolution, usize)> = async {
        let matrix = state.connector.get(&state.http).await.unwrap_or_default();
        let resolution = crate::plan::resolve::resolve(
            &state.http,
            plan,
            &matrix,
            &state.plan_support,
            cf_key,
            progress,
        )
        .await?;

        let chosen = resolution
            .targets
            .iter()
            .position(|t| t.loader == loader && t.game_version == game_version)
            .ok_or_else(|| {
                Error::msg(format!(
                    "{loader} {game_version} is no longer one of this plan's options — \
                     resolve it again"
                ))
            })?;
        Ok((resolution, chosen))
    }
    .await;

    if let Err(e) = &found {
        state.clear_cancel(progress.id());
        progress.fail(&e.to_string()).await;
    }
    found
}

/// Create an instance at the chosen target and install the whole plan into it.
#[tauri::command]
pub async fn build_plan(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    loader: String,
    game_version: String,
    name: Option<String>,
) -> Result<crate::plan::build::BuildReport> {
    let plan = crate::plan::load(&state.paths, &id).await?;
    let cf_key = state.cf_key().await;

    let progress = Progress::new(crate::desktop::sink(app), format!("plan-build:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let (resolution, chosen) = resolved_target(
        &state,
        &plan,
        &loader,
        &game_version,
        cf_key.as_deref(),
        &progress,
    )
    .await?;

    let result = crate::plan::build::build(
        &state.http,
        &state.paths,
        &plan,
        &resolution.targets[chosen],
        name,
        cf_key.as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    match result {
        Ok(report) => {
            progress.finish().await;
            Ok(report)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// List the mods in a published modpack, stripped of the version it was built
/// for, so they can be copied into a plan.
#[tauri::command]
pub async fn pack_contents(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: String,
    project_id: String,
    version_id: Option<String>,
) -> Result<Vec<crate::plan::PlanMod>> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let config = state.config().await;
    let progress = Progress::new(crate::desktop::sink(app), format!("pack-read:{project_id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let result = crate::plan::pack::contents(
        &state.http,
        &state.paths,
        provider,
        &project_id,
        version_id.as_deref(),
        config.effective_curseforge_key().as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    if let Err(e) = &result {
        progress.fail(&e.to_string()).await;
    }
    result
}

// ---------------------------------------------------------------------------
// Servers
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerView {
    #[serde(flatten)]
    pub server: crate::server::Server,
    pub running: bool,
    /// What *this* server can do, rather than what its kind can. Cardboard
    /// makes a Fabric server take plugins, and the UI should not have to
    /// rediscover that from the content list.
    pub takes_plugins: bool,
    pub takes_mods: bool,
    pub can_add_cardboard: bool,
    /// Whether its software is in place, which is not the same as having a jar
    /// — modern Forge and NeoForge servers have none.
    pub installed: bool,
    /// The software's proper name — "NeoForge", not the "neoforge" the id uses.
    /// Sent rather than mapped in the UI, so there is one list of these.
    pub kind_name: String,
    /// Whether it hosts a world of its own. A proxy does not, so it has no
    /// place to put datapacks and no level to configure.
    pub has_world: bool,
    /// The loader its mods are built for — "forge" for a Mohist server, not
    /// "mohist". Sent rather than derived in the UI so the hybrids stay right
    /// in one place: this is what a search has to be narrowed to, and getting
    /// it wrong is how a Forge server ends up being offered Fabric mods.
    pub mod_loader: Option<String>,
    /// Absolute paths to the uploaded images, resolved the same way an
    /// instance's are.
    pub icon_path: Option<String>,
    pub banner_path: Option<String>,
}

fn server_view(server: crate::server::Server, state: &AppState) -> ServerView {
    let media = state.paths.server_media(&server.id);
    ServerView {
        icon_path: media_path(&media, &server.icon),
        banner_path: media_path(&media, &server.banner),
        running: state.servers.is_running(&server.id),
        takes_plugins: server.takes_plugins(),
        takes_mods: server.takes_mods(),
        can_add_cardboard: server.can_add_cardboard(),
        installed: server.is_installed(),
        kind_name: server.kind.display_name().to_string(),
        has_world: !server.kind.is_proxy(),
        mod_loader: server.kind.mod_loader().map(str::to_string),
        // Moved last, so the capabilities above can still read it.
        server,
    }
}

/// What the create dialog needs to describe each choice.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerKindInfo {
    pub id: String,
    pub name: String,
    pub takes_plugins: bool,
    pub takes_mods: bool,
    pub is_proxy: bool,
}

fn kind_info(kind: crate::server::ServerKind) -> ServerKindInfo {
    ServerKindInfo {
        id: kind.as_str().to_string(),
        name: kind.display_name().to_string(),
        takes_plugins: kind.takes_plugins(),
        takes_mods: kind.takes_mods(),
        is_proxy: kind.is_proxy(),
    }
}

#[tauri::command]
pub async fn server_kinds() -> Result<Vec<ServerKindInfo>> {
    Ok(crate::server::ServerKind::ALL
        .iter()
        .map(|&kind| kind_info(kind))
        .collect())
}

/// Which server software could run this modpack.
///
/// Answered from the published version's own loader list rather than by
/// downloading the pack, so the choice can be offered before committing to a
/// few hundred megabytes. A Forge pack comes back as Forge, Mohist and
/// Arclight — the last two run the same mods and take Bukkit plugins as well,
/// which is the reason for asking rather than assuming.
#[tauri::command]
pub async fn server_kinds_for_pack(
    state: State<'_, AppState>,
    provider: String,
    project_id: String,
    version_id: Option<String>,
) -> Result<Vec<ServerKindInfo>> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let cf_key = state.cf_key().await;

    // Packs are never filtered by loader or version: the pack chose those.
    let versions =
        providers::versions(&state.http, provider, &project_id, None, None, cf_key.as_deref())
            .await?;
    let version = match &version_id {
        Some(want) => versions.iter().find(|v| &v.id == want),
        None => crate::install::content::pick_best(&versions),
    }
    .ok_or_else(|| Error::msg("that pack version is no longer available"))?;

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for loader in &version.loaders {
        for kind in crate::server::from_pack::kinds_for(loader) {
            if seen.insert(kind) {
                out.push(kind_info(kind));
            }
        }
    }
    Ok(out)
}

/// Install a modpack as a server rather than as an instance.
#[tauri::command]
pub async fn create_server_from_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    provider: String,
    project_id: String,
    version_id: Option<String>,
    kind: String,
    name: Option<String>,
) -> Result<crate::server::from_plan::ServerBuildReport> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let kind = parse_server_kind(&kind)?;
    let cf_key = state.cf_key().await;

    let progress = Progress::new(crate::desktop::sink(app), format!("pack-server:{project_id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let result = async {
        let contents = crate::plan::pack::detailed(
            &state.http,
            &state.paths,
            provider,
            &project_id,
            version_id.as_deref(),
            cf_key.as_deref(),
            &progress,
        )
        .await?;
        crate::server::from_pack::from_modpack(
            &state.http,
            &state.paths,
            &contents,
            kind,
            name,
            cf_key.as_deref(),
            &progress,
        )
        .await
    }
    .await;

    state.clear_cancel(progress.id());
    match result {
        Ok(report) => {
            progress.finish().await;
            Ok(report)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Build a plan into a server rather than an instance.
///
/// The plan resolves exactly as it would for an instance; what differs is that
/// mods which do not run on a server go into its Client mods, and that most
/// targets cannot be a server at all.
#[tauri::command]
pub async fn create_server_from_plan(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    loader: String,
    game_version: String,
    name: Option<String>,
) -> Result<crate::server::from_plan::ServerBuildReport> {
    let plan = crate::plan::load(&state.paths, &id).await?;
    let cf_key = state.cf_key().await;

    let progress = Progress::new(crate::desktop::sink(app), format!("plan-server:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let (resolution, chosen) = resolved_target(
        &state,
        &plan,
        &loader,
        &game_version,
        cf_key.as_deref(),
        &progress,
    )
    .await?;

    let result = crate::server::from_plan::from_plan(
        &state.http,
        &state.paths,
        &plan,
        &resolution.targets[chosen],
        name,
        cf_key.as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    match result {
        Ok(report) => {
            progress.finish().await;
            Ok(report)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Log files, and handing them to somebody who can read them
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_instance_logs(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::logs::LogFile>> {
    crate::instance::logs::list(&state.paths, &id).await
}

#[tauri::command]
pub async fn read_instance_log(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<String> {
    crate::instance::logs::read(&state.paths, &id, &name).await
}

#[tauri::command]
pub async fn delete_instance_log(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<()> {
    crate::instance::logs::delete(&state.paths, &id, &name).await
}

/// A report about an instance, ready to paste to someone who can diagnose it.
///
/// `log_name` picks which log to include; without one it uses whatever the
/// caller is looking at, passed in as `live`. The console holds only what was
/// captured since the window opened, and the run that crashed is usually the
/// one nobody was watching — so a named file is the more useful input.
#[tauri::command]
pub async fn instance_report(
    state: State<'_, AppState>,
    id: String,
    log_name: Option<String>,
    live: Option<String>,
) -> Result<String> {
    let inst = instance::load(&state.paths, &id).await?;

    let mut facts = vec![("Minecraft".to_string(), inst.mc_version.clone())];
    facts.push((
        "Loader".to_string(),
        if inst.loader == crate::meta::Loader::Vanilla {
            "Vanilla".to_string()
        } else {
            format!("{} {}", inst.loader.as_str(), inst.loader_version)
        },
    ));
    if let Some(pack) = &inst.pack {
        facts.push(("From pack".to_string(), format!("{} {}", pack.name, pack.version)));
    }
    if let Some(mb) = inst.overrides.memory_mb {
        facts.push(("Memory".to_string(), format!("{mb} MB")));
    }
    if let Some(args) = &inst.overrides.java_args {
        facts.push(("Java arguments".to_string(), args.clone()));
    }

    let mods = instance::content::list(
        &state.paths,
        &inst,
        crate::instance::content::ContentKind::Mod,
        &state.jars,
    )
    .await
    .unwrap_or_default();

    let subject = crate::diagnostics::Subject {
        kind: "instance",
        name: inst.name.clone(),
        facts,
        content: mods
            .iter()
            .map(|m| describe_content(&m.name, &m.version, m.enabled))
            .collect(),
        crash_dir: Some(crate::instance::logs::crash_dir(&state.paths, &id)),
    };

    let log = pick_log(
        crate::instance::logs::path(&state.paths, &id, log_name.as_deref().unwrap_or("latest.log")),
        live,
    )
    .await;
    Ok(crate::diagnostics::report(&subject, &log).await)
}

/// The same, for a server.
#[tauri::command]
pub async fn server_report(
    state: State<'_, AppState>,
    id: String,
    log_name: Option<String>,
    live: Option<String>,
) -> Result<String> {
    let server = crate::server::load(&state.paths, &id).await?;

    let mut facts = vec![
        ("Minecraft".to_string(), server.mc_version.clone()),
        (
            "Software".to_string(),
            format!("{} {}", server.kind.display_name(), server.build),
        ),
        ("Memory".to_string(), format!("{} MB", server.settings.memory_mb)),
    ];
    if let Some(major) = server.java_major {
        facts.push(("Java".to_string(), major.to_string()));
    }
    if let Some(args) = &server.settings.java_args {
        facts.push(("Java arguments".to_string(), args.clone()));
    }

    // A hybrid runs both, and which one a broken thing came from is exactly
    // what a reader needs to know.
    let mut content = Vec::new();
    for kind in [ContentKind::Mod, ContentKind::Plugin] {
        if (kind == ContentKind::Mod && !server.takes_mods())
            || (kind == ContentKind::Plugin && !server.takes_plugins())
        {
            continue;
        }
        let dir = crate::server::content_dir(&state.paths, &id, kind).await;
        let items = crate::instance::content::list_in(&dir, &server.content, kind, &state.jars)
            .await
            .unwrap_or_default();
        for item in items {
            let label = describe_content(&item.name, &item.version, item.enabled);
            content.push(match kind {
                ContentKind::Plugin => format!("{label} (plugin)"),
                _ => label,
            });
        }
    }

    let subject = crate::diagnostics::Subject {
        kind: "server",
        name: server.name.clone(),
        facts,
        content,
        crash_dir: Some(crate::server::logs::crash_dir(&state.paths, &id)),
    };

    let log = pick_log(
        crate::server::logs::path(&state.paths, &id, log_name.as_deref().unwrap_or("latest.log")),
        live,
    )
    .await;
    Ok(crate::diagnostics::report(&subject, &log).await)
}

/// A disabled mod is still worth listing — "I disabled it and it still breaks"
/// is information, and a reader cannot infer it from a name.
fn describe_content(name: &str, version: &str, enabled: bool) -> String {
    let base = if version.is_empty() {
        name.to_string()
    } else {
        format!("{name} {version}")
    };
    if enabled {
        base
    } else {
        format!("{base} [disabled]")
    }
}

/// Prefer the log file, fall back to whatever the console had.
///
/// The file is the better source — it covers the whole run — but a server that
/// died before writing one, or an instance launched from elsewhere, leaves only
/// what was captured live.
async fn pick_log(path: Result<std::path::PathBuf>, live: Option<String>) -> String {
    if let Ok(path) = path {
        if let Ok(text) = crate::diagnostics::tail_of(&path).await {
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    live.unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Servers: software and lifecycle
// ---------------------------------------------------------------------------

/// Every command this server has, as the server itself reported them.
///
/// Written by the probe that rides along on the server's own JVM, so it covers
/// vanilla, every mod and every plugin alike, and needs nobody to join. Empty
/// until the server has run once — there is nowhere else this could be known
/// from, since a mod declares its commands nowhere but in its code.
#[tauri::command]
pub async fn server_commands(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::server::probe::CommandNode>> {
    Ok(crate::server::probe::read(&state.paths, &id).await)
}

/// Whether this machine has the Git that compiling Spigot needs, and what
/// Geode would do about it if not.
///
/// Asked by the create dialog before Spigot is confirmed, so the answer arrives
/// while there is still something to do about it rather than ten minutes into a
/// build.
#[tauri::command]
pub async fn git_status(state: State<'_, AppState>) -> Result<crate::server::git::GitStatus> {
    Ok(crate::server::git::status(&state.paths).await)
}

/// Get Git, having been told to.
///
/// Only ever called from a confirmation, because on Linux and macOS this
/// installs software system-wide — which is also why the dialog shows the
/// exact command first.
#[tauri::command]
pub async fn install_git(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::server::git::GitStatus> {
    let progress = Progress::new(crate::desktop::sink(app), "git".to_string());
    state.register_cancel(progress.id(), progress.cancel_flag());
    let result = crate::server::git::install(&state.http, &state.paths, &progress).await;
    state.clear_cancel(progress.id());
    match result {
        Ok(status) => {
            progress.finish().await;
            Ok(status)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

fn parse_server_kind(s: &str) -> Result<crate::server::ServerKind> {
    crate::server::ServerKind::parse(s)
        .ok_or_else(|| Error::msg(format!("unknown server type '{s}'")))
}

#[tauri::command]
pub async fn server_versions(state: State<'_, AppState>, kind: String) -> Result<Vec<String>> {
    crate::server::meta::versions(&state.http, parse_server_kind(&kind)?).await
}

#[tauri::command]
pub async fn list_servers(state: State<'_, AppState>) -> Result<Vec<ServerView>> {
    Ok(crate::server::list(&state.paths)
        .await?
        .into_iter()
        .map(|s| server_view(s, &state))
        .collect())
}

#[tauri::command]
pub async fn get_server(state: State<'_, AppState>, id: String) -> Result<ServerView> {
    Ok(server_view(
        crate::server::load(&state.paths, &id).await?,
        &state,
    ))
}

/// Create the server and put its jar in place.
#[tauri::command]
pub async fn create_server(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    kind: String,
    version: String,
) -> Result<ServerView> {
    let kind = parse_server_kind(&kind)?;
    let mut server = crate::server::create(&state.paths, name, kind, version).await?;

    let progress = Progress::new(crate::desktop::sink(app), format!("server:{}", server.id));
    state.register_cancel(progress.id(), progress.cancel_flag());
    let result =
        crate::server::install::install_jar(&state.http, &state.paths, &mut server, &progress).await;
    state.clear_cancel(progress.id());

    match result {
        Ok(()) => {
            progress.finish().await;
            Ok(server_view(server, &state))
        }
        Err(e) => {
            // The folder stays: a server with no jar is something the UI can
            // offer to retry, and deleting it would throw away the name and
            // settings the user just chose.
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn delete_server(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<()> {
    if state.servers.is_running(&id) {
        return Err(Error::msg("stop the server before deleting it"));
    }
    let deleted = crate::server::delete(&state.paths, &id).await;
    // Whichever way the delete went. If that was the last one shared, the pack
    // server and the router's port go too, rather than staying open for a
    // server that no longer exists.
    reconcile_sharing(&app, &state).await;
    deleted
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPatch {
    pub name: Option<String>,
    pub notes: Option<String>,
    pub settings: Option<crate::server::ServerSettings>,
    /// The whole list, edited as a unit — there are only ever a handful.
    pub schedules: Option<Vec<crate::server::schedule::Schedule>>,
}

#[tauri::command]
pub async fn update_server(
    state: State<'_, AppState>,
    id: String,
    patch: ServerPatch,
) -> Result<ServerView> {
    let mut server = crate::server::load(&state.paths, &id).await?;
    if let Some(name) = patch.name {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(Error::msg("the server needs a name"));
        }
        server.name = name;
    }
    if let Some(notes) = patch.notes {
        server.notes = notes;
    }
    if let Some(settings) = patch.settings {
        server.settings = settings;
    }
    if let Some(mut schedules) = patch.schedules {
        // A new schedule is stamped as if it had just run, so adding one does
        // not immediately fire it.
        let now = chrono::Utc::now().timestamp();
        for schedule in &mut schedules {
            if schedule.last_run.is_none() {
                schedule.last_run = Some(now);
            }
        }
        server.schedules = schedules;
    }
    server.save(&state.paths).await?;
    Ok(server_view(server, &state))
}

/// Record that the user accepted Mojang's EULA, and write `eula.txt`.
///
/// Its own command because it is the user's act: the UI asks, they answer, and
/// only then does the file exist.
#[tauri::command]
pub async fn accept_server_eula(state: State<'_, AppState>, id: String) -> Result<ServerView> {
    let mut server = crate::server::load(&state.paths, &id).await?;
    crate::server::accept_eula(&state.paths, &mut server).await?;
    Ok(server_view(server, &state))
}

/// Install Cardboard, which lets a Fabric server load Bukkit plugins.
///
/// Its own command rather than an ordinary mod install because of what it
/// changes: the `plugins` folder means nothing until Cardboard is there to read
/// it, and afterwards this server should be offered plugins it could not run
/// before.
#[tauri::command]
pub async fn add_cardboard(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<ServerView> {
    let mut server = crate::server::load(&state.paths, &id).await?;

    if server.kind.takes_plugins() {
        return Err(Error::msg(format!(
            "a {} server already runs Bukkit plugins on its own",
            server.kind.display_name()
        )));
    }
    if !matches!(
        server.kind,
        crate::server::ServerKind::Fabric | crate::server::ServerKind::Quilt
    ) {
        return Err(Error::msg(format!(
            "Cardboard is a Fabric mod, so it cannot run on a {} server",
            server.kind.display_name()
        )));
    }
    if server.has_cardboard() {
        return Err(Error::msg("Cardboard is already installed here"));
    }

    let progress = Progress::new(crate::desktop::sink(app), format!("server:{}", server.id));
    progress.stage("Finding Cardboard", 1).await;

    // One block, one failure path: every step below can fail, and the dock
    // drops a job only when it hears one is done, so a step that returned
    // straight out would leave its bar there for the rest of the session.
    let result = async {
        let slug = crate::server::CARDBOARD_SLUG;
        let project = providers::project(&state.http, Provider::Modrinth, slug, None).await?;
        // Quilt runs Fabric mods, and Cardboard only publishes for Fabric.
        let versions = providers::versions(
            &state.http,
            Provider::Modrinth,
            slug,
            Some(&server.mc_version),
            Some("fabric"),
            None,
        )
        .await?;
        let best = crate::install::content::pick_best(&versions).ok_or_else(|| {
            Error::msg(format!(
                "Cardboard has no build for Minecraft {}",
                server.mc_version
            ))
        })?;
        progress.advance(1, &project.name).await;

        let run = state.paths.server_run(&server.id);
        progress.stage("Installing Cardboard", 1).await;
        crate::install::content::install_into(
            &state.http,
            &run.join(ContentKind::Mod.folder()),
            &mut server.content,
            best,
            ContentKind::Mod,
            &project.name,
            project.icon_url.clone(),
            Some(project.slug.clone()),
            false,
            project.listed_side(),
        )
        .await?;

        // The folder a plugin gets dropped into should exist before anyone goes
        // looking for it.
        tokio::fs::create_dir_all(run.join(ContentKind::Plugin.folder())).await?;
        server.save_if_present(&state.paths).await
    }
    .await;

    if let Err(e) = result {
        progress.fail(&e.to_string()).await;
        return Err(e);
    }
    progress.finish().await;

    Ok(server_view(server, &state))
}

/// A server that started, and the mods moved out of its Mods first.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStart {
    pub pid: u32,
    /// File names moved to Client mods, so the app can say so on whichever
    /// tab the admin pressed Start from.
    pub moved: Vec<String>,
}

#[tauri::command]
pub async fn start_server(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<ServerStart> {
    if state.servers.is_running(&id) {
        return Err(Error::msg("that server is already running"));
    }
    let mut server = crate::server::load(&state.paths, &id).await?;

    // While nothing is running, so the server never loads a mod that only
    // runs in players' games. Saved straight away, so the moves stay recorded
    // even if the start fails, and said in the console too, which is the only
    // place a start that fails later still says it.
    let moved = crate::server::move_client_only_mods(&state.paths, &mut server).await;
    if !moved.is_empty() {
        server.save_if_present(&state.paths).await?;
        let sink = crate::desktop::sink(app.clone());
        for name in &moved {
            crate::server::schedule::announce(
                &sink,
                &id,
                format!("Moved {name} to Client mods: it only runs in players' games"),
            );
        }
    }

    // The server rotates its own log as it starts, so this is the moment the
    // oldest archive becomes one too many.
    if let Some(keep) = server.settings.keep_logs {
        let _ = crate::server::logs::prune(&state.paths, &id, keep as usize).await;
    }

    let progress = Progress::new(crate::desktop::sink(app.clone()), format!("server:{id}"));
    let java = match server.settings.java_path.clone() {
        Some(path) => PathBuf::from(path),
        None => match server.java_major {
            Some(major) => {
                crate::java::ensure(&state.http, &state.paths, major, None, &progress).await?
            }
            // Nothing told us which Java this wants — a proxy, or a jar
            // installed before we started asking. The newest one is the best
            // guess available.
            None => crate::java::detect(&state.paths)
                .await
                .into_iter()
                .next()
                .map(|j| j.path)
                .ok_or_else(|| Error::msg("no Java runtime was found on this system"))?,
        },
    };

    let pid = crate::server::run::start(
        &crate::desktop::sink(app.clone()),
        &state.paths,
        &server,
        java,
        state.servers.clone(),
    )
    .await?;

    server.last_started = Some(chrono::Utc::now().timestamp());
    server.save_if_present(&state.paths).await?;
    Ok(ServerStart { pid, moved })
}

/// Ask the server to stop the way an operator would, so it saves first.
#[tauri::command]
pub async fn stop_server(state: State<'_, AppState>, id: String) -> Result<bool> {
    let server = crate::server::load(&state.paths, &id).await?;
    Ok(state.servers.stop(&id, server.kind))
}

#[tauri::command]
pub async fn kill_server(state: State<'_, AppState>, id: String) -> Result<bool> {
    Ok(state.servers.kill(&id))
}

#[tauri::command]
pub async fn send_server_command(
    state: State<'_, AppState>,
    id: String,
    command: String,
) -> Result<bool> {
    Ok(state.servers.send(&id, &command))
}

#[tauri::command]
pub async fn running_servers(state: State<'_, AppState>) -> Result<Vec<String>> {
    Ok(state.servers.running_ids())
}

#[tauri::command]
pub async fn server_folder(state: State<'_, AppState>, id: String) -> Result<String> {
    Ok(state.paths.server_run(&id).to_string_lossy().to_string())
}

// ---------------------------------------------------------------------------
// Servers: content
// ---------------------------------------------------------------------------

/// Plugins or mods installed in a server, read from the folder itself.
#[tauri::command]
pub async fn list_server_content(
    state: State<'_, AppState>,
    id: String,
    kind: String,
) -> Result<Vec<ContentItem>> {
    let kind = parse_kind(&kind)?;
    let server = crate::server::load(&state.paths, &id).await?;
    let dir = crate::server::content_dir(&state.paths, &id, kind).await;
    instance::content::list_in(&dir, &server.content, kind, &state.jars).await
}

#[tauri::command]
pub async fn remove_server_content(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    file: String,
) -> Result<()> {
    let kind = parse_kind(&kind)?;
    let mut server = crate::server::load(&state.paths, &id).await?;
    let dir = crate::server::content_dir(&state.paths, &id, kind).await;
    instance::content::remove_in(&dir, &mut server.content, kind, &file).await?;
    server.save(&state.paths).await
}

#[tauri::command]
pub async fn set_server_content_enabled(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    file: String,
    enabled: bool,
) -> Result<String> {
    let kind = parse_kind(&kind)?;
    let dir = crate::server::content_dir(&state.paths, &id, kind).await;
    instance::content::set_enabled_in(&dir, &file, enabled).await
}

/// Copy files the user picked into one of a server's folders.
///
/// The same affordance an instance has. Nothing is looked up: a file put here
/// by hand stays untracked until something identifies it, exactly as it would
/// in an instance.
#[tauri::command]
pub async fn add_local_server_content(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    files: Vec<String>,
) -> Result<usize> {
    let kind = parse_kind(&kind)?;
    let dir = crate::server::content_dir(&state.paths, &id, kind).await;
    tokio::fs::create_dir_all(&dir).await?;

    let mut added = 0;
    for file in files {
        let source = PathBuf::from(&file);
        let Some(name) = source.file_name() else {
            continue;
        };
        if tokio::fs::copy(&source, dir.join(name)).await.is_ok() {
            added += 1;
        }
    }
    Ok(added)
}

/// Newer versions available for what is installed in a server.
#[tauri::command]
pub async fn check_server_content_updates(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::install::content::UpdateCandidate>> {
    let config = state.config().await;
    let server = crate::server::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app), format!("updates:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let result = crate::install::content::check_updates(
        &state.http,
        &server.content,
        &server.mc_version,
        server.kind.mod_loader(),
        config.effective_curseforge_key().as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    if let Err(e) = &result {
        progress.fail(&e.to_string()).await;
    }
    result
}

/// Install a named newer version over what a server has.
///
/// The kind travels with it because a server's folders are not implied by the
/// project: the same file is a mod in one folder and a plugin in another.
#[tauri::command]
pub async fn apply_server_content_update(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    provider: String,
    project_id: String,
    version_id: String,
    kind: String,
) -> Result<crate::install::content::InstallReport> {
    install_server_content(app, state, id, provider, project_id, Some(version_id), kind).await
}

/// Identify files dropped into a server's folders by hand, by their hashes.
#[tauri::command]
pub async fn link_untracked_server_content(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<crate::install::content::LinkReport> {
    let mut server = crate::server::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app), format!("link:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    // Only the folders this server actually has. Looking in the others would
    // hash nothing and report on a folder that does not exist.
    let mut wanted = Vec::new();
    if server.takes_mods() {
        wanted.extend([ContentKind::Mod, ContentKind::ClientMod]);
    }
    if server.takes_plugins() {
        wanted.push(ContentKind::Plugin);
    }
    if !server.kind.is_proxy() {
        wanted.push(ContentKind::DataPack);
    }

    let mut folders = Vec::new();
    for kind in wanted {
        folders.push((kind, crate::server::content_dir(&state.paths, &id, kind).await));
    }

    let result = crate::install::content::link_untracked_in(
        &state.http,
        &folders,
        &mut server.content,
        &state.jars,
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    match result {
        Ok(report) => {
            server.save_if_present(&state.paths).await?;
            Ok(report)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Install a plugin or mod into a server.
///
/// The caller says which it is, because the two live in different folders and
/// a project's own type cannot always tell them apart — Modrinth lists plenty
/// of multi-platform projects as mods that are equally plugins.
#[tauri::command]
pub async fn install_server_content(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    provider: String,
    project_id: String,
    version_id: Option<String>,
    kind: String,
) -> Result<crate::install::content::InstallReport> {
    let provider = Provider::parse(&provider).ok_or_else(|| Error::msg("unknown provider"))?;
    let kind = parse_kind(&kind)?;
    let cf_key = state.cf_key().await;

    let mut server = crate::server::load(&state.paths, &id).await?;
    let project = providers::project(&state.http, provider, &project_id, cf_key.as_deref()).await?;
    let kind = install_folder(kind, version_id.as_deref(), &project, &server.content);

    // SpigotMC lists Bukkit plugins and nothing else. A proxy speaks its own
    // plugin API and would take the jar, write it into plugins/, and fail at
    // start-up for a reason nothing had mentioned.
    if provider == Provider::Spiget && kind == ContentKind::Plugin {
        let speaks_bukkit = server
            .plugin_loaders()
            .iter()
            .any(|l| *l == "bukkit" || *l == "spigot");
        if !speaks_bukkit {
            return Err(Error::msg(format!(
                "{} is a Bukkit plugin, and a {} server does not load those. \
                 Look for a {} plugin instead.",
                project.name,
                server.kind.display_name(),
                server
                    .plugin_loaders()
                    .first()
                    .copied()
                    .unwrap_or("different"),
            )));
        }
    }

    // A Paper server runs Paper, Spigot and Bukkit plugins alike, so each is
    // tried in turn rather than insisting on the most specific one.
    // Asked of the server rather than its kind: a Fabric server with Cardboard
    // installed takes Bukkit plugins, and the kind alone cannot know that.
    let loaders: Vec<&str> = match kind {
        ContentKind::Plugin => server.plugin_loaders(),
        ContentKind::Mod | ContentKind::ClientMod => server.kind.mod_loader().into_iter().collect(),
        _ => Vec::new(),
    };

    // Falling back to "any loader" is right for a plugin and wrong for a mod.
    // Plugins are routinely published with no loader tag at all — most of
    // SpigotMC predates the idea — so insisting on one would refuse files that
    // work. A mod always declares its loader, so an untagged match is not a
    // lenient reading of the metadata, it is a Fabric jar being put into a
    // Forge server, where it will not load and will not say why.
    let allow_any_loader = !matches!(kind, ContentKind::Mod | ContentKind::ClientMod);
    let attempts: Vec<Option<&str>> = loaders
        .iter()
        .map(|l| Some(*l))
        .chain(allow_any_loader.then_some(None))
        .collect();

    let mut chosen: Option<Version> = None;
    for loader in attempts {
        let versions = providers::versions(
            &state.http,
            provider,
            &project_id,
            Some(&server.mc_version),
            loader,
            cf_key.as_deref(),
        )
        .await
        .unwrap_or_default();

        if let Some(want) = &version_id {
            if let Some(exact) = versions.iter().find(|v| &v.id == want) {
                chosen = Some(exact.clone());
                break;
            }
        } else if let Some(best) = crate::install::content::pick_best(&versions) {
            chosen = Some(best.clone());
            break;
        }
        // Only fall through to an unfiltered query when there was a filter to
        // drop; otherwise the last pass repeats the first.
        if loaders.is_empty() {
            break;
        }
    }

    let version = chosen.ok_or_else(|| {
        Error::msg(format!(
            "{} has no build for {} {}",
            project.name,
            server.kind.display_name(),
            server.mc_version
        ))
    })?;

    let progress = Progress::new(crate::desktop::sink(app), format!("server-content:{id}:{project_id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());
    let dir = crate::server::content_dir(&state.paths, &id, kind).await;

    // The same walk an instance gets, from the same code: a mod that needs
    // three others brings the three. The loader is whichever one matched the
    // version above, so a plugin's dependencies are looked up as plugins.
    let dep_loader = loaders.first().copied().unwrap_or_default();
    let mc_version = server.mc_version.clone();
    let result = crate::install::content::install_with_dependencies_into(
        &state.http,
        &dir,
        &mut server.content,
        &version,
        &project,
        kind,
        &mc_version,
        dep_loader,
        cf_key.as_deref(),
        &progress,
    )
    .await;

    // Saved either way: a walk that failed part-way through still put files on
    // disk, and a record list that does not mention them is worse than none.
    let _ = server.save_if_present(&state.paths).await;

    state.clear_cancel(progress.id());
    match result {
        Ok(report) => {
            progress.finish().await;
            Ok(report)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Which of a server's folders a new install goes into.
///
/// A mod Modrinth lists as not running on servers is kept for players in
/// Client mods rather than put where the server would load it. Only a fresh
/// install is routed: an update names its version and stays in the folder it
/// is in, and so does a project the server already has in Mods, where the
/// admin put it or kept it.
fn install_folder(
    asked: ContentKind,
    version_id: Option<&str>,
    project: &crate::providers::Project,
    records: &[crate::instance::content::ContentRecord],
) -> ContentKind {
    let in_mods = records.iter().any(|r| {
        r.kind == ContentKind::Mod
            && r.provider == project.provider.as_str()
            && r.project_id == project.id
    });
    if asked == ContentKind::Mod
        && version_id.is_none()
        && project.listed_side() == Some(crate::instance::jar::Side::Client)
        && !in_mods
    {
        ContentKind::ClientMod
    } else {
        asked
    }
}

/// Move a server's mod between Mods and Client mods, record and all.
///
/// Refused while the server runs. The loader read `mods/` when it started, so
/// a move would change nothing until a restart, and the Content tab would
/// stop describing the server that is running.
#[tauri::command]
pub async fn move_server_mod(
    state: State<'_, AppState>,
    id: String,
    file: String,
    to_client: bool,
) -> Result<()> {
    if state.servers.is_running(&id) {
        return Err(Error::msg(
            "Stop the server before moving mods between folders",
        ));
    }
    let mut server = crate::server::load(&state.paths, &id).await?;
    crate::server::move_mod(&state.paths, &mut server, &file, to_client).await?;
    server.save(&state.paths).await
}

// ---------------------------------------------------------------------------
// Servers: logs, backups and properties
// ---------------------------------------------------------------------------

/// The server's logs: the one it is writing now, and the ones it rotated.
#[tauri::command]
pub async fn list_server_logs(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::server::logs::LogFile>> {
    crate::server::logs::list(&state.paths, &id).await
}

/// One log as text, decompressed if it has been rotated.
#[tauri::command]
pub async fn read_server_log(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<String> {
    crate::server::logs::read(&state.paths, &id, &name).await
}

#[tauri::command]
pub async fn delete_server_log(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<()> {
    crate::server::logs::delete(&state.paths, &id, &name).await
}

#[tauri::command]
pub async fn list_server_backups(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::server::backup::BackupInfo>> {
    crate::server::backup::list(&state.paths, &id).await
}

#[tauri::command]
pub async fn create_server_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<crate::server::backup::BackupInfo> {
    let server = crate::server::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app), format!("server-backup:{id}"));
    // No cancel flag is registered, so the dock's Cancel does nothing here on
    // purpose: whether a half-written archive is safe to abandon is undecided.
    // Registering one would not be enough on its own either — `backup::create`
    // zips the whole run directory inside one `spawn_blocking` with nowhere to
    // read the flag.

    let result = crate::server::backup::create(&state.paths, &id, &progress).await;
    match result {
        Ok(info) => {
            progress.finish().await;
            // Retention is the point of the setting; applying it only when a
            // new backup lands keeps the folder bounded without a sweeper.
            if let Some(keep) = server.settings.keep_backups {
                let _ = crate::server::backup::prune(&state.paths, &id, keep as usize).await;
            }
            Ok(info)
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn delete_server_backup(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<()> {
    crate::server::backup::delete(&state.paths, &id, &name).await
}

/// Unpack a backup over the server's files.
///
/// Refused while it is running: restoring underneath a live server would have
/// it writing over what was just put back.
#[tauri::command]
pub async fn restore_server_backup(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<()> {
    if state.servers.is_running(&id) {
        return Err(Error::msg("stop the server before restoring a backup"));
    }
    crate::server::backup::restore(&state.paths, &id, &name).await
}

/// The server's own `server.properties`. Empty until it has been started once,
/// because the server writes that file rather than us.
#[tauri::command]
pub async fn read_server_properties(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::server::properties::Property>> {
    crate::server::properties::read(&state.paths, &id).await
}

#[tauri::command]
pub async fn write_server_properties(
    state: State<'_, AppState>,
    id: String,
    properties: Vec<crate::server::properties::Property>,
) -> Result<()> {
    crate::server::properties::write(&state.paths, &id, &properties).await
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_accounts(state: State<'_, AppState>) -> Result<Vec<AccountView>> {
    Ok(AccountsFile::load(&state.paths).await?.views())
}

#[tauri::command]
pub async fn begin_microsoft_login(
    state: State<'_, AppState>,
) -> Result<crate::auth::DeviceCodeStart> {
    let config = state.config().await;
    let client_id = config.client_id.ok_or_else(|| {
        Error::msg(
            "Microsoft sign-in needs an Azure client ID. Add one in Settings — \
             see the README for how to register one.",
        )
    })?;
    crate::auth::begin_device_code(&state.http, &client_id).await
}

/// Blocks until the user finishes signing in on Microsoft's page.
#[tauri::command]
pub async fn complete_microsoft_login(
    state: State<'_, AppState>,
    start: crate::auth::DeviceCodeStart,
) -> Result<AccountView> {
    let config = state.config().await;
    let client_id = config
        .client_id
        .ok_or_else(|| Error::msg("no Microsoft client ID is configured"))?;

    let msa = crate::auth::poll_device_code(&state.http, &client_id, &start).await?;
    let (token, expires_at, profile) =
        crate::auth::minecraft_session(&state.http, &msa.access_token).await?;

    let mut file = AccountsFile::load(&state.paths).await?;
    let account = Account {
        id: crate::auth::store::dash_uuid(&profile.id),
        username: profile.name,
        kind: crate::auth::store::AccountKind::Microsoft,
        mc_access_token: Some(token),
        mc_expires_at: expires_at,
        msa_refresh_token: msa.refresh_token,
        added_at: chrono::Utc::now().timestamp(),
        source_path: None,
        source_label: None,
    };
    let id = account.id.clone();
    file.upsert(account);
    file.save(&state.paths).await?;

    file.views()
        .into_iter()
        .find(|v| v.id == id)
        .ok_or_else(|| Error::msg("account was not saved"))
}

/// Sessions found in other launchers on this machine.
///
/// Needs no Azure application: the token belongs to the user's own Minecraft
/// account, and the launcher that issued it stays responsible for renewing it.
#[tauri::command]
pub async fn find_importable_accounts(
    state: State<'_, AppState>,
) -> Result<crate::auth::import::ImportScan> {
    Ok(crate::auth::import::discover(&state.http).await)
}

#[tauri::command]
pub async fn import_account(state: State<'_, AppState>, uuid: String) -> Result<AccountView> {
    // Re-discover rather than trusting a token round-tripped through the
    // frontend — the candidate list deliberately does not serialise one.
    let candidate = crate::auth::import::discover_candidates(&state.http)
        .await
        .into_iter()
        .find(|c| c.uuid == uuid)
        .ok_or_else(|| Error::msg("that session is no longer available — try scanning again"))?;

    let mut file = AccountsFile::load(&state.paths).await?;
    let account = Account::imported(&candidate);
    let id = account.id.clone();
    file.upsert(account);
    file.save(&state.paths).await?;

    file.views()
        .into_iter()
        .find(|v| v.id == id)
        .ok_or_else(|| Error::msg("account was not saved"))
}

#[tauri::command]
pub async fn add_offline_account(
    state: State<'_, AppState>,
    username: String,
) -> Result<AccountView> {
    let username = username.trim().to_string();
    // Mojang's own rules for a Java username; rejecting here beats a confusing
    // failure once the game is already running.
    if username.len() < 3 || username.len() > 16 {
        return Err(Error::msg("usernames are 3 to 16 characters"));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(Error::msg("usernames may only use letters, digits and _"));
    }

    let mut file = AccountsFile::load(&state.paths).await?;
    let account = Account::offline(username);
    let id = account.id.clone();
    file.upsert(account);
    file.save(&state.paths).await?;

    file.views()
        .into_iter()
        .find(|v| v.id == id)
        .ok_or_else(|| Error::msg("account was not saved"))
}

#[tauri::command]
pub async fn remove_account(state: State<'_, AppState>, id: String) -> Result<Vec<AccountView>> {
    let mut file = AccountsFile::load(&state.paths).await?;
    file.remove(&id);
    file.save(&state.paths).await?;
    Ok(file.views())
}

#[tauri::command]
pub async fn set_active_account(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<AccountView>> {
    let mut file = AccountsFile::load(&state.paths).await?;
    if !file.accounts.iter().any(|a| a.id == id) {
        return Err(Error::msg("no such account"));
    }
    file.active = Some(id);
    file.save(&state.paths).await?;
    Ok(file.views())
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn detect_java(state: State<'_, AppState>) -> Result<Vec<crate::java::JavaInstall>> {
    Ok(crate::java::detect(&state.paths).await)
}

#[tauri::command]
pub async fn install_java(
    app: AppHandle,
    state: State<'_, AppState>,
    major: u32,
) -> Result<crate::java::JavaInstall> {
    let progress = Progress::new(crate::desktop::sink(app), format!("java:{major}"));
    // No cancel flag is registered, so the dock's Cancel does nothing here on
    // purpose: whether a half-extracted JDK is safe to abandon is undecided.
    // Registering one would not be enough on its own either — `java::provision`
    // downloads through `net::fetch_one`, which never reads the flag, and
    // unpacks inside one `spawn_blocking`.
    let result = crate::java::provision(&state.http, &state.paths, major, &progress).await;
    match &result {
        Ok(_) => progress.finish().await,
        Err(e) => progress.fail(&e.to_string()).await,
    }
    result
}

// ---------------------------------------------------------------------------
// Launch
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchResult {
    pub pid: u32,
    pub instance_id: String,
    /// What the player should hear about once the game is on its way: an
    /// update that could not be checked for, or one that changed something
    /// they had a say in.
    pub notices: Vec<String>,
}

/// Whether the server is offering a revision this instance does not hold.
///
/// A server that names none — an admin on a Geode from before revisions — is
/// never behind, because there is nothing to compare it with.
fn update_due(stored: Option<&str>, offered: &str) -> bool {
    !offered.is_empty() && stored != Some(offered)
}

/// Bring a joined instance up to its server's current pack, before it plays.
///
/// Returned notices are for after the game starts; an error means it must not.
/// The difference is what the player can do about it. A server that cannot be
/// reached, or that no longer takes this instance's password, leaves them with
/// a working instance that may be a little behind, and telling them to wait
/// would be telling them not to play a single-player world either. An update
/// the server does say is due is different: a client that does not match its
/// server cannot join it, so a download or an import that does not finish
/// stops the launch, and the next Play runs it again.
async fn bring_up_to_date(
    state: &AppState,
    inst: &mut Instance,
    progress: &Progress,
) -> Result<Vec<String>> {
    let Some(link) = inst.joined.clone() else {
        return Ok(Vec::new());
    };
    let address = link.address.display();
    progress.stage("Checking the server for changes", 1).await;

    let info = match share::join::preview(link.clone()).await {
        Ok(preview) => preview.info,
        // Nothing was touched in either case, so the instance is as playable as
        // it was a moment ago.
        //
        // The refusal names both of its causes, because they read the same from
        // here: a server whose sharing is stopped knows none of its passwords
        // either, and answering a stranger differently for a server that exists
        // would be telling them it exists.
        Err(Error::PasswordRefused) => {
            return Ok(vec![format!(
                "{address} did not accept this instance's password, so Geode could not check it for changes: the password may have been replaced, or that server may not be shared right now. If the admin sends you a new link, open it — Geode will save the new password to this instance."
            )])
        }
        Err(e) => {
            return Ok(vec![format!(
                "Geode could not reach {address} to check for changes, so this instance may be out of date: {e}"
            )])
        }
    };

    // Learned from the first description a server new enough to send one gives,
    // so an instance joined before this can be found again under a new
    // password from here on.
    let learned_id = !info.server_id.is_empty()
        && inst.joined_server_id.as_deref() != Some(info.server_id.as_str());
    if learned_id {
        inst.joined_server_id = Some(info.server_id.clone());
    }
    if !update_due(inst.joined_revision.as_deref(), &info.revision) {
        if learned_id {
            save_learned_id(&state.paths, &inst.id, &info.server_id).await?;
        }
        return Ok(Vec::new());
    }

    let archive =
        share::join::incoming_dir(&state.paths).join(format!("{}.geodepack", uuid::Uuid::new_v4()));
    let config = state.config().await;
    if let Err(e) = share::join::download(&link, &archive, progress).await {
        let _ = tokio::fs::remove_file(&archive).await;
        // A pack that never arrived reached nothing of the instance, so there
        // is nothing of the update to write — only the server's id, when this
        // was the first description to name one.
        if learned_id {
            save_learned_id(&state.paths, &inst.id, &info.server_id).await?;
        }
        return Err(e);
    }
    let updated = share::pack::update(
        &state.http,
        &state.paths,
        inst,
        &archive,
        config.effective_curseforge_key().as_deref(),
        progress,
    )
    .await;
    let _ = tokio::fs::remove_file(&archive).await;

    let report = match updated {
        Ok(report) => report,
        Err(e) => {
            // As far as it got is saved: what landed is the server's, and the
            // revision is untouched, so the next Play finishes the job. A save
            // that fails as well is not what to say here — what stopped the
            // update is, and a cancel has its own wording downstream.
            let _ = save_update(&state.paths, inst).await;
            return Err(e);
        }
    };
    // What the pack itself carried, when it carried anything: the description
    // was worked out before the pack was built, so its revision can name a
    // moment this instance does not hold.
    inst.joined_revision = Some(report.revision.clone().unwrap_or(info.revision));
    save_update(&state.paths, inst).await?;

    let mut notices = Vec::new();
    if !report.turned_off.is_empty() {
        notices.push(own_copies_off(&report.turned_off));
    }
    if !report.missing.is_empty() {
        notices.push(fetch_by_hand(&report.missing));
    }
    Ok(notices)
}

/// Write what an update decided onto the instance as it stands on disk now.
///
/// The launch loaded its copy before any of this ran, and an update takes as
/// long as a download and an import take, which on a big pack is minutes.
/// Saving that copy whole would put back a name, notes or a memory setting the
/// player changed while they waited — the same reason the launch reads the
/// instance again before recording it as played. Only what the update decides
/// crosses over; the rest of an instance is the player's, and theirs is the
/// newer copy.
async fn save_update(paths: &crate::paths::Paths, updated: &Instance) -> Result<()> {
    let mut inst = instance::load(paths, &updated.id).await?;
    inst.mc_version = updated.mc_version.clone();
    inst.loader = updated.loader;
    inst.loader_version = updated.loader_version.clone();
    inst.content = updated.content.clone();
    inst.pack = updated.pack.clone();
    inst.joined_server_id = updated.joined_server_id.clone();
    inst.joined_revision = updated.joined_revision.clone();
    inst.save(paths).await
}

/// Record the server's id on an instance joined before servers sent one, and
/// nothing else: no files were touched, so no other field of the launch's copy
/// has any business being written over what is on disk.
async fn save_learned_id(paths: &crate::paths::Paths, id: &str, server_id: &str) -> Result<()> {
    let mut inst = instance::load(paths, id).await?;
    inst.joined_server_id = Some(server_id.to_string());
    inst.save(paths).await
}

/// What to say about a player's own mods that are off because the server has
/// the same mod.
///
/// A state rather than something just done: some of these were already off
/// before this update, and an update that runs again after a failure says the
/// same thing again.
fn own_copies_off(names: &[String]) -> String {
    let (verb, it) = if names.len() == 1 {
        ("stays", "it")
    } else {
        ("stay", "them")
    };
    format!(
        "Your own {} {verb} off: the server provides {it}.",
        names.join(", ")
    )
}

/// What to say about the files a server's pack pointed at that could not be
/// fetched — the same on an update as on the join that made the instance.
fn fetch_by_hand(missing: &[String]) -> String {
    format!(
        "{} file(s) in this server's pack have to be fetched by hand: {}",
        missing.len(),
        missing.join(", ")
    )
}

#[tauri::command]
pub async fn launch_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    account_id: Option<String>,
) -> Result<LaunchResult> {
    if state.games.is_running(&id) {
        return Err(Error::msg("that instance is already running"));
    }
    // Held until this returns. Two launches preparing at once would update the
    // instance from its server twice over, each rewriting the other's files.
    let Some(_claim) = state.claim_launch(&id) else {
        return Err(Error::msg("that instance is already starting"));
    };

    let config = state.config().await;
    let mut inst = instance::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app.clone()), format!("launch:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    // Resolve the account first: a sign-in problem should surface before we
    // spend minutes downloading.
    let session = crate::auth::store::session_for(
        &state.http,
        &state.paths,
        config.client_id.as_deref(),
        account_id.as_deref(),
    )
    .await
    .inspect_err(|e| {
        let progress = progress.clone();
        let msg = e.to_string();
        tauri::async_runtime::spawn(async move { progress.fail(&msg).await });
    })?;

    // Before the Java and version work: an update can move the instance to
    // another Minecraft or loader version, and that is the one to install.
    let notices = match bring_up_to_date(&state, &mut inst, &progress).await {
        Ok(notices) => notices,
        Err(e) => {
            state.clear_cancel(progress.id());
            let msg = if matches!(e, Error::Cancelled) {
                "the update from its server was cancelled. It has to match its server before it can start — press Play to update it again.".to_string()
            } else {
                format!("updating from its server did not finish: {e}. It has to match its server before it can start — press Play to try again.")
            };
            progress.fail(&msg).await;
            return Err(Error::msg(msg));
        }
    };

    let java_override = inst
        .overrides
        .java_path
        .clone()
        .or_else(|| config.java_path.clone())
        .map(PathBuf::from);

    let installed = crate::install::install_version(
        &state.http,
        &state.paths,
        &state.manifests,
        &inst.mc_version,
        inst.loader,
        &inst.loader_version,
        java_override.as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    let installed = match installed {
        Ok(i) => i,
        Err(e) => {
            progress.fail(&e.to_string()).await;
            return Err(e);
        }
    };

    let plan = crate::launch::build_plan(&state.paths, &config, &inst, &installed, &session)?;
    let pid = crate::launch::spawn(&app, &state.paths, &inst, plan, state.games.clone()).await?;

    // Read again, as the game's exit does, rather than saving the copy from
    // before preparing, which can take minutes: a setting changed meanwhile —
    // server-side mods turned off, their jars already renamed — would be put
    // back while the files stayed as they are.
    if let Ok(mut played) = instance::load(&state.paths, &id).await {
        played.last_played = Some(chrono::Utc::now().timestamp());
        played.save(&state.paths).await?;
    }

    if config.hide_on_launch {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.hide();
        }
    }

    Ok(LaunchResult {
        pid,
        instance_id: id,
        notices,
    })
}

/// Download everything without starting the game.
#[tauri::command]
pub async fn prepare_instance(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<()> {
    let config = state.config().await;
    let inst = instance::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app), format!("launch:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let java_override = inst
        .overrides
        .java_path
        .clone()
        .or_else(|| config.java_path.clone())
        .map(PathBuf::from);

    let result = crate::install::install_version(
        &state.http,
        &state.paths,
        &state.manifests,
        &inst.mc_version,
        inst.loader,
        &inst.loader_version,
        java_override.as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn stop_instance(state: State<'_, AppState>, id: String) -> Result<bool> {
    Ok(state.games.stop(&id))
}

#[tauri::command]
pub async fn running_instances(state: State<'_, AppState>) -> Result<Vec<String>> {
    Ok(state.games.running_ids())
}

#[tauri::command]
pub async fn cancel_task(state: State<'_, AppState>, id: String) -> Result<bool> {
    Ok(state.cancel(&id))
}

// ---------------------------------------------------------------------------
// Modpacks
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn preview_modpack(path: String) -> Result<instance::modpack::PackPreview> {
    instance::modpack::preview(PathBuf::from(path)).await
}

#[tauri::command]
pub async fn import_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    name: Option<String>,
) -> Result<InstanceView> {
    let archive = PathBuf::from(&path);
    let preview = instance::modpack::preview(archive.clone()).await?;
    let config = state.config().await;

    let display_name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| preview.name.clone());

    let mut inst = instance::create(
        &state.paths,
        display_name,
        preview.mc_version,
        preview.loader,
        preview.loader_version,
    )
    .await?;

    let progress = Progress::new(crate::desktop::sink(app), format!("import:{}", inst.id));
    state.register_cancel(progress.id(), progress.cancel_flag());

    // Both halves: the mrpack reader, then the Geode half if the file has one —
    // settings, artwork, and the mods the pack named rather than carried.
    let missing = share::pack::import(
        &state.http,
        &state.paths,
        &mut inst,
        &archive,
        config.effective_curseforge_key().as_deref(),
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    // Save either way: a partially imported pack is more useful than a
    // directory the launcher refuses to acknowledge.
    inst.save(&state.paths).await?;

    match missing {
        Ok(missing) if missing.is_empty() => {
            progress.finish().await;
            Ok(view(inst, &state))
        }
        // Named, not carried — so the import ends with a list to act on rather
        // than an instance that is quietly short of mods.
        Ok(missing) => {
            let detail = format!(
                "{} file(s) in this pack have to be fetched by hand: {}",
                missing.len(),
                missing.join(", ")
            );
            progress.fail(&detail).await;
            Err(Error::msg(detail))
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Write an instance out as a `.geodepack`.
///
/// The same `.mrpack` other launchers read, plus the Geode half: settings,
/// artwork, and a reference for every mod that came from a provider instead of
/// the jar itself.
#[tauri::command]
pub async fn export_geodepack(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    dest: String,
    include_saves: bool,
) -> Result<instance::modpack::ExportReport> {
    let inst = instance::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app), format!("export:{id}"));
    let dest = PathBuf::from(dest);
    let cf_key = state.cf_key().await;

    // Failed explicitly rather than returned through `?`: the dock drops a job
    // only when it hears one is done, so a bar left unfinished stays there for
    // the rest of the session.
    let result = share::pack::export(
        crate::install::identity::Ask::exporting(
            &state.http,
            cf_key.as_deref(),
            crate::install::identity::Carry::GEODEPACK,
        ),
        &state.paths,
        &inst,
        dest,
        include_saves,
        &progress,
    )
    .await;
    if let Err(e) = &result {
        progress.fail(&e.to_string()).await;
    }
    result
}

#[tauri::command]
pub async fn export_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    dest: String,
    include_saves: bool,
) -> Result<instance::modpack::ExportReport> {
    let inst = instance::load(&state.paths, &id).await?;
    let progress = Progress::new(crate::desktop::sink(app), format!("export:{id}"));
    let dest = PathBuf::from(dest);
    let cf_key = state.cf_key().await;

    let result = instance::modpack::export_mrpack(
        crate::install::identity::Ask::exporting(
            &state.http,
            cf_key.as_deref(),
            crate::install::identity::Carry::MRPACK,
        ),
        &state.paths,
        &inst,
        dest,
        include_saves,
        &progress,
    )
    .await;
    if let Err(e) = &result {
        progress.fail(&e.to_string()).await;
    }
    result
}

// ---------------------------------------------------------------------------
// Sharing a server with players
// ---------------------------------------------------------------------------

/// A server's sharing as its admin sees it, shared now or stopped.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerShareView {
    /// The server's password.
    pub code: String,
    pub link: Option<String>,
    /// The second link, for a player on the admin's own network, so they can
    /// click rather than type this machine's address. Beside `link`, never
    /// instead of it: it reaches no further than this network.
    pub local_link: Option<String>,
    pub include_config: bool,
    /// False while sharing is stopped. The password is kept either way.
    pub shared: bool,
    pub unsupported: Option<String>,
    /// The address this server's links were last taken at, if the admin has
    /// taken one. What the Share tab goes by to say the address has moved on
    /// since, and that those links lead nowhere now.
    pub last_link_address: Option<String>,
}

async fn share_view(
    state: &AppState,
    server: &crate::server::Server,
    share: &crate::server::share::ServerShare,
) -> ServerShareView {
    let config = state.config().await;
    let link = state
        .share
        .link_for(&state.paths, &config.share, share)
        .await
        .map(|link| link.to_url());
    let local_link = state
        .share
        .local_link_for(&state.paths, &config.share, share)
        .await
        .map(|link| link.to_url());
    ServerShareView {
        code: share.code.display(),
        link,
        local_link,
        include_config: share.include_config,
        shared: share.shared,
        unsupported: share::pack::client_loader(server).err().map(|e| e.to_string()),
        last_link_address: crate::server::share::link_address(&state.paths, &server.id).await,
    }
}

/// Start or stop the pack server to match what is shared now.
async fn reconcile_sharing(app: &AppHandle, state: &AppState) {
    state
        .share
        .reconcile(state, Some(crate::desktop::sink(app.clone())))
        .await;
}

/// The share state of a server that must be shared right now for the command
/// to mean anything.
async fn shared(state: &AppState, id: &str) -> Result<crate::server::share::ServerShare> {
    crate::server::share::load(&state.paths, id)
        .await?
        .filter(|share| share.shared)
        .ok_or_else(|| Error::msg("this server is not being shared"))
}

#[tauri::command]
pub async fn get_share_status(state: State<'_, AppState>) -> Result<share::service::ShareStatus> {
    let config = state.config().await;
    Ok(state.share.status(&state.paths, &config.share).await)
}

#[tauri::command]
pub async fn set_share_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: crate::config::ShareConfig,
) -> Result<share::service::ShareStatus> {
    // Below 1024 needs privileges on most systems and some routers refuse to
    // map it; refused here rather than failing somewhere less clear later.
    if settings.port < 1024 {
        return Err(Error::msg("pick a port from 1024 to 65535"));
    }
    let public_address = settings
        .public_address
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string);
    if let Some(address) = &public_address {
        check_public_address(address, settings.port)?;
    }

    let mut config = state.config().await;
    config.share = crate::config::ShareConfig {
        public_address,
        ..settings
    };
    config.save(&state.paths).await?;
    state.set_config(config.clone()).await;

    reconcile_sharing(&app, &state).await;
    Ok(state.share.status(&state.paths, &config.share).await)
}

/// Refuse a public address that could only ever produce a link that does not
/// work, while the admin is still looking at the setting.
fn check_public_address(address: &str, port: u16) -> Result<()> {
    let parsed = share::link::Address::parse(address, port)
        .map_err(|e| Error::msg(format!("that public address cannot be used: {e}")))?;

    // An address nobody else can connect to: every player would reach their
    // own machine, or nothing.
    if let share::link::Address::Direct { host, .. } = &parsed {
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            let unusable = ip.is_unspecified()
                || ip.is_multicast()
                || matches!(ip, std::net::IpAddr::V4(v4) if v4.is_broadcast());
            if unusable {
                return Err(Error::msg(format!(
                    "{ip} is not an address players can connect to — use this machine's public address, or a domain or tunnel"
                )));
            }
        }
    }

    // Built into a real link and read back, exactly as a player's Geode will:
    // an address the parser accepts can still make a link too long to read.
    let sample = share::link::Link {
        pin: matches!(parsed, share::link::Address::Direct { .. }).then_some(share::link::Pin([0; 32])),
        address: parsed,
        code: share::code::Code::generate(),
    };
    share::link::Link::parse(&sample.to_url())
        .map(|_| ())
        .map_err(|e| Error::msg(format!("links made with that public address would not work: {e}")))
}

/// The server's sharing, shared now or stopped; `None` only for a server that
/// has never been shared.
#[tauri::command]
pub async fn get_server_share(
    state: State<'_, AppState>,
    id: String,
) -> Result<Option<ServerShareView>> {
    let server = crate::server::load(&state.paths, &id).await?;
    Ok(match crate::server::share::load(&state.paths, &id).await? {
        Some(share) => Some(share_view(&state, &server, &share).await),
        None => None,
    })
}

#[tauri::command]
pub async fn share_server(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<ServerShareView> {
    let server = crate::server::load(&state.paths, &id).await?;
    // Refused before a password exists rather than after: a share that can
    // only ever answer "not supported" would hand players a link that goes
    // nowhere.
    share::pack::client_loader(&server)?;
    let share = crate::server::share::start(&state.paths, &id).await?;
    reconcile_sharing(&app, &state).await;
    Ok(share_view(&state, &server, &share).await)
}

/// Stop sharing, keeping the server's password for when it is shared again.
#[tauri::command]
pub async fn unshare_server(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<()> {
    crate::server::share::stop(&state.paths, &id).await?;
    reconcile_sharing(&app, &state).await;
    Ok(())
}

#[tauri::command]
pub async fn set_server_share_config(
    state: State<'_, AppState>,
    id: String,
    include_config: bool,
) -> Result<ServerShareView> {
    let server = crate::server::load(&state.paths, &id).await?;
    let mut share = shared(&state, &id).await?;
    share.include_config = include_config;
    crate::server::share::save(&state.paths, &id, &share).await?;
    // A pack already being built or sent was built as the share was, and one
    // with `config/` in it must not keep streaming once config is not sent.
    state.share.cut_revoked(&state.paths).await;
    Ok(share_view(&state, &server, &share).await)
}

/// The admin has taken this server's link to send it: remember the address it
/// points at.
///
/// Called when a link is taken and at no other time, because that is when
/// there start to be links in other people's hands. Geode re-reads the public
/// address while it shares, and when what it sees is no longer this, the Share
/// tab says the links already sent lead nowhere and the link above the note is
/// the one to send now.
#[tauri::command]
pub async fn share_link_copied(state: State<'_, AppState>, id: String) -> Result<ServerShareView> {
    let server = crate::server::load(&state.paths, &id).await?;
    let share = shared(&state, &id).await?;
    let config = state.config().await;
    state
        .share
        .remember_link_address(&state.paths, &config.share, &id)
        .await?;
    Ok(share_view(&state, &server, &share).await)
}

/// Replace a server's password, whether it is shared now or stopped. Everyone
/// holding the old link is cut off at once: the pack server reads passwords
/// fresh on every request, a preview or download already under way with the
/// old one is cut as soon as the new one is saved, and nothing but sharing ever
/// writes the file the password lives in.
#[tauri::command]
pub async fn rotate_server_share_code(
    state: State<'_, AppState>,
    id: String,
) -> Result<ServerShareView> {
    let server = crate::server::load(&state.paths, &id).await?;
    let share = crate::server::share::replace_password(&state.paths, &id).await?;
    state.share.cut_revoked(&state.paths).await;
    Ok(share_view(&state, &server, &share).await)
}

// ---------------------------------------------------------------------------
// Joining a shared server
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn preview_join(
    state: State<'_, AppState>,
    input: share::join::JoinInput,
) -> Result<share::join::JoinPreview> {
    preview_join_at(&state.paths, input).await
}

async fn preview_join_at(
    paths: &crate::paths::Paths,
    input: share::join::JoinInput,
) -> Result<share::join::JoinPreview> {
    let typed = matches!(input, share::join::JoinInput::Parts { .. });
    // A typed address with no port means Geode's default sharing port — the
    // admin's, which is not necessarily the one this player has configured.
    let mut link = input.into_link(crate::config::ShareConfig::default().port)?;

    // A typed address this player has joined before is held to the certificate
    // it had then. That is what "trusted from now on" means; without it, every
    // typed join would be a first use again, and whoever answered would win.
    let remembered = if typed && link.pin.is_none() {
        link.pin = share::join::remembered_pin(paths, &link.address).await;
        link.pin.is_some()
    } else {
        false
    };
    let address = link.address.display();

    let mut preview = share::join::preview(link).await.map_err(|e| {
        // The generic message speaks of a link. Here there was none: the
        // certificate was the one remembered from joining this address
        // before, and a different one answered.
        if remembered && e.to_string().contains("did not prove") {
            Error::msg(format!(
                "{address} did not prove it is the server you joined there before, so nothing was downloaded — if its admin replaced its certificate, ask them for a new link"
            ))
        } else {
            e
        }
    })?;
    preview.existing =
        share::join::existing_instance(paths, &preview.link, &preview.info.server_id).await;
    Ok(preview)
}

/// Save a link onto the instances already joined from the server it names.
///
/// This is how a player given a new link after their server's password was
/// replaced keeps their instance updating, instead of joining again into a
/// second one. It is only ever a new password for the same server: the address
/// has to be the one the instance was joined at, and the server answering
/// there has to be the same server it was joined from, which is why this asks
/// it rather than believing the link.
async fn relink(
    paths: &crate::paths::Paths,
    id: &str,
    link: share::link::Link,
) -> Result<Instance> {
    // Back from the webview, so checked again by the same strict parser as a
    // link off the internet.
    let link = share::link::Link::parse(&link.to_url())?;
    let inst = instance::load(paths, id).await?;
    let Some(joined) = inst.joined.as_ref() else {
        return Err(Error::msg("this instance was not joined from a server"));
    };
    if joined.address != link.address {
        return Err(Error::msg(
            "that link is for a different address than this instance was joined from",
        ));
    }
    let server_id = inst.joined_server_id.clone();
    let preview = share::join::preview(link).await?;
    if preview.info.server_id.is_empty()
        || Some(preview.info.server_id.as_str()) != server_id.as_deref()
    {
        return Err(Error::msg(
            "that link is for a different server than this instance was joined from",
        ));
    }
    // The preview's link, not the one handed in: a typed address has the
    // fingerprint it just saw filled in.
    let link = preview.link;

    // Every copy joined from this server, and read again rather than saved
    // from the copy loaded before the server was asked — that answer takes up
    // to half a minute, and a name or a setting changed meanwhile is not this
    // command's to put back. A player who imported a second copy gets both:
    // the copy relinked here is the one every later link finds, so one left
    // behind would never be handed a password that works again.
    let mut relinked = Vec::new();
    for mut copy in instance::list(paths).await? {
        let same_server = copy
            .joined
            .as_ref()
            .is_some_and(|joined| joined.address == link.address)
            && copy.joined_server_id == server_id;
        if !same_server {
            continue;
        }
        copy.joined = Some(link.clone());
        copy.save(paths).await?;
        relinked.push(copy);
    }
    relinked
        .into_iter()
        .find(|copy| copy.id == id)
        .ok_or_else(|| Error::msg("that instance is no longer joined from this server"))
}

#[tauri::command]
pub async fn relink_joined_instance(
    state: State<'_, AppState>,
    id: String,
    link: share::link::Link,
) -> Result<InstanceView> {
    let inst = relink(&state.paths, &id, link).await?;
    Ok(view(inst, &state))
}

/// A join that got as far as creating the instance.
///
/// Anything that goes wrong after that is a problem to show, not a failure:
/// the instance exists either way, and reporting it as a failure would invite
/// a retry that makes a second one.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinOutcome {
    pub instance: InstanceView,
    pub problems: Vec<String>,
}

#[tauri::command]
pub async fn join_server(
    app: AppHandle,
    state: State<'_, AppState>,
    link: share::link::Link,
    info: share::host::PackInfo,
    server_side_mods: bool,
    name: Option<String>,
) -> Result<JoinOutcome> {
    // Back from the webview, so checked again by the same strict parser as a
    // link off the internet — and a direct address must by now carry the
    // fingerprint the preview settled on.
    let link = share::link::Link::parse(&link.to_url())?;

    let run = uuid::Uuid::new_v4();
    let archive = share::join::incoming_dir(&state.paths).join(format!("{run}.geodepack"));
    let progress = Progress::new(crate::desktop::sink(app.clone()), format!("join:{run}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let outcome = join_into_new_instance(
        &state,
        &link,
        &info,
        server_side_mods,
        name,
        &archive,
        &progress,
    )
    .await;

    state.clear_cancel(progress.id());
    let _ = tokio::fs::remove_file(&archive).await;

    match outcome {
        Ok((inst, problems)) => {
            if problems.is_empty() {
                progress.finish().await;
            } else {
                progress.fail(&problems.join("; ")).await;
            }
            Ok(JoinOutcome {
                instance: view(inst, &state),
                problems,
            })
        }
        Err(e) => {
            progress.fail(&e.to_string()).await;
            Err(e)
        }
    }
}

/// Download, check, create, import.
///
/// Errors before the instance exists are errors. Once it exists, what goes
/// wrong is returned as problems beside it — and the instance is always saved,
/// the same rule as any other pack.
async fn join_into_new_instance(
    state: &AppState,
    link: &share::link::Link,
    approved: &share::host::PackInfo,
    server_side_mods: bool,
    name: Option<String>,
    archive: &std::path::Path,
    progress: &Progress,
) -> Result<(Instance, Vec<String>)> {
    share::join::download(link, archive, progress).await?;
    install_joined(
        state,
        link,
        approved,
        server_side_mods,
        name,
        archive,
        progress,
    )
    .await
}

/// Check a downloaded server pack against what the player approved, and
/// install it as a new instance, with its server-side mods on or off as the
/// player chose.
async fn install_joined(
    state: &AppState,
    link: &share::link::Link,
    approved: &share::host::PackInfo,
    server_side_mods: bool,
    name: Option<String>,
    archive: &std::path::Path,
    progress: &Progress,
) -> Result<(Instance, Vec<String>)> {
    let described = instance::modpack::preview(archive.to_path_buf()).await?;
    // What gets installed must be what the player said yes to.
    share::join::check_matches(approved, &described)?;

    // A joined pack must be a server's. A personal pack's manifest may set the
    // player's memory and Java arguments; a server has no business doing that,
    // and only a server manifest is refused those settings on import.
    let probe = archive.to_path_buf();
    let manifest = tokio::task::spawn_blocking(move || share::pack::read_manifest(&probe))
        .await
        .map_err(|e| Error::msg(format!("reading the pack's manifest panicked: {e}")))??
        .filter(|m| m.kind == share::manifest::Kind::Server)
        .ok_or_else(|| {
            Error::msg("that address did not send a server's pack, so nothing was installed")
        })?;
    // The import checks this too; checked here as well so a refusal leaves no
    // empty instance behind.
    share::pack::check_server_downloads(archive).await?;

    let mut inst = instance::create(
        &state.paths,
        described.name.clone(),
        described.mc_version,
        described.loader,
        described.loader_version,
    )
    .await?;
    inst.joined = Some(link.clone());
    if !approved.server_id.is_empty() {
        inst.joined_server_id = Some(approved.server_id.clone());
    }
    // Before the import, which turns them off as they land.
    inst.server_side_mods_off = !server_side_mods;
    inst.save(&state.paths).await?;

    let config = state.config().await;
    let imported = share::pack::import(
        &state.http,
        &state.paths,
        &mut inst,
        archive,
        config.effective_curseforge_key().as_deref(),
        progress,
    )
    .await;

    // The pack's own name is applied by the import; a name the player chose
    // goes on after, so it wins.
    if let Some(chosen) = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
        inst.name = chosen;
    }
    // Only an import that finished has installed a revision; files left to
    // fetch by hand do not change what the server sent. What is kept is the
    // revision the pack itself carries, so it describes what this instance
    // actually holds: a server changed and changed back between the
    // description and the pack would otherwise leave the instance holding one
    // revision's content under another's name, and the next check would see
    // nothing to do. A pack from an admin's Geode that carries none falls back
    // to the description's, which is never newer than the pack built after it.
    let revision =
        share::pack::pack_revision(&manifest).unwrap_or_else(|| approved.revision.clone());
    if imported.is_ok() && !revision.is_empty() {
        inst.joined_revision = Some(revision);
    }
    inst.save(&state.paths).await?;

    let problems = match imported {
        Ok(missing) if missing.is_empty() => Vec::new(),
        Ok(missing) => vec![fetch_by_hand(&missing)],
        Err(e) => vec![format!("the import stopped partway: {e}")],
    };
    Ok((inst, problems))
}

#[cfg(test)]
mod tests {
    //! The command-layer rules around sharing that no module owns on its own:
    //! which public addresses are allowed, what a join may install, and holding
    //! a typed address to the certificate it had before.

    use super::*;
    use crate::instance::jar::tests::fabric_jar;
    use crate::paths::{Paths, Scratch};
    use crate::server::share::ServerShare;
    use crate::server::{Server, ServerKind};
    use crate::share::code::Code;
    use crate::share::join::{JoinInput, Verified};
    use crate::share::link::{Address, Link, Pin};
    use std::io::{Read, Write};

    async fn shared_server(paths: &Paths) -> (Server, ServerShare) {
        shared_server_called(paths, "survival", "Survival").await
    }

    /// A shared server with an id and name of its own, for a machine sharing
    /// more than one.
    async fn shared_server_called(paths: &Paths, id: &str, name: &str) -> (Server, ServerShare) {
        let mut server = Server::new(name.into(), ServerKind::Fabric, "1.21.1".into());
        server.id = id.into();
        server.build = "0.16.9".into();
        server.save(paths).await.unwrap();
        let mods = paths.server_run(&server.id).join("mods");
        tokio::fs::create_dir_all(&mods).await.unwrap();
        tokio::fs::write(mods.join("homemade.jar"), fabric_jar("homemade", "*"))
            .await
            .unwrap();
        tokio::fs::write(
            mods.join("server-side.jar"),
            fabric_jar("serverside", "server"),
        )
        .await
        .unwrap();
        let share = ServerShare { code: Code::generate(), include_config: false, shared: true };
        crate::server::share::save(paths, &server.id, &share).await.unwrap();
        (server, share)
    }

    /// Replace the pack's manifest kind, to make the pack a personal one.
    fn make_personal(archive: &std::path::Path) {
        rewrite_entry(archive, share::manifest::Manifest::PATH, |manifest| {
            manifest["kind"] = "instance".into();
        });
    }

    /// Edit one JSON entry of a pack in place.
    fn rewrite_entry(
        archive: &std::path::Path,
        entry_name: &str,
        edit: impl FnOnce(&mut serde_json::Value),
    ) {
        let mut edit = Some(edit);
        let mut entries = Vec::new();
        {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                entries.push((entry.name().to_string(), bytes));
            }
        }
        let mut zip = zip::ZipWriter::new(std::fs::File::create(archive).unwrap());
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for (name, mut bytes) in entries {
            if name == entry_name {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                (edit.take().unwrap())(&mut value);
                bytes = serde_json::to_vec(&value).unwrap();
            }
            zip.start_file(name, options).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A link to a server shared on this machine, pinned to the certificate it
    /// actually presents.
    async fn loopback(paths: &Paths, port: u16, code: &Code) -> Link {
        let pin = share::identity::Identity::load_or_create(paths)
            .await
            .unwrap()
            .pin();
        Link {
            address: Address::Direct {
                host: "127.0.0.1".into(),
                port,
            },
            code: code.clone(),
            pin: Some(pin),
        }
    }

    /// Export `server` and install it as a joined instance, as a join does,
    /// and say which instance that made.
    async fn join_over(state: &AppState, admin: &Paths, server: &Server, link: &Link) -> String {
        let archive = admin.root.join(format!("join-{}.geodepack", server.id));
        share::pack::export_server(admin, server, archive.clone(), &Progress::silent("t"))
            .await
            .unwrap();
        let approved = share::pack::server_pack_info(admin, server).await.unwrap();
        let (inst, problems) = install_joined(
            state,
            link,
            &approved,
            true,
            None,
            &archive,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        inst.id
    }

    /// Put a jar in a shared server's mods folder, so it has something new to
    /// send.
    async fn add_jar(paths: &Paths, server: &Server, name: &str) {
        tokio::fs::write(
            paths.server_run(&server.id).join("mods").join(name),
            fabric_jar("new", "*"),
        )
        .await
        .unwrap();
    }

    async fn instance_count(paths: &Paths) -> usize {
        instance::list(paths).await.map(|l| l.len()).unwrap_or(0)
    }

    /// Only a fresh install of a mod Modrinth lists as client-only is sent to
    /// Client mods. An update, a project the server already has in Mods, and
    /// anything listed otherwise or not at all stays where it was asked for.
    #[test]
    fn a_mod_no_server_runs_goes_to_client_mods() {
        use crate::instance::content::ContentRecord;

        let listed = |client: Option<&str>, server: Option<&str>| -> crate::providers::Project {
            serde_json::from_value(serde_json::json!({
                "provider": "modrinth", "id": "SODIUM", "slug": "sodium", "name": "Sodium",
                "summary": "", "downloads": 0, "follows": 0, "author": "",
                "kind": "mod", "pageUrl": "", "clientSide": client, "serverSide": server,
            }))
            .unwrap()
        };
        let client_only = listed(Some("required"), Some("unsupported"));
        let in_mods: ContentRecord = serde_json::from_value(serde_json::json!({
            "fileName": "sodium.jar", "kind": "mod", "provider": "modrinth",
            "projectId": "SODIUM", "versionId": "v", "name": "Sodium", "versionNumber": "1",
        }))
        .unwrap();
        let in_client_mods = ContentRecord {
            kind: ContentKind::ClientMod,
            ..in_mods.clone()
        };

        assert_eq!(
            install_folder(ContentKind::Mod, None, &client_only, &[]),
            ContentKind::ClientMod
        );
        assert_eq!(
            install_folder(ContentKind::Mod, None, &client_only, &[in_client_mods]),
            ContentKind::ClientMod
        );
        assert_eq!(
            install_folder(ContentKind::Mod, Some("v2"), &client_only, &[]),
            ContentKind::Mod
        );
        assert_eq!(
            install_folder(ContentKind::Mod, None, &client_only, &[in_mods]),
            ContentKind::Mod
        );
        for (client, server) in [
            (Some("unsupported"), Some("required")),
            (Some("required"), Some("required")),
            (None, None),
        ] {
            assert_eq!(
                install_folder(ContentKind::Mod, None, &listed(client, server), &[]),
                ContentKind::Mod,
                "{client:?} {server:?}"
            );
        }
        assert_eq!(
            install_folder(
                ContentKind::ClientMod,
                None,
                &listed(Some("required"), Some("required")),
                &[]
            ),
            ContentKind::ClientMod
        );
    }

    /// An address that parses can still be one no player can use. Those are
    /// refused while the admin is looking at the setting, not discovered when
    /// every player's lookup fails.
    #[test]
    fn a_public_address_that_can_only_make_broken_links_is_refused() {
        for unusable in ["0.0.0.0", "[::]", "224.0.0.1", "255.255.255.255"] {
            let err = check_public_address(unusable, 25590).expect_err(unusable);
            assert!(err.to_string().contains("not an address players can connect to"), "{unusable}: {err}");
        }
        let overlong = format!("https://pack.example.com/{}", "a".repeat(2000));
        let err = check_public_address(&overlong, 25590).expect_err("an overlong address");
        assert!(err.to_string().contains("would not work"), "{err}");

        // Now that the pack server listens on IPv6, an IPv6 address is fine.
        for usable in ["203.0.113.7", "play.example.com", "host.playit.gg:12345", "https://pack.example.com", "[2001:db8::5]:25590"] {
            check_public_address(usable, 25590).unwrap_or_else(|e| panic!("{usable}: {e}"));
        }
    }

    /// What a join installs must be a server's pack, and the one the player
    /// said yes to. Refused before an instance exists, so a refusal leaves
    /// nothing behind to retry into a duplicate.
    #[tokio::test]
    async fn a_joined_pack_installs_only_if_it_is_the_servers_and_what_was_approved() {
        let admin = Scratch::new("cmd-install-admin");
        let (server, share) = shared_server(&admin.paths).await;
        let archive = admin.paths.root.join("pack.geodepack");
        share::pack::export_server(&admin.paths, &server, archive.clone(), &Progress::silent("t")).await.unwrap();
        let approved = share::pack::server_pack_info(&admin.paths, &server).await.unwrap();
        let link = Link {
            address: Address::Direct { host: "127.0.0.1".into(), port: 25590 },
            code: share.code.clone(),
            pin: Some(Pin([1; 32])),
        };

        let player = Scratch::new("cmd-install-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());

        // Not what was approved: refused, and nothing created.
        let changed = share::host::PackInfo { loader: "forge".into(), ..approved.clone() };
        install_joined(
            &state,
            &link,
            &changed,
            true,
            None,
            &archive,
            &Progress::silent("t"),
        )
        .await
        .expect_err("a pack that is not what the player approved");
        assert_eq!(instance_count(&player.paths).await, 0, "a refused join left an instance behind");

        // What was approved: installed, remembered as joined, and the server's.
        let (inst, problems) = install_joined(
            &state,
            &link,
            &approved,
            true,
            None,
            &archive,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        let saved = instance::load(&player.paths, &inst.id).await.unwrap();
        assert_eq!(saved.joined.as_ref().map(|l| &l.code), Some(&share.code));
        assert!(saved
            .content
            .iter()
            .any(|r| r.file_name == "homemade.jar" && r.origin == instance::content::Origin::Server));
        assert!(saved.pack.is_none(), "a joined server is not a published modpack");
        assert!(!saved.server_side_mods_off);
        let mods = player.paths.game_dir(&inst.id).join("mods");
        assert!(
            mods.join("server-side.jar").is_file(),
            "a server-side mod the player kept is off"
        );

        // Without server-side mods: the choice is kept, and the server-side mod
        // arrives turned off while the rest are on.
        let (inst, _) = install_joined(
            &state,
            &link,
            &approved,
            false,
            None,
            &archive,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        let saved = instance::load(&player.paths, &inst.id).await.unwrap();
        assert!(saved.server_side_mods_off);
        let mods = player.paths.game_dir(&inst.id).join("mods");
        assert!(mods.join("server-side.jar.disabled").is_file());
        assert!(!mods.join("server-side.jar").exists());
        assert!(mods.join("homemade.jar").is_file());

        // A personal pack at that address: refused, and nothing more created.
        make_personal(&archive);
        let before = instance_count(&player.paths).await;
        let err = install_joined(
            &state,
            &link,
            &approved,
            true,
            None,
            &archive,
            &Progress::silent("t"),
        )
        .await
        .expect_err("a personal pack is not a server's");
        assert!(err.to_string().contains("server's pack"), "{err}");
        assert_eq!(instance_count(&player.paths).await, before);
    }

    /// A joined instance remembers which server it came from, and which of
    /// that server's revisions it has — but only once the import finished, so
    /// one that stopped partway is updated next time.
    #[tokio::test]
    async fn a_joined_instance_records_the_revision_it_installed() {
        let admin = Scratch::new("cmd-revision-admin");
        let (mut server, share) = shared_server(&admin.paths).await;
        let archive = admin.paths.root.join("pack.geodepack");
        share::pack::export_server(&admin.paths, &server, archive.clone(), &Progress::silent("t"))
            .await
            .unwrap();
        let approved = share::pack::server_pack_info(&admin.paths, &server)
            .await
            .unwrap();
        assert!(!approved.revision.is_empty());
        let link = Link {
            address: Address::Direct {
                host: "127.0.0.1".into(),
                port: 25590,
            },
            code: share.code.clone(),
            pin: Some(Pin([1; 32])),
        };

        let player = Scratch::new("cmd-revision-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());
        let (inst, problems) = install_joined(
            &state,
            &link,
            &approved,
            true,
            None,
            &archive,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        let saved = instance::load(&player.paths, &inst.id).await.unwrap();
        assert_eq!(
            saved.joined_revision.as_deref(),
            Some(approved.revision.as_str())
        );
        assert_eq!(saved.joined_server_id.as_deref(), Some(server.id.as_str()));

        // The description a player was shown is worked out before the pack is
        // built, and the server can change — or change and change back — in
        // between. What is kept is the revision of the pack that was actually
        // installed, so it describes what this instance holds.
        let stale = share::host::PackInfo {
            revision: "a revision this pack is not".into(),
            ..approved.clone()
        };
        let (from_stale, problems) = install_joined(
            &state,
            &link,
            &stale,
            true,
            None,
            &archive,
            &Progress::silent("t"),
        )
        .await
        .unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        let saved = instance::load(&player.paths, &from_stale.id).await.unwrap();
        assert_eq!(
            saved.joined_revision.as_deref(),
            Some(approved.revision.as_str())
        );

        // A reference, which the import resolves one at a time, looking for a
        // cancel before it asks anyone for anything.
        tokio::fs::write(
            admin.paths.server_run(&server.id).join("mods/create.jar"),
            b"from curseforge",
        )
        .await
        .unwrap();
        server.content.push(
            serde_json::from_value(serde_json::json!({
                "fileName": "create.jar", "kind": "mod", "provider": "curseforge",
                "projectId": "1", "versionId": "2", "name": "Create", "versionNumber": "1",
            }))
            .unwrap(),
        );
        share::pack::export_server(&admin.paths, &server, archive.clone(), &Progress::silent("t"))
            .await
            .unwrap();
        let approved = share::pack::server_pack_info(&admin.paths, &server)
            .await
            .unwrap();
        let cancelled = Progress::silent("t");
        cancelled
            .cancel_flag()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (inst, problems) =
            install_joined(&state, &link, &approved, true, None, &archive, &cancelled)
                .await
                .unwrap();
        assert!(
            problems.iter().any(|p| p.contains("stopped partway")),
            "{problems:?}"
        );
        let saved = instance::load(&player.paths, &inst.id).await.unwrap();
        assert_eq!(saved.joined_revision, None);
        assert_eq!(saved.joined_server_id.as_deref(), Some(server.id.as_str()));
    }

    /// A server's pack names Modrinth for the mods it does not carry. One that
    /// names anywhere else is refused before anything is fetched, whether it
    /// arrives by joining or as a file.
    #[tokio::test]
    async fn a_server_pack_that_would_download_from_elsewhere_fetches_nothing() {
        let admin = Scratch::new("cmd-elsewhere-admin");
        let (server, share) = shared_server(&admin.paths).await;
        let archive = admin.paths.root.join("pack.geodepack");
        share::pack::export_server(&admin.paths, &server, archive.clone(), &Progress::silent("t"))
            .await
            .unwrap();
        let approved = share::pack::server_pack_info(&admin.paths, &server)
            .await
            .unwrap();
        let link = Link {
            address: Address::Direct {
                host: "127.0.0.1".into(),
                port: 25590,
            },
            code: share.code.clone(),
            pin: Some(Pin([1; 32])),
        };

        // Somewhere on the player's own machine that notices being asked.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let elsewhere = format!(
            "http://127.0.0.1:{}/router/reboot",
            listener.local_addr().unwrap().port()
        );
        rewrite_entry(&archive, "modrinth.index.json", |index| {
            index["files"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "path": "mods/elsewhere.jar",
                    "hashes": { "sha1": "0000000000000000000000000000000000000000" },
                    "downloads": [elsewhere],
                    "fileSize": 1,
                }));
        });

        let player = Scratch::new("cmd-elsewhere-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());
        let err = install_joined(
            &state,
            &link,
            &approved,
            true,
            None,
            &archive,
            &Progress::silent("t"),
        )
        .await
        .expect_err("a pack downloading from elsewhere");
        assert!(
            err.to_string().contains("somewhere other than Modrinth"),
            "{err}"
        );
        assert_eq!(
            instance_count(&player.paths).await,
            0,
            "a refused join left an instance behind"
        );

        let mut inst = instance::create(
            &player.paths,
            "File".into(),
            "1.21.1".into(),
            crate::meta::Loader::Fabric,
            "0.16.9".into(),
        )
        .await
        .unwrap();
        // Were it fetched, a listener that never answers would hold it for the
        // client's whole timeout; a short one keeps that a failure, not a hang.
        let impatient = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let err = share::pack::import(
            &impatient,
            &player.paths,
            &mut inst,
            &archive,
            None,
            &Progress::silent("t"),
        )
        .await
        .expect_err("the same pack imported from a file");
        assert!(
            err.to_string().contains("somewhere other than Modrinth"),
            "{err}"
        );

        assert_eq!(
            listener.accept().map_err(|e| e.kind()).err(),
            Some(std::io::ErrorKind::WouldBlock),
            "the pack's download was requested"
        );
    }

    /// "Trusted from now on" has to be true: a typed address this player has
    /// joined before is held to the certificate it had, and another machine
    /// answering there is refused.
    #[tokio::test]
    async fn a_typed_address_is_held_to_the_certificate_it_had_when_joined() {
        let admin = Scratch::new("cmd-typed-admin");
        let (_server, share) = shared_server(&admin.paths).await;
        let host = share::host::Host::start(admin.paths.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();
        let real = share::identity::Identity::load_or_create(&admin.paths).await.unwrap().pin();
        let typed = || JoinInput::Parts { address: format!("127.0.0.1:{port}"), code: share.code.display() };

        let player = Scratch::new("cmd-typed-player");
        let remember = |pin: Pin| {
            let paths = player.paths.clone();
            let code = share.code.clone();
            async move {
                let mut inst = Instance::new("Joined".into(), "1.21.1".into(), crate::meta::Loader::Fabric, "0.16.9".into());
                inst.id = "joined".into();
                inst.joined = Some(Link {
                    address: Address::Direct { host: "127.0.0.1".into(), port },
                    code,
                    pin: Some(pin),
                });
                inst.save(&paths).await.unwrap();
            }
        };

        // Never joined: the first certificate is trusted, and nothing exists yet.
        let first = preview_join_at(&player.paths, typed()).await.unwrap();
        assert_eq!(first.verified, Verified::FirstUse);
        assert!(first.existing.is_none());

        // Joined before, and the same machine answers: verified, and the earlier
        // instance is offered instead of a second one.
        remember(real).await;
        let again = preview_join_at(&player.paths, typed()).await.unwrap();
        assert_eq!(again.verified, Verified::Pinned);
        assert_eq!(again.existing.as_ref().map(|e| e.id.as_str()), Some("joined"));

        // Joined before, and a different machine answers there now: refused.
        remember(Pin([9; 32])).await;
        let err = preview_join_at(&player.paths, typed()).await.expect_err("a different certificate at a remembered address");
        assert!(err.to_string().contains("joined there before"), "{err}");

        host.stop().await;
    }

    /// Every Play asks the server what it holds, and only a revision this
    /// instance does not have is worth downloading for. A server that names
    /// none — an admin on a Geode from before revisions — never asks for one.
    #[test]
    fn an_update_is_due_only_when_the_server_names_a_different_revision() {
        assert!(update_due(None, "a"), "an instance that has installed none");
        assert!(update_due(Some("a"), "b"), "a server that has moved on");
        assert!(!update_due(Some("a"), "a"), "the revision already held");
        assert!(!update_due(None, ""), "a server that names no revision");
        assert!(
            !update_due(Some("a"), ""),
            "an admin's Geode from before this"
        );
    }

    /// Play on a joined instance brings it up to its server first: what the
    /// server added arrives as the server's, the revision it now holds is
    /// recorded, a server with nothing new is not downloaded from, and a
    /// server that cannot be reached is a notice rather than a refusal.
    #[tokio::test]
    async fn a_joined_instance_is_brought_up_to_date_before_it_plays() {
        let admin = Scratch::new("cmd-play-admin");
        let (server, share) = shared_server(&admin.paths).await;
        let host = share::host::Host::start(admin.paths.clone(), 0, None).await.unwrap();
        let link = loopback(&admin.paths, host.local_addr.port(), &share.code).await;

        let player = Scratch::new("cmd-play-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());
        let id = join_over(&state, &admin.paths, &server, &link).await;
        let mods = player.paths.game_dir(&id).join("mods");

        // The server has moved on since that join.
        add_jar(&admin.paths, &server, "new.jar").await;
        let mut inst = instance::load(&player.paths, &id).await.unwrap();
        let notices = bring_up_to_date(&state, &mut inst, &Progress::silent("t"))
            .await
            .unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        assert!(
            mods.join("new.jar").is_file(),
            "the server's new mod did not arrive"
        );
        assert!(inst
            .content
            .iter()
            .any(|r| r.file_name == "new.jar" && r.origin == instance::content::Origin::Server));
        let now = share::pack::server_pack_info(&admin.paths, &server)
            .await
            .unwrap()
            .revision;
        let saved = instance::load(&player.paths, &id).await.unwrap();
        assert_eq!(saved.joined_revision.as_deref(), Some(now.as_str()));

        // And now there is nothing to do: the check happens, and no pack is
        // asked for.
        let recorder = std::sync::Arc::new(crate::events::Recorder::default());
        let progress = Progress::new(recorder.clone(), "launch:t");
        let mut inst = instance::load(&player.paths, &id).await.unwrap();
        let notices = bring_up_to_date(&state, &mut inst, &progress)
            .await
            .unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        let stages: Vec<String> = recorder
            .events()
            .into_iter()
            .filter_map(|(_, e)| e["stage"].as_str().map(str::to_string))
            .collect();
        assert!(
            stages
                .iter()
                .any(|s| s == "Checking the server for changes"),
            "{stages:?}"
        );
        assert!(
            !stages.iter().any(|s| s == "Downloading the server's pack"),
            "a server with nothing new was downloaded from: {stages:?}"
        );

        // The admin's machine is off: the player still plays what they have.
        host.stop().await;
        let mut inst = instance::load(&player.paths, &id).await.unwrap();
        let notices = bring_up_to_date(&state, &mut inst, &Progress::silent("t"))
            .await
            .unwrap();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("could not reach"), "{}", notices[0]);
        let saved = instance::load(&player.paths, &id).await.unwrap();
        assert_eq!(saved.joined_revision.as_deref(), Some(now.as_str()));
        assert!(mods.join("new.jar").is_file());
    }

    /// A launch loads the instance before it asks the server anything, and the
    /// update that follows runs for as long as a download and an import take.
    /// What it writes back is what it decided, over the instance as it stands
    /// on disk — so a name, a memory setting or a note the player changed
    /// while they waited is still there afterwards.
    #[tokio::test]
    async fn an_update_keeps_what_the_player_changed_while_it_ran() {
        let admin = Scratch::new("cmd-meanwhile-admin");
        let (server, share) = shared_server(&admin.paths).await;
        let host = share::host::Host::start(admin.paths.clone(), 0, None).await.unwrap();
        let link = loopback(&admin.paths, host.local_addr.port(), &share.code).await;

        let player = Scratch::new("cmd-meanwhile-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());
        let id = join_over(&state, &admin.paths, &server, &link).await;

        // The copy a launch is holding, read before the server was asked.
        add_jar(&admin.paths, &server, "new.jar").await;
        let mut launching = instance::load(&player.paths, &id).await.unwrap();

        // The player, waiting for it, changes the instance on its own page.
        let mut meanwhile = instance::load(&player.paths, &id).await.unwrap();
        meanwhile.name = "Survival with Dad".into();
        meanwhile.overrides.memory_mb = Some(6144);
        meanwhile.save(&player.paths).await.unwrap();

        let notices = bring_up_to_date(&state, &mut launching, &Progress::silent("t"))
            .await
            .unwrap();
        assert!(notices.is_empty(), "{notices:?}");

        let saved = instance::load(&player.paths, &id).await.unwrap();
        assert_eq!(
            saved.name, "Survival with Dad",
            "the update put back the name from before it"
        );
        assert_eq!(saved.overrides.memory_mb, Some(6144));
        // And what the update decided is all there.
        let now = share::pack::server_pack_info(&admin.paths, &server)
            .await
            .unwrap()
            .revision;
        assert_eq!(saved.joined_revision.as_deref(), Some(now.as_str()));
        assert!(saved.content.iter().any(|r| r.file_name == "new.jar"));
        assert!(player.paths.game_dir(&id).join("mods/new.jar").is_file());

        // Nothing to download, and the server's id learned for the first time:
        // a save with no update behind it writes that id and nothing else.
        let mut older = instance::load(&player.paths, &id).await.unwrap();
        older.joined_server_id = None;
        older.save(&player.paths).await.unwrap();
        let mut launching = instance::load(&player.paths, &id).await.unwrap();
        let mut meanwhile = instance::load(&player.paths, &id).await.unwrap();
        meanwhile.notes = "Dad's seed: 12345".into();
        meanwhile.save(&player.paths).await.unwrap();

        let notices = bring_up_to_date(&state, &mut launching, &Progress::silent("t"))
            .await
            .unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        let saved = instance::load(&player.paths, &id).await.unwrap();
        assert_eq!(
            saved.notes, "Dad's seed: 12345",
            "a note written while the server was being asked"
        );
        assert_eq!(saved.joined_server_id.as_deref(), Some(server.id.as_str()));

        host.stop().await;
    }

    /// A player's own copy of a mod the server also has is off, and may well
    /// have been off before this update: the notice says what is so, and reads
    /// for one mod as well as for several.
    #[test]
    fn the_notice_about_a_players_own_copies_reads_for_one_and_for_several() {
        assert_eq!(
            own_copies_off(&["Sodium".to_string()]),
            "Your own Sodium stays off: the server provides it."
        );
        assert_eq!(
            own_copies_off(&["Sodium".to_string(), "Lithium".to_string()]),
            "Your own Sodium, Lithium stay off: the server provides them."
        );
    }

    /// A password replaced while a player was away stops Geode checking for
    /// changes, and the player is told how to put it right — but the game
    /// still starts, and nothing is touched.
    #[tokio::test]
    async fn a_replaced_password_lets_the_player_play_and_says_how_to_fix_it() {
        let admin = Scratch::new("cmd-refused-admin");
        let (server, share) = shared_server(&admin.paths).await;
        let host = share::host::Host::start(admin.paths.clone(), 0, None).await.unwrap();
        let link = loopback(&admin.paths, host.local_addr.port(), &share.code).await;

        let player = Scratch::new("cmd-refused-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());
        let id = join_over(&state, &admin.paths, &server, &link).await;

        // Something to update to, so nothing but the password stands in the way.
        add_jar(&admin.paths, &server, "new.jar").await;
        crate::server::share::replace_password(&admin.paths, &server.id)
            .await
            .unwrap();

        let mut inst = instance::load(&player.paths, &id).await.unwrap();
        let held = inst.joined_revision.clone();
        let notices = bring_up_to_date(&state, &mut inst, &Progress::silent("t"))
            .await
            .unwrap();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].contains("did not accept this instance's password"),
            "{}",
            notices[0]
        );
        // Sharing a server that is stopped right now answers the same way, and
        // a player told only about a password would ask for a link they have.
        assert!(
            notices[0].contains("may not be shared right now"),
            "{}",
            notices[0]
        );
        let saved = instance::load(&player.paths, &id).await.unwrap();
        assert_eq!(saved.joined_revision, held);
        assert!(!player.paths.game_dir(&id).join("mods/new.jar").exists());

        host.stop().await;
    }

    /// An update the server says is due has to finish: one that does not stops
    /// the launch, leaves the revision it had, and runs again next time.
    #[tokio::test]
    async fn an_update_that_cannot_finish_refuses_to_start_and_retries_next_time() {
        let admin = Scratch::new("cmd-unfinished-admin");
        let (server, share) = shared_server(&admin.paths).await;
        let host = share::host::Host::start(admin.paths.clone(), 0, None).await.unwrap();
        let link = loopback(&admin.paths, host.local_addr.port(), &share.code).await;

        let player = Scratch::new("cmd-unfinished-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());
        let id = join_over(&state, &admin.paths, &server, &link).await;
        add_jar(&admin.paths, &server, "new.jar").await;

        let mut inst = instance::load(&player.paths, &id).await.unwrap();
        let held = inst.joined_revision.clone();
        let cancelled = Progress::silent("t");
        cancelled
            .cancel_flag()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let err = bring_up_to_date(&state, &mut inst, &cancelled)
            .await
            .expect_err("an update the player cancelled");
        assert!(matches!(err, Error::Cancelled), "{err}");
        let saved = instance::load(&player.paths, &id).await.unwrap();
        assert_eq!(
            saved.joined_revision, held,
            "a cancelled update recorded a revision it did not install"
        );

        let mut inst = instance::load(&player.paths, &id).await.unwrap();
        let notices = bring_up_to_date(&state, &mut inst, &Progress::silent("t"))
            .await
            .unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        let saved = instance::load(&player.paths, &id).await.unwrap();
        assert_ne!(saved.joined_revision, held);
        assert!(player.paths.game_dir(&id).join("mods/new.jar").is_file());

        host.stop().await;
    }

    /// The link an admin hands out after replacing a password saves it onto
    /// the instance joined from that very server — and onto no other, however
    /// much of the link matches.
    #[tokio::test]
    async fn a_new_password_is_saved_only_for_the_same_server() {
        let admin = Scratch::new("cmd-relink-admin");
        let (one, share_one) = shared_server(&admin.paths).await;
        let (_two, share_two) = shared_server_called(&admin.paths, "creative", "Creative").await;
        let host = share::host::Host::start(admin.paths.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();

        let player = Scratch::new("cmd-relink-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());
        let joined = loopback(&admin.paths, port, &share_one.code).await;
        let id = join_over(&state, &admin.paths, &one, &joined).await;

        let replaced = crate::server::share::replace_password(&admin.paths, &one.id)
            .await
            .unwrap();
        let relinked = relink(
            &player.paths,
            &id,
            loopback(&admin.paths, port, &replaced.code).await,
        )
        .await
        .unwrap();
        assert_eq!(
            relinked.joined.as_ref().map(|l| &l.code),
            Some(&replaced.code)
        );
        assert_eq!(
            instance::load(&player.paths, &id)
                .await
                .unwrap()
                .joined
                .unwrap()
                .code,
            replaced.code
        );

        // The machine's other server, sharing the same address and certificate.
        let err = relink(
            &player.paths,
            &id,
            loopback(&admin.paths, port, &share_two.code).await,
        )
        .await
        .expect_err("another server on the same machine");
        assert!(err.to_string().contains("different server"), "{err}");

        // The right password, somewhere this instance was never joined from.
        let err = relink(
            &player.paths,
            &id,
            loopback(&admin.paths, 25591, &replaced.code).await,
        )
        .await
        .expect_err("another address");
        assert!(err.to_string().contains("different address"), "{err}");

        assert_eq!(
            instance::load(&player.paths, &id)
                .await
                .unwrap()
                .joined
                .unwrap()
                .code,
            replaced.code,
            "a refused link changed the instance"
        );

        host.stop().await;
    }

    /// A player who imported a second copy of a server keeps both when its
    /// password is replaced. A link names one instance, and that one is what
    /// every later link finds — so a copy left behind would be holding a
    /// password nothing can put right, and it is saved the new one too.
    #[tokio::test]
    async fn a_new_password_reaches_every_copy_joined_from_that_server() {
        let admin = Scratch::new("cmd-copies-admin");
        let (one, share_one) = shared_server(&admin.paths).await;
        let (two, share_two) = shared_server_called(&admin.paths, "creative", "Creative").await;
        let host = share::host::Host::start(admin.paths.clone(), 0, None).await.unwrap();
        let port = host.local_addr.port();

        let player = Scratch::new("cmd-copies-player");
        let state = AppState::new(player.paths.clone(), crate::config::Config::default());
        let joined = loopback(&admin.paths, port, &share_one.code).await;
        let first = join_over(&state, &admin.paths, &one, &joined).await;
        let second = join_over(&state, &admin.paths, &one, &joined).await;
        let elsewhere = {
            let link = loopback(&admin.paths, port, &share_two.code).await;
            join_over(&state, &admin.paths, &two, &link).await
        };
        assert_ne!(first, second, "a second copy is a second instance");

        let replaced = crate::server::share::replace_password(&admin.paths, &one.id)
            .await
            .unwrap();
        relink(
            &player.paths,
            &first,
            loopback(&admin.paths, port, &replaced.code).await,
        )
        .await
        .unwrap();

        let code_of = |id: &str| {
            let (paths, id) = (player.paths.clone(), id.to_string());
            async move {
                instance::load(&paths, &id)
                    .await
                    .unwrap()
                    .joined
                    .unwrap()
                    .code
            }
        };
        assert_eq!(code_of(&first).await, replaced.code);
        assert_eq!(
            code_of(&second).await,
            replaced.code,
            "the copy the link did not name was left unreachable"
        );
        assert_eq!(
            code_of(&elsewhere).await,
            share_two.code,
            "another server's instance took this server's password"
        );

        host.stop().await;
    }

    /// Add files is a way into the mods folder like any other, so it obeys the
    /// same rule: a server's mod is the server's, by whichever name it is on
    /// disk. The player is told, and nothing is copied.
    #[tokio::test]
    async fn add_files_refuses_to_copy_over_a_servers_mod() {
        let player = Scratch::new("cmd-add-local");
        let paths = &player.paths;

        let mut inst = Instance::new(
            "Joined".into(),
            "1.21.1".into(),
            Loader::Fabric,
            "0.16.9".into(),
        );
        inst.content = vec![serde_json::from_value(serde_json::json!({
            "fileName": "anticheat.jar", "kind": "mod", "provider": "modrinth",
            "projectId": "AC", "versionId": "v", "name": "Anti Cheat",
            "versionNumber": "1", "origin": "server",
        }))
        .unwrap()];
        inst.save(paths).await.unwrap();

        let mods = instance::content::content_dir(paths, &inst.id, ContentKind::Mod);
        tokio::fs::create_dir_all(&mods).await.unwrap();
        tokio::fs::write(mods.join("anticheat.jar"), b"the server's")
            .await
            .unwrap();

        let source_dir = paths.root.join("picked");
        tokio::fs::create_dir_all(&source_dir).await.unwrap();
        // The last two are the same mod as the record, spelled the way the
        // player's own copy happens to be spelled: on a case-insensitive disk
        // that name is the server's file.
        for spelling in [
            "anticheat.jar",
            "anticheat.jar.disabled",
            "AntiCheat.jar",
            "AntiCheat.JAR.disabled",
        ] {
            let source = source_dir.join(spelling);
            tokio::fs::write(&source, b"the player's").await.unwrap();
            let picked = vec![source.to_string_lossy().to_string()];
            let err = add_local_files(paths, &inst.id, ContentKind::Mod, &picked)
                .await
                .expect_err(spelling);
            assert!(
                err.to_string().contains("came from the server's pack"),
                "{spelling}: {err}"
            );
            assert_eq!(
                tokio::fs::read(mods.join("anticheat.jar")).await.unwrap(),
                b"the server's",
                "{spelling} was copied over the server's mod"
            );
            let mut left: Vec<String> = std::fs::read_dir(&mods)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            left.sort();
            assert_eq!(
                left,
                ["anticheat.jar"],
                "{spelling} landed beside the server's mod"
            );
        }

        // A mod of the player's own still arrives, so the refusal is about
        // where the file came from and not about the route.
        let own = source_dir.join("minimap.jar");
        tokio::fs::write(&own, b"the player's").await.unwrap();
        let added = add_local_files(
            paths,
            &inst.id,
            ContentKind::Mod,
            &[own.to_string_lossy().into()],
        )
        .await
        .unwrap();
        assert_eq!(added, 1);
    }

    /// A delete that cannot finish leaves the server visible and not shared,
    /// rather than gone from the UI with its listener still up.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_server_that_will_not_delete_stops_being_shared() {
        use std::os::unix::fs::PermissionsExt;

        let admin = Scratch::new("cmd-delete-server");
        let paths = &admin.paths;
        let (server, share) = shared_server(paths).await;
        // Both files at once — the state `share::load` allows for, where an
        // older Geode shared this again after this one stopped it — so the
        // password cannot outlive the server by hiding in the stopped one.
        crate::server::share::save(
            paths,
            &server.id,
            &ServerShare {
                shared: false,
                ..share
            },
        )
        .await
        .unwrap();

        // A folder the delete cannot get through: `remove_dir_all` has to list
        // what is inside, and this one refuses to be read. Removing a file by
        // name still works, which is what makes the order the two happen in
        // visible at all.
        let dir = paths.server(&server.id);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o300)).unwrap();

        let failed = crate::server::delete(paths, &server.id).await;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let failed = failed.expect_err("a delete that cannot read the folder it is deleting");

        // What the admin is told has to match what they can see: the server is
        // still in their list, so "was not found" would send them looking for
        // something that is right there.
        let said = failed.to_string();
        assert!(!said.contains("was not found"), "{said}");
        assert!(said.contains("no longer shared"), "{said}");

        assert!(
            !crate::server::share::path(paths, &server.id).exists(),
            "a share left behind keeps the listener up for a server nobody can see"
        );
        assert!(
            !crate::server::share::stopped_path(paths, &server.id).exists(),
            "a password left behind for a server that is going away"
        );
        assert!(
            crate::server::share::list(paths)
                .await
                .unwrap()
                .servers
                .is_empty(),
            "the share service still wants to share it"
        );
    }

    /// The guard on where a delete may reach runs before any of it happens, so
    /// an id that points outside the servers tree costs nothing — not even the
    /// two files clearing the share would remove by name.
    #[tokio::test]
    async fn a_delete_pointed_outside_the_tree_touches_nothing() {
        let admin = Scratch::new("cmd-delete-escape");
        let paths = &admin.paths;

        let elsewhere = paths.root.join("elsewhere");
        tokio::fs::create_dir_all(&elsewhere).await.unwrap();
        let share = elsewhere.join("share.json");
        tokio::fs::write(&share, b"somebody else's").await.unwrap();

        // Absolute, so `servers/<id>` is just `<id>` — the shape `Path::join`
        // lets through.
        let outside = elsewhere.to_string_lossy().to_string();
        let refused = crate::server::delete(paths, &outside)
            .await
            .expect_err("a delete outside the servers tree");
        assert!(refused.to_string().contains("unexpected path"), "{refused}");
        assert!(share.exists(), "the guard ran after something was removed");
    }
}

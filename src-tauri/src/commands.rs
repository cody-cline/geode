//! The IPC surface. Every command is thin: validate, delegate, return.

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
    let dir = instance::content::content_dir(&state.paths, &id, kind);
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
    let config = state.config().await;
    let cf_key = config.effective_curseforge_key();
    let cf_key = cf_key.as_deref();

    let mut inst = instance::load(&state.paths, &id).await?;
    let project = providers::project(&state.http, provider, &project_id, cf_key).await?;

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
        cf_key,
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

    let progress = Progress::new(app, format!("install:{id}:{project_id}"));
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
        cf_key,
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
    let config = state.config().await;
    let cf_key = config.effective_curseforge_key();
    let cf_key = cf_key.as_deref();

    let project = providers::project(&state.http, provider, &project_id, cf_key).await?;
    if project.kind != providers::ProjectKind::Modpack {
        return Err(Error::msg(format!("{} is not a modpack", project.name)));
    }

    // Packs are not filtered by loader or game version: the pack chooses those.
    let versions =
        providers::versions(&state.http, provider, &project_id, None, None, cf_key).await?;
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

    let progress = Progress::new(app, format!("pack:{}", project.id));
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
        cf_key,
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
    let progress = Progress::new(app, format!("link:{id}"));
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
    let progress = Progress::new(app, format!("updates:{id}"));
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
    let progress = Progress::new(app, format!("plan:{id}"));
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
    let config = state.config().await;
    let cf_key = config.effective_curseforge_key();
    let cf_key = cf_key.as_deref();

    let progress = Progress::new(app, format!("plan-build:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    // Re-resolve rather than trusting a target round-tripped through the
    // frontend: the per-mod verdicts decide which file each mod gets, and they
    // are cheap to recompute against the session cache.
    let matrix = state.connector.get(&state.http).await.unwrap_or_default();
    let resolution = crate::plan::resolve::resolve(
        &state.http,
        &plan,
        &matrix,
        &state.plan_support,
        cf_key,
        &progress,
    )
    .await?;

    let target = resolution
        .targets
        .iter()
        .find(|t| t.loader == loader && t.game_version == game_version)
        .ok_or_else(|| {
            Error::msg(format!(
                "{loader} {game_version} is no longer one of this plan's options — \
                 resolve it again"
            ))
        })?;

    let result = crate::plan::build::build(
        &state.http,
        &state.paths,
        &plan,
        target,
        name,
        cf_key,
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
    let progress = Progress::new(app, format!("pack-read:{project_id}"));
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
    let config = state.config().await;
    let cf_key = config.effective_curseforge_key();

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
    let config = state.config().await;
    let cf_key = config.effective_curseforge_key();
    let cf_key = cf_key.as_deref();

    let progress = Progress::new(app, format!("pack-server:{project_id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let result = async {
        let contents = crate::plan::pack::detailed(
            &state.http,
            &state.paths,
            provider,
            &project_id,
            version_id.as_deref(),
            cf_key,
            &progress,
        )
        .await?;
        crate::server::from_pack::from_modpack(
            &state.http,
            &state.paths,
            &contents,
            kind,
            name,
            cf_key,
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
    let progress = Progress::new(app, "git".to_string());
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

    let progress = Progress::new(app, format!("server:{}", server.id));
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
pub async fn delete_server(state: State<'_, AppState>, id: String) -> Result<()> {
    if state.servers.is_running(&id) {
        return Err(Error::msg("stop the server before deleting it"));
    }
    crate::server::delete(&state.paths, &id).await
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

    let progress = Progress::new(app, format!("server:{}", server.id));
    progress.stage("Finding Cardboard", 1).await;

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
    )
    .await?;

    // The folder a plugin gets dropped into should exist before anyone goes
    // looking for it.
    tokio::fs::create_dir_all(run.join(ContentKind::Plugin.folder())).await?;
    server.save(&state.paths).await?;
    progress.finish().await;

    Ok(server_view(server, &state))
}

#[tauri::command]
pub async fn start_server(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<u32> {
    if state.servers.is_running(&id) {
        return Err(Error::msg("that server is already running"));
    }
    let mut server = crate::server::load(&state.paths, &id).await?;

    // The server rotates its own log as it starts, so this is the moment the
    // oldest archive becomes one too many.
    if let Some(keep) = server.settings.keep_logs {
        let _ = crate::server::logs::prune(&state.paths, &id, keep as usize).await;
    }

    let progress = Progress::new(app.clone(), format!("server:{id}"));
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
        &app,
        &state.paths,
        &server,
        java,
        state.servers.clone(),
    )
    .await?;

    server.last_started = Some(chrono::Utc::now().timestamp());
    server.save(&state.paths).await?;
    Ok(pid)
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
    let progress = Progress::new(app, format!("updates:{id}"));
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
    let progress = Progress::new(app, format!("link:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    // Only the folders this server actually has. Looking in the others would
    // hash nothing and report on a folder that does not exist.
    let mut wanted = Vec::new();
    if server.takes_mods() {
        wanted.push(ContentKind::Mod);
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
            server.save(&state.paths).await?;
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
    let config = state.config().await;
    let cf_key = config.effective_curseforge_key();
    let cf_key = cf_key.as_deref();

    let mut server = crate::server::load(&state.paths, &id).await?;
    let project = providers::project(&state.http, provider, &project_id, cf_key).await?;

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
        ContentKind::Mod => server.kind.mod_loader().into_iter().collect(),
        _ => Vec::new(),
    };

    let mut chosen: Option<Version> = None;
    for loader in loaders.iter().map(|l| Some(*l)).chain(std::iter::once(None)) {
        let versions = providers::versions(
            &state.http,
            provider,
            &project_id,
            Some(&server.mc_version),
            loader,
            cf_key,
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

    let progress = Progress::new(app, format!("server-content:{id}:{project_id}"));
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
        cf_key,
        &progress,
    )
    .await;

    // Saved either way: a walk that failed part-way through still put files on
    // disk, and a record list that does not mention them is worse than none.
    let _ = server.save(&state.paths).await;

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

/// The server's logs: the one it is writing now, and the ones it rotated.
/// Build a plan into a server rather than an instance.
///
/// The plan resolves exactly as it would for an instance; what differs is that
/// mods which do not run on a server are left out and named, and that most
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
    let config = state.config().await;
    let cf_key = config.effective_curseforge_key();
    let cf_key = cf_key.as_deref();

    let progress = Progress::new(app, format!("plan-server:{id}"));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let matrix = state.connector.get(&state.http).await.unwrap_or_default();
    let resolution = crate::plan::resolve::resolve(
        &state.http,
        &plan,
        &matrix,
        &state.plan_support,
        cf_key,
        &progress,
    )
    .await?;

    let target = resolution
        .targets
        .iter()
        .find(|t| t.loader == loader && t.game_version == game_version)
        .ok_or_else(|| {
            Error::msg(format!(
                "{loader} {game_version} is no longer one of this plan's options — \
                 resolve it again"
            ))
        })?;

    let result = crate::server::from_plan::from_plan(
        &state.http,
        &state.paths,
        &plan,
        target,
        name,
        cf_key,
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
    let progress = Progress::new(app, format!("server-backup:{id}"));

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
    let progress = Progress::new(app, format!("java:{major}"));
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

    let config = state.config().await;
    let mut inst = instance::load(&state.paths, &id).await?;
    let progress = Progress::new(app.clone(), format!("launch:{id}"));
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

    inst.last_played = Some(chrono::Utc::now().timestamp());
    inst.save(&state.paths).await?;

    if config.hide_on_launch {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.hide();
        }
    }

    Ok(LaunchResult {
        pid,
        instance_id: id,
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
    let progress = Progress::new(app, format!("launch:{id}"));
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

    let progress = Progress::new(app, format!("import:{}", inst.id));
    state.register_cancel(progress.id(), progress.cancel_flag());

    let result = instance::modpack::import_into(
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

#[tauri::command]
pub async fn export_modpack(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    dest: String,
    include_saves: bool,
) -> Result<String> {
    let inst = instance::load(&state.paths, &id).await?;
    let progress = Progress::new(app, format!("export:{id}"));
    let dest = PathBuf::from(dest);

    instance::modpack::export_mrpack(&state.paths, &inst, dest.clone(), include_saves, &progress)
        .await?;
    Ok(dest.to_string_lossy().to_string())
}

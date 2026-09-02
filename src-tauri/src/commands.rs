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
}

fn view(instance: Instance, state: &AppState) -> InstanceView {
    InstanceView {
        running: state.games.is_running(&instance.id),
        mod_count: instance
            .content
            .iter()
            .filter(|c| c.kind == ContentKind::Mod)
            .count(),
        instance,
    }
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
    if let Some(icon) = patch.icon {
        inst.icon = icon;
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

/// Promote a mod that arrived as a dependency to one you keep on purpose, or
/// put it back.
///
/// Dependency data is never going to be perfect — a jar can require an id
/// nothing here declares, and the mod holding it up is then reported as unused.
/// This is the escape hatch: say you want it, and it stops being offered for
/// cleanup, permanently.
#[tauri::command]
pub async fn set_content_kept(
    state: State<'_, AppState>,
    id: String,
    kind: String,
    file: String,
    kept: bool,
) -> Result<()> {
    let kind = parse_kind(&kind)?;
    let mut inst = instance::load(&state.paths, &id).await?;
    let base = file.strip_suffix(".disabled").unwrap_or(&file);
    let record = inst
        .content
        .iter_mut()
        .find(|r| r.file_name == base && r.kind == kind)
        .ok_or_else(|| Error::msg("that file has no install record to change"))?;
    record.dependency = !kept;
    inst.save(&state.paths).await
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

    let result = crate::install::content::install_with_dependencies(
        &state.http,
        &state.paths,
        &mut inst,
        version,
        &project,
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
        &inst,
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

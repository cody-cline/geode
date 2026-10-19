pub mod auth;
pub mod commands;
pub mod config;
pub mod connector;
pub mod deeplink;
pub mod diagnostics;
pub mod desktop;
pub mod drive;
pub mod error;
pub mod events;
pub mod install;
pub mod instance;
pub mod java;
pub mod launch;
pub mod logs;
pub mod meta;
pub mod net;
pub mod paths;
pub mod plan;
pub mod providers;
pub mod server;
pub mod share;
pub mod state;

use tauri::Manager;

use crate::config::Config;
use crate::paths::Paths;
use crate::state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // First, before any other plugin starts anything: a second Geode, run
        // by the OS to open a `geode://` link, hands its arguments to this one
        // and exits instead of becoming a second launcher. The `deep-link`
        // feature passes the link on to the plugin below; this callback only
        // brings the window forward, which is all a plain second launch needs.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            deeplink::bring_forward(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let paths = Paths::new();

            // Let the webview load images out of Geode's own data directory.
            // The scope in tauri.conf.json is a guess at where that is — it
            // cannot know about GEODE_HOME, and its platform globs do not all
            // match what `dirs::data_dir` returns. This is the real path.
            if let Err(e) = app.asset_protocol_scope().allow_directory(&paths.root, true) {
                eprintln!("geode: could not allow images from the data directory: {e}");
            }

            // Config has to exist before the first command lands, and `setup`
            // is synchronous, so block just for this one read.
            let config = tauri::async_runtime::block_on(async {
                if let Err(e) = paths.ensure_all().await {
                    eprintln!("geode: could not create {}: {e}", paths.root.display());
                }
                Config::load(&paths).await.unwrap_or_default()
            });

            let state = AppState::new(paths.clone(), config);
            // Cloned out before the state is handed over, so the ticker owns
            // what it needs rather than reaching back through app state on
            // every pass.
            let servers = state.servers.clone();
            app.manage(state);
            // Tauri's own spawn, because `setup` runs before any Tokio
            // runtime context exists and `tokio::spawn` would panic here.
            tauri::async_runtime::spawn(crate::server::schedule::ticker(
                crate::desktop::sink(app.handle().clone()),
                paths,
                servers,
            ));
            // A join interrupted by the last exit left its download behind.
            // Cleared to completion here, before any link can start a new
            // join whose archive a slower sweep would then delete mid-download.
            tauri::async_runtime::block_on(crate::share::join::clear_incoming(
                &app.state::<AppState>().paths,
            ));
            // And a pack staged for an upload that never happened, for the
            // same reason: one pack is staged at a time and nothing can still
            // be using the last one.
            tauri::async_runtime::block_on(crate::drive::clear_staged(
                &app.state::<AppState>().paths,
            ));
            // Only once the state is managed: links are parked in it.
            deeplink::start(app);
            // A server shared when Geode last closed is shared again now,
            // without anyone having to reopen it — its players' links point
            // here and should keep working.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                state
                    .share
                    .reconcile(&state, Some(crate::desktop::sink(handle.clone())))
                    .await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Leaving orphaned JVMs behind is worse than a hard stop.
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.games.stop_all();
                    // Servers get the same treatment: an orphaned server still
                    // holds its port and its world files.
                    state.servers.stop_all();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_config,
            commands::set_curseforge_key,
            commands::test_curseforge_key,
            commands::app_info,
            commands::system_memory_mb,
            commands::list_game_versions,
            commands::list_loader_versions,
            commands::list_instances,
            commands::get_instance,
            commands::create_instance,
            commands::update_instance,
            commands::delete_instance,
            commands::duplicate_instance,
            commands::instance_folder,
            commands::list_content,
            commands::set_content_enabled,
            commands::set_server_side_mods,
            commands::remove_content,
            commands::add_local_content,
            commands::install_content,
            commands::connector_support,
            commands::install_modpack,
            commands::link_untracked_content,
            commands::check_content_updates,
            commands::apply_content_update,
            commands::search_projects,
            commands::get_project,
            commands::get_project_description,
            commands::get_project_versions,
            commands::get_categories,
            commands::list_plans,
            commands::get_plan,
            commands::create_plan,
            commands::save_plan,
            commands::delete_plan,
            commands::resolve_plan,
            commands::build_plan,
            commands::pack_contents,
            commands::server_kinds,
            commands::server_versions,
            commands::list_servers,
            commands::get_server,
            commands::create_server,
            commands::delete_server,
            commands::update_server,
            commands::accept_server_eula,
            commands::start_server,
            commands::stop_server,
            commands::kill_server,
            commands::send_server_command,
            commands::running_servers,
            commands::server_folder,
            commands::list_server_content,
            commands::install_server_content,
            commands::remove_server_content,
            commands::set_server_content_enabled,
            commands::move_server_mod,
            commands::add_cardboard,
            commands::set_instance_image,
            commands::set_server_image,
            commands::add_local_server_content,
            commands::check_server_content_updates,
            commands::apply_server_content_update,
            commands::link_untracked_server_content,
            commands::create_server_from_plan,
            commands::server_kinds_for_pack,
            commands::create_server_from_modpack,
            commands::server_commands,
            commands::list_instance_logs,
            commands::read_instance_log,
            commands::delete_instance_log,
            commands::instance_report,
            commands::server_report,
            commands::git_status,
            commands::install_git,
            commands::list_server_logs,
            commands::read_server_log,
            commands::delete_server_log,
            commands::list_server_backups,
            commands::create_server_backup,
            commands::delete_server_backup,
            commands::restore_server_backup,
            commands::read_server_properties,
            commands::write_server_properties,
            commands::list_accounts,
            commands::begin_microsoft_login,
            commands::complete_microsoft_login,
            commands::find_importable_accounts,
            commands::import_account,
            commands::add_offline_account,
            commands::remove_account,
            commands::set_active_account,
            commands::detect_java,
            commands::install_java,
            commands::launch_instance,
            commands::prepare_instance,
            commands::stop_instance,
            commands::running_instances,
            commands::cancel_task,
            commands::preview_modpack,
            commands::import_modpack,
            commands::export_modpack,
            commands::export_geodepack,
            commands::export_curseforge_pack,
            commands::get_share_status,
            commands::set_share_settings,
            commands::get_server_share,
            commands::share_server,
            commands::unshare_server,
            commands::set_server_share_config,
            commands::share_link_copied,
            commands::rotate_server_share_code,
            commands::preview_join,
            commands::join_server,
            commands::relink_joined_instance,
            commands::drive_status,
            commands::connect_drive,
            commands::disconnect_drive,
            commands::set_google_client_secret,
            commands::stage_drive_pack,
            commands::upload_drive_pack,
            commands::preview_pack_link,
            commands::fetch_pack_link,
            commands::import_pack_stage,
            deeplink::take_pending_join,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Geode")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Give the router its port back. Bounded inside, so a router
                // that has stopped answering cannot hold the exit up; if this
                // never runs — a crash — the mapping's lease runs out instead.
                if let Some(state) = app.try_state::<AppState>() {
                    tauri::async_runtime::block_on(state.share.shutdown());
                }
            }
        });
}

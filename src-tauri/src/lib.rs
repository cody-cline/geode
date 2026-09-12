pub mod auth;
pub mod commands;
pub mod config;
pub mod connector;
pub mod error;
pub mod install;
pub mod instance;
pub mod java;
pub mod launch;
pub mod meta;
pub mod net;
pub mod paths;
pub mod plan;
pub mod providers;
pub mod server;
pub mod state;

use tauri::Manager;

use crate::config::Config;
use crate::paths::Paths;
use crate::state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let paths = Paths::new();

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
            crate::server::schedule::spawn(app.handle().clone(), paths, servers);
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
            commands::add_cardboard,
            commands::create_server_from_plan,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Geode");
}

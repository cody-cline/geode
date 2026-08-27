pub mod auth;
pub mod commands;
pub mod config;
pub mod error;
pub mod install;
pub mod instance;
pub mod java;
pub mod launch;
pub mod meta;
pub mod net;
pub mod paths;
pub mod providers;
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

            app.manage(AppState::new(paths, config));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Leaving orphaned JVMs behind is worse than a hard stop.
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.games.stop_all();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_config,
            commands::set_curseforge_key,
            commands::app_info,
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
            commands::check_content_updates,
            commands::apply_content_update,
            commands::search_projects,
            commands::get_project,
            commands::get_project_description,
            commands::get_project_versions,
            commands::get_categories,
            commands::list_accounts,
            commands::begin_microsoft_login,
            commands::complete_microsoft_login,
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

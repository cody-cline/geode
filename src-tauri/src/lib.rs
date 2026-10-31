//! Geode's library: everything the app does, with nothing about how it is
//! driven.
//!
//! Two front ends share it. The desktop app — `commands`, `desktop`,
//! `deeplink` and [`run`] below — is behind the `desktop` feature, which is on
//! by default and is the only thing that pulls in Tauri. The server host,
//! [`daemon`], is built without it, so a container image compiles with no
//! webview toolchain anywhere near it.

pub mod auth;
#[cfg(feature = "desktop")]
pub mod commands;
pub mod config;
pub mod connector;
pub mod crash;
pub mod daemon;
#[cfg(feature = "desktop")]
pub mod deeplink;
pub mod diagnostics;
#[cfg(feature = "desktop")]
pub mod desktop;
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
/// The `geoded` hosts this app can send a server to, or bring one back from.
pub mod remote;
pub mod rt;
pub mod server;
pub mod settings;
pub mod share;
pub mod state;
pub mod update;

#[cfg(feature = "desktop")]
use tauri::Manager;

#[cfg(feature = "desktop")]
use crate::config::Config;
#[cfg(feature = "desktop")]
use crate::paths::Paths;
#[cfg(feature = "desktop")]
use crate::state::AppState;

#[cfg(feature = "desktop")]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default()
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
        .plugin(tauri_plugin_dialog::init());

    // Registered only in a build that was compiled with a public key to verify
    // an update against. Without one there is no plugin at all, so nothing can
    // reach a manifest and nothing can be downloaded — which is the whole point
    // of the feature: an update Geode cannot prove came from the same place
    // this build did is worse than no updater. The key overrides the
    // deliberately empty `pubkey` in tauri.conf.json; see `update.rs` for why
    // it is read from the environment rather than committed.
    //
    // It goes here rather than at the top of the chain because single-instance
    // has to be first, for the reason given above.
    if let Some(pubkey) = update::public_key() {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().pubkey(pubkey).build());
    }

    builder
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
            // And a server that was arriving when Geode last died. The host
            // has swept this since it gained the route; the app drives every
            // pull and never did, so a transfer whose process was killed
            // part-way left up to the bundle ceiling sitting under
            // `transfers/` for ever. Nothing can be arriving yet, for the
            // same reason as the two above.
            tauri::async_runtime::block_on(crate::server::transfer::sweep(
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
            commands::app_info,
            commands::check_for_update,
            commands::install_update,
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
            commands::set_server_settings_allowed,
            commands::last_server_sync,
            commands::change_setting_back,
            commands::remove_content,
            commands::add_local_content,
            commands::install_content,
            commands::connector_support,
            commands::install_modpack,
            commands::check_pack_update,
            commands::plan_pack_update,
            commands::apply_pack_update,
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
            commands::set_server_mod_side,
            commands::set_server_mod_kept,
            commands::list_server_client_only_mods,
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
            commands::instance_crash,
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
            commands::remote_hosts,
            commands::add_remote_host,
            commands::forget_remote_host,
            commands::remote_host_servers,
            commands::transfer_size,
            commands::send_server_to_host,
            commands::bring_server_from_host,
            commands::read_server_properties,
            commands::write_server_properties,
            commands::server_player_lists,
            commands::server_online_players,
            commands::add_to_server_list,
            commands::remove_from_server_list,
            commands::kick_server_player,
            commands::set_server_whitelist,
            commands::game_port_clashes,
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
            commands::export_groups,
            commands::export_plan,
            commands::export_modpack,
            commands::export_geodepack,
            commands::export_curseforge_pack,
            commands::export_prism_instance,
            commands::get_share_status,
            commands::recheck_router,
            commands::set_share_settings,
            commands::get_server_share,
            commands::share_server,
            commands::unshare_server,
            commands::set_server_share_config,
            commands::get_server_settings_picker,
            commands::set_server_settings_picks,
            commands::review_server_settings,
            commands::push_server_settings,
            commands::share_link_copied,
            commands::set_server_listing,
            commands::set_server_forbidden,
            commands::rotate_server_share_code,
            commands::preview_join,
            commands::join_server,
            commands::relink_joined_instance,
            commands::preview_pack_link,
            commands::build_pack_link,
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

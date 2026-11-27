//! WEave — Tauri 2 + Rust port of the WEave (Wallpaper Engine Workshop Manager)
//! Python/PyQt6 application. Preserves full feature parity with the original
//! while adding a modern, animated web-based UI.

pub mod commands;
pub mod config;
pub mod core;
pub mod errors;
pub mod plugins;
pub mod services;

pub use core::app_state;
pub use core::constants;
pub use core::logger;
pub use core::runtime;
pub use errors::{AppError, AuthReason, ErrorCode};

// `plugins` is deliberately not re-exported at the crate root: callers use the
// full `crate::plugins::…` path, which keeps the layer visible at the call site.

pub use services::accounts;
pub use services::download;
pub use services::extract;
pub use services::i18n;
pub use services::metadata;
pub use services::qr;
pub use services::translator;
pub use services::updater;
pub use services::we_client;
pub use services::workshop;

use std::sync::Arc;
use tauri::Manager;

use crate::app_state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize custom logger - fallback to env_logger if it fails
    let log_level = if cfg!(debug_assertions) {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    // Try to initialize custom logger, fallback to env_logger.
    // The data directory comes from the one resolver so the logs always sit
    // next to the config, WebView2 profile and plugins.
    let log_dir = app_state::resolve_app_data_dir();
    if logger::WEaveLogger::init(&log_dir, log_level).is_err() {
        env_logger::try_init().ok();
    }

    let builder = tauri::Builder::default()
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    // Alt+F4 and the taskbar close button must take the same
                    // path as the custom title bar: a plain close would destroy
                    // only the main window and leave the hidden steam-webview
                    // (and therefore the process) alive.
                    api.prevent_close();
                    crate::core::shutdown::request(window.app_handle(), "close-requested");
                }
            }
        })
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                // Showing a window that has not had its size applied yet leaves it at
                // the platform default, which on Windows can be a handful of pixels —
                // the app then looks like it failed to start. Make sure the size is
                // sane before revealing it.
                let healthy = window
                    .outer_size()
                    .map(|s| s.width >= 400 && s.height >= 300)
                    .unwrap_or(false);
                if !healthy {
                    let _ = window.set_size(tauri::PhysicalSize::new(1200, 700));
                    let _ = window.center();
                }
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .setup(|app| {
            let state = AppState::initialize(app.handle().clone())?;
            let arc_state = Arc::new(state);
            // Sync cookies from WebView2 on startup so Workshop requests are authenticated
            arc_state.init_cookies();
            // Don't start .NET Runtime check here - let frontend trigger it
            // arc_state.init_dotnet_runtime();
            let sync_settings_before_manage = arc_state.settings.clone();
            let sync_data_dir_before_manage = arc_state.app_data_dir.clone();
            app.manage(arc_state);

            // Streams live outside AppState so they can be torn down
            // independently and survive account refreshes.
            app.manage(Arc::new(commands::video_stream::StreamRegistry::new()));
            // Phone sync engine: owns its worker thread, fed by the we-sync
            // crate; settings are stored under the "sync" key.
            let sync_settings = sync_settings_before_manage;
            let sync_data_dir = sync_data_dir_before_manage;
            let sync = Arc::new(services::sync::SyncService::new(
                app.handle().clone(),
                sync_settings,
                sync_data_dir,
            ));
            app.manage(sync.clone());
            sync.auto_start();
            // Watchdog: if the main window ever disappears without going
            // through CloseRequested, still terminate the process tree.
            crate::core::shutdown::install_watchdog(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_get_info,
            commands::app::app_open_data_dir,
            commands::app::app_quit,
            commands::app::app_save_window_geometry,
            commands::app::app_restore_window_geometry,
            commands::app::app_show_main_window,
            commands::app::app_init_metadata,
            commands::app::app_cancel_init_metadata,
            commands::config::config_get_all,
            commands::config::config_set,
            commands::config::config_get,
            commands::i18n::i18n_get_available_languages,
            commands::i18n::i18n_set_language,
            commands::accounts::accounts_list,
            commands::accounts::accounts_list_custom,
            commands::accounts::accounts_set_custom,
            commands::accounts::accounts_remove_custom,
            commands::accounts::accounts_verify_custom,
            commands::accounts::accounts_submit_2fa,
            commands::accounts::accounts_cancel_auth,
            commands::updater::updater_check,
            commands::updater::updater_skip_version,
            commands::metadata::metadata_get_all,
            commands::metadata::metadata_get,
            commands::translator::translator_translate,
            commands::we::we_set_directory,
            commands::we::we_get_directory,
            commands::we::we_list_installed,
            commands::we::we_apply,
            commands::we::we_apply_properties,
            commands::we::we_get_project_json,
            commands::we::we_open,
            commands::we::we_delete_wallpaper,
            commands::we::we_delete_wallpapers_bulk,
            commands::we::we_save_project_properties,
            commands::we::we_restore_project_properties,
            commands::we::we_active_pubfileids,
            commands::we::we_play,
            commands::we::we_pause,
            commands::we::we_stop,
            commands::we::we_mute,
            commands::we::we_unmute,
            commands::we::we_next_wallpaper,
            commands::we::we_previous_wallpaper,
            commands::we::we_show_icons,
            commands::we::we_hide_icons,
            commands::we::we_open_playlist,
            commands::we::we_open_profile,
            commands::workshop::workshop_browse,
            commands::workshop::workshop_browse_sized,
            commands::workshop::workshop_browse_collections,
            commands::workshop::workshop_get_item,
            commands::workshop::workshop_get_collection,
            commands::workshop::workshop_get_author_items,
            commands::workshop::workshop_get_author_collections,
            commands::workshop::workshop_refresh_cache,
            commands::workshop::workshop_debug_log,
            commands::workshop::workshop_debug_clear,
            commands::download::download_start,
            commands::download::download_cancel,
            commands::download::download_multi_start,
            commands::download::download_submit_2fa,
            commands::extract::extract_start,
            commands::image::open_path,
            commands::steam::steam_parser_show,
            commands::steam::steam_sync_cookies,
            commands::steam::steam_is_logged_in,
            commands::steam::steam_current_account,
            commands::steam::steam_auto_login,
            commands::steam::steam_login_prepare,
            commands::steam::steam_sign_out,
            commands::steam::steam_clear_browsing_cache,
            commands::steam::steam_qr_begin,
            commands::steam::steam_qr_poll,
            commands::steam::steam_qr_login_finalize,
            commands::steam::steam_login_poll_error,
            commands::steam::steam_restore_session,
            commands::steam::steam_probe_account,
            commands::steam::steam_oauth_begin,
            commands::steam::steam_oauth_submit_guard,
            commands::dotnet::dotnet_init,
            commands::plugins::plugins_init,
            commands::logging::log_frontend_message,
            commands::video_stream::video_stream_cache_clear,
            commands::video_stream::video_stream_cache_stats,
            commands::video_stream::video_stream_player,
            commands::video_stream::video_stream_prewarm,
            commands::video_stream::video_stream_start,
            commands::video_stream::video_stream_status,
            commands::video_stream::video_stream_stop,
            commands::video_stream::video_stream_stop_all,
            commands::sync::sync_config_get,
            commands::sync::sync_config_set,
            commands::sync::sync_now,
            commands::sync::sync_pause,
            commands::sync::sync_start,
            commands::sync::sync_status,
            commands::sync::sync_stop,
            commands::sync::sync_transfer_pause,
        ]);

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

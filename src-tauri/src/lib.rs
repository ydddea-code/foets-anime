//! Fets Anime — Tauri 2 backend.

pub mod commands;
pub mod database;
pub mod library;
pub mod player;
pub mod scanner;
pub mod settings;
pub mod state;

use std::sync::atomic::{AtomicI64, AtomicU64};
use std::sync::{Arc, Mutex};

use tauri::Manager;

/// Project-level configuration (rename the app here).
pub const APP_NAME: &str = "Fets Anime";

pub fn run() {
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,fets_anime_lib=debug"),
    )
    .format_timestamp_millis()
    .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            std::fs::create_dir_all(data_dir.join("covers"))?;
            std::fs::create_dir_all(data_dir.join("logs"))?;

            let db = Arc::new(database::Db::open(&data_dir.join("library.db"))?);

            // Player backend: real libmpv embedding on Windows, explicit
            // unavailable stub elsewhere (never faked).
            #[cfg(windows)]
            let (backend, embed) = {
                use player::windows::MpvEmbed;
                let e = Arc::new(MpvEmbed::new());
                e.attach_app(handle.clone());
                (e.clone() as Arc<dyn player::api::PlayerBackend>, Some(e))
            };
            #[cfg(not(windows))]
            let (backend, embed): (Arc<dyn player::api::PlayerBackend>, Option<()>) =
                (player::api::create_backend(), None);

            let state = Arc::new(state::AppState {
                db,
                player: backend,
                embed,
                scan: Mutex::new(None),
                current_video: AtomicI64::new(0),
                current_anime: AtomicI64::new(0),
                last_position_saved: AtomicU64::new(0),
                data_dir,
            });
            app.manage(state.clone());

            // Flush playback position + cancel scans when the window closes.
            let s = state.clone();
            if let Some(win) = handle.get_webview_window("main") {
                let s2 = s.clone();
                win.on_window_event(move |ev| match ev {
                    tauri::WindowEvent::CloseRequested { .. } => {
                        log::info!("window closing: flushing playback position");
                        state::flush_position(&s2);
                        // request scan cancellation so no orphan worker remains
                        if let Ok(g) = s2.scan.lock() {
                            if let Some(job) = g.as_ref() {
                                job.request_cancel();
                            }
                        }
                    }
                    tauri::WindowEvent::Resized(_) => {
                        // frontend re-measures #player-host and calls
                        // player_set_bounds; nothing native to do here.
                    }
                    _ => {}
                });
            }

            log::info!("{APP_NAME} v{} started (data: {:?})", env!("CARGO_PKG_VERSION"), state.data_dir);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::add_anime_folder,
            commands::list_anime,
            commands::get_anime,
            commands::list_episodes,
            commands::rename_anime,
            commands::remove_from_library,
            commands::delete_folder_from_disk,
            commands::change_cover,
            commands::reset_cover,
            commands::remove_custom_cover,
            commands::scan_library,
            commands::cancel_scan,
            commands::scan_status,
            commands::player_init,
            commands::player_set_bounds,
            commands::player_load,
            commands::player_pause,
            commands::player_seek,
            commands::player_volume,
            commands::player_mute,
            commands::player_speed,
            commands::player_set_track,
            commands::player_status,
            commands::player_tracks,
            commands::player_stop,
            commands::continue_watching,
            commands::recently_watched,
            commands::clear_history,
            commands::report_position,
            commands::get_settings,
            commands::set_setting,
            commands::open_data_dir,
            commands::app_info,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // final safety flush before process exit
                if let Some(state) = app_handle.try_state::<Arc<state::AppState>>() {
                    state::flush_position(&state.inner());
                    if let Ok(g) = state.scan.lock() {
                        if let Some(job) = g.as_ref() {
                            job.request_cancel();
                            let _ = job.wait_finished(std::time::Duration::from_secs(3));
                        }
                    }
                    state.player.shutdown();
                }
            }
        });
}

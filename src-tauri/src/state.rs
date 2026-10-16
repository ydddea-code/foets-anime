//! Shared application state managed by Tauri.

use std::sync::atomic::{AtomicI64, AtomicU64};
use std::sync::{Arc, Mutex};

use crate::database::Db;
use crate::player::api::PlayerBackend;
use crate::scanner::ScanJob;

pub struct AppState {
    pub db: Arc<Db>,
    pub player: Arc<dyn PlayerBackend>,
    /// The concrete embed (Windows only) for HWND-specific operations.
    #[cfg(windows)]
    pub embed: Option<Arc<crate::player::windows::MpvEmbed>>,
    #[cfg(not(windows))]
    #[allow(dead_code)]
    pub embed: Option<()>,

    pub scan: Mutex<Option<Arc<ScanJob>>>,
    pub current_video: AtomicI64,
    pub current_anime: AtomicI64,
    pub last_position_saved: AtomicU64,
    pub data_dir: std::path::PathBuf,
}

impl AppState {
    pub fn history_enabled(&self) -> bool {
        self.db
            .lock()
            .ok()
            .and_then(|c| crate::database::repo::get_setting(&c, "history_enabled").ok().flatten())
            .map(|v| v != "false")
            .unwrap_or(true)
    }
}

/// Called on app exit and when leaving the player: persist position safely.
pub fn flush_position(state: &Arc<AppState>) {
    let vid = state.current_video.load(std::sync::atomic::Ordering::SeqCst);
    if vid == 0 || !state.history_enabled() {
        return;
    }
    if let Ok(st) = state.player.status() {
        if st.position.is_nan() || st.position <= 0.0 {
            return;
        }
        let dur = if st.duration.is_nan() || st.duration <= 0.0 { None } else { Some(st.duration) };
        if let Ok(c) = state.db.lock() {
            let _ = crate::database::repo::save_position(&c, vid, st.position, dur);
        }
    }
}

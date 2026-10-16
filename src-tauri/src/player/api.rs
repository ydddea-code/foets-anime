//! Backend-agnostic player API. The Windows implementation embeds libmpv;
//! other platforms report unavailability instead of pretending to work.

use serde::{Deserialize, Serialize};

use crate::database::error::{AppError, AppResult};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlayerStatus {
    pub idle: bool,
    pub paused: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: i64,
    pub muted: bool,
    pub speed: f64,
    pub filename: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfo {
    pub id: i64,
    pub kind: String, // video | audio | sub
    pub selected: bool,
    pub title: Option<String>,
    pub lang: Option<String>,
}

/// Abstract control surface used by the Tauri command layer.
pub trait PlayerBackend: Send + Sync {
    fn init(&self, hwnd: isize) -> AppResult<()>;
    fn load(&self, path: &str, start_pos: Option<f64>) -> AppResult<()>;
    fn play_pause(&self, pause: bool) -> AppResult<()>;
    fn seek(&self, secs: f64, absolute: bool) -> AppResult<()>;
    fn set_volume(&self, vol: i64) -> AppResult<()>;
    fn set_muted(&self, muted: bool) -> AppResult<()>;
    fn set_speed(&self, speed: f64) -> AppResult<()>;
    fn cycle_track(&self, kind: &str, id: i64) -> AppResult<()>;
    fn status(&self) -> AppResult<PlayerStatus>;
    fn tracks(&self) -> AppResult<Vec<TrackInfo>>;
    fn resize(&self, width: u32, height: u32);
    fn shutdown(&self);
    fn is_active(&self) -> bool;
}

pub struct UnavailablePlayer;

impl PlayerBackend for UnavailablePlayer {
    fn init(&self, _hwnd: isize) -> AppResult<()> {
        Err(AppError::PlayerUnavailable(
            "MPV embedding is only available on Windows builds (libmpv-2.dll)."
                .to_string(),
        ))
    }
    fn load(&self, _p: &str, _s: Option<f64>) -> AppResult<()> {
        self.init(0)
    }
    fn play_pause(&self, _: bool) -> AppResult<()> {
        self.init(0)
    }
    fn seek(&self, _: f64, _: bool) -> AppResult<()> {
        self.init(0)
    }
    fn set_volume(&self, _: i64) -> AppResult<()> {
        self.init(0)
    }
    fn set_muted(&self, _: bool) -> AppResult<()> {
        self.init(0)
    }
    fn set_speed(&self, _: f64) -> AppResult<()> {
        self.init(0)
    }
    fn cycle_track(&self, _: &str, _: i64) -> AppResult<()> {
        self.init(0)
    }
    fn status(&self) -> AppResult<PlayerStatus> {
        self.init(0)
    }
    fn tracks(&self) -> AppResult<Vec<TrackInfo>> {
        self.init(0)
    }
    fn resize(&self, _: u32, _: u32) {}
    fn shutdown(&self) {}
    fn is_active(&self) -> bool {
        false
    }
}

#[cfg(windows)]
pub fn create_backend() -> std::sync::Arc<dyn PlayerBackend> {
    std::sync::Arc::new(crate::player::windows::MpvEmbed::new())
}

#[cfg(not(windows))]
pub fn create_backend() -> std::sync::Arc<dyn PlayerBackend> {
    std::sync::Arc::new(UnavailablePlayer)
}

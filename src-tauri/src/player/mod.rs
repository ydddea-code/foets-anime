//! MPV player integration.
//!
//! Architecture decision (documented in docs/ARCHITECTURE.md):
//! - On Windows, `windows::MpvEmbed` loads `libmpv-2.dll` at runtime and uses
//!   the **MPV C render API** (`mpv_render_context_create` with the native
//!   window HWND obtained from Tauri's root WebView via `WM_GETPARENT`) to
//!   render video *inside* the application window. No external mpv.exe, no
//!   separate player window.
//! - The DLL is loaded dynamically so the app still launches (with a clear
//!   error message) when MPV is missing, and unit tests can run without it.
//! - On Linux/macOS dev machines the same command surface exists but returns
//!   "player unavailable" — embedding is implemented only for the supported
//!   target (Windows), never faked.

pub mod api;
#[cfg(windows)]
pub mod windows;

use serde::{Deserialize, Serialize};

pub use api::{PlayerBackend, PlayerStatus, TrackInfo};

/// One property change forwarded from the real MPV event loop.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MpvEvent {
    pub name: String,
    pub value: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadParams {
    pub path: String,
    pub start_pos: Option<f64>,
    pub anime_id: i64,
    pub video_id: i64,
}

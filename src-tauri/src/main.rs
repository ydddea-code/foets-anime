// Prevents an extra console window on Windows in release builds (windows_subsystem
// is set via Cargo/Tauri config; this guards the debug binary too).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    fets_anime_lib::run();
}

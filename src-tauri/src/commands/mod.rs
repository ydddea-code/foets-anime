//! Tauri IPC command layer. Every path from the frontend is validated here;
//! no shell commands are ever constructed from user input.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::Value;

use crate::database::{repo, AppError, AppResult, Db};
use crate::library::{covers, paths};
use crate::player::api::PlayerBackend;
use crate::scanner;
use crate::state::{flush_position, AppState};

fn data_dir(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| AppError::Other(format!("cannot resolve app data dir: {e}")))
}

/// Ensure an anime has the app-generated default cover (auto-thumbnailing of
/// videos is opt-in only — never a mandatory step when adding folders).
fn ensure_default_cover(db: &Db, dd: &Path, id: i64, name: &str) {
    let have = db
        .lock()
        .ok()
        .and_then(|c| repo::get_anime(&c, id).ok())
        .and_then(|a| a.cover_url);
    if have.is_some() {
        return;
    }
    if let Ok(p) = covers::default_cover(name, &dd.join("covers"), id) {
        if let Ok(c) = db.lock() {
            let _ = repo::set_covers(&c, id, Some(&p.to_string_lossy()), None);
        }
    }
}

// ---------- library ----------

#[tauri::command]
pub async fn add_anime_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    path: String,
    as_parent: Option<bool>,
) -> AppResult<Value> {
    let norm = paths::validate_folder_path(&path).map_err(AppError::InvalidInput)?;
    let real = PathBuf::from(&norm);
    if !real.is_dir() {
        return Err(AppError::NotFound(format!(
            "Folder does not exist or is not accessible: {norm}"
        )));
    }
    let dd = data_dir(&app)?;
    let mut added: Vec<Value> = Vec::new();

    if as_parent.unwrap_or(false) {
        // Parent-folder mode: each direct subfolder becomes its own entry.
        let entries: Vec<PathBuf> = std::fs::read_dir(&real)
            .map_err(AppError::Io)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        if entries.is_empty() {
            return Err(AppError::InvalidInput(
                "The selected parent folder contains no subfolders.".into(),
            ));
        }
        for e in entries {
            let name = e.file_name().unwrap_or_default().to_string_lossy().to_string();
            let pth = paths::normalize_path(&e.to_string_lossy());
            let (id, created) = {
                let c = state.db.lock()?;
                repo::add_anime_folder(&c, &name, &pth)?
            };
            if created {
                ensure_default_cover(&state.db, &dd, id, &name);
            }
            added.push(serde_json::json!({ "id": id, "name": name, "created": created }));
        }
    } else {
        let name = real
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| norm.clone());
        let (id, created) = {
            let c = state.db.lock()?;
            repo::add_anime_folder(&c, &name, &norm)?
        };
        if created {
            ensure_default_cover(&state.db, &dd, id, &name);
        }
        added.push(serde_json::json!({ "id": id, "name": name, "created": created }));
    }
    // NOTE: adding a folder does NOT scan it. Scanning is manual only.
    Ok(serde_json::json!({ "added": added }))
}

#[tauri::command]
pub async fn list_anime(
    state: tauri::State<'_, Arc<AppState>>,
    search: Option<String>,
    sort: Option<String>,
) -> AppResult<Vec<repo::AnimeSummary>> {
    let c = state.db.lock()?;
    repo::list_anime(&c, search.as_deref(), sort.as_deref().unwrap_or("name"))
}

#[tauri::command]
pub async fn get_anime(state: tauri::State<'_, Arc<AppState>>, id: i64) -> AppResult<repo::AnimeSummary> {
    let c = state.db.lock()?;
    repo::get_anime(&c, id)
}

#[tauri::command]
pub async fn list_episodes(
    state: tauri::State<'_, Arc<AppState>>,
    anime_id: i64,
) -> AppResult<Vec<repo::EpisodeInfo>> {
    let c = state.db.lock()?;
    repo::list_episodes(&c, anime_id)
}

#[tauri::command]
pub async fn rename_anime(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
    name: String,
) -> AppResult<()> {
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(AppError::InvalidInput("display name must be 1–200 characters".into()));
    }
    let c = state.db.lock()?;
    repo::rename_anime(&c, id, &name)
}

/// Remove the library entry ONLY. Video files on disk are never touched.
#[tauri::command]
pub async fn remove_from_library(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
    confirm: bool,
) -> AppResult<()> {
    if !confirm {
        return Err(AppError::InvalidInput("confirmation required".into()));
    }
    let dd = data_dir(&app)?;
    let entry = {
        let c = state.db.lock()?;
        let a = repo::get_anime(&c, id)?;
        repo::delete_anime_entry(&c, id)?;
        a
    };
    // clean up app-owned cover thumbnails (never user originals)
    for p in [entry.custom_cover_path.clone(), entry.cover_url.clone()].into_iter().flatten() {
        let pb = PathBuf::from(&p);
        if pb.starts_with(dd.join("covers")) {
            let _ = std::fs::remove_file(pb);
        }
    }
    log::info!("removed anime {id} from library (files on disk untouched)");
    Ok(())
}

/// Separate destructive action requiring the exact full path as confirmation.
#[tauri::command]
pub async fn delete_folder_from_disk(
    state: tauri::State<'_, Arc<AppState>>,
    id: i64,
    confirmed_path: String,
) -> AppResult<()> {
    let folder = {
        let c = state.db.lock()?;
        repo::get_anime(&c, id)?.folder_path
    };
    let given = paths::normalize_path(&confirmed_path);
    let stored = paths::normalize_path(&folder);
    if given != stored {
        return Err(AppError::InvalidInput(
            "Confirmation path does not match the stored folder path.".into(),
        ));
    }
    let p = PathBuf::from(&stored);
    let md = std::fs::symlink_metadata(&p)?;
    if md.file_type().is_symlink() {
        return Err(AppError::InvalidInput(
            "Refusing to delete a symlink/junction; remove it manually.".into(),
        ));
    }
    if !md.is_dir() {
        return Err(AppError::InvalidInput("target is not a directory".into()));
    }
    std::fs::remove_dir_all(&p).map_err(|e| {
        AppError::Other(format!("failed to delete folder ({e}); original files were left as-is"))
    })?;
    let c = state.db.lock()?;
    repo::delete_anime_entry(&c, id)?;
    log::warn!("DELETED folder from disk: {stored}");
    Ok(())
}

// ---------- covers ----------

#[tauri::command]
pub async fn change_cover(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    anime_id: i64,
    image_path: String,
) -> AppResult<String> {
    let src_str = paths::normalize_path(&image_path);
    if src_str.is_empty() {
        return Err(AppError::InvalidInput("empty image path".into()));
    }
    let src = PathBuf::from(&src_str);
    let dd = data_dir(&app)?;
    let thumb = covers::make_thumbnail(&src, &dd.join("covers"), &format!("anime{anime_id}"))?;
    let c = state.db.lock()?;
    repo::get_anime(&c, anime_id)?;
    repo::set_custom_cover(&c, anime_id, Some(&thumb.to_string_lossy()))?;
    Ok(thumb.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn reset_cover(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    anime_id: i64,
) -> AppResult<()> {
    let dd = data_dir(&app)?;
    let (custom, name) = {
        let c = state.db.lock()?;
        let a = repo::get_anime(&c, anime_id)?;
        repo::set_custom_cover(&c, anime_id, None)?;
        (a.custom_cover_path, a.display_name)
    };
    if let Some(p) = custom {
        let pb = PathBuf::from(p);
        if pb.starts_with(dd.join("covers")) {
            let _ = std::fs::remove_file(pb);
        }
    }
    ensure_default_cover(&state.db, &dd, anime_id, &name);
    Ok(())
}

#[tauri::command]
pub async fn remove_custom_cover(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    anime_id: i64,
) -> AppResult<()> {
    reset_cover(app, state, anime_id).await
}

// ---------- scanning ----------

#[derive(serde::Serialize)]
pub struct ScanStatus {
    pub running: bool,
    pub job_id: u64,
}

#[tauri::command]
pub async fn scan_library(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    anime_ids: Option<Vec<i64>>,
    force_full: Option<bool>,
) -> AppResult<ScanStatus> {
    let mut guard = state.scan.lock().map_err(|_| AppError::Other("state poisoned".into()))?;
    if let Some(job) = guard.as_ref() {
        if !job.is_finished() {
            return Err(AppError::Other("A scan is already running. Cancel it first.".into()));
        }
    }
    // Ensure the previous worker thread has truly exited before starting anew.
    if let Some(job) = guard.take() {
        if !job.wait_finished(std::time::Duration::from_secs(5)) {
            return Err(AppError::Other("previous scan is still finishing".into()));
        }
        drop(job);
    }
    let ids: Vec<i64> = match anime_ids {
        Some(v) if !v.is_empty() => v,
        _ => {
            let c = state.db.lock()?;
            repo::all_folders(&c)?.into_iter().map(|(id, _)| id).collect()
        }
    };
    if ids.is_empty() {
        return Err(AppError::InvalidInput(
            "Library is empty — add an anime folder first.".into(),
        ));
    }
    let job = scanner::spawn(app, state.db.clone(), ids, force_full.unwrap_or(false));
    let id = job.id;
    *guard = Some(job);
    Ok(ScanStatus { running: true, job_id: id })
}

#[tauri::command]
pub async fn cancel_scan(state: tauri::State<'_, Arc<AppState>>) -> AppResult<bool> {
    let guard = state.scan.lock().map_err(|_| AppError::Other("state poisoned".into()))?;
    match guard.as_ref() {
        Some(job) if !job.is_finished() => {
            job.request_cancel();
            log::info!("scan {} cancellation requested", job.id);
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[tauri::command]
pub async fn scan_status(state: tauri::State<'_, Arc<AppState>>) -> AppResult<ScanStatus> {
    let guard = state.scan.lock().map_err(|_| AppError::Other("state poisoned".into()))?;
    let running = guard.as_ref().map(|j| !j.is_finished()).unwrap_or(false);
    let job_id = guard.as_ref().map(|j| j.id).unwrap_or(0);
    Ok(ScanStatus { running, job_id })
}

// ---------- player ----------

#[tauri::command]
pub async fn player_init(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> AppResult<()> {
    #[cfg(windows)]
    {
        if let Some(embed) = &state.embed {
            let win = app
                .get_webview_window("main")
                .ok_or_else(|| AppError::PlayerUnavailable("main window missing".into()))?;
            // Resolve the real native frame HWND and read its current client
            // rect so the child render window starts at sane coordinates.
            let (cx, cy, cw, ch) = embed.frame_client_rect()?;
            embed.set_parent_from_window(&win)?;
            state.player.init(0)?;
            crate::player::windows::set_player_bounds(embed, 0, 0, cw as i32, ch as i32)?;
            // The frontend immediately follows up with player_set_bounds using
            // the measured #player-host rect; this is just a safe fallback.
            let _ = (cx, cy);
            return Ok(());
        }
    }
    state.player.init(0)
}

#[tauri::command]
pub async fn player_set_bounds(
    state: tauri::State<'_, Arc<AppState>>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> AppResult<()> {
    #[cfg(windows)]
    {
        if let Some(embed) = &state.embed {
            return crate::player::windows::set_player_bounds(embed, x, y, w, h);
        }
    }
    let _ = (x, y, w, h);
    Ok(())
}

#[tauri::command]
pub async fn player_load(
    state: tauri::State<'_, Arc<AppState>>,
    path: String,
    start_pos: Option<f64>,
    video_id: Option<i64>,
) -> AppResult<()> {
    let norm = paths::normalize_path(&path);
    let p = PathBuf::from(&norm);
    if !p.is_file() {
        return Err(AppError::NotFound(format!("Video file not found: {norm}")));
    }
    // save position of whatever was playing before switching
    flush_position(state.inner());
    if let Some(vid) = video_id {
        state.current_video.store(vid, Ordering::SeqCst);
        let c = state.db.lock()?;
        if let Ok(ep) = repo::get_episode(&c, vid) {
            state.current_anime.store(ep.anime_id, Ordering::SeqCst);
        }
    }
    state.player.load(&norm, start_pos)
}

#[tauri::command]
pub async fn player_pause(state: tauri::State<'_, Arc<AppState>>, pause: bool) -> AppResult<()> {
    if pause {
        flush_position(state.inner());
    }
    state.player.play_pause(pause)
}

#[tauri::command]
pub async fn player_seek(state: tauri::State<'_, Arc<AppState>>, secs: f64, absolute: bool) -> AppResult<()> {
    if !secs.is_finite() {
        return Err(AppError::InvalidInput("invalid seek value".into()));
    }
    state.player.seek(secs, absolute)
}

#[tauri::command]
pub async fn player_volume(state: tauri::State<'_, Arc<AppState>>, volume: i64) -> AppResult<()> {
    state.player.set_volume(volume.clamp(0, 130))
}

#[tauri::command]
pub async fn player_mute(state: tauri::State<'_, Arc<AppState>>, muted: bool) -> AppResult<()> {
    state.player.set_muted(muted)
}

#[tauri::command]
pub async fn player_speed(state: tauri::State<'_, Arc<AppState>>, speed: f64) -> AppResult<()> {
    if !(0.25..=4.0).contains(&speed) {
        return Err(AppError::InvalidInput("speed must be between 0.25 and 4.0".into()));
    }
    state.player.set_speed(speed)
}

#[tauri::command]
pub async fn player_set_track(
    state: tauri::State<'_, Arc<AppState>>,
    kind: String,
    id: i64,
) -> AppResult<()> {
    state.player.cycle_track(&kind, id)
}

#[tauri::command]
pub async fn player_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> AppResult<crate::player::api::PlayerStatus> {
    state.player.status()
}

#[tauri::command]
pub async fn player_tracks(
    state: tauri::State<'_, Arc<AppState>>,
) -> AppResult<Vec<crate::player::api::TrackInfo>> {
    state.player.tracks()
}

#[tauri::command]
pub async fn player_stop(state: tauri::State<'_, Arc<AppState>>) -> AppResult<()> {
    flush_position(state.inner());
    state.player.play_pause(true)
}

// ---------- history ----------

#[tauri::command]
pub async fn continue_watching(
    state: tauri::State<'_, Arc<AppState>>,
) -> AppResult<Vec<repo::HistoryEntry>> {
    let c = state.db.lock()?;
    repo::continue_watching(&c)
}

#[tauri::command]
pub async fn recently_watched(
    state: tauri::State<'_, Arc<AppState>>,
) -> AppResult<Vec<repo::HistoryEntry>> {
    let c = state.db.lock()?;
    repo::recently_watched(&c)
}

#[tauri::command]
pub async fn clear_history(state: tauri::State<'_, Arc<AppState>>) -> AppResult<()> {
    let c = state.db.lock()?;
    c.execute("DELETE FROM watch_history", [])?;
    Ok(())
}

/// Called by the frontend periodically (every ~5 s) while playing.
#[tauri::command]
pub async fn report_position(
    state: tauri::State<'_, Arc<AppState>>,
    video_id: i64,
    position: f64,
    duration: Option<f64>,
) -> AppResult<()> {
    if video_id <= 0 || !position.is_finite() || position < 0.0 {
        return Ok(());
    }
    if !state.history_enabled() {
        return Ok(());
    }
    // throttle writes: at most once per 4 seconds per session
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = state.last_position_saved.swap(now, Ordering::SeqCst);
    if now - last >= 4 {
        let c = state.db.lock()?;
        repo::save_position(&c, video_id, position, duration.filter(|d| *d > 0.0))?;
    }
    Ok(())
}

// ---------- settings ----------

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub theme: String,
    pub history_enabled: bool,
    pub auto_thumbnail: bool,
    pub subtitle_lang: String,
    pub audio_lang: String,
    pub default_speed: f64,
    pub default_volume: i64,
    pub keep_awake: bool,
}

#[tauri::command]
pub async fn get_settings(state: tauri::State<'_, Arc<AppState>>) -> AppResult<Settings> {
    let c = state.db.lock()?;
    let g = |k: &str, d: &str| repo::get_setting(&c, k).ok().flatten().unwrap_or_else(|| d.to_string());
    Ok(Settings {
        theme: g("theme", "dark"),
        history_enabled: g("history_enabled", "true") != "false",
        auto_thumbnail: g("auto_thumbnail", "false") == "true",
        subtitle_lang: g("subtitle_lang", ""),
        audio_lang: g("audio_lang", ""),
        default_speed: g("default_speed", "1.0").parse().unwrap_or(1.0),
        default_volume: g("default_volume", "100").parse().unwrap_or(100),
        keep_awake: g("keep_awake", "false") == "true",
    })
}

#[tauri::command]
pub async fn set_setting(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> AppResult<()> {
    if !crate::settings::is_allowed_key(&key) {
        return Err(AppError::InvalidInput(format!("unknown setting key: {key}")));
    }
    if value.len() > 200 {
        return Err(AppError::InvalidInput("setting value too long".into()));
    }
    let c = state.db.lock()?;
    repo::set_setting(&c, &key, &value)
}

#[tauri::command]
pub async fn open_data_dir(app: tauri::AppHandle) -> AppResult<String> {
    let dd = data_dir(&app)?;
    let logs = dd.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(dd.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| AppError::Other(e.to_string()))?;
    Ok(dd.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn app_info(app: tauri::AppHandle) -> AppResult<Value> {
    Ok(serde_json::json!({
        "name": "Fets Anime",
        "version": app.package_info().version.to_string(),
        "platform": std::env::consts::OS,
        "mpvEmbedded": cfg!(windows),
        "dataDir": data_dir(&app).ok().map(|p| p.to_string_lossy().to_string()),
    }))
}

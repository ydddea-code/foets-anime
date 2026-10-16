//! Asynchronous library scanner with real cancellation.
//!
//! Design:
//! - The scan job runs on a dedicated OS thread (UI never blocks).
//! - An `AtomicBool` cancellation token is checked during file discovery and
//!   between every batch of work, so Cancel stops actual backend work — not
//!   just the progress bar. Already-committed results are kept.
//! - No subprocesses are launched at all: metadata comes from filesystem stat
//!   plus in-process MKV/MP4 container probing, therefore no console window
//!   can ever appear (no CMD / PowerShell / FFprobe intermediary).
//! - Incremental: parsed metadata is cached keyed by path+size+mtime; a
//!   force-rescan clears the cache (size+mtime alone is only a cache hint).

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::Emitter;

use crate::database::{repo, Db};
use crate::library::paths;

pub const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "webm", "avi", "mov", "m4v", "flv", "wmv", "mpg", "mpeg", "ts", "ogv",
];

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub state: String, // scanning | completed | cancelled | error
    pub files_checked: u64,
    pub videos_found: u64,
    pub current: String,
    pub total_estimate: Option<u64>,
    pub message: Option<String>,
}

/// Handle to one running scan. Only one global scan runs at a time (enforced
/// by AppState); the join handle lets app-shutdown wait for the worker.
pub struct ScanJob {
    pub cancel: Arc<AtomicBool>,
    pub done: Arc<AtomicBool>,
    pub id: u64,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl ScanJob {
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn is_finished(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
    /// Bounded wait until the worker thread actually exits.
    pub fn wait_finished(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.is_finished() {
            if let Ok(mut g) = self.join.lock() {
                if let Some(h) = g.take() {
                    let _ = h.join();
                }
            }
            return true;
        }
        false
    }
}

impl Drop for ScanJob {
    fn drop(&mut self) {
        // Never leave an orphan worker: signal cancel, then join.
        self.cancel.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.join.lock() {
            if let Some(h) = g.take() {
                let _ = h.join();
            }
        }
    }
}

static SCAN_ID: AtomicU64 = AtomicU64::new(1);

/// Spawn a scan over the given anime folder ids. Emits `scan://progress`.
pub fn spawn(app: tauri::AppHandle, db: Arc<Db>, anime_ids: Vec<i64>, force_full: bool) -> Arc<ScanJob> {
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let job = Arc::new(ScanJob {
        cancel: cancel.clone(),
        done: done.clone(),
        id: SCAN_ID.fetch_add(1, Ordering::Relaxed),
        join: Mutex::new(None),
    });

    let handle = std::thread::Builder::new()
        .name("fets-scanner".into())
        .spawn(move || {
            run(db, anime_ids, force_full, &cancel, &done, &|p| emit(&app, p));
        })
        .expect("failed to spawn scanner thread");
    if let Ok(mut g) = job.join.lock() {
        *g = Some(handle);
    }
    job
}

fn emit(app: &tauri::AppHandle, p: &ScanProgress) {
    let _ = app.emit("scan://progress", p);
}

/// Core loop. Split out from `spawn` so tests can drive it without Tauri.
pub fn run(
    db: Arc<Db>,
    anime_ids: Vec<i64>,
    force_full: bool,
    cancel: &AtomicBool,
    done: &AtomicBool,
    sink: &dyn Fn(&ScanProgress),
) {
    let mut prog = ScanProgress {
        state: "scanning".into(),
        files_checked: 0,
        videos_found: 0,
        current: String::new(),
        total_estimate: None,
        message: None,
    };

    if force_full {
        if let Ok(c) = db.lock() {
            let _ = repo::cache_clear(&c);
        }
    }

    for id in &anime_ids {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (folder, name) = {
            let Ok(c) = db.lock() else { break };
            match repo::get_anime(&c, *id) {
                Ok(a) => (a.folder_path, a.display_name),
                Err(_) => continue,
            }
        };
        let root = PathBuf::from(&folder);
        if !root.is_dir() {
            if let Ok(c) = db.lock() {
                let _ = repo::update_folder_status(&c, *id, "missing");
            }
            prog.message = Some(format!("Folder not found: {name}"));
            sink(&prog);
            continue;
        }
        scan_one_folder(&db, *id, &root, cancel, sink, &mut prog);
    }

    prog.state = if cancel.load(Ordering::Relaxed) {
        "cancelled".into()
    } else {
        "completed".into()
    };
    prog.current.clear();
    sink(&prog);
    log::info!(
        "scan finished: {} ({} files checked, {} videos)",
        prog.state,
        prog.files_checked,
        prog.videos_found
    );
    done.store(true, Ordering::SeqCst);
}

/// Bounded BFS discovery feeding small batches so directory walking and DB
/// writes overlap without unbounded memory use.
pub fn scan_one_folder(
    db: &Arc<Db>,
    anime_id: i64,
    root: &Path,
    cancel: &AtomicBool,
    sink: &dyn Fn(&ScanProgress),
    prog: &mut ScanProgress,
) {
    let scan_started = repo::now_secs();
    let mut dirs = VecDeque::new();
    dirs.push_back(root.to_path_buf());
    let mut batch: Vec<(PathBuf, fs::Metadata)> = Vec::with_capacity(64);
    let mut last_emit = Instant::now();
    let mut err_count = 0u32;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return; // keep prior committed data untouched
        }
        let Some(dir) = dirs.pop_front() else { break };

        match fs::read_dir(&dir) {
            Ok(rd) => {
                for entry in rd.flatten() {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let path = entry.path();
                    let ft = match entry.file_type() {
                        Ok(ft) => ft,
                        Err(_) => {
                            err_count += 1;
                            continue;
                        }
                    };
                    if ft.is_dir() {
                        // Do not descend into symlinked dirs (loops/junctions).
                        let md = fs::symlink_metadata(&path);
                        if md.map(|m| m.file_type().is_symlink()).unwrap_or(true) {
                            continue;
                        }
                        dirs.push_back(path);
                        continue;
                    }
                    if !ft.is_file() {
                        continue;
                    }
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_ascii_lowercase())
                        .unwrap_or_default();
                    if !VIDEO_EXTS.contains(&ext.as_str()) {
                        prog.files_checked += 1;
                        continue;
                    }
                    let md = match fs::metadata(&path) {
                        Ok(m) => m,
                        Err(_) => {
                            // locked / partially copied / permission issues
                            err_count += 1;
                            prog.files_checked += 1;
                            continue;
                        }
                    };
                    batch.push((path, md));
                    if batch.len() >= 64 {
                        flush_batch(db, anime_id, root, &mut batch, prog, cancel, sink, &mut last_emit);
                    }
                }
            }
            Err(e) => {
                log::warn!("cannot read {}: {e}", dir.display());
                err_count += 1;
            }
        }
        prog.total_estimate = Some(prog.files_checked + (dirs.len() as u64) * 8);
        if last_emit.elapsed() > Duration::from_millis(250) {
            sink(prog);
            last_emit = Instant::now();
        }
    }

    if !batch.is_empty() {
        flush_batch(db, anime_id, root, &mut batch, prog, cancel, sink, &mut last_emit);
    }

    if cancel.load(Ordering::Relaxed) {
        return;
    }

    // Prune files that vanished since this pass started (single tx).
    if let Ok(c) = db.lock() {
        let tx = match c.transaction() {
            Ok(t) => t,
            Err(_) => return,
        };
        match repo::prune_unseen(&tx, anime_id, scan_started) {
            Ok(n) if n > 0 => log::info!("pruned {n} stale files for anime {anime_id}"),
            Err(e) => log::error!("prune failed: {e}"),
            _ => {}
        }
        let _ = repo::mark_scan_time(&tx, anime_id);
        let _ = repo::update_folder_status(&tx, anime_id, "ok");
        if tx.commit().is_err() {
            return;
        }
    }
    if err_count > 0 {
        prog.message = Some(format!("{err_count} entries could not be read (permissions/locked)"));
    }
    sink(prog);
}

fn flush_batch(
    db: &Arc<Db>,
    anime_id: i64,
    root: &Path,
    batch: &mut Vec<(PathBuf, fs::Metadata)>,
    prog: &mut ScanProgress,
    cancel: &AtomicBool,
    sink: &dyn Fn(&ScanProgress),
    last_emit: &mut Instant,
) {
    if cancel.load(Ordering::Relaxed) {
        batch.clear();
        return;
    }
    if let Ok(c) = db.lock() {
        for (path, md) in batch.drain(..) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let size = md.len() as i64;
            let modified = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let path_str = path.to_string_lossy().to_string();
            prog.current = paths::safe_rel_path(root, &path);
            prog.files_checked += 1;

            let cached = repo::cache_get(&c, &path_str).ok().flatten();
            let (dur, w, h) = match cached {
                Some(cm) if cm.size == size && cm.modified == modified => {
                    (cm.duration, cm.width, cm.height)
                }
                _ => {
                    let meta = probe_container(&path, size).unwrap_or((None, None, None));
                    let _ = repo::cache_put(&c, &path_str, size, modified, meta.0, meta.1, meta.2);
                    meta
                }
            };
            let rel = paths::safe_rel_path(root, &path);
            let fname = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            match repo::upsert_video(&c, anime_id, &path_str, &rel, &fname, size, modified, dur, w, h) {
                Ok(()) => prog.videos_found += 1,
                Err(e) => log::error!("upsert failed for {path_str}: {e}"),
            }
        }
    }
    if last_emit.elapsed() > Duration::from_millis(250) {
        sink(prog);
        *last_emit = Instant::now();
    }
}

/// Minimal in-process container prober (no FFprobe subprocess → no console).
/// Reads Matroska EBML duration and MP4/MOV moov/mvhd + tkhd dimensions.
pub fn probe_container(path: &Path, size: i64) -> Option<(Option<f64>, Option<i64>, Option<i64>)> {
    if size < 12 {
        return None;
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mkv" | "webm" => mkv::duration(path).map(|d| (Some(d), None, None)),
        "mp4" | "m4v" | "mov" => mp4::parse(path),
        _ => None,
    }
}

mod mkv {
    use std::fs::File;
    use std::io::{BufReader, Read, Seek, SeekFrom};

    /// Parse Segment/Info/TimecodeScale + Duration. Bounded header walk.
    pub fn duration(path: &std::path::Path) -> Option<f64> {
        let mut f = BufReader::new(File::open(path).ok()?);
        let seg_start;
        loop {
            let id = read_element_id(&mut f)?;
            let size = read_element_size(&mut f)?;
            if id == 0x1853_8067 {
                seg_start = f.stream_position().ok()?;
                break;
            }
            if size == u64::MAX {
                return None;
            }
            f.seek(SeekFrom::Current(size as i64)).ok()?;
            if f.stream_position().ok()? > 4_000_000 {
                return None;
            }
        }
        let limit = seg_start + 2_000_000;
        let mut pos = seg_start;
        let mut info_body: Option<u64> = None;
        while pos < limit {
            f.seek(SeekFrom::Start(pos)).ok()?;
            let id = read_element_id(&mut f)?;
            let size = read_element_size(&mut f)?;
            let body = f.stream_position().ok()?;
            if id == 0x448A {
                info_body = Some(body);
                break;
            }
            if size == u64::MAX {
                return None;
            }
            pos = body + size;
        }
        let mut scale: f64 = 1_000_000.0;
        let mut p = info_body?;
        while p < limit {
            f.seek(SeekFrom::Start(p)).ok()?;
            let id = read_element_id(&mut f)?;
            let size = read_element_size(&mut f)?;
            let body = f.stream_position().ok()?;
            match id {
                0x4489 => {
                    let dur = read_float_at(&mut f, body, size)?;
                    return Some(dur * scale / 1e9);
                }
                0x2AD7_B80 => {
                    if size <= 8 {
                        scale = read_uint_at(&mut f, body, size)? as f64;
                    }
                }
                _ => {}
            }
            if size == u64::MAX {
                return None;
            }
            p = body + size;
        }
        None
    }

    fn read_element_id<R: Read>(r: &mut R) -> Option<u64> {
        let first = read_u8(r)?;
        let len = match first {
            b if b & 0x80 != 0 => 1,
            b if b & 0xC0 == 0x40 => 2,
            b if b & 0xE0 == 0x20 => 3,
            b if b & 0xF0 == 0x10 => 4,
            _ => return None,
        };
        let mut id = first as u64;
        for _ in 1..len {
            id = (id << 8) | read_u8(r)? as u64;
        }
        Some(id)
    }

    fn read_element_size<R: Read>(r: &mut R) -> Option<u64> {
        let first = read_u8(r)?;
        let mut len = 1u32;
        let mut mask = 0x80u8;
        while first & mask == 0 {
            mask >>= 1;
            len += 1;
            if len > 8 {
                return None;
            }
        }
        let mut size = (first & (mask - 1)) as u64;
        for _ in 1..len {
            size = (size << 8) | read_u8(r)? as u64;
        }
        if size == (1u64 << (7 * len)) - 1 {
            return Some(u64::MAX); // unknown-size marker
        }
        Some(size)
    }

    fn read_u8<R: Read>(r: &mut R) -> Option<u8> {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).ok()?;
        Some(b[0])
    }

    fn read_uint_at<R: Read + Seek>(r: &mut R, at: u64, size: u64) -> Option<u64> {
        r.seek(SeekFrom::Start(at)).ok()?;
        let mut v = 0u64;
        for _ in 0..size.min(8) {
            v = (v << 8) | read_u8(r)? as u64;
        }
        Some(v)
    }

    fn read_float_at<R: Read + Seek>(r: &mut R, at: u64, size: u64) -> Option<f64> {
        r.seek(SeekFrom::Start(at)).ok()?;
        match size {
            4 => {
                let mut b = [0u8; 4];
                r.read_exact(&mut b).ok()?;
                Some(f32::from_bits(u32::from_be_bytes(b)) as f64)
            }
            8 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b).ok()?;
                Some(f64::from_bits(u64::from_be_bytes(b)))
            }
            _ => None,
        }
    }
}

mod mp4 {
    use std::fs::File;
    use std::io::{BufReader, Read, Seek, SeekFrom};

    pub fn parse(path: &std::path::Path) -> Option<(Option<f64>, Option<i64>, Option<i64>)> {
        let mut f = BufReader::new(File::open(path).ok()?);
        let file_len = f.seek(SeekFrom::End(0)).ok()?;
        let mut pos = 0u64;
        let head_limit = file_len.min(64 * 1024 * 1024);
        while pos + 8 <= head_limit {
            let hdr = read_at_raw(&mut f, pos)?;
            let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
            let ty: [u8; 4] = [hdr[4], hdr[5], hdr[6], hdr[7]];
            if size == 1 {
                size = u64::from_be_bytes(read8(&mut f, pos + 8)?);
            } else if size == 0 {
                size = file_len - pos;
            }
            if size < 8 {
                return None;
            }
            if ty == *b"moov" {
                return parse_moov(&mut f, pos + 8, pos + size);
            }
            pos += size;
        }
        None
    }

    fn parse_moov<R: Read + Seek>(
        f: &mut R,
        mut pos: u64,
        end: u64,
    ) -> Option<(Option<f64>, Option<i64>, Option<i64>)> {
        let mut mvhd: Option<(f64, f64)> = None; // (timescale, duration)
        let mut dims: Option<(i64, i64)> = None;
        while pos + 8 <= end {
            let hdr = read_at_raw(f, pos)?;
            let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
            let ty: [u8; 4] = [hdr[4], hdr[5], hdr[6], hdr[7]];
            if size == 1 {
                size = u64::from_be_bytes(read8(f, pos + 8)?);
            } else if size == 0 {
                size = end - pos;
            }
            if size < 8 {
                break;
            }
            let body = pos + 8;
            match &ty {
                b"mvhd" => {
                    let ver = read1(f, body)?;
                    let (ts, dur) = if ver == 1 {
                        let ts = u32::from_be_bytes(read4(f, body + 20)?) as f64;
                        let dur = u64::from_be_bytes(read8(f, body + 24)?) as f64;
                        (ts, dur)
                    } else {
                        let ts = u32::from_be_bytes(read4(f, body + 12)?) as f64;
                        let dur = u32::from_be_bytes(read4(f, body + 16)?) as f64;
                        (ts, dur)
                    };
                    if ts > 0.0 {
                        mvhd = Some((ts, dur));
                    }
                }
                b"trak" => {
                    if dims.is_none() {
                        dims = find_tkhd(f, body, body + size);
                    }
                }
                _ => {}
            }
            pos = body + size;
        }
        let dur = mvhd.map(|(ts, d)| d / ts);
        match dims {
            Some((w, h)) => Some((dur, Some(w), Some(h))),
            None => Some((dur, None, None)),
        }
    }

    fn find_tkhd<R: Read + Seek>(f: &mut R, mut pos: u64, end: u64) -> Option<(i64, i64)> {
        while pos + 8 <= end {
            let hdr = read_at_raw(f, pos)?;
            let size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
            let ty: [u8; 4] = [hdr[4], hdr[5], hdr[6], hdr[7]];
            if size < 8 {
                return None;
            }
            if ty == *b"tkhd" {
                // width/height: final 8 bytes of the box, 16.16 fixed point
                let off = pos + size - 8;
                let w = u32::from_be_bytes(read4(f, off)?);
                let h = u32::from_be_bytes(read4(f, off + 4)?);
                let wi = (w >> 16) as i64;
                let hi = (h >> 16) as i64;
                if wi > 0 && hi > 0 {
                    return Some((wi, hi));
                }
                return None;
            }
            pos += size;
        }
        None
    }

    fn read_at_raw<R: Read + Seek>(f: &mut R, at: u64) -> Option<[u8; 8]> {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut b = [0u8; 8];
        f.read_exact(&mut b).ok()?;
        Some(b)
    }
    fn read1<R: Read + Seek>(f: &mut R, at: u64) -> Option<u8> {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut b = [0u8; 1];
        f.read_exact(&mut b).ok()?;
        Some(b[0])
    }
    fn read4<R: Read + Seek>(f: &mut R, at: u64) -> Option<[u8; 4]> {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut b = [0u8; 4];
        f.read_exact(&mut b).ok()?;
        Some(b)
    }
    fn read8<R: Read + Seek>(f: &mut R, at: u64) -> Option<[u8; 8]> {
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut b = [0u8; 8];
        f.read_exact(&mut b).ok()?;
        Some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn setup() -> (Arc<Db>, tempfile::TempDir, i64) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("t.db")).unwrap());
        let anime_dir = dir.path().join("anime");
        fs::create_dir_all(&anime_dir).unwrap();
        let (id, _) = {
            let c = db.lock().unwrap();
            repo::add_anime_folder(&c, "Sample", &anime_dir.to_string_lossy()).unwrap()
        };
        (db, dir, id)
    }

    fn touch_video(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, b"\x1aE\xdf\xa3padding-not-a-real-mkv").unwrap();
        p
    }

    #[test]
    fn full_scan_finds_videos_and_skips_nonvideo() {
        let (db, _t, id) = setup();
        let root = {
            let c = db.lock().unwrap();
            PathBuf::from(repo::get_anime(&c, id).unwrap().folder_path)
        };
        touch_video(&root, "[Sub][Sample][01][720p].mkv");
        touch_video(&root, "[Sub][Sample][02][720p].mkv");
        fs::write(root.join("notes.txt"), b"hi").unwrap();
        fs::create_dir_all(root.join("Extras")).unwrap();
        touch_video(&root.join("Extras"), "special.mkv");

        let cancel = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        let events = Mutex::new(Vec::<String>::new());
        run(
            db.clone(),
            vec![id],
            false,
            &cancel,
            &done,
            &|p: &ScanProgress| events.lock().unwrap().push(p.state.clone()),
        );
        assert!(done.load(Ordering::SeqCst));
        let c = db.lock().unwrap();
        let eps = repo::list_episodes(&c, id).unwrap();
        assert_eq!(eps.len(), 3); // nested Extras included
        assert_eq!(eps[0].file_name, "[Sub][Sample][01][720p].mkv"); // episode sort
        let last = events.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last, "completed");
    }

    #[test]
    fn cancel_stops_backend_and_keeps_committed_data() {
        let (db, _t, id) = setup();
        let root = {
            let c = db.lock().unwrap();
            PathBuf::from(repo::get_anime(&c, id).unwrap().folder_path)
        };
        for i in 0..300 {
            touch_video(&root, &format!("Episode {:03}.mkv", i));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(AtomicU64::new(0));

        let c2 = cancel.clone();
        let d2 = done.clone();
        let s2 = seen.clone();
        let db2 = db.clone();
        let h = std::thread::spawn(move || {
            run(
                db2,
                vec![id],
                false,
                &c2,
                &d2,
                &|_p: &ScanProgress| {
                    // cancel shortly after the first progress event
                    if s2.fetch_add(1, Ordering::SeqCst) == 0 {
                        c2.store(true, Ordering::Relaxed);
                    }
                },
            );
        });
        h.join().unwrap();
        assert!(done.load(Ordering::SeqCst));
        assert!(cancel.load(Ordering::Relaxed));
        // some data may or may not be committed depending on timing; ensure DB consistent
        let c = db.lock().unwrap();
        let n = c
            .query_row("SELECT count(*) FROM video_file", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert!((0..300).contains(&n));
    }

    #[test]
    fn missing_folder_marked() {
        let (db, _t, id) = setup();
        {
            let c = db.lock().unwrap();
            let fp = repo::get_anime(&c, id).unwrap().folder_path;
            fs::remove_dir_all(&fp).unwrap();
        }
        let cancel = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        run(db.clone(), vec![id], false, &cancel, &done, &|_| {});
        let c = db.lock().unwrap();
        assert_eq!(repo::get_anime(&c, id).unwrap().status, "missing");
    }

    #[test]
    fn incremental_second_scan_uses_cache_no_prune_churn() {
        let (db, _t, id) = setup();
        let root = {
            let c = db.lock().unwrap();
            PathBuf::from(repo::get_anime(&c, id).unwrap().folder_path)
        };
        touch_video(&root, "Episode 01.mkv");
        let cancel = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        run(db.clone(), vec![id], false, &cancel, &done, &|_| {});
        run(db.clone(), vec![id], false, &cancel, &done, &|_| {});
        let c = db.lock().unwrap();
        assert_eq!(repo::list_episodes(&c, id).unwrap().len(), 1);
        // removed file is pruned on second scan
        fs::remove_file(root.join("Episode 01.mkv")).unwrap();
        run(db.clone(), vec![id], false, &cancel, &done, &|_| {});
        assert!(repo::list_episodes(&c, id).unwrap().is_empty());
    }
}

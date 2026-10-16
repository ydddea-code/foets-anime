//! Relational queries for anime / video_file / watch_history / settings.
//! No file-list JSON blobs: everything is row-based.

use rusqlite::{params, Connection, Row};
use serde::Serialize;

use super::error::{AppError, AppResult};

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeSummary {
    pub id: i64,
    pub display_name: String,
    pub folder_path: String,
    pub episode_count: i64,
    pub status: String,
    pub cover_url: Option<String>,
    pub last_watched_at: Option<i64>,
    pub max_episode_sort: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeInfo {
    pub id: i64,
    pub anime_id: i64,
    pub file_name: String,
    pub rel_path: String,
    pub path: String,
    pub duration_secs: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub size: i64,
    pub position_secs: Option<f64>,
    pub watched_at: Option<i64>,
    pub completed: bool,
}

fn map_anime(r: &Row) -> rusqlite::Result<AnimeSummary> {
    Ok(AnimeSummary {
        id: r.get(0)?,
        display_name: r.get(1)?,
        folder_path: r.get(2)?,
        episode_count: r.get(3)?,
        status: r.get(4)?,
        cover_url: r.get(5)?,
        last_watched_at: r.get::<_, Option<i64>>(6).ok().flatten(),
        max_episode_sort: r.get::<_, Option<f64>>(7).ok().flatten(),
    })
}

const ANIME_SELECT: &str = "
SELECT a.id, a.display_name, a.folder_path,
  (SELECT count(*) FROM video_file v WHERE v.anime_id = a.id) AS ep_count,
  a.status,
  COALESCE(a.custom_cover_path, a.default_cover_path),
  (SELECT max(h.watched_at) FROM watch_history h JOIN video_file v ON v.id = h.video_id WHERE v.anime_id = a.id),
  (SELECT max(v.episode_sort) FROM video_file v WHERE v.anime_id = a.id)
FROM anime a";

/// List library with search/sort. `sort`: name | recent | episodes
pub fn list_anime(conn: &Connection, search: Option<&str>, sort: &str) -> AppResult<Vec<AnimeSummary>> {
    let base = ANIME_SELECT;
    let where_sql = if search.map(|s| !s.is_empty()).unwrap_or(false) {
        " WHERE a.display_name LIKE '%' || ?1 || '%' OR EXISTS (SELECT 1 FROM video_file v WHERE v.anime_id=a.id AND v.file_name LIKE '%' || ?1 || '%')"
    } else {
        ""
    };
    let order_sql = match sort {
        "recent" => "ORDER BY (SELECT max(h.watched_at) FROM watch_history h JOIN video_file v ON v.id=h.video_id WHERE v.anime_id=a.id) DESC NULLS LAST, a.display_name COLLATE NOCASE ASC",
        "episodes" => "ORDER BY (SELECT count(*) FROM video_file v WHERE v.anime_id=a.id) DESC, a.display_name COLLATE NOCASE ASC",
        _ => "ORDER BY a.display_name COLLATE NOCASE ASC",
    };
    let full = format!("{base}{where_sql}{order_sql}");
    let mut stmt = conn.prepare(&full)?;
    let items = match search {
        Some(s) if !s.is_empty() => stmt
            .query_map(params![s], map_anime)?
            .collect::<Result<Vec<_>, _>>()?,
        _ => stmt
            .query_map([], map_anime)?
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(items)
}

pub fn get_anime(conn: &Connection, id: i64) -> AppResult<AnimeSummary> {
    let sql = format!("{ANIME_SELECT} WHERE a.id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_row(params![id], map_anime)
        .map_err(|_| AppError::NotFound(format!("anime {id}")))
}

/// Insert folder entry. Returns (id, created). Duplicate normalized paths are
/// rejected silently by returning the existing id.
pub fn add_anime_folder(conn: &Connection, name: &str, path: &str) -> AppResult<(i64, bool)> {
    match conn.try_insert(
        rusqlite::InsertStrategy::DoNothing,
        "INSERT INTO anime (display_name, folder_path, added_at) VALUES (?1, ?2, ?3)",
        params![name, path, now_secs()],
    ) {
        Ok(info) if info.changes() > 0 => Ok((info.last_insert_rowid(), true)),
        Ok(_) => {
            let id: i64 = conn.query_row(
                "SELECT id FROM anime WHERE folder_path = ?1 COLLATE NOCASE",
                params![path],
                |r| r.get(0),
            )?;
            Ok((id, false))
        }
        Err(e) => Err(AppError::Database(e)),
    }
}

pub fn rename_anime(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    let n = conn.execute(
        "UPDATE anime SET display_name = ?2 WHERE id = ?1",
        params![id, name],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("anime {id}")));
    }
    Ok(())
}

pub fn set_covers(conn: &Connection, id: i64, default_cover: Option<&str>, custom_cover: Option<&str>) -> AppResult<()> {
    conn.execute(
        "UPDATE anime SET default_cover_path = COALESCE(?2, default_cover_path),
                        custom_cover_path = ?3 WHERE id = ?1",
        params![id, default_cover, custom_cover],
    )?;
    Ok(())
}

pub fn set_custom_cover(conn: &Connection, id: i64, cover: Option<&str>) -> AppResult<()> {
    conn.execute(
        "UPDATE anime SET custom_cover_path = ?2 WHERE id = ?1",
        params![id, cover],
    )?;
    Ok(())
}

pub fn delete_anime_entry(conn: &Connection, id: i64) -> AppResult<()> {
    // Cascades remove video_file + watch_history rows. Files on disk untouched.
    conn.execute("DELETE FROM anime WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn update_folder_status(conn: &Connection, id: i64, status: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE anime SET status = ?2 WHERE id = ?1",
        params![id, status],
    )?;
    Ok(())
}

pub fn mark_scan_time(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE anime SET last_scanned_at = ?2 WHERE id = ?1",
        params![id, now_secs()],
    )?;
    Ok(())
}

pub fn all_folders(conn: &Connection) -> AppResult<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT id, folder_path FROM anime")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------- video files ----------

fn map_episode(r: &Row) -> rusqlite::Result<EpisodeInfo> {
    Ok(EpisodeInfo {
        id: r.get(0)?,
        anime_id: r.get(1)?,
        file_name: r.get(2)?,
        rel_path: r.get(3)?,
        path: r.get(4)?,
        duration_secs: r.get(5)?,
        width: r.get(6)?,
        height: r.get(7)?,
        size: r.get(8)?,
        position_secs: r.get(9)?,
        watched_at: r.get(10)?,
        completed: r.get::<_, i64>(11)? != 0,
    })
}

const EPISODE_SELECT: &str = "
SELECT v.id, v.anime_id, v.file_name, v.rel_path, v.path, v.duration_secs,
       v.width, v.height, v.size, h.position_secs, h.watched_at, COALESCE(h.completed,0)
FROM video_file v LEFT JOIN watch_history h ON h.video_id = v.id";

pub fn list_episodes(conn: &Connection, anime_id: i64) -> AppResult<Vec<EpisodeInfo>> {
    let sql = format!("{EPISODE_SELECT} WHERE v.anime_id = ?1 ORDER BY v.episode_sort IS NULL, v.episode_sort ASC, v.file_name COLLATE NOCASE ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![anime_id], map_episode)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_episode(conn: &Connection, video_id: i64) -> AppResult<EpisodeInfo> {
    let sql = format!("{EPISODE_SELECT} WHERE v.id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_row(params![video_id], map_episode)
        .map_err(|_| AppError::NotFound(format!("video {video_id}")))
}

/// Upsert one scanned file inside an open transaction. Returns rowid.
pub fn upsert_video(
    tx: &Connection,
    anime_id: i64,
    path: &str,
    rel_path: &str,
    file_name: &str,
    size: i64,
    modified: i64,
    duration: Option<f64>,
    width: Option<i64>,
    height: Option<i64>,
) -> AppResult<()> {
    let sort = crate::library::naming::episode_sort_key(file_name);
    tx.execute(
        "INSERT INTO video_file (anime_id, path, rel_path, file_name, size, modified, duration_secs, width, height, episode_sort, scanned_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(path) DO UPDATE SET
            anime_id=excluded.anime_id, rel_path=excluded.rel_path, file_name=excluded.file_name,
            size=excluded.size, modified=excluded.modified,
            duration_secs=COALESCE(excluded.duration_secs, video_file.duration_secs),
            width=COALESCE(excluded.width, video_file.width),
            height=COALESCE(excluded.height, video_file.height),
            episode_sort=excluded.episode_sort, scanned_at=excluded.scanned_at",
        params![anime_id, path, rel_path, file_name, size, modified, duration, width, height, sort, now_secs()],
    )?;
    Ok(())
}

/// Remove files of this anime that were not seen in the current scan pass.
pub fn prune_unseen(tx: &Connection, anime_id: i64, scanned_at_cutoff: i64) -> AppResult<usize> {
    let n = tx.execute(
        "DELETE FROM video_file WHERE anime_id = ?1 AND scanned_at < ?2",
        params![anime_id, scanned_at_cutoff],
    )?;
    Ok(n)
}

// ---------- watch history ----------

pub fn save_position(conn: &Connection, video_id: i64, position: f64, duration: Option<f64>) -> AppResult<()> {
    let completed = match duration {
        Some(d) if d > 0.0 => (position >= d - 30.0) as i64,
        _ => 0,
    };
    conn.execute(
        "INSERT INTO watch_history (video_id, position_secs, duration_secs, watched_at, completed)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(video_id) DO UPDATE SET
           position_secs=excluded.position_secs,
           duration_secs=COALESCE(excluded.duration_secs, watch_history.duration_secs),
           watched_at=excluded.watched_at,
           completed=excluded.completed",
        params![video_id, position, duration, now_secs(), completed],
    )?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub video_id: i64,
    pub anime_id: i64,
    pub anime_name: String,
    pub file_name: String,
    pub rel_path: String,
    pub path: String,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub watched_at: i64,
    pub completed: bool,
    pub cover_url: Option<String>,
}

fn map_hist(r: &Row) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        video_id: r.get(0)?,
        anime_id: r.get(1)?,
        anime_name: r.get(2)?,
        file_name: r.get(3)?,
        rel_path: r.get(4)?,
        path: r.get(5)?,
        position_secs: r.get(6)?,
        duration_secs: r.get(7)?,
        watched_at: r.get(8)?,
        completed: r.get::<_, i64>(9)? != 0,
        cover_url: r.get(10)?,
    })
}

pub fn continue_watching(conn: &Connection) -> AppResult<Vec<HistoryEntry>> {
    let sql = "
      SELECT v.id, v.anime_id, a.display_name, v.file_name, v.rel_path, v.path,
             h.position_secs, COALESCE(h.duration_secs, v.duration_secs), h.watched_at, h.completed,
             COALESCE(a.custom_cover_path, a.default_cover_path)
      FROM watch_history h JOIN video_file v ON v.id=h.video_id JOIN anime a ON a.id=v.anime_id
      WHERE h.completed = 0 AND h.position_secs > 5
      ORDER BY h.watched_at DESC LIMIT 50";
    let mut stmt = conn.prepare(sql)?;
    Ok(stmt.query_map([], map_hist)?.collect::<Result<Vec<_>, _>>()?)
}

pub fn recently_watched(conn: &Connection) -> AppResult<Vec<HistoryEntry>> {
    let sql = "
      SELECT v.id, v.anime_id, a.display_name, v.file_name, v.rel_path, v.path,
             h.position_secs, COALESCE(h.duration_secs, v.duration_secs), h.watched_at, h.completed,
             COALESCE(a.custom_cover_path, a.default_cover_path)
      FROM watch_history h JOIN video_file v ON v.id=h.video_id JOIN anime a ON a.id=v.anime_id
      ORDER BY h.watched_at DESC LIMIT 50";
    let mut stmt = conn.prepare(sql)?;
    Ok(stmt.query_map([], map_hist)?.collect::<Result<Vec<_>, _>>()?)
}

// ---------- settings ----------

pub fn get_setting(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    Ok(conn.query_row(
        "SELECT value FROM app_settings WHERE key = ?1",
        params![key],
        |r| r.get(0),
    ).optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

// ---------- scan cache ----------

pub struct CachedMeta {
    pub size: i64,
    pub modified: i64,
    pub duration: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

pub fn cache_get(conn: &Connection, path: &str) -> AppResult<Option<CachedMeta>> {
    Ok(conn
        .query_row(
            "SELECT size, modified, duration_secs, width, height FROM scan_cache WHERE path = ?1 COLLATE NOCASE",
            params![path],
            |r| {
                Ok(CachedMeta {
                    size: r.get(0)?,
                    modified: r.get(1)?,
                    duration: r.get(2)?,
                    width: r.get(3)?,
                    height: r.get(4)?,
                })
            },
        )
        .optional()?)
}

pub fn cache_put(
    conn: &Connection,
    path: &str,
    size: i64,
    modified: i64,
    duration: Option<f64>,
    width: Option<i64>,
    height: Option<i64>,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO scan_cache (path,size,modified,duration_secs,width,height) VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(path) DO UPDATE SET size=excluded.size, modified=excluded.modified,
           duration_secs=excluded.duration_secs, width=excluded.width, height=excluded.height",
        params![path, size, modified, duration, width, height],
    )?;
    Ok(())
}

pub fn cache_clear(conn: &Connection) -> AppResult<()> {
    conn.execute("DELETE FROM scan_cache", [])?;
    Ok(())
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Db;

    fn db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Db::open(&dir.path().join("t.db")).unwrap(), dir)
    }

    #[test]
    fn add_duplicate_and_list() {
        let (d, _t) = db();
        let c = d.lock().unwrap();
        let (id1, created1) = add_anime_folder(&c, "One Piece", "C:\\Anime\\One Piece").unwrap();
        assert!(created1);
        let (id2, created2) = add_anime_folder(&c, "One Piece", "C:\\Anime\\One Piece").unwrap();
        assert!(!created2);
        assert_eq!(id1, id2);
        let list = list_anime(&c, None, "name").unwrap();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn upsert_prune_and_history() {
        let (d, _t) = db();
        {
            let c = d.lock().unwrap();
            let (aid, _) = add_anime_folder(&c, "Test", "/anime/test").unwrap();
            upsert_video(&c, aid, "/anime/test/e1.mkv", "e1.mkv", "e1.mkv", 100, 1, Some(1400.0), Some(1920), Some(1080)).unwrap();
            save_position(&c, 1, 60.0, Some(1400.0)).unwrap();
            let eps = list_episodes(&c, aid).unwrap();
            assert_eq!(eps.len(), 1);
            assert_eq!(eps[0].position_secs, Some(60.0));
            let cont = continue_watching(&c).unwrap();
            assert_eq!(cont.len(), 1);
        }
        {
            let c = d.lock().unwrap();
            // simulate rescan where e1 disappeared
            let aid: i64 = c.query_row("SELECT id FROM anime", [], |r| r.get(0)).unwrap();
            let n = prune_unseen(&c, aid, now_secs() + 1).unwrap();
            assert_eq!(n, 1);
            assert!(continue_watching(&c).unwrap().is_empty());
        }
    }

    #[test]
    fn settings_roundtrip() {
        let (d, _t) = db();
        let c = d.lock().unwrap();
        assert_eq!(get_setting(&c, "theme").unwrap(), None);
        set_setting(&c, "theme", "dark").unwrap();
        assert_eq!(get_setting(&c, "theme").unwrap().as_deref(), Some("dark"));
    }
}

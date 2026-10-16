//! SQLite schema + migrations for Fets Anime.

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Mutex;

use super::error::{AppError, AppResult};

/// Thread-safe database handle. All access goes through the mutex; scans write
/// in small transactions so a cancelled scan never corrupts or wipes data.
pub struct Db(pub Mutex<Connection>);

const MIGRATIONS: &[&str] = &[
    // v1
    "
    PRAGMA foreign_keys = ON;

    CREATE TABLE anime (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        display_name TEXT NOT NULL,
        folder_path TEXT NOT NULL UNIQUE COLLATE NOCASE,
        added_at INTEGER NOT NULL,
        last_scanned_at INTEGER,
        custom_cover_path TEXT,
        default_cover_path TEXT,
        status TEXT NOT NULL DEFAULT 'ok'          -- ok | missing
    );

    CREATE TABLE video_file (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        anime_id INTEGER NOT NULL REFERENCES anime(id) ON DELETE CASCADE,
        path TEXT NOT NULL UNIQUE COLLATE NOCASE,
        rel_path TEXT NOT NULL,
        file_name TEXT NOT NULL,
        size INTEGER NOT NULL DEFAULT 0,
        modified INTEGER NOT NULL DEFAULT 0,
        duration_secs REAL,
        width INTEGER,
        height INTEGER,
        episode_sort REAL,
        scanned_at INTEGER NOT NULL
    );
    CREATE INDEX idx_video_anime ON video_file(anime_id);

    CREATE TABLE watch_history (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        video_id INTEGER NOT NULL UNIQUE REFERENCES video_file(id) ON DELETE CASCADE,
        position_secs REAL NOT NULL DEFAULT 0,
        duration_secs REAL,
        watched_at INTEGER NOT NULL,
        completed INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE app_settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE scan_cache (
        path TEXT PRIMARY KEY COLLATE NOCASE,
        size INTEGER NOT NULL,
        modified INTEGER NOT NULL,
        duration_secs REAL,
        width INTEGER,
        height INTEGER
    );
    ",
];

fn user_version(conn: &Connection) -> AppResult<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

pub fn migrate(conn: &Connection) -> AppResult<()> {
    let mut version = user_version(conn)?;
    if version == 0 {
        // Fresh DB that may predate migration bookkeeping: apply from scratch.
        version = 0;
    }
    while (version as usize) < MIGRATIONS.len() {
        let idx = version as usize;
        conn.execute_batch(MIGRATIONS[idx])?;
        conn.pragma_update(None, "user_version", idx + 1)?;
        version = idx as i32 + 1;
    }
    Ok(())
}

impl Db {
    pub fn open(db_path: &Path) -> AppResult<Db> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(AppError::Database)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        migrate(&conn)?;
        Ok(Db(Mutex::new(conn)))
    }

    pub fn lock(&self) -> AppResult<std::sync::MutexGuard<'_, Connection>> {
        self.0
            .lock()
            .map_err(|_| AppError::Other("database mutex poisoned".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_db_migrates_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lib.db");
        {
            let db = Db::open(&p).unwrap();
            let c = db.lock().unwrap();
            let n: i32 = c
                .query_row("SELECT count(*) FROM sqlite_master WHERE type='table'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(n >= 5);
        }
        // reopen applies no duplicate migrations
        let db = Db::open(&p).unwrap();
        let c = db.lock().unwrap();
        assert_eq!(user_version(&c).unwrap(), MIGRATIONS.len() as i32);
    }
}

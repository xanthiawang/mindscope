//! SQLite database for MindScope — based on OpenReLife + Retrace architecture
//!
//! Schema design:
//! - `frames` table: one row per screenshot (like OpenReLife's `entries`)
//!   `ocr_text` holds plain text (FTS-indexed); `ocr_regions` holds the bounding
//!   box JSON (NOT indexed). Legacy rows that packed both into `ocr_text` with a
//!   `---REGIONS---` separator are split on first start.
//! - `search_index` FTS5 table: full-text search on OCR text
//! - `segments` table: continuous app-focus periods (like Retrace's `segment`)
//!
//! All data is local, under `paths::data_dir()`.

use rusqlite::OptionalExtension;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use super::paths;
use super::timeutil;
use super::util::LockExt;

static DB: Mutex<Option<rusqlite::Connection>> = Mutex::new(None);

const REGIONS_SEP: &str = "\n---REGIONS---\n";

pub fn data_dir() -> PathBuf { paths::data_dir() }
pub fn frames_dir(date: &str) -> PathBuf { paths::frames_dir(date) }

/// Initialize database and create tables. Idempotent.
pub fn init_db() -> Result<(), String> {
    if DB.lock_or_recover().is_some() { return Ok(()); }

    let path = paths::db_path();
    let conn = rusqlite::Connection::open(&path).map_err(|e| format!("DB open: {}", e))?;

    conn.execute_batch("
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        PRAGMA cache_size = -16384;
        PRAGMA temp_store = MEMORY;
        PRAGMA busy_timeout = 5000;
    ").map_err(|e| format!("DB pragmas: {}", e))?;

    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS frames (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp INTEGER UNIQUE NOT NULL,
            app_name TEXT NOT NULL DEFAULT '',
            window_name TEXT NOT NULL DEFAULT '',
            ocr_text TEXT NOT NULL DEFAULT '',
            image_path TEXT NOT NULL DEFAULT '',
            is_idle INTEGER NOT NULL DEFAULT 0,
            segment_id INTEGER,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_frames_timestamp ON frames(timestamp);
        CREATE INDEX IF NOT EXISTS idx_frames_app ON frames(app_name);
        CREATE INDEX IF NOT EXISTS idx_frames_segment ON frames(segment_id);

        CREATE TABLE IF NOT EXISTS segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            app_name TEXT NOT NULL,
            window_name TEXT NOT NULL DEFAULT '',
            start_time INTEGER NOT NULL,
            end_time INTEGER NOT NULL,
            frame_count INTEGER NOT NULL DEFAULT 0,
            is_idle INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_segments_time ON segments(start_time, end_time);

        CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
            ocr_text,
            app_name,
            window_name,
            content='frames',
            content_rowid='id',
            tokenize='porter unicode61'
        );

        CREATE TRIGGER IF NOT EXISTS frames_ai AFTER INSERT ON frames BEGIN
            INSERT INTO search_index(rowid, ocr_text, app_name, window_name)
            VALUES (new.id, new.ocr_text, new.app_name, new.window_name);
        END;

        CREATE TRIGGER IF NOT EXISTS frames_ad AFTER DELETE ON frames BEGIN
            INSERT INTO search_index(search_index, rowid, ocr_text, app_name, window_name)
            VALUES ('delete', old.id, old.ocr_text, old.app_name, old.window_name);
        END;

        CREATE TRIGGER IF NOT EXISTS frames_au AFTER UPDATE ON frames BEGIN
            INSERT INTO search_index(search_index, rowid, ocr_text, app_name, window_name)
            VALUES ('delete', old.id, old.ocr_text, old.app_name, old.window_name);
            INSERT INTO search_index(rowid, ocr_text, app_name, window_name)
            VALUES (new.id, new.ocr_text, new.app_name, new.window_name);
        END;

        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
    ").map_err(|e| format!("DB schema: {}", e))?;

    // Migration: regions JSON out of the FTS-indexed column.
    let has_regions_col: bool = conn
        .prepare("PRAGMA table_info(frames)")
        .and_then(|mut s| {
            let names = s.query_map([], |r| r.get::<_, String>(1))?.filter_map(|r| r.ok()).collect::<Vec<_>>();
            Ok(names.iter().any(|n| n == "ocr_regions"))
        })
        .unwrap_or(false);
    if !has_regions_col {
        conn.execute_batch("ALTER TABLE frames ADD COLUMN ocr_regions TEXT NOT NULL DEFAULT ''")
            .map_err(|e| format!("DB migrate ocr_regions: {}", e))?;
    }
    let packed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM frames WHERE instr(ocr_text, char(10)||'---REGIONS---'||char(10)) > 0",
            [], |r| r.get(0),
        )
        .unwrap_or(0);

    *DB.lock_or_recover() = Some(conn);
    log::info!("MindScope: Database initialized at {:?}", path);

    if packed > 0 {
        // Existing installs can have hundreds of thousands of packed rows; migrate
        // in batches on a background thread so startup and queries are not blocked.
        log::info!("MindScope: migrating {} packed OCR rows into ocr_regions column (background)", packed);
        std::thread::Builder::new()
            .name("mindscope-db-migrate".into())
            .spawn(move || migrate_packed_regions(packed))
            .ok();
    }
    Ok(())
}

/// Split legacy "text\n---REGIONS---\njson" rows into ocr_text/ocr_regions, 2000 rows per
/// transaction, releasing the DB lock between batches.
fn migrate_packed_regions(total: i64) {
    let started = Instant::now();
    let mut done: i64 = 0;
    loop {
        let res = with_db(|conn| {
            conn.execute(
                "UPDATE frames SET
                    ocr_regions = substr(ocr_text, instr(ocr_text, char(10)||'---REGIONS---'||char(10)) + 15),
                    ocr_text    = substr(ocr_text, 1, instr(ocr_text, char(10)||'---REGIONS---'||char(10)) - 1)
                 WHERE id IN (SELECT id FROM frames
                              WHERE instr(ocr_text, char(10)||'---REGIONS---'||char(10)) > 0 LIMIT 2000)",
                [],
            )
        });
        match res {
            Ok(0) => break,
            Ok(n) => { done += n as i64; std::thread::sleep(std::time::Duration::from_millis(50)); }
            Err(e) => { log::warn!("MindScope: OCR regions migration failed after {} rows: {}", done, e); return; }
        }
    }
    log::info!("MindScope: OCR regions migration done ({} of {} rows) in {:.1}s", done, total, started.elapsed().as_secs_f32());
}

fn with_db<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
{
    let guard = DB.lock_or_recover();
    match guard.as_ref() {
        Some(conn) => f(conn).map_err(|e| format!("{}", e)),
        None => Err("Database not initialized".to_string()),
    }
}

/// Insert a captured frame (text and regions stored separately).
pub fn insert_frame(
    timestamp: i64,
    app_name: &str,
    window_name: &str,
    ocr_text: &str,
    ocr_regions_json: &str,
    image_path: &str,
) -> Result<(), String> {
    with_db(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO frames (timestamp, app_name, window_name, ocr_text, ocr_regions, image_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![timestamp, app_name, window_name, ocr_text, ocr_regions_json, image_path],
        )?;
        Ok(())
    })
}

/// Split a legacy packed `ocr_text` ("text\n---REGIONS---\njson") into (text, regions).
pub fn unpack_ocr_regions(ocr_text: &str) -> (String, Vec<super::ocr::OcrRegion>) {
    if let Some(idx) = ocr_text.find(REGIONS_SEP) {
        let text = ocr_text[..idx].to_string();
        let json = &ocr_text[idx + REGIONS_SEP.len()..];
        let regions: Vec<super::ocr::OcrRegion> = serde_json::from_str(json).unwrap_or_default();
        (text, regions)
    } else {
        (ocr_text.to_string(), vec![])
    }
}

/// A pending frame to be written in a batch transaction.
struct PendingFrame {
    timestamp: i64,
    app_name: String,
    window_name: String,
    ocr_text: String,
    ocr_regions: String,
    image_path: String,
}

/// Batched frame writer: buffers frames and flushes them in a single SQLite
/// transaction to reduce write lock contention and WAL overhead.
pub struct FrameBuffer {
    pending: Mutex<Vec<PendingFrame>>,
    last_flush: Mutex<Instant>,
    max_batch_size: usize,
    max_age_secs: u64,
}

impl FrameBuffer {
    pub fn new(max_batch_size: usize, max_age_secs: u64) -> Self {
        Self {
            pending: Mutex::new(Vec::with_capacity(max_batch_size)),
            last_flush: Mutex::new(Instant::now()),
            max_batch_size,
            max_age_secs,
        }
    }

    pub fn buffer_frame(
        &self,
        timestamp: i64,
        app_name: &str,
        window_name: &str,
        ocr: &super::ocr::OcrResult,
        image_path: &str,
    ) {
        let regions_json = if ocr.regions.is_empty() { String::new() } else {
            serde_json::to_string(&ocr.regions).unwrap_or_default()
        };
        self.pending.lock_or_recover().push(PendingFrame {
            timestamp,
            app_name: app_name.to_string(),
            window_name: window_name.to_string(),
            ocr_text: ocr.text.clone(),
            ocr_regions: regions_json,
            image_path: image_path.to_string(),
        });
    }

    /// Re-point a still-buffered frame at its `video://` reference. Returns false
    /// if the frame was already flushed (caller then updates the DB).
    pub fn update_path_by_ts(&self, timestamp: i64, new_path: &str) -> bool {
        let mut pending = self.pending.lock_or_recover();
        if let Some(f) = pending.iter_mut().find(|f| f.timestamp == timestamp) {
            f.image_path = new_path.to_string();
            true
        } else {
            false
        }
    }

    pub fn maybe_flush(&self) -> Result<(), String> {
        let should_flush = {
            let pending = self.pending.lock_or_recover();
            let last_flush = self.last_flush.lock_or_recover();
            pending.len() >= self.max_batch_size
                || (!pending.is_empty() && last_flush.elapsed().as_secs() >= self.max_age_secs)
        };
        if should_flush { self.flush_frames() } else { Ok(()) }
    }

    pub fn flush_frames(&self) -> Result<(), String> {
        let frames: Vec<PendingFrame> = std::mem::take(&mut *self.pending.lock_or_recover());
        if frames.is_empty() { return Ok(()); }

        let count = frames.len();
        let result = with_db(|conn| {
            let tx = conn.unchecked_transaction()?;
            for f in &frames {
                tx.execute(
                    "INSERT OR IGNORE INTO frames (timestamp, app_name, window_name, ocr_text, ocr_regions, image_path)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![f.timestamp, f.app_name, f.window_name, f.ocr_text, f.ocr_regions, f.image_path],
                )?;
            }
            tx.commit()?;
            Ok(())
        });

        match &result {
            Ok(_) => {
                *self.last_flush.lock_or_recover() = Instant::now();
                log::debug!("MindScope: flushed {} frames in batch transaction", count);
            }
            Err(e) => {
                let mut pending = self.pending.lock_or_recover();
                pending.extend(frames);
                log::error!("MindScope: batch flush failed ({}), {} frames re-buffered", e, count);
            }
        }
        result
    }
}

/// Re-point the frame with exactly this timestamp at a `video://` reference.
pub fn update_frame_path_by_timestamp(timestamp: i64, video_ref: &str) -> Result<bool, String> {
    with_db(|conn| {
        let n = conn.execute(
            "UPDATE frames SET image_path = ?1 WHERE timestamp = ?2",
            rusqlite::params![video_ref, timestamp],
        )?;
        Ok(n > 0)
    })
}

/// Legacy fallback (encoder without `ts`): update the newest non-video row.
pub fn update_latest_frame_path(video_ref: &str) -> Result<(), String> {
    with_db(|conn| {
        conn.execute(
            "UPDATE frames SET image_path = ?1 WHERE id = (SELECT MAX(id) FROM frames WHERE image_path NOT LIKE 'video://%')",
            rusqlite::params![video_ref],
        )?;
        Ok(())
    })
}

/// Full frame row (search results, by-id lookups, background summarisers).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrameRow {
    pub id: i64,
    pub timestamp: i64,
    pub app_name: String,
    pub window_name: String,
    /// Plain OCR text (never contains regions JSON).
    pub ocr_text: String,
    pub image_path: String,
    /// Bounding-box JSON; not sent to the UI (fetched via /frames/:id/text).
    #[serde(skip)]
    pub regions_json: String,
}

impl FrameRow {
    pub fn regions(&self) -> Vec<super::ocr::OcrRegion> {
        if self.regions_json.is_empty() { return vec![]; }
        serde_json::from_str(&self.regions_json).unwrap_or_default()
    }
}

/// Light row for the timeline: no OCR text, no image path (images are served by id).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrameLite {
    pub id: i64,
    pub timestamp: i64,
    pub app_name: String,
    pub window_name: String,
}

const FULL_COLS: &str = "id, timestamp, app_name, window_name, ocr_text, image_path, ocr_regions";

fn row_to_frame(row: &rusqlite::Row<'_>) -> rusqlite::Result<FrameRow> {
    let raw_text: String = row.get(4)?;
    let mut regions_json: String = row.get(6)?;
    let text = if raw_text.contains(REGIONS_SEP) {
        let (t, regs) = unpack_ocr_regions(&raw_text);
        if regions_json.is_empty() && !regs.is_empty() {
            regions_json = serde_json::to_string(&regs).unwrap_or_default();
        }
        t
    } else {
        raw_text
    };
    Ok(FrameRow {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        app_name: row.get(2)?,
        window_name: row.get(3)?,
        ocr_text: text,
        image_path: row.get(5)?,
        regions_json,
    })
}

fn row_to_lite(row: &rusqlite::Row<'_>) -> rusqlite::Result<FrameLite> {
    Ok(FrameLite {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        app_name: row.get(2)?,
        window_name: row.get(3)?,
    })
}

/// Full rows for a local calendar date (used by summarisers; the UI uses the lite variant).
pub fn get_frames_for_date(date: &str) -> Result<Vec<FrameRow>, String> {
    let Some((start, end)) = timeutil::local_day_bounds_micros(date) else { return Ok(vec![]); };
    get_frames_in_range(start, end, 0)
}

/// Full rows with timestamp in [start, end). `limit` 0 = unlimited.
pub fn get_frames_in_range(start: i64, end: i64, limit: i64) -> Result<Vec<FrameRow>, String> {
    with_db(|conn| {
        let sql = format!(
            "SELECT {FULL_COLS} FROM frames WHERE timestamp >= ?1 AND timestamp < ?2 ORDER BY timestamp ASC {}",
            if limit > 0 { format!("LIMIT {limit}") } else { String::new() }
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![start, end], row_to_frame)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

/// Light rows for a local date, optionally only those with id > `after_id`.
pub fn get_frames_lite_for_date(date: &str, after_id: Option<i64>) -> Result<Vec<FrameLite>, String> {
    let Some((start, end)) = timeutil::local_day_bounds_micros(date) else { return Ok(vec![]); };
    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, app_name, window_name FROM frames
             WHERE timestamp >= ?1 AND timestamp < ?2 AND id > ?3
             ORDER BY timestamp ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![start, end, after_id.unwrap_or(0)], row_to_lite)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

/// The newest `limit` full rows of a local date (ascending), for AI context.
pub fn get_recent_frames_for_date(date: &str, limit: i64) -> Result<Vec<FrameRow>, String> {
    let Some((start, end)) = timeutil::local_day_bounds_micros(date) else { return Ok(vec![]); };
    with_db(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT * FROM (SELECT {FULL_COLS} FROM frames WHERE timestamp >= ?1 AND timestamp < ?2
             ORDER BY timestamp DESC LIMIT ?3) ORDER BY timestamp ASC"
        ))?;
        let rows = stmt.query_map(rusqlite::params![start, end, limit], row_to_frame)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

/// Search frames — hybrid approach:
/// - English words: FTS5 with wildcard prefix matching
/// - Chinese/non-ASCII or very short queries: LIKE fallback
pub fn search_frames(query: &str, limit: i64) -> Result<Vec<FrameRow>, String> {
    if query.trim().is_empty() { return Ok(vec![]); }

    let has_non_ascii = query.chars().any(|c| !c.is_ascii());
    let is_short = query.trim().chars().count() <= 2;

    if has_non_ascii || is_short {
        return search_frames_like(query, limit);
    }

    let fts_query: String = query
        .split_whitespace()
        .map(|word| {
            let clean: String = word.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
            if clean.is_empty() { String::new() } else { format!("{}*", clean) }
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if fts_query.is_empty() { return search_frames_like(query, limit); }

    let fts_result = with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT f.id, f.timestamp, f.app_name, f.window_name, f.ocr_text, f.image_path, f.ocr_regions
             FROM search_index si
             JOIN frames f ON f.id = si.rowid
             WHERE search_index MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![fts_query, limit], row_to_frame)?;
        Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    });

    match fts_result {
        Ok(results) if !results.is_empty() => Ok(results),
        _ => search_frames_like(query, limit),
    }
}

fn search_frames_like(query: &str, limit: i64) -> Result<Vec<FrameRow>, String> {
    let pattern = format!("%{}%", query.trim().replace('%', "\\%").replace('_', "\\_"));
    with_db(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {FULL_COLS} FROM frames
             WHERE ocr_text LIKE ?1 ESCAPE '\\'
                OR app_name LIKE ?1 ESCAPE '\\'
                OR window_name LIKE ?1 ESCAPE '\\'
             ORDER BY timestamp DESC
             LIMIT ?2"
        ))?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit], row_to_frame)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

/// Get storage statistics (walks the data dir — call from a background thread).
pub fn get_storage_stats() -> Result<StorageStats, String> {
    let total_size = dir_size_bytes(&paths::data_dir());
    let total_mb = total_size as f64 / (1024.0 * 1024.0);

    let (frame_count, oldest, newest) = with_db(|conn| {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0))?;
        let oldest: Option<i64> = conn.query_row("SELECT MIN(timestamp) FROM frames", [], |r| r.get(0)).ok();
        let newest: Option<i64> = conn.query_row("SELECT MAX(timestamp) FROM frames", [], |r| r.get(0)).ok();
        Ok((count, oldest, newest))
    })?;

    let display = if total_mb >= 1024.0 {
        format!("{:.1} GB", total_mb / 1024.0)
    } else {
        format!("{:.0} MB", total_mb)
    };

    Ok(StorageStats {
        total_size_mb: total_mb,
        total_size_display: display,
        frame_count,
        oldest_timestamp: oldest.unwrap_or(0),
        newest_timestamp: newest.unwrap_or(0),
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StorageStats {
    pub total_size_mb: f64,
    pub total_size_display: String,
    pub frame_count: i64,
    pub oldest_timestamp: i64,
    pub newest_timestamp: i64,
}

/// Number of frames and the highest frame id — cheap "did anything happen" signal.
pub fn frame_watermark() -> (i64, i64) {
    with_db(|conn| {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0))?;
        let max_id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM frames", [], |r| r.get(0))?;
        Ok((count, max_id))
    }).unwrap_or((0, 0))
}

/// Oldest frame timestamp (micros), if any.
pub fn oldest_frame_timestamp() -> Option<i64> {
    with_db(|conn| conn.query_row("SELECT MIN(timestamp) FROM frames", [], |r| r.get::<_, Option<i64>>(0)))
        .ok()
        .flatten()
}

/// Delete frame rows (and their standalone JPEGs) older than `cutoff_micros`.
/// Returns (rows deleted, files deleted). Video segments are handled by the caller
/// (one segment file is shared by many rows).
pub fn delete_frames_before(cutoff_micros: i64) -> Result<(u32, u32), String> {
    let paths: Vec<String> = with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT image_path FROM frames WHERE timestamp < ?1 AND image_path NOT LIKE 'video://%'",
        )?;
        let rows = stmt.query_map(rusqlite::params![cutoff_micros], |row| row.get(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })?;

    let frames_root = paths::frames_root();
    let mut files = 0u32;
    for p in &paths {
        // Only ever delete inside our own frames directory.
        if std::path::Path::new(p).starts_with(&frames_root) && fs::remove_file(p).is_ok() {
            files += 1;
        }
    }

    let rows = with_db(|conn| {
        let n = conn.execute("DELETE FROM frames WHERE timestamp < ?1", rusqlite::params![cutoff_micros])?;
        Ok(n as u32)
    })?;
    Ok((rows, files))
}

/// Delete frames older than N days (legacy entry point; retention loop does more).
pub fn cleanup_old_frames(retention_days: u32) -> Result<u32, String> {
    if retention_days == 0 { return Ok(0); }
    let cutoff = timeutil::now_micros() - (retention_days as i64 * 86_400 * 1_000_000);
    let (rows, _) = delete_frames_before(cutoff)?;
    remove_empty_frame_dirs();
    log::info!("MindScope: Cleaned up {} old frames", rows);
    Ok(rows)
}

pub fn remove_empty_frame_dirs() {
    if let Ok(entries) = fs::read_dir(paths::frames_root()) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Ok(mut rd) = fs::read_dir(entry.path()) {
                    if rd.next().is_none() {
                        let _ = fs::remove_dir(entry.path());
                    }
                }
            }
        }
    }
}

/// Read frame image as base64 (only from inside our frames directory).
pub fn read_image_base64(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    if !p.starts_with(paths::frames_root()) { return None; }
    let bytes = fs::read(p).ok()?;
    Some(base64_encode(&bytes))
}

pub fn dir_size_bytes(path: &std::path::Path) -> u64 {
    let mut size = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(m) = entry.metadata() {
                if m.is_dir() { size += dir_size_bytes(&entry.path()); }
                else { size += m.len(); }
            }
        }
    }
    size
}

/// Get all unique app names from the database
pub fn get_all_apps() -> Result<Vec<String>, String> {
    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT app_name FROM frames WHERE app_name != '' ORDER BY app_name"
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

/// Get a single frame by ID
pub fn get_frame_by_id(frame_id: i64) -> Result<Option<FrameRow>, String> {
    with_db(|conn| {
        let mut stmt = conn.prepare(&format!("SELECT {FULL_COLS} FROM frames WHERE id = ?1"))?;
        let row = stmt.query_row(rusqlite::params![frame_id], row_to_frame).optional()?;
        Ok(row)
    })
}

/// Get a single frame by its stored image path (JPEG or video:// reference).
pub fn get_frame_by_path(image_path: &str) -> Result<Option<FrameRow>, String> {
    with_db(|conn| {
        let mut stmt = conn.prepare(&format!("SELECT {FULL_COLS} FROM frames WHERE image_path = ?1 LIMIT 1"))?;
        let row = stmt.query_row(rusqlite::params![image_path], row_to_frame).optional()?;
        Ok(row)
    })
}

pub fn base64_encode_bytes(bytes: &[u8]) -> String { base64_encode(bytes) }

fn base64_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 { result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char); } else { result.push('='); }
        if chunk.len() > 2 { result.push(CHARS[(triple & 0x3F) as usize] as char); } else { result.push('='); }
    }
    result
}

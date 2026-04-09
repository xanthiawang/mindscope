//! SQLite database for MindScope — based on OpenReLife + Retrace architecture
//!
//! Schema design:
//! - `frames` table: one row per screenshot (like OpenReLife's `entries`)
//! - `search_index` FTS5 table: full-text search on OCR text (like Retrace's `searchRanking`)
//! - `segments` table: continuous app-focus periods (like Retrace's `segment`)
//!
//! All data is local. Screenshots stored as WebP files on disk.

use rusqlite::OptionalExtension;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

static DB: Mutex<Option<rusqlite::Connection>> = Mutex::new(None);

fn db_path() -> PathBuf {
    let dir = data_dir();
    dir.join("mindscope.db")
}

pub fn data_dir() -> PathBuf {
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join(".mindscope").join("data");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn frames_dir(date: &str) -> PathBuf {
    let dir = data_dir().join("frames").join(date);
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Initialize database and create tables
pub fn init_db() -> Result<(), String> {
    let path = db_path();
    let conn = rusqlite::Connection::open(&path).map_err(|e| format!("DB open: {}", e))?;

    // Performance pragmas (same as Retrace)
    conn.execute_batch("
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        PRAGMA cache_size = -65536;
        PRAGMA temp_store = MEMORY;
    ").map_err(|e| format!("DB pragmas: {}", e))?;

    // Core tables
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

        -- App focus segments (like Retrace)
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

        -- FTS5 full-text search (like Retrace's searchRanking)
        CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
            ocr_text,
            app_name,
            window_name,
            content='frames',
            content_rowid='id',
            tokenize='porter unicode61'
        );

        -- Triggers to keep FTS in sync
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

        -- Settings
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
    ").map_err(|e| format!("DB schema: {}", e))?;

    let mut guard = DB.lock().unwrap();
    *guard = Some(conn);
    log::info!("MindScope: Database initialized at {:?}", path);
    Ok(())
}

fn with_db<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
{
    let guard = DB.lock().unwrap();
    match guard.as_ref() {
        Some(conn) => f(conn).map_err(|e| format!("{}", e)),
        None => Err("Database not initialized".to_string()),
    }
}

/// Insert a video frame (from HEVC segment)
pub fn insert_video_frame(
    timestamp: i64,
    app_name: &str,
    window_name: &str,
    ocr_text: &str,
    segment_path: &str,
    frame_index: u32,
) -> Result<(), String> {
    // Store segment:frame_index as image_path for video frames
    let video_ref = format!("video://{}#{}", segment_path, frame_index);
    insert_frame(timestamp, app_name, window_name, ocr_text, &video_ref)
}

/// Insert frame with OCR regions (stored as JSON in ocr_text field: "text\n---\nJSON_REGIONS")
pub fn insert_frame_with_regions(
    timestamp: i64,
    app_name: &str,
    window_name: &str,
    ocr: &super::ocr::OcrResult,
    image_path: &str,
) -> Result<(), String> {
    // Pack text + regions into ocr_text field (text first, then JSON regions after separator)
    let regions_json = serde_json::to_string(&ocr.regions).unwrap_or_default();
    let packed = format!("{}\n---REGIONS---\n{}", ocr.text, regions_json);
    insert_frame(timestamp, app_name, window_name, &packed, image_path)
}

/// Unpack OCR regions from the packed format
pub fn unpack_ocr_regions(ocr_text: &str) -> (String, Vec<super::ocr::OcrRegion>) {
    if let Some(idx) = ocr_text.find("\n---REGIONS---\n") {
        let text = ocr_text[..idx].to_string();
        let json = &ocr_text[idx + 15..];
        let regions: Vec<super::ocr::OcrRegion> = serde_json::from_str(json).unwrap_or_default();
        (text, regions)
    } else {
        (ocr_text.to_string(), vec![])
    }
}

/// Insert a captured frame
pub fn insert_frame(
    timestamp: i64,
    app_name: &str,
    window_name: &str,
    ocr_text: &str,
    image_path: &str,
) -> Result<(), String> {
    with_db(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO frames (timestamp, app_name, window_name, ocr_text, image_path)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![timestamp, app_name, window_name, ocr_text, image_path],
        )?;
        Ok(())
    })
}

/// A pending frame to be written in a batch transaction.
struct PendingFrame {
    timestamp: i64,
    app_name: String,
    window_name: String,
    ocr_packed: String,
    image_path: String,
}

/// Batched frame writer inspired by Screenpipe's write coalescing pattern.
/// Buffers frames and flushes them in a single SQLite transaction to reduce
/// write lock contention and WAL overhead.
pub struct FrameBuffer {
    pending: Mutex<Vec<PendingFrame>>,
    last_flush: Mutex<Instant>,
    max_batch_size: usize,
    max_age_secs: u64,
}

impl FrameBuffer {
    /// Create a new FrameBuffer.
    /// - `max_batch_size`: flush when this many frames are buffered (e.g. 5)
    /// - `max_age_secs`: flush when oldest buffered frame is this old (e.g. 10)
    pub fn new(max_batch_size: usize, max_age_secs: u64) -> Self {
        Self {
            pending: Mutex::new(Vec::with_capacity(max_batch_size)),
            last_flush: Mutex::new(Instant::now()),
            max_batch_size,
            max_age_secs,
        }
    }

    /// Add a frame to the buffer (does not write to DB yet).
    pub fn buffer_frame(
        &self,
        timestamp: i64,
        app_name: &str,
        window_name: &str,
        ocr: &super::ocr::OcrResult,
        image_path: &str,
    ) {
        let regions_json = serde_json::to_string(&ocr.regions).unwrap_or_default();
        let packed = format!("{}\n---REGIONS---\n{}", ocr.text, regions_json);

        let mut pending = self.pending.lock().unwrap();
        pending.push(PendingFrame {
            timestamp,
            app_name: app_name.to_string(),
            window_name: window_name.to_string(),
            ocr_packed: packed,
            image_path: image_path.to_string(),
        });
    }

    /// Check if flush thresholds are met and flush if so.
    /// Call this after every `buffer_frame()`.
    pub fn maybe_flush(&self) -> Result<(), String> {
        let should_flush = {
            let pending = self.pending.lock().unwrap();
            let last_flush = self.last_flush.lock().unwrap();
            pending.len() >= self.max_batch_size
                || (!pending.is_empty() && last_flush.elapsed().as_secs() >= self.max_age_secs)
        };
        if should_flush {
            self.flush_frames()
        } else {
            Ok(())
        }
    }

    /// Flush all buffered frames to the database in a single transaction.
    /// Uses BEGIN/COMMIT for atomicity and reduced WAL overhead.
    pub fn flush_frames(&self) -> Result<(), String> {
        let frames: Vec<PendingFrame> = {
            let mut pending = self.pending.lock().unwrap();
            std::mem::take(&mut *pending)
        };

        if frames.is_empty() {
            return Ok(());
        }

        let count = frames.len();
        let result = with_db(|conn| {
            conn.execute_batch("BEGIN TRANSACTION")?;
            for f in &frames {
                conn.execute(
                    "INSERT OR IGNORE INTO frames (timestamp, app_name, window_name, ocr_text, image_path)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![f.timestamp, f.app_name, f.window_name, f.ocr_packed, f.image_path],
                )?;
            }
            conn.execute_batch("COMMIT")?;
            Ok(())
        });

        match &result {
            Ok(_) => {
                let mut last_flush = self.last_flush.lock().unwrap();
                *last_flush = Instant::now();
                log::debug!("MindScope: flushed {} frames in batch transaction", count);
            }
            Err(e) => {
                // On failure, try to rollback and re-buffer the frames
                let _ = with_db(|conn| {
                    conn.execute_batch("ROLLBACK").ok();
                    Ok(())
                });
                // Put frames back so they can be retried
                let mut pending = self.pending.lock().unwrap();
                for f in frames {
                    pending.push(f);
                }
                log::error!("MindScope: batch flush failed ({}), {} frames re-buffered", e, count);
            }
        }

        result
    }
}

/// Update the most recent frame's image_path to a video:// reference
/// Called when encoder confirms a frame has been encoded
pub fn update_latest_frame_path(video_ref: &str) -> Result<(), String> {
    with_db(|conn| {
        conn.execute(
            "UPDATE frames SET image_path = ?1 WHERE id = (SELECT MAX(id) FROM frames WHERE image_path NOT LIKE 'video://%')",
            rusqlite::params![video_ref],
        )?;
        Ok(())
    })
}

/// Frame data for frontend
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FrameRow {
    pub id: i64,
    pub timestamp: i64,
    pub app_name: String,
    pub window_name: String,
    pub ocr_text: String,
    pub image_path: String,
}

/// Get frames for a date range
pub fn get_frames_for_date(date: &str) -> Result<Vec<FrameRow>, String> {
    // Convert date "2026-04-09" to microsecond range
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 { return Ok(vec![]); }
    let y: i64 = parts[0].parse().unwrap_or(0);
    let m: i64 = parts[1].parse().unwrap_or(0);
    let d: i64 = parts[2].parse().unwrap_or(0);

    // Get local timezone offset, then compute precise UTC boundaries for this local date
    // Use current local time vs UTC to determine offset
    let now_utc = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // Get local time components to compute offset
    // Simple approach: parse "date" command output for timezone offset
    let tz_offset_secs: i64 = std::process::Command::new("date")
        .args(["+%z"])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // Format: "+0800" or "-0500"
            if s.len() >= 5 {
                let sign: i64 = if s.starts_with('-') { -1 } else { 1 };
                let hours: i64 = s[1..3].parse().unwrap_or(0);
                let mins: i64 = s[3..5].parse().unwrap_or(0);
                Some(sign * (hours * 3600 + mins * 60))
            } else { None }
        })
        .unwrap_or(0);

    // Precise day boundaries
    let month_days: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let mut days: i64 = 0;
    for yr in 1970..y {
        days += if (yr % 4 == 0 && yr % 100 != 0) || yr % 400 == 0 { 366 } else { 365 };
    }
    for mo in 0..(m - 1) as usize {
        days += if mo == 1 && is_leap { 29 } else { month_days[mo] };
    }
    days += d - 1;
    // Convert to UTC micros, adjusting for local timezone
    let start = (days * 86400 - tz_offset_secs) * 1_000_000;
    let end = start + 86400 * 1_000_000;

    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, app_name, window_name, ocr_text, image_path
             FROM frames WHERE timestamp >= ?1 AND timestamp < ?2
             ORDER BY timestamp ASC"
        )?;
        let rows = stmt.query_map(rusqlite::params![start, end], |row| {
            Ok(FrameRow {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                app_name: row.get(2)?,
                window_name: row.get(3)?,
                ocr_text: row.get(4)?,
                image_path: row.get(5)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

/// Search frames — hybrid approach:
/// - English words: FTS5 with wildcard prefix matching
/// - Chinese/non-ASCII: LIKE fallback (FTS5 can't tokenize Chinese)
/// - Single characters: LIKE fallback
pub fn search_frames(query: &str, limit: i64) -> Result<Vec<FrameRow>, String> {
    if query.trim().is_empty() { return Ok(vec![]); }

    let has_non_ascii = query.chars().any(|c| !c.is_ascii());
    let is_short = query.trim().len() <= 2;

    // Use LIKE for Chinese, short queries, or single chars (FTS5 can't handle these well)
    if has_non_ascii || is_short {
        return search_frames_like(query, limit);
    }

    // Try FTS5 first for English words
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
            "SELECT f.id, f.timestamp, f.app_name, f.window_name, f.ocr_text, f.image_path
             FROM search_index si
             JOIN frames f ON f.id = si.rowid
             WHERE search_index MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(rusqlite::params![fts_query, limit], |row| {
            Ok(FrameRow {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                app_name: row.get(2)?,
                window_name: row.get(3)?,
                ocr_text: row.get(4)?,
                image_path: row.get(5)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    });

    // If FTS5 returns nothing, fall back to LIKE
    match fts_result {
        Ok(results) if !results.is_empty() => Ok(results),
        _ => search_frames_like(query, limit),
    }
}

/// LIKE-based search fallback — works for any language including Chinese
fn search_frames_like(query: &str, limit: i64) -> Result<Vec<FrameRow>, String> {
    let pattern = format!("%{}%", query.trim());

    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, app_name, window_name, ocr_text, image_path
             FROM frames
             WHERE ocr_text LIKE ?1
                OR app_name LIKE ?1
                OR window_name LIKE ?1
             ORDER BY timestamp DESC
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit], |row| {
            Ok(FrameRow {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                app_name: row.get(2)?,
                window_name: row.get(3)?,
                ocr_text: row.get(4)?,
                image_path: row.get(5)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

/// Get storage statistics
pub fn get_storage_stats() -> Result<StorageStats, String> {
    let total_size = dir_size_bytes(&data_dir());
    let total_mb = total_size as f64 / (1024.0 * 1024.0);

    let (frame_count, oldest, newest) = with_db(|conn| {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0))?;
        let oldest: Option<i64> = conn.query_row(
            "SELECT MIN(timestamp) FROM frames", [], |r| r.get(0)
        ).ok();
        let newest: Option<i64> = conn.query_row(
            "SELECT MAX(timestamp) FROM frames", [], |r| r.get(0)
        ).ok();
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

/// Delete frames older than N days
pub fn cleanup_old_frames(retention_days: u32) -> Result<u32, String> {
    if retention_days == 0 { return Ok(0); }

    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64 - (retention_days as i64 * 86400 * 1_000_000);

    // Get image paths to delete
    let paths: Vec<String> = with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT image_path FROM frames WHERE timestamp < ?1"
        )?;
        let rows = stmt.query_map(rusqlite::params![cutoff], |row| row.get(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })?;

    // Delete files
    let mut deleted = 0u32;
    for path in &paths {
        if fs::remove_file(path).is_ok() {
            deleted += 1;
        }
    }

    // Delete from DB (FTS triggers will auto-clean search_index)
    with_db(|conn| {
        conn.execute("DELETE FROM frames WHERE timestamp < ?1", rusqlite::params![cutoff])?;
        Ok(())
    })?;

    // Clean up empty date directories
    let frames_base = data_dir().join("frames");
    if let Ok(entries) = fs::read_dir(&frames_base) {
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

    log::info!("MindScope: Cleaned up {} old frames", deleted);
    Ok(deleted)
}

/// Read frame image as base64
pub fn read_image_base64(path: &str) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(base64_encode(&bytes))
}

fn dir_size_bytes(path: &std::path::Path) -> u64 {
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
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, app_name, window_name, ocr_text, image_path
             FROM frames WHERE id = ?1"
        )?;
        let row = stmt.query_row(rusqlite::params![frame_id], |row| {
            Ok(FrameRow {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                app_name: row.get(2)?,
                window_name: row.get(3)?,
                ocr_text: row.get(4)?,
                image_path: row.get(5)?,
            })
        }).optional()?;
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

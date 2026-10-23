//! Automatic retention: hourly, bounded by age AND a byte budget.
//!
//! Removes, oldest day first: frame JPEGs + DB rows, HEVC segments
//! (`video://` sources), audio transcripts; prunes the frame cache; rotates
//! debug logs. Only ever deletes inside `$MINDSCOPE_HOME/data`.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::db;
use super::paths;
use super::settings;
use super::timeutil;
use super::util;
use super::video;

static RUNNING: AtomicBool = AtomicBool::new(false);

const CACHE_MAX_FILES: usize = 500;
const CACHE_MAX_BYTES: u64 = 200 * 1024 * 1024;
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

pub fn start_retention_loop() {
    if RUNNING.swap(true, Ordering::SeqCst) { return; }
    thread::Builder::new()
        .name("mindscope-retention".into())
        .spawn(|| {
            thread::sleep(Duration::from_secs(90));
            loop {
                let s = settings::load_settings();
                let report = enforce(s.retention_days, s.storage_budget_mb);
                log::info!("MindScope retention: {}", report);
                thread::sleep(Duration::from_secs(3600));
            }
        })
        .ok();
}

/// Manual trigger from Settings → "Clean up old data". Returns frame rows removed.
pub fn run_now(retention_days: u32) -> u32 {
    let s = settings::load_settings();
    let r = enforce(retention_days, s.storage_budget_mb);
    r.rows_deleted
}

#[derive(Debug, Default)]
pub struct Report {
    pub rows_deleted: u32,
    pub files_deleted: u32,
    pub days_deleted: u32,
    pub cache_files_deleted: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} rows, {} files, {} day dirs, {} cache files removed; data {:.0} MB → {:.0} MB",
            self.rows_deleted, self.files_deleted, self.days_deleted, self.cache_files_deleted,
            self.bytes_before as f64 / 1_048_576.0, self.bytes_after as f64 / 1_048_576.0)
    }
}

/// Delete everything recorded on a local date (frames dir, segments dir, audio files, DB rows).
fn delete_day(date: &str, report: &mut Report) {
    let data = paths::data_dir();
    for dir in [data.join("frames").join(date), data.join("segments").join(date), data.join("audio").join(date)] {
        if dir.is_dir() && fs::remove_dir_all(&dir).is_ok() { report.days_deleted += 1; }
    }
    for f in [data.join("audio").join(format!("{date}.json")), data.join("audio").join(format!("{date}.jsonl"))] {
        if f.is_file() && fs::remove_file(&f).is_ok() { report.files_deleted += 1; }
    }
    if let Some((_, end)) = timeutil::local_day_bounds_micros(date) {
        if let Ok((rows, files)) = db::delete_frames_before(end) {
            report.rows_deleted += rows;
            report.files_deleted += files;
        }
    }
}

/// Dated subdirectories/files under data/{frames,segments,audio}, oldest first, excluding today.
fn recorded_dates() -> Vec<String> {
    let data = paths::data_dir();
    let today = timeutil::today();
    let mut dates = std::collections::BTreeSet::new();
    for sub in ["frames", "segments", "audio"] {
        if let Ok(rd) = fs::read_dir(data.join(sub)) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let date = name.split('.').next().unwrap_or("").to_string();
                if timeutil::parse_date(&date).is_some() && date < today {
                    dates.insert(date);
                }
            }
        }
    }
    dates.into_iter().collect()
}

pub fn enforce(retention_days: u32, budget_mb: u64) -> Report {
    let mut report = Report::default();
    let data = paths::data_dir();
    report.bytes_before = db::dir_size_bytes(&data);

    // 1. Age-based: everything older than retention_days.
    if retention_days > 0 {
        let cutoff_date = timeutil::days_ago(retention_days as i64);
        for date in recorded_dates() {
            if date < cutoff_date { delete_day(&date, &mut report); }
        }
        let cutoff_micros = timeutil::now_micros() - retention_days as i64 * 86_400 * 1_000_000;
        if let Ok((rows, files)) = db::delete_frames_before(cutoff_micros) {
            report.rows_deleted += rows;
            report.files_deleted += files;
        }
    }

    // 2. Byte budget: drop whole days, oldest first, until under budget (never today).
    if budget_mb > 0 {
        let budget = budget_mb * 1024 * 1024;
        let mut size = db::dir_size_bytes(&data);
        if size > budget {
            for date in recorded_dates() {
                if size <= budget { break; }
                delete_day(&date, &mut report);
                size = db::dir_size_bytes(&data);
            }
            if size > budget {
                log::warn!("MindScope retention: still {:.0} MB over the {} MB budget after removing old days (today's data is never deleted)",
                    (size - budget) as f64 / 1_048_576.0, budget_mb);
            }
        }
    }

    // 3. Frame cache + logs.
    let (cache_files, _) = video::evict_cache(CACHE_MAX_FILES, CACHE_MAX_BYTES);
    report.cache_files_deleted = cache_files;
    util::rotate_log(&paths::debug_log_path(), LOG_MAX_BYTES);
    util::rotate_log(&paths::meeting_log_path(), LOG_MAX_BYTES);
    db::remove_empty_frame_dirs();

    report.bytes_after = db::dir_size_bytes(&data);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end against a temp MINDSCOPE_HOME: old day dirs + DB rows go, today stays,
    /// and the byte budget removes the oldest day first.
    #[test]
    fn enforce_removes_old_days_and_respects_budget() {
        let tmp = std::env::temp_dir().join(format!("mindscope-retention-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        std::env::set_var("MINDSCOPE_HOME", &tmp);
        assert_eq!(paths::mindscope_home(), tmp);
        db::init_db().expect("db");

        let today = timeutil::today();
        let old = timeutil::days_ago(100);
        let mid = timeutil::days_ago(10);
        let big = vec![0u8; 3 * 1024 * 1024];
        for (date, n) in [(&old, 2usize), (&mid, 1usize), (&today, 1usize)] {
            let (start, _) = timeutil::local_day_bounds_micros(date).unwrap();
            for i in 0..n {
                let p = paths::frames_dir(date).join(format!("frame_{i}.jpg"));
                fs::write(&p, &big).unwrap();
                db::insert_frame(start + 3_600_000_000 * (i as i64 + 1), "App", "Win", "text", "", p.to_str().unwrap()).unwrap();
            }
            fs::create_dir_all(paths::segments_dir().join(date)).unwrap();
            fs::write(paths::segments_dir().join(date).join("seg_000000.mp4"), &big).unwrap();
            fs::write(paths::audio_root().join(format!("{date}.jsonl")), "{}\n").unwrap();
        }
        assert_eq!(db::frame_watermark().0, 4);

        // 1. Age: 90-day retention removes only the 100-day-old day.
        let r = enforce(90, 0);
        assert!(!paths::frames_root().join(&old).exists(), "old frames dir removed");
        assert!(!paths::segments_dir().join(&old).exists(), "old segments removed");
        assert!(!paths::audio_root().join(format!("{old}.jsonl")).exists(), "old audio removed");
        assert!(paths::frames_root().join(&mid).exists());
        assert!(paths::frames_root().join(&today).exists());
        assert_eq!(db::frame_watermark().0, 2, "rows for the old day deleted: {r}");

        // 2. Budget: 1 MB budget drops the 10-day-old day but never today.
        let r = enforce(0, 1);
        assert!(!paths::frames_root().join(&mid).exists(), "mid day removed by budget: {r}");
        assert!(paths::frames_root().join(&today).exists(), "today kept");
        assert_eq!(db::frame_watermark().0, 1);

        let _ = fs::remove_dir_all(&tmp);
    }
}

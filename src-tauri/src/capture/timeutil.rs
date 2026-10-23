//! The one local-timezone date/time helper shared by every module (the UI uses
//! local dates too). Replaces the hand-rolled UTC calendar math and the
//! `date +%z` subprocess that used to run per query.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32, // 1-12
    pub day: u32,   // 1-31
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    /// Seconds east of UTC in effect at that instant (DST-aware).
    pub offset_secs: i64,
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Break an epoch-seconds instant into local calendar fields.
pub fn local_time(secs: i64) -> LocalTime {
    let t: libc::time_t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm); }
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        min: tm.tm_min as u32,
        sec: tm.tm_sec as u32,
        offset_secs: tm.tm_gmtoff as i64,
    }
}

/// Local calendar fields → epoch seconds (DST resolved by the C library).
pub fn local_to_epoch(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> i64 {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month as i32 - 1;
    tm.tm_mday = day as i32;
    tm.tm_hour = hour as i32;
    tm.tm_min = min as i32;
    tm.tm_sec = sec as i32;
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) as i64 }
}

pub fn date_string(secs: i64) -> String {
    let t = local_time(secs);
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.day)
}

/// Local "YYYY-MM-DD" for today.
pub fn today() -> String { date_string(now_secs()) }

/// Local "YYYY-MM-DDTHH:MM:SS" for now (the timestamp format used in audio segments).
pub fn local_timestamp() -> String { timestamp_string(now_secs()) }

pub fn timestamp_string(secs: i64) -> String {
    let t = local_time(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", t.year, t.month, t.day, t.hour, t.min, t.sec)
}

pub fn local_hour() -> u32 { local_time(now_secs()).hour }

pub fn local_hm() -> (u32, u32) {
    let t = local_time(now_secs());
    (t.hour, t.min)
}

/// Parse and validate "YYYY-MM-DD". Rejects month 0/13, day 0/32, etc.
pub fn parse_date(date: &str) -> Option<(i32, u32, u32)> {
    let date = date.trim();
    let mut it = date.splitn(3, '-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
        _ => 30,
    }
}

/// [start, end) of a local calendar day in epoch microseconds.
pub fn local_day_bounds_micros(date: &str) -> Option<(i64, i64)> {
    let (y, m, d) = parse_date(date)?;
    let start = local_to_epoch(y, m, d, 0, 0, 0);
    // mktime normalises day+1 past month end, and resolves DST transitions.
    let end = local_to_epoch(y, m, d + 1, 0, 0, 0);
    Some((start * 1_000_000, end * 1_000_000))
}

/// "YYYY-MM-DDTHH:MM:SS" (local) → epoch micros.
pub fn parse_local_datetime_micros(ts: &str) -> Option<i64> {
    let ts = ts.trim();
    let (date, time) = ts.split_once('T')?;
    let (y, m, d) = parse_date(date)?;
    let mut it = time.splitn(3, ':');
    let h: u32 = it.next()?.parse().ok()?;
    let mi: u32 = it.next()?.parse().ok()?;
    let s: u32 = it.next()?.get(..2)?.parse().ok()?;
    if h > 23 || mi > 59 || s > 60 { return None; }
    Some(local_to_epoch(y, m, d, h, mi, s) * 1_000_000)
}

/// Previous calendar day of a "YYYY-MM-DD" string.
pub fn prev_day(date: &str) -> Option<String> {
    let (y, m, d) = parse_date(date)?;
    let secs = local_to_epoch(y, m, d, 12, 0, 0) - 86_400;
    Some(date_string(secs))
}

/// Local date `n` days ago.
pub fn days_ago(n: i64) -> String { date_string(now_secs() - n * 86_400) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_date_validates() {
        assert_eq!(parse_date("2026-04-09"), Some((2026, 4, 9)));
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("2026-00-10"), None);
        assert_eq!(parse_date("2026-02-30"), None);
        assert_eq!(parse_date("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_date("garbage"), None);
    }

    #[test]
    fn day_bounds_are_24h_and_contain_noon() {
        let (s, e) = local_day_bounds_micros("2026-04-09").unwrap();
        let noon = local_to_epoch(2026, 4, 9, 12, 0, 0) * 1_000_000;
        assert!(s <= noon && noon < e);
        assert_eq!(e - s, 86_400 * 1_000_000);
    }

    #[test]
    fn roundtrip_timestamp() {
        let micros = parse_local_datetime_micros("2026-04-09T22:50:15").unwrap();
        assert_eq!(timestamp_string(micros / 1_000_000), "2026-04-09T22:50:15");
        assert_eq!(date_string(micros / 1_000_000), "2026-04-09");
    }

    #[test]
    fn prev_day_crosses_month() {
        assert_eq!(prev_day("2026-03-01").unwrap(), "2026-02-28");
        assert_eq!(prev_day("2026-01-01").unwrap(), "2025-12-31");
    }
}

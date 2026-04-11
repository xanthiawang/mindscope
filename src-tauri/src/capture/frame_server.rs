//! Axum HTTP API server — ported from Screenpipe's pattern.
//! Serves frames via HTTP so browser can load them natively with caching.

use axum::{
    extract::{Path, Query},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tower_http::cors::{Any, CorsLayer};

use super::audio;
use super::db;
use super::recorder;
use super::screenshot;
use super::video;

/// GET /frames/:id — serve frame image (supports both JPEG files and video:// refs)
async fn get_frame(Path(frame_id): Path<i64>) -> Result<Response, StatusCode> {
    let frame = db::get_frame_by_id(frame_id)
        .map_err(|_| StatusCode::NOT_FOUND)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Handle video:// references — extract frame from HEVC segment
    if frame.image_path.starts_with("video://") {
        let rest = &frame.image_path[8..];
        if let Some((seg_path, frame_str)) = rest.rsplit_once('#') {
            let frame_idx: u32 = frame_str.parse().unwrap_or(0);
            let bytes = video::extract_frame(seg_path, frame_idx)
                .ok_or(StatusCode::NOT_FOUND)?;
            return Ok(image_response(bytes));
        }
        return Err(StatusCode::NOT_FOUND);
    }

    let bytes = tokio::fs::read(&frame.image_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    Ok(image_response(bytes))
}

#[derive(serde::Deserialize)]
struct PathQuery { path: Option<String> }

/// GET /frame?path=... — serve by file path
async fn get_frame_by_path(Query(q): Query<PathQuery>) -> Result<Response, StatusCode> {
    let path = q.path.ok_or(StatusCode::BAD_REQUEST)?;
    let bytes = tokio::fs::read(&path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(image_response(bytes))
}

/// GET /frames/:id/text — OCR regions
async fn get_frame_text(Path(frame_id): Path<i64>) -> Result<axum::Json<serde_json::Value>, StatusCode> {
    let frame = db::get_frame_by_id(frame_id)
        .map_err(|_| StatusCode::NOT_FOUND)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let (text, regions) = db::unpack_ocr_regions(&frame.ocr_text);
    Ok(axum::Json(serde_json::json!({ "frame_id": frame_id, "text": text, "regions": regions })))
}

#[derive(serde::Deserialize)]
struct SearchQ { q: Option<String>, limit: Option<u32> }

/// GET /search?q=...&limit=...
async fn search_handler(Query(p): Query<SearchQ>) -> axum::Json<serde_json::Value> {
    let query = p.q.unwrap_or_default();
    let results = db::search_frames(&query, p.limit.unwrap_or(30) as i64).unwrap_or_default();
    let items: Vec<_> = results.iter().map(|f| {
        let (text, regions) = db::unpack_ocr_regions(&f.ocr_text);
        serde_json::json!({ "id": f.id, "timestamp": f.timestamp, "app_name": f.app_name, "window_name": f.window_name, "text": text, "regions": regions })
    }).collect();
    axum::Json(serde_json::json!({ "results": items, "total": items.len() }))
}

/// GET /debug/meeting — dump all meeting-related internal state for live debugging.
async fn debug_meeting_handler() -> axum::Json<serde_json::Value> {
    let mut dump = recorder::dump_debug_state();
    // Also include ffmpeg process presence for correlation.
    let ffmpeg_running = std::process::Command::new("pgrep")
        .args(["-f", "ffmpeg.*avfoundation"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if let Some(obj) = dump.as_object_mut() {
        obj.insert("ffmpeg_avfoundation_running".to_string(), serde_json::json!(ffmpeg_running));
        obj.insert("timestamp_ms".to_string(), serde_json::json!(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        ));
    }
    axum::Json(dump)
}

/// GET /search_meetings?q=...&limit=...
/// Linear-scan search over audio transcripts (last 14 days).
/// Returns matching segments with a snippet around the first match for highlighting.
async fn search_meetings_handler(Query(p): Query<SearchQ>) -> axum::Json<serde_json::Value> {
    let query_raw = p.q.unwrap_or_default();
    let query = query_raw.trim().to_lowercase();
    let limit = p.limit.unwrap_or(30) as usize;

    if query.is_empty() {
        return axum::Json(serde_json::json!({ "results": [], "total": 0 }));
    }

    // Scan the last 14 days of audio indexes by walking back day-by-day
    // from `today()`. Uses the same date format as the audio pipeline.
    let mut hits: Vec<serde_json::Value> = Vec::new();
    let today_str = today();
    let mut date = today_str.clone();

    for _ in 0..14 {
        let segments = audio::load_audio_segments(&date);
        for seg in &segments {
            if seg.transcript.is_empty() { continue; }
            let hay = seg.transcript.to_lowercase();
            if !hay.contains(&query) { continue; }

            // Extract a ~140-char window around the first match for highlighting.
            let idx = hay.find(&query).unwrap_or(0);
            let chars: Vec<char> = seg.transcript.chars().collect();
            let match_char_idx = seg.transcript[..idx].chars().count();
            let start = match_char_idx.saturating_sub(50);
            let end = (match_char_idx + query.chars().count() + 90).min(chars.len());
            let snippet: String = chars[start..end].iter().collect();

            hits.push(serde_json::json!({
                "date": date,
                "timestamp": seg.timestamp,
                "session_id": seg.session_id,
                "session_type": seg.session_type,
                "duration_secs": seg.duration_secs,
                "transcript": seg.transcript,
                "snippet": if start > 0 { format!("…{}", snippet) } else { snippet },
                "match_offset": match_char_idx,
                "match_length": query.chars().count(),
            }));
            if hits.len() >= limit { break; }
        }
        if hits.len() >= limit { break; }

        // Walk back one day — parse YYYY-MM-DD and subtract 1 day.
        if let Some(prev) = prev_day(&date) {
            date = prev;
        } else { break; }
    }

    axum::Json(serde_json::json!({ "results": hits, "total": hits.len() }))
}

/// Walk back one calendar day given a YYYY-MM-DD string.
fn prev_day(date: &str) -> Option<String> {
    let (y, m, d) = {
        let parts: Vec<&str> = date.split('-').collect();
        if parts.len() != 3 { return None; }
        (parts[0].parse::<i32>().ok()?, parts[1].parse::<u32>().ok()?, parts[2].parse::<u32>().ok()?)
    };
    let (ny, nm, nd) = if d > 1 {
        (y, m, d - 1)
    } else if m > 1 {
        let prev_m = m - 1;
        let dim = days_in_month(y, prev_m);
        (y, prev_m, dim)
    } else {
        (y - 1, 12, 31)
    };
    Some(format!("{:04}-{:02}-{:02}", ny, nm, nd))
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
        _ => 30,
    }
}

#[derive(serde::Deserialize)]
struct TimelineQ { date: Option<String> }

/// GET /timeline?date=YYYY-MM-DD
async fn timeline_handler(Query(p): Query<TimelineQ>) -> axum::Json<Vec<db::FrameRow>> {
    let date = p.date.unwrap_or_else(today);
    axum::Json(db::get_frames_for_date(&date).unwrap_or_default())
}

#[derive(serde::Deserialize)]
struct VideoFrameQ { segment: Option<String>, index: Option<u32> }

/// GET /video/frame?segment=PATH&index=N — extract and serve a video frame with caching
async fn get_video_frame(Query(q): Query<VideoFrameQ>) -> Result<Response, StatusCode> {
    let segment = q.segment.ok_or(StatusCode::BAD_REQUEST)?;
    let index = q.index.unwrap_or(0);
    let bytes = video::extract_frame(&segment, index).ok_or(StatusCode::NOT_FOUND)?;
    Ok(image_response(bytes))
}

/// GET /app-icon/:app_name — serve cached app icon PNG
async fn get_app_icon(Path(app_name): Path<String>) -> Result<Response, StatusCode> {
    let icon_path = screenshot::get_app_icon_path(&app_name)
        .ok_or(StatusCode::NOT_FOUND)?;
    let bytes = tokio::fs::read(&icon_path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((
        [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=604800, immutable")],
        bytes,
    ).into_response())
}

/// GET /meeting/status — live meeting state + recent transcripts (current session only)
async fn meeting_status_handler() -> axum::Json<serde_json::Value> {
    let (active, app_name, start_time) = recorder::get_meeting_state();

    let mut recent_transcripts = Vec::new();
    if active && start_time > 0 {
        let date = today();
        let segments = audio::load_audio_segments(&date);

        // Parse segment timestamp to epoch micros and filter to current session
        let parse_ts = |ts: &str| -> i64 {
            // Format: "2026-04-09T22:50:15" — treat as local time
            if ts.len() < 19 { return 0; }
            let year: i64 = ts[0..4].parse().unwrap_or(0);
            let month: i64 = ts[5..7].parse().unwrap_or(0);
            let day: i64 = ts[8..10].parse().unwrap_or(0);
            let hour: i64 = ts[11..13].parse().unwrap_or(0);
            let min: i64 = ts[14..16].parse().unwrap_or(0);
            let sec: i64 = ts[17..19].parse().unwrap_or(0);
            let tz_offset: i64 = std::process::Command::new("date").args(["+%z"]).output().ok()
                .and_then(|o| {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if s.len() >= 5 {
                        let sign: i64 = if s.starts_with('-') { -1 } else { 1 };
                        let h: i64 = s[1..3].parse().unwrap_or(0);
                        let m: i64 = s[3..5].parse().unwrap_or(0);
                        Some(sign * (h * 3600 + m * 60))
                    } else { None }
                }).unwrap_or(0);
            let mut days: i64 = 0;
            for y in 1970..year {
                days += if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
            }
            let month_days: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
            let is_leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            for m in 0..(month - 1) as usize {
                days += if m == 1 && is_leap { 29 } else { month_days[m] };
            }
            days += day - 1;
            ((days * 86400 + hour * 3600 + min * 60 + sec) - tz_offset) * 1_000_000
        };

        // Only include segments from current session (ts >= start_time)
        let mut matched: Vec<_> = segments.into_iter()
            .filter(|seg| !seg.transcript.is_empty())
            .filter(|seg| parse_ts(&seg.timestamp) >= start_time - 5_000_000) // 5s margin
            .collect();
        if matched.len() > 30 {
            matched = matched.split_off(matched.len() - 30);
        }
        for seg in matched {
            let seg_ts = parse_ts(&seg.timestamp);
            let time_str = if seg.timestamp.len() >= 16 {
                seg.timestamp[11..16].to_string()
            } else {
                seg.timestamp.clone()
            };
            recent_transcripts.push(serde_json::json!({
                "time": time_str,
                "speaker": "Speaker",
                "text": seg.transcript,
                "ts": seg_ts,
            }));
        }
    }

    axum::Json(serde_json::json!({
        "active": active,
        "app_name": app_name,
        "start_ts": start_time,
        "start_time": start_time,
        "recent_transcripts": recent_transcripts,
    }))
}

fn image_response(bytes: Vec<u8>) -> Response {
    let ct = if bytes.starts_with(&[0xFF, 0xD8]) { "image/jpeg" }
             else if bytes.starts_with(b"RIFF") { "image/webp" }
             else { "image/png" };
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "public, max-age=604800, immutable")], bytes).into_response()
}

fn today() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let d = s / 86400; let t = s % 86400;
    let mut y = 1970u64; let mut r = d;
    loop { let dy = if (y%4==0&&y%100!=0)||y%400==0 {366} else {365}; if r<dy {break;} r-=dy; y+=1; }
    let l = (y%4==0&&y%100!=0)||y%400==0;
    let md = if l {[31,29,31,30,31,30,31,31,30,31,30,31]} else {[31,28,31,30,31,30,31,31,30,31,30,31]};
    let mut m = 0u64;
    for (i,&v) in md.iter().enumerate() { if r<v {m=i as u64+1; break;} r-=v; }
    if m==0 {m=12;} format!("{:04}-{:02}-{:02}",y,m,r+1)
}

pub async fn start(port: u16) -> Result<(), String> {
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);
    let app = Router::new()
        .route("/frames/:frame_id", get(get_frame))
        .route("/frames/:frame_id/text", get(get_frame_text))
        .route("/frame", get(get_frame_by_path))
        .route("/search", get(search_handler))
        .route("/search_meetings", get(search_meetings_handler))
        .route("/debug/meeting", get(debug_meeting_handler))
        .route("/timeline", get(timeline_handler))
        .route("/app-icon/:app_name", get(get_app_icon))
        .route("/video/frame", get(get_video_frame))
        .route("/meeting/status", get(meeting_status_handler))
        .layer(cors);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    log::info!("MindScope API on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("{}", e))?;
    axum::serve(listener, app).await.map_err(|e| format!("{}", e))
}

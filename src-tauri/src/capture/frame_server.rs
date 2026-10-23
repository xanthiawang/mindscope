//! Local HTTP API (127.0.0.1:9457) used by the webview for images and search.
//!
//! Locked down: every request needs the per-launch token (`Authorization:
//! Bearer <token>` or `?token=`), CORS only allows the Tauri webview origins,
//! and there is no "serve any file by path" route any more.

use axum::{
    extract::{Path, Query, Request},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::sync::OnceLock;
use tower_http::cors::CorsLayer;

use super::audio;
use super::db;
use super::recorder;
use super::screenshot;
use super::text;
use super::timeutil;
use super::video;

pub const PORT: u16 = 9457;

static TOKEN: OnceLock<String> = OnceLock::new();

/// Per-launch random token (64 hex chars).
pub fn api_token() -> &'static str {
    TOKEN.get_or_init(|| {
        let mut bytes = [0u8; 32];
        let ok = std::fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
            .is_ok();
        if !ok {
            let seed = timeutil::now_micros() as u128 ^ ((std::process::id() as u128) << 64);
            bytes[..16].copy_from_slice(&seed.to_le_bytes());
        }
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    })
}

fn token_from_query(query: Option<&str>) -> Option<&str> {
    query?.split('&').find_map(|kv| kv.strip_prefix("token="))
}

async fn require_token(req: Request, next: Next) -> Result<Response, StatusCode> {
    let expected = api_token();
    let header_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t == expected)
        .unwrap_or(false);
    let query_ok = token_from_query(req.uri().query()).map(|t| t == expected).unwrap_or(false);
    if header_ok || query_ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// GET /frames/:id — serve frame image (JPEG file inside data/frames, or a video:// frame)
async fn get_frame(Path(frame_id): Path<i64>) -> Result<Response, StatusCode> {
    let frame = tokio::task::spawn_blocking(move || db::get_frame_by_id(frame_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::NOT_FOUND)?
        .ok_or(StatusCode::NOT_FOUND)?;

    if let Some(rest) = frame.image_path.strip_prefix("video://") {
        let (seg_path, frame_str) = rest.rsplit_once('#').ok_or(StatusCode::NOT_FOUND)?;
        let frame_idx: u32 = frame_str.parse().unwrap_or(0);
        let seg = seg_path.to_string();
        let bytes = tokio::task::spawn_blocking(move || video::extract_frame(&seg, frame_idx))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;
        return Ok(image_response(bytes));
    }

    let p = std::path::PathBuf::from(&frame.image_path);
    if !p.starts_with(super::paths::frames_root()) { return Err(StatusCode::NOT_FOUND); }
    let bytes = tokio::fs::read(&p).await.map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(image_response(bytes))
}

/// GET /frames/:id/text — OCR text + regions
async fn get_frame_text(Path(frame_id): Path<i64>) -> Result<axum::Json<serde_json::Value>, StatusCode> {
    let frame = tokio::task::spawn_blocking(move || db::get_frame_by_id(frame_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::NOT_FOUND)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::Json(serde_json::json!({ "frame_id": frame_id, "text": frame.ocr_text, "regions": frame.regions() })))
}

#[derive(serde::Deserialize)]
struct SearchQ { q: Option<String>, limit: Option<u32> }

/// GET /search?q=...&limit=...
async fn search_handler(Query(p): Query<SearchQ>) -> axum::Json<serde_json::Value> {
    let query = p.q.unwrap_or_default();
    let limit = p.limit.unwrap_or(30).min(200) as i64;
    let results = tokio::task::spawn_blocking(move || db::search_frames(&query, limit).unwrap_or_default())
        .await
        .unwrap_or_default();
    let items: Vec<_> = results.iter().map(|f| {
        serde_json::json!({
            "id": f.id, "timestamp": f.timestamp, "app_name": f.app_name, "window_name": f.window_name,
            "text": f.ocr_text, "regions": f.regions(), "image_path": f.image_path,
        })
    }).collect();
    axum::Json(serde_json::json!({ "results": items, "total": items.len() }))
}

/// GET /debug/meeting — dump meeting-related internal state.
async fn debug_meeting_handler() -> axum::Json<serde_json::Value> {
    let mut dump = recorder::dump_debug_state();
    if let Some(obj) = dump.as_object_mut() {
        obj.insert("timestamp_ms".to_string(), serde_json::json!(timeutil::now_micros() / 1000));
        obj.insert("claude_calls_today".to_string(), serde_json::json!(super::claude_cli::calls_today()));
    }
    axum::Json(dump)
}

/// GET /search_meetings?q=...&limit=... — linear scan over the last 14 days of transcripts.
async fn search_meetings_handler(Query(p): Query<SearchQ>) -> axum::Json<serde_json::Value> {
    let query = p.q.unwrap_or_default().trim().to_lowercase();
    let limit = p.limit.unwrap_or(30).min(200) as usize;
    if query.is_empty() {
        return axum::Json(serde_json::json!({ "results": [], "total": 0 }));
    }

    let hits = tokio::task::spawn_blocking(move || {
        let mut hits: Vec<serde_json::Value> = Vec::new();
        let mut date = timeutil::today();
        let qlen = query.chars().count();
        'days: for _ in 0..14 {
            for seg in audio::load_audio_segments(&date) {
                if seg.transcript.is_empty() { continue; }
                let hay = seg.transcript.to_lowercase();
                let Some(idx) = hay.find(&query) else { continue; };
                let match_char_idx = hay[..idx].chars().count();
                let chars: Vec<char> = seg.transcript.chars().collect();
                let start = match_char_idx.saturating_sub(50).min(chars.len());
                let end = (match_char_idx + qlen + 90).min(chars.len());
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
                    "match_length": qlen,
                }));
                if hits.len() >= limit { break 'days; }
            }
            match timeutil::prev_day(&date) {
                Some(d) => date = d,
                None => break,
            }
        }
        hits
    }).await.unwrap_or_default();

    axum::Json(serde_json::json!({ "total": hits.len(), "results": hits }))
}

#[derive(serde::Deserialize)]
struct TimelineQ { date: Option<String>, recent: Option<u32> }

/// GET /timeline?date=YYYY-MM-DD — light rows (id, timestamp, app, window).
/// GET /timeline?date=...&recent=N — the newest N rows WITH OCR text (for AI context).
async fn timeline_handler(Query(p): Query<TimelineQ>) -> axum::Json<serde_json::Value> {
    let date = p.date.unwrap_or_else(timeutil::today);
    let recent = p.recent.unwrap_or(0).min(500);
    let value = tokio::task::spawn_blocking(move || {
        if recent > 0 {
            let rows = db::get_recent_frames_for_date(&date, recent as i64).unwrap_or_default();
            let items: Vec<_> = rows.iter().map(|f| serde_json::json!({
                "id": f.id, "timestamp": f.timestamp, "app_name": f.app_name,
                "window_name": f.window_name, "text": text::truncate_chars(&f.ocr_text, 600),
            })).collect();
            serde_json::json!(items)
        } else {
            serde_json::json!(db::get_frames_lite_for_date(&date, None).unwrap_or_default())
        }
    }).await.unwrap_or(serde_json::json!([]));
    axum::Json(value)
}

#[derive(serde::Deserialize)]
struct VideoFrameQ { segment: Option<String>, index: Option<u32> }

/// GET /video/frame?segment=PATH&index=N — only segments under data/segments are served.
async fn get_video_frame(Query(q): Query<VideoFrameQ>) -> Result<Response, StatusCode> {
    let segment = q.segment.ok_or(StatusCode::BAD_REQUEST)?;
    let index = q.index.unwrap_or(0);
    let bytes = tokio::task::spawn_blocking(move || video::extract_frame(&segment, index))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(image_response(bytes))
}

/// GET /app-icon/:app_name — cached app icon PNG
async fn get_app_icon(Path(app_name): Path<String>) -> Result<Response, StatusCode> {
    let icon_path = screenshot::get_app_icon_path(&app_name).ok_or(StatusCode::NOT_FOUND)?;
    let bytes = tokio::fs::read(&icon_path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((
        [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=604800, immutable")],
        bytes,
    ).into_response())
}

/// GET /meeting/status — live meeting state + recent transcripts (current session only)
async fn meeting_status_handler() -> axum::Json<serde_json::Value> {
    let (active, app_name, start_time) = recorder::get_meeting_state();

    let recent_transcripts = if active && start_time > 0 {
        tokio::task::spawn_blocking(move || {
            let segments = audio::load_audio_segments(&timeutil::today());
            let mut matched: Vec<(i64, audio::AudioSegment)> = segments.into_iter()
                .filter(|seg| !seg.transcript.is_empty())
                .filter_map(|seg| timeutil::parse_local_datetime_micros(&seg.timestamp).map(|ts| (ts, seg)))
                .filter(|(ts, _)| *ts >= start_time - 5_000_000) // 5 s margin
                .collect();
            if matched.len() > 30 {
                matched = matched.split_off(matched.len() - 30);
            }
            matched.into_iter().map(|(ts, seg)| {
                let time_str = seg.timestamp.get(11..16).unwrap_or(&seg.timestamp).to_string();
                serde_json::json!({ "time": time_str, "speaker": "Speaker", "text": seg.transcript, "ts": ts })
            }).collect::<Vec<_>>()
        }).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    axum::Json(serde_json::json!({
        "active": active,
        "app_name": app_name,
        "start_ts": start_time,
        "start_time": start_time,
        "recording": recorder::is_any_audio_running(),
        "recent_transcripts": recent_transcripts,
    }))
}

fn image_response(bytes: Vec<u8>) -> Response {
    let ct = if bytes.starts_with(&[0xFF, 0xD8]) { "image/jpeg" }
             else if bytes.starts_with(b"RIFF") { "image/webp" }
             else { "image/png" };
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "private, max-age=604800, immutable")], bytes).into_response()
}

pub async fn start(port: u16) -> Result<(), String> {
    let origins: Vec<HeaderValue> = [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "http://localhost:5173",
        "http://127.0.0.1:5173",
    ].iter().filter_map(|o| o.parse().ok()).collect();
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    let app = Router::new()
        .route("/frames/:frame_id", get(get_frame))
        .route("/frames/:frame_id/text", get(get_frame_text))
        .route("/search", get(search_handler))
        .route("/search_meetings", get(search_meetings_handler))
        .route("/debug/meeting", get(debug_meeting_handler))
        .route("/timeline", get(timeline_handler))
        .route("/app-icon/:app_name", get(get_app_icon))
        .route("/video/frame", get(get_video_frame))
        .route("/meeting/status", get(meeting_status_handler))
        .layer(middleware::from_fn(require_token))
        .layer(cors);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("bind {}: {}", addr, e))?;
    log::info!("MindScope API on http://{} (token-protected)", addr);
    axum::serve(listener, app).await.map_err(|e| format!("{}", e))
}

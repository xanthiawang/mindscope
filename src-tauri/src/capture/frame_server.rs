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

/// GET /meeting/status — live meeting state + recent transcripts
async fn meeting_status_handler() -> axum::Json<serde_json::Value> {
    let (active, app_name, start_time) = recorder::get_meeting_state();

    let mut recent_transcripts = Vec::new();
    if active && start_time > 0 {
        let date = today();
        let segments = audio::load_audio_segments(&date);
        // Filter segments and take last 10 with transcripts
        let mut matched: Vec<_> = segments.into_iter()
            .filter(|seg| !seg.transcript.is_empty())
            .collect();
        // Keep only last 10
        if matched.len() > 10 {
            matched = matched.split_off(matched.len() - 10);
        }
        for seg in matched {
            // timestamp is like "2026-01-15T17:35:02" — extract HH:MM
            let time_str = if seg.timestamp.len() >= 16 {
                seg.timestamp[11..16].to_string()
            } else {
                seg.timestamp.clone()
            };
            recent_transcripts.push(serde_json::json!({
                "time": time_str,
                "speaker": "Speaker",
                "text": seg.transcript,
            }));
        }
    }

    axum::Json(serde_json::json!({
        "active": active,
        "app_name": app_name,
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

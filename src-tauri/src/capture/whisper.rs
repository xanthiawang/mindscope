//! Whisper transcription engine using whisper-rs (whisper.cpp bindings).
//! The model context (~150 MB for ggml-base + Metal buffers) is loaded lazily on
//! the first transcription and released again with `unload()` when no audio
//! session is active, so idle RSS stays low.

use std::path::PathBuf;
use std::sync::Mutex;

use super::paths;
use super::util::LockExt;

struct Engine {
    ctx: whisper_rs::WhisperContext,
    /// Reused across batches — creating a state per 2 s batch churned tens of MB.
    state: whisper_rs::WhisperState,
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

fn default_model_path() -> PathBuf {
    paths::models_dir().join("ggml-base.bin")
}

/// Check if Whisper model is downloaded
pub fn is_model_available() -> bool {
    default_model_path().exists()
}

/// Is the model currently resident in memory?
pub fn is_loaded() -> bool {
    ENGINE.lock_or_recover().is_some()
}

/// Release the Whisper context (frees model weights + Metal buffers).
pub fn unload() {
    let mut guard = ENGINE.lock_or_recover();
    if guard.take().is_some() {
        log::info!("MindScope: Whisper context unloaded");
    }
}

/// Download Whisper model from Hugging Face using curl (avoids nested runtime issues)
pub fn download_model() -> Result<(), String> {
    let model_path = default_model_path();
    if model_path.exists() { return Ok(()); }

    let _ = std::fs::create_dir_all(paths::models_dir());
    log::info!("MindScope: Downloading Whisper model via curl...");
    let url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";
    let tmp = model_path.with_extension("bin.part");

    let status = std::process::Command::new("curl")
        .args(["-L", "--fail", "-o", tmp.to_str().unwrap_or(""), url])
        .status()
        .map_err(|e| format!("Download failed: {}", e))?;

    let size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if status.success() && size > 1_000_000 {
        std::fs::rename(&tmp, &model_path).map_err(|e| format!("rename failed: {}", e))?;
        log::info!("MindScope: Whisper model downloaded ({:.1} MB)", size as f64 / 1_048_576.0);
        Ok(())
    } else {
        let _ = std::fs::remove_file(&tmp);
        Err("Download failed or incomplete".into())
    }
}

fn ensure_loaded(guard: &mut Option<Engine>) -> Result<(), String> {
    if guard.is_some() { return Ok(()); }
    let model_path = default_model_path();
    if !model_path.exists() {
        return Err("Whisper model not downloaded".into());
    }
    let ctx = whisper_rs::WhisperContext::new_with_params(
        model_path.to_str().unwrap_or(""),
        whisper_rs::WhisperContextParameters::default(),
    ).map_err(|e| format!("Whisper init failed: {:?}", e))?;
    let state = ctx.create_state().map_err(|e| format!("State error: {:?}", e))?;
    *guard = Some(Engine { ctx, state });
    log::info!("MindScope: Whisper context initialized");
    Ok(())
}

fn params() -> whisper_rs::FullParams<'static, 'static> {
    let mut params = whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(4);
    params.set_language(None);  // auto-detect (Chinese, English, ...)
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    // Anti-hallucination parameters (stricter to avoid repetition loops)
    params.set_entropy_thold(2.2);
    params.set_logprob_thold(-1.5);
    params.set_no_speech_thold(0.5);
    params.set_max_tokens(128);
    params
}

/// Transcribe raw PCM samples directly from memory (no file I/O).
/// Input: f32 samples normalized to [-1, 1] at 16kHz mono.
pub fn transcribe_samples(samples: &[f32]) -> Result<String, String> {
    if samples.is_empty() { return Ok(String::new()); }

    // Skip silence (RMS energy gate)
    let rms = (samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / samples.len() as f64).sqrt();
    if rms < 0.015 { return Ok(String::new()); }

    let mut guard = ENGINE.lock_or_recover();
    ensure_loaded(&mut guard)?;
    let engine = guard.as_mut().ok_or("Whisper not initialized")?;
    let _ = &engine.ctx;

    engine.state.full(params(), samples).map_err(|e| format!("Transcribe error: {:?}", e))?;

    let num_segments = engine.state.full_n_segments();
    let mut text = String::new();
    for i in 0..num_segments {
        if let Some(segment) = engine.state.get_segment(i) {
            if let Ok(segment_text) = segment.to_str_lossy() {
                text.push_str(&segment_text);
                text.push(' ');
            }
        }
    }

    Ok(deduplicate_text(text.trim()))
}

/// Remove hallucinated repetitions from Whisper output.
/// Supports both English (split by .) and Chinese (split by 。，)
fn deduplicate_text(text: &str) -> String {
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if matches!(c, '.' | '。' | '！' | '？') || (c == ' ' && current.trim().chars().count() > 15) {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() && trimmed != "." && trimmed != "。" {
                sentences.push(trimmed);
            }
            current.clear();
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() { sentences.push(trimmed); }

    if sentences.len() < 3 { return text.to_string(); }

    let mut result: Vec<String> = Vec::new();
    let mut prev = String::new();
    let mut repeat_count = 0;

    for s in &sentences {
        if *s == prev {
            repeat_count += 1;
            if repeat_count >= 2 { continue; }
        } else {
            repeat_count = 0;
        }
        prev = s.clone();
        result.push(s.clone());
    }

    if result.is_empty() { return String::new(); }

    // If over 60% were repeats, it's hallucination
    if result.len() * 3 < sentences.len() {
        return String::new();
    }

    result.join(" ")
}

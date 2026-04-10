//! Whisper transcription engine using whisper-rs (whisper.cpp bindings)
//! Parameters tuned from Screenpipe's production config for best accuracy

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static WHISPER_CTX: Mutex<Option<whisper_rs::WhisperContext>> = Mutex::new(None);

fn models_dir() -> PathBuf {
    let dir = dirs_next::home_dir().unwrap_or_default()
        .join(".mindscope").join("models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn default_model_path() -> PathBuf {
    models_dir().join("ggml-base.bin")
}

/// Check if Whisper model is downloaded
pub fn is_model_available() -> bool {
    default_model_path().exists()
}

/// Download Whisper model from Hugging Face using curl (avoids nested runtime issues)
pub fn download_model() -> Result<(), String> {
    let model_path = default_model_path();
    if model_path.exists() { return Ok(()); }

    let _ = std::fs::create_dir_all(models_dir());
    log::info!("MindScope: Downloading Whisper model via curl...");
    let url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";

    let status = std::process::Command::new("curl")
        .args(["-L", "-o", model_path.to_str().unwrap_or(""), url])
        .status()
        .map_err(|e| format!("Download failed: {}", e))?;

    if status.success() && model_path.exists() {
        let size = std::fs::metadata(&model_path).map(|m| m.len()).unwrap_or(0);
        log::info!("MindScope: Whisper model downloaded ({:.1} MB)", size as f64 / 1_048_576.0);
        if size < 1_000_000 {
            let _ = std::fs::remove_file(&model_path);
            return Err("Download incomplete — file too small".into());
        }
        Ok(())
    } else {
        let _ = std::fs::remove_file(&model_path);
        Err("Download failed".into())
    }
}

/// Initialize Whisper context (lazy, first call only)
fn ensure_context() -> Result<(), String> {
    let mut guard = WHISPER_CTX.lock().unwrap();
    if guard.is_some() { return Ok(()); }

    let model_path = default_model_path();
    if !model_path.exists() {
        return Err("Whisper model not downloaded".into());
    }

    let ctx = whisper_rs::WhisperContext::new_with_params(
        model_path.to_str().unwrap(),
        whisper_rs::WhisperContextParameters::default(),
    ).map_err(|e| format!("Whisper init failed: {:?}", e))?;

    *guard = Some(ctx);
    log::info!("MindScope: Whisper context initialized");
    Ok(())
}

/// Transcribe audio file using Whisper
/// Input: path to m4a/wav file
/// Output: transcribed text
pub fn transcribe(audio_path: &Path) -> Result<String, String> {
    // Convert to WAV 16kHz mono if needed
    let wav_path = convert_to_wav(audio_path)?;
    let samples = load_wav_samples(&wav_path)?;

    // Skip silence (RMS energy gate — from Screenpipe)
    let rms = (samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / samples.len() as f64).sqrt();
    if rms < 0.015 {
        log::info!("MindScope: Skipping silent audio (RMS={:.4})", rms);
        return Ok(String::new());
    }

    ensure_context()?;
    let guard = WHISPER_CTX.lock().unwrap();
    let ctx = guard.as_ref().ok_or("Whisper not initialized")?;

    let mut state = ctx.create_state().map_err(|e| format!("State error: {:?}", e))?;

    // Whisper parameters — tuned for multilingual (Chinese + English)
    let mut params = whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(2);
    params.set_language(None); // Auto-detect language (Chinese, English, etc.)
    params.set_translate(false); // Keep original language, don't translate to English
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
    params.set_max_tokens(128); // Limit tokens per segment to prevent runaway repetition

    state.full(params, &samples).map_err(|e| format!("Transcribe error: {:?}", e))?;

    let num_segments = state.full_n_segments();
    let mut text = String::new();
    for i in 0..num_segments {
        if let Some(segment) = state.get_segment(i) {
            if let Ok(segment_text) = segment.to_str_lossy() {
                text.push_str(&segment_text);
                text.push(' ');
            }
        }
    }

    // Clean up temp wav
    if wav_path != audio_path {
        let _ = std::fs::remove_file(&wav_path);
    }

    // Post-process: remove hallucinated repetitions
    let text = deduplicate_text(text.trim());
    Ok(text)
}

/// Convert m4a to WAV 16kHz mono using afconvert (macOS)
fn convert_to_wav(audio_path: &Path) -> Result<PathBuf, String> {
    let ext = audio_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext == "wav" {
        return Ok(audio_path.to_path_buf());
    }

    let wav_path = audio_path.with_extension("wav");
    let status = std::process::Command::new("afconvert")
        .args([
            "-d", "LEI16",           // 16-bit little-endian integer
            "-c", "1",               // mono
            "-r", "16000",           // 16kHz
            audio_path.to_str().unwrap_or(""),
            wav_path.to_str().unwrap_or(""),
        ])
        .status()
        .map_err(|e| format!("afconvert failed: {}", e))?;

    if status.success() {
        Ok(wav_path)
    } else {
        Err("afconvert failed".into())
    }
}

/// Load WAV file as f32 samples normalized to [-1, 1]
fn load_wav_samples(wav_path: &Path) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::open(wav_path)
        .map_err(|e| format!("WAV open failed: {}", e))?;
    let spec = reader.spec();

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max_val = (1i32 << (spec.bits_per_sample - 1)) as f32;
            reader.into_samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / max_val)
                .collect()
        }
        hound::SampleFormat::Float => {
            reader.into_samples::<f32>()
                .filter_map(|s| s.ok())
                .collect()
        }
    };

    // If stereo, average to mono
    if spec.channels == 2 {
        Ok(samples.chunks(2).map(|c| (c[0] + c.get(1).copied().unwrap_or(0.0)) / 2.0).collect())
    } else {
        Ok(samples)
    }
}

/// Remove hallucinated repetitions from Whisper output.
/// Supports both English (split by .) and Chinese (split by 。，)
fn deduplicate_text(text: &str) -> String {
    // Split by sentence boundaries (English + Chinese punctuation)
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if matches!(c, '.' | '。' | '！' | '？') || (c == ' ' && current.trim().len() > 15) {
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

    // Detect repeated phrases
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

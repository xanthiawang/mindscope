//! Speaker identification using audio feature comparison.
//! Simplified version without ONNX — uses audio spectral features + cosine similarity.
//! When a proper ONNX model is available, this can be upgraded.

use std::sync::Mutex;

const SIMILARITY_THRESHOLD: f32 = 0.85;

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let d = na.sqrt() * nb.sqrt();
    if d < 1e-9 { 0.0 } else { dot / d }
}

/// Extract a simple spectral fingerprint from audio samples (16kHz mono f32).
/// Uses energy in frequency bands as a poor-man's speaker embedding.
/// Not as accurate as ONNX but works without external models.
fn extract_features(samples: &[f32]) -> Vec<f32> {
    let n = samples.len();
    if n < 1600 { return vec![]; }

    // Split into 20 windows, compute energy + zero-crossing rate per window
    let window_size = n / 20;
    let mut features = Vec::with_capacity(40);

    for w in 0..20 {
        let start = w * window_size;
        let end = (start + window_size).min(n);
        let window = &samples[start..end];

        // RMS energy
        let energy: f32 = (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt();
        features.push(energy);

        // Zero-crossing rate
        let zcr = window.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count() as f32 / window.len() as f32;
        features.push(zcr);
    }

    features
}

pub struct SpeakerManager {
    speakers: Vec<(String, Vec<f32>)>,
    next_id: usize,
}

impl SpeakerManager {
    pub fn new() -> Self {
        Self { speakers: Vec::new(), next_id: 1 }
    }

    pub fn is_available(&self) -> bool { true }

    pub fn identify_speaker(&mut self, audio_samples: &[f32]) -> String {
        let features = extract_features(audio_samples);
        if features.is_empty() { return "Unknown Speaker".to_string(); }

        let mut best: Option<(usize, f32)> = None;
        for (idx, (_, emb)) in self.speakers.iter().enumerate() {
            let sim = cosine_similarity(&features, emb);
            if sim > SIMILARITY_THRESHOLD {
                match best {
                    Some((_, s)) if sim > s => best = Some((idx, sim)),
                    None => best = Some((idx, sim)),
                    _ => {}
                }
            }
        }

        if let Some((idx, _)) = best {
            return self.speakers[idx].0.clone();
        }

        let label = format!("Speaker {}", self.next_id);
        self.next_id += 1;
        self.speakers.push((label.clone(), features));
        label
    }

    pub fn clear(&mut self) {
        self.speakers.clear();
        self.next_id = 1;
    }

    pub fn speaker_count(&self) -> usize { self.speakers.len() }
}

/// Thread-safe wrapper
pub struct SharedSpeakerManager {
    inner: Mutex<SpeakerManager>,
}

impl SharedSpeakerManager {
    pub fn new() -> Self {
        Self { inner: Mutex::new(SpeakerManager::new()) }
    }

    pub fn identify_speaker(&self, audio_samples: &[f32]) -> String {
        self.inner.lock().map(|mut m| m.identify_speaker(audio_samples)).unwrap_or_else(|_| "Unknown Speaker".to_string())
    }

    pub fn is_available(&self) -> bool {
        self.inner.lock().map(|m| m.is_available()).unwrap_or(false)
    }

    pub fn clear(&self) {
        if let Ok(mut m) = self.inner.lock() { m.clear(); }
    }
}

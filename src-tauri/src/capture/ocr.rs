use std::path::Path;
use std::process::Command;

/// OCR result with text and bounding boxes
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct OcrResult {
    pub text: String,
    pub regions: Vec<OcrRegion>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OcrRegion {
    pub text: String,
    pub x: f64,    // 0-1 normalized, top-left origin
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Run OCR and return text + bounding boxes
pub fn extract_with_regions(image_path: &Path) -> OcrResult {
    let helper = dirs_next::home_dir()
        .unwrap_or_default()
        .join(".mindscope")
        .join("bin")
        .join("ocr_helper");

    if !helper.exists() {
        return OcrResult { text: extract_text(image_path), regions: vec![] };
    }

    let output = Command::new(helper.to_str().unwrap_or(""))
        .arg(image_path.to_str().unwrap_or(""))
        .output()
        .ok();

    match output {
        Some(out) if out.status.success() => {
            let json_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            serde_json::from_str(&json_str).unwrap_or_default()
        }
        _ => OcrResult::default(),
    }
}

/// Simple text extraction (backward compatible)
pub fn extract_text(image_path: &Path) -> String {
    let result = extract_with_regions(image_path);
    result.text
}

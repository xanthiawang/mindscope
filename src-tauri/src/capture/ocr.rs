use std::path::Path;

use super::helpers;

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

/// Run Apple Vision OCR (bundled `ocr_helper`) and return text + bounding boxes.
/// Returns an empty result when the helper is missing or fails.
pub fn extract_with_regions(image_path: &Path) -> OcrResult {
    let Some(path) = image_path.to_str() else { return OcrResult::default(); };
    helpers::run_json::<OcrResult>("ocr_helper", &[path]).unwrap_or_default()
}

/// Simple text extraction (backward compatible)
pub fn extract_text(image_path: &Path) -> String {
    extract_with_regions(image_path).text
}

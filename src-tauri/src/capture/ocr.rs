use std::path::Path;

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
/// Uses platform-specific implementation (Apple Vision on macOS, Windows.Media.Ocr on Windows)
pub fn extract_with_regions(image_path: &Path) -> OcrResult {
    super::platform::ocr_extract_with_regions(image_path)
}

/// Simple text extraction (backward compatible)
pub fn extract_text(image_path: &Path) -> String {
    super::platform::ocr_extract_text(image_path)
}

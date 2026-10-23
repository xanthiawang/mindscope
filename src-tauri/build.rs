//! Build script: compiles the Swift helper CLIs in `swift/` into `binaries/`
//! (as Tauri "external binaries" / sidecars) BEFORE handing over to
//! `tauri_build`, which copies them next to the app binary (target/<profile>/
//! in dev, MindScope.app/Contents/MacOS/ in a bundle). The app resolves them at
//! runtime via `capture::helpers`, so a fresh clone no longer depends on
//! pre-compiled files in ~/.mindscope/bin.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// (helper name, extra frameworks to link)
const HELPERS: &[(&str, &[&str])] = &[
    ("ocr_helper", &["Vision", "AppKit"]),
    ("hevc_encoder", &["AVFoundation", "CoreMedia", "CoreVideo", "AppKit"]),
    ("frame_reader", &["AVFoundation", "AppKit"]),
    ("screen_info", &["AppKit"]),
    ("active_app", &["AppKit", "ApplicationServices"]),
    ("system_audio", &["ScreenCaptureKit", "AVFoundation", "CoreMedia"]),
    ("is_meeting", &["CoreAudio", "AppKit"]),
    ("topmost_window", &["AppKit", "CoreGraphics"]),
];

fn newer_than(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a).and_then(|m| m.modified()), fs::metadata(b).and_then(|m| m.modified())) {
        (Ok(ma), Ok(mb)) => ma > mb,
        _ => true,
    }
}

fn swift_target(rust_target: &str) -> String {
    let arch = if rust_target.starts_with("aarch64") { "arm64" } else { "x86_64" };
    let min = std::env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "11.0".to_string());
    format!("{arch}-apple-macos{min}")
}

fn compile_helpers() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let target = std::env::var("TARGET").expect("TARGET");
    println!("cargo:rustc-env=MINDSCOPE_TARGET_TRIPLE={target}");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    println!("cargo:rerun-if-env-changed=MINDSCOPE_SKIP_SWIFT");

    if !cfg!(target_os = "macos") || std::env::var_os("MINDSCOPE_SKIP_SWIFT").is_some() {
        return;
    }

    let out_dir = manifest_dir.join("binaries");
    fs::create_dir_all(&out_dir).expect("create binaries/");
    let swift_tgt = swift_target(&target);

    for (name, frameworks) in HELPERS {
        let src = manifest_dir.join("swift").join(format!("{name}.swift"));
        println!("cargo:rerun-if-changed={}", src.display());
        let out = out_dir.join(format!("{name}-{target}"));
        if out.is_file() && !newer_than(&src, &out) {
            continue;
        }
        let mut cmd = Command::new("swiftc");
        cmd.arg("-O").arg("-target").arg(&swift_tgt).arg(&src).arg("-o").arg(&out);
        for fw in frameworks.iter() {
            cmd.arg("-framework").arg(fw);
        }
        let status = cmd.status().unwrap_or_else(|e| {
            panic!("failed to run swiftc for {name}: {e}. Install Xcode Command Line Tools (xcode-select --install).")
        });
        if !status.success() {
            panic!("swiftc failed for swift/{name}.swift (see errors above)");
        }
        println!("cargo:warning=compiled Swift helper {name}");
    }
}

fn main() {
    compile_helpers();
    tauri_build::build()
}

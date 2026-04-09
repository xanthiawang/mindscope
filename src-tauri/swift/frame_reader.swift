// MindScope Frame Reader — extracts a single frame from HEVC video as JPEG
// Based on Retrace's ImageExtractor (AVAssetImageGenerator with zero tolerance)
// Usage: frame_reader <video_path> <frame_index>
// Output: JPEG data to stdout

import AVFoundation
import AppKit
import Foundation

guard CommandLine.arguments.count >= 3 else {
    FileHandle.standardError.write("Usage: frame_reader <video_path> <frame_index>\n".data(using: .utf8)!)
    exit(1)
}

let videoPath = CommandLine.arguments[1]
let frameIndex = Int(CommandLine.arguments[2]) ?? 0

// Handle extensionless files (like Retrace does)
var assetURL = URL(fileURLWithPath: videoPath)
if assetURL.pathExtension.isEmpty {
    // Create temp symlink with .mp4 extension
    let tmpLink = NSTemporaryDirectory() + "mindscope_\(ProcessInfo.processInfo.processIdentifier).mp4"
    try? FileManager.default.removeItem(atPath: tmpLink)
    try? FileManager.default.createSymbolicLink(atPath: tmpLink, withDestinationPath: videoPath)
    assetURL = URL(fileURLWithPath: tmpLink)
}

let asset = AVURLAsset(url: assetURL, options: [
    AVURLAssetPreferPreciseDurationAndTimingKey: true
])

let generator = AVAssetImageGenerator(asset: asset)
generator.appliesPreferredTrackTransform = true
// Zero tolerance = exact frame (like Retrace)
generator.requestedTimeToleranceBefore = .zero
generator.requestedTimeToleranceAfter = .zero

// Frame time: index * 20 ticks at timescale 600 (matches encoder)
let requestedTime = CMTime(value: Int64(frameIndex) * 20, timescale: 600)

do {
    var actualTime = CMTime.zero
    let cgImage = try generator.copyCGImage(at: requestedTime, actualTime: &actualTime)

    // Convert to JPEG (like Retrace: NSImage → TIFF → BitmapRep → JPEG)
    let nsImage = NSImage(cgImage: cgImage, size: NSSize(width: cgImage.width, height: cgImage.height))
    guard let tiffData = nsImage.tiffRepresentation,
          let bitmap = NSBitmapImageRep(data: tiffData),
          let jpegData = bitmap.representation(using: .jpeg, properties: [.compressionFactor: 0.85]) else {
        FileHandle.standardError.write("Failed to convert to JPEG\n".data(using: .utf8)!)
        exit(1)
    }

    // Write JPEG to stdout
    FileHandle.standardOutput.write(jpegData)
} catch {
    FileHandle.standardError.write("Error: \(error)\n".data(using: .utf8)!)
    exit(1)
}

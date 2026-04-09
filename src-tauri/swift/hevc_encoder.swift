// MindScope HEVC Encoder — based on Retrace's HEVCEncoder architecture
// Persistent process: reads frame paths from stdin, encodes to HEVC .mp4 segments
// Usage: hevc_encoder <output_dir>
// Input (stdin): one image file path per line
// Output (stdout): JSON per frame {"segment":"path","frame":N,"timestamp":"ISO8601"}

import AVFoundation
import CoreMedia
import CoreVideo
import AppKit
import Foundation

guard CommandLine.arguments.count >= 2 else {
    FileHandle.standardError.write("Usage: hevc_encoder <output_dir>\n".data(using: .utf8)!)
    exit(1)
}

let outputDir = CommandLine.arguments[1]
try? FileManager.default.createDirectory(atPath: outputDir, withIntermediateDirectories: true)

// === HEVC Writer ===
class HEVCWriter {
    let outputURL: URL
    let width: Int
    let height: Int

    private var writer: AVAssetWriter!
    private var input: AVAssetWriterInput!
    private var adaptor: AVAssetWriterInputPixelBufferAdaptor!
    private(set) var frameCount = 0

    init(path: String, width: Int, height: Int) throws {
        self.outputURL = URL(fileURLWithPath: path)
        self.width = width
        self.height = height

        writer = try AVAssetWriter(url: outputURL, fileType: .mp4)
        // Fragmented MP4: enables reading before finalization (like Retrace)
        writer.movieFragmentInterval = CMTime(seconds: 0.1, preferredTimescale: 600)

        let compression: [String: Any] = [
            AVVideoAverageBitRateKey: 800_000,  // 800kbps — good quality for screen content
            AVVideoMaxKeyFrameIntervalKey: 30,
            AVVideoAllowFrameReorderingKey: true,
            AVVideoProfileLevelKey: "HEVC_Main_AutoLevel",
        ]

        let settings: [String: Any] = [
            AVVideoCodecKey: AVVideoCodecType.hevc,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: compression,
        ]

        input = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
        input.expectsMediaDataInRealTime = true

        adaptor = AVAssetWriterInputPixelBufferAdaptor(
            assetWriterInput: input,
            sourcePixelBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: width,
                kCVPixelBufferHeightKey as String: height,
            ]
        )

        writer.add(input)
        writer.startWriting()
        writer.startSession(atSourceTime: .zero)
    }

    func appendFrame(cgImage: CGImage) -> Bool {
        if !input.isReadyForMoreMediaData {
            Thread.sleep(forTimeInterval: 0.05)
            if !input.isReadyForMoreMediaData { return false }
        }

        var pb: CVPixelBuffer?
        CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, nil, &pb)
        guard let pixelBuffer = pb else { return false }

        CVPixelBufferLockBaseAddress(pixelBuffer, [])
        let ctx = CGContext(
            data: CVPixelBufferGetBaseAddress(pixelBuffer),
            width: width, height: height,
            bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(pixelBuffer),
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
        )!
        ctx.draw(cgImage, in: CGRect(x: 0, y: 0, width: width, height: height))
        CVPixelBufferUnlockBaseAddress(pixelBuffer, [])

        // Timestamp: frame_index * 20 ticks at timescale 600 (= 30fps encoding)
        let time = CMTime(value: Int64(frameCount) * 20, timescale: 600)
        let ok = adaptor.append(pixelBuffer, withPresentationTime: time)
        if ok { frameCount += 1 }
        return ok
    }

    func finalize() {
        input.markAsFinished()
        let sem = DispatchSemaphore(value: 0)
        writer.finishWriting { sem.signal() }
        sem.wait()
    }
}

// === Main ===
let maxFramesPerSegment = 300  // 300 frames × 5s interval = 25 min per segment
var segmentIndex = 0
var currentWriter: HEVCWriter?
var currentSegmentPath = ""

func screenSize() -> (Int, Int) {
    guard let screen = NSScreen.main else { return (1920, 1080) }
    let s = screen.frame.size
    // Half resolution for storage efficiency
    return (Int(s.width), Int(s.height))
}

func startNewSegment() throws {
    if let w = currentWriter {
        w.finalize()
        FileHandle.standardError.write("Segment finalized: \(currentSegmentPath) (\(w.frameCount) frames)\n".data(using: .utf8)!)
    }

    let (w, h) = screenSize()
    let dateStr = ISO8601DateFormatter().string(from: Date()).prefix(10)
    let segDir = "\(outputDir)/\(dateStr)"
    try? FileManager.default.createDirectory(atPath: segDir, withIntermediateDirectories: true)

    currentSegmentPath = "\(segDir)/seg_\(String(format: "%06d", segmentIndex)).mp4"
    currentWriter = try HEVCWriter(path: currentSegmentPath, width: w, height: h)
    segmentIndex += 1
}

FileHandle.standardError.write("HEVC encoder ready. Reading frame paths from stdin...\n".data(using: .utf8)!)

// Read frame paths from stdin, one per line
while let line = readLine()?.trimmingCharacters(in: .whitespacesAndNewlines) {
    if line.isEmpty { continue }
    if line == "QUIT" { break }

    // Load image
    guard let image = NSImage(contentsOfFile: line),
          let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
        FileHandle.standardError.write("Skip (can't load): \(line)\n".data(using: .utf8)!)
        continue
    }

    // Start new segment if needed
    if currentWriter == nil || currentWriter!.frameCount >= maxFramesPerSegment {
        do { try startNewSegment() } catch {
            FileHandle.standardError.write("Error starting segment: \(error)\n".data(using: .utf8)!)
            continue
        }
    }

    // Encode frame
    if currentWriter!.appendFrame(cgImage: cgImage) {
        let ts = ISO8601DateFormatter().string(from: Date())
        let json = "{\"segment\":\"\(currentSegmentPath)\",\"frame\":\(currentWriter!.frameCount - 1),\"timestamp\":\"\(ts)\"}"
        print(json)
        fflush(stdout)

        // Delete source image (already encoded in video)
        try? FileManager.default.removeItem(atPath: line)
    }
}

// Finalize on exit
currentWriter?.finalize()
FileHandle.standardError.write("HEVC encoder shutdown.\n".data(using: .utf8)!)

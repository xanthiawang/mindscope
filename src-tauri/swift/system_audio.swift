// MindScope system audio capture helper
// Uses ScreenCaptureKit (macOS 13+) to record system audio output to a file.
// Usage: system_audio <output_path> <duration_seconds>

import Foundation
import AVFoundation
import ScreenCaptureKit

@available(macOS 13.0, *)
class SystemAudioRecorder: NSObject, SCStreamDelegate, SCStreamOutput {
    private var stream: SCStream?
    private var audioFile: AVAudioFile?
    private let outputURL: URL
    private let duration: TimeInterval
    private let semaphore = DispatchSemaphore(value: 0)
    private var sampleCount: Int = 0

    init(outputPath: String, duration: TimeInterval) {
        self.outputURL = URL(fileURLWithPath: outputPath)
        self.duration = duration
        super.init()
    }

    func start() {
        Task {
            do {
                // Get shareable content (displays)
                let content = try await SCShareableContent.current
                guard let display = content.displays.first else {
                    fputs("Error: No displays found\n", stderr)
                    semaphore.signal()
                    return
                }

                // Configure for audio-only capture
                let config = SCStreamConfiguration()
                config.capturesAudio = true
                config.excludesCurrentProcessAudio = true
                config.sampleRate = 16000
                config.channelCount = 1
                // Minimize video overhead — we only want audio
                config.width = 2
                config.height = 2
                config.minimumFrameInterval = CMTime(value: 1, timescale: 1)

                // Create a filter for the display (required, but we only capture audio)
                let filter = SCContentFilter(display: display, excludingWindows: [])

                // Create and start the stream
                let stream = SCStream(filter: filter, configuration: config, delegate: self)
                try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: DispatchQueue(label: "audio"))
                try await stream.startCapture()
                self.stream = stream

                // Record for the specified duration
                try await Task.sleep(nanoseconds: UInt64(duration * 1_000_000_000))

                // Stop capture
                try await stream.stopCapture()
                self.audioFile = nil  // close file
                semaphore.signal()
            } catch {
                fputs("Error: \(error.localizedDescription)\n", stderr)
                semaphore.signal()
            }
        }
        semaphore.wait()
    }

    // SCStreamOutput — receive audio samples
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio else { return }
        guard sampleBuffer.isValid else { return }
        guard let formatDesc = sampleBuffer.formatDescription else { return }

        let mediaType = CMFormatDescriptionGetMediaType(formatDesc)
        guard mediaType == kCMMediaType_Audio else { return }

        // Create audio file on first sample (so we know the format)
        if audioFile == nil {
            guard let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(formatDesc)?.pointee else { return }
            let settings: [String: Any] = [
                AVFormatIDKey: Int(kAudioFormatLinearPCM),
                AVSampleRateKey: asbd.mSampleRate,
                AVNumberOfChannelsKey: Int(asbd.mChannelsPerFrame),
                AVLinearPCMBitDepthKey: 16,
                AVLinearPCMIsFloatKey: false,
                AVLinearPCMIsBigEndianKey: false,
                AVLinearPCMIsNonInterleaved: false,
            ]
            do {
                audioFile = try AVAudioFile(forWriting: outputURL, settings: settings)
            } catch {
                fputs("Error creating audio file: \(error.localizedDescription)\n", stderr)
                return
            }
        }

        // Write samples to file
        guard let blockBuffer = CMSampleBufferGetDataBuffer(sampleBuffer) else { return }
        let length = CMBlockBufferGetDataLength(blockBuffer)
        var data = Data(count: length)
        data.withUnsafeMutableBytes { ptr in
            if let baseAddress = ptr.baseAddress {
                CMBlockBufferCopyDataBytes(blockBuffer, atOffset: 0, dataLength: length, destination: baseAddress)
            }
        }

        guard let format = audioFile?.processingFormat else { return }
        let frameCount = AVAudioFrameCount(UInt32(length) / format.streamDescription.pointee.mBytesPerFrame)
        guard frameCount > 0 else { return }
        guard let pcmBuffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frameCount) else { return }
        pcmBuffer.frameLength = frameCount

        // Copy raw data into the PCM buffer
        if let channelData = pcmBuffer.int16ChannelData {
            data.withUnsafeBytes { rawPtr in
                if let src = rawPtr.baseAddress {
                    memcpy(channelData[0], src, length)
                }
            }
        }

        do {
            try audioFile?.write(from: pcmBuffer)
            sampleCount += Int(frameCount)
        } catch {
            fputs("Error writing audio: \(error.localizedDescription)\n", stderr)
        }
    }

    // SCStreamDelegate — handle errors
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        fputs("Stream stopped with error: \(error.localizedDescription)\n", stderr)
        semaphore.signal()
    }
}

// --- Main ---
guard CommandLine.arguments.count >= 3 else {
    fputs("Usage: system_audio <output_path> <duration_seconds>\n", stderr)
    exit(1)
}

let outputPath = CommandLine.arguments[1]
guard let duration = Double(CommandLine.arguments[2]), duration > 0 else {
    fputs("Error: duration must be a positive number\n", stderr)
    exit(1)
}

if #available(macOS 13.0, *) {
    let recorder = SystemAudioRecorder(outputPath: outputPath, duration: duration)
    recorder.start()
    print("OK")
} else {
    fputs("Error: macOS 13.0 or later required for system audio capture\n", stderr)
    exit(1)
}

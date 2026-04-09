// MindScope OCR Helper — returns text + bounding boxes as JSON
// Usage: ocr_helper <image_path>
// Output: JSON { "text": "...", "regions": [{"text":"...","x":0.1,"y":0.2,"w":0.5,"h":0.03},...] }
// Coordinates are normalized 0-1 (relative to image dimensions)

import Vision
import AppKit
import Foundation

guard CommandLine.arguments.count > 1 else { exit(1) }
let imagePath = CommandLine.arguments[1]

guard let image = NSImage(contentsOfFile: imagePath),
      let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    print("{\"text\":\"\",\"regions\":[]}")
    exit(0)
}

let request = VNRecognizeTextRequest()
request.recognitionLevel = .fast
request.recognitionLanguages = ["en-US", "zh-Hans", "zh-Hant"]
request.usesLanguageCorrection = false

let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
try? handler.perform([request])

var fullText: [String] = []
var regions: [[String: Any]] = []

if let results = request.results {
    for observation in results {
        if let candidate = observation.topCandidates(1).first {
            fullText.append(candidate.string)

            // Bounding box: Vision uses bottom-left origin, normalize to top-left
            let box = observation.boundingBox
            let region: [String: Any] = [
                "text": candidate.string,
                "x": box.origin.x,
                "y": 1.0 - box.origin.y - box.height,  // Flip Y to top-left origin
                "w": box.width,
                "h": box.height
            ]
            regions.append(region)
        }
    }
}

// Output as JSON
let output: [String: Any] = [
    "text": fullText.joined(separator: "\n"),
    "regions": regions
]

if let data = try? JSONSerialization.data(withJSONObject: output),
   let json = String(data: data, encoding: .utf8) {
    print(json)
} else {
    print("{\"text\":\"\",\"regions\":[]}")
}

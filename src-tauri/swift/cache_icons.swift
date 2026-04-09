// Pre-cache app icons for all installed apps
import AppKit
import Foundation

let iconDir = FileManager.default.homeDirectoryForCurrentUser
    .appendingPathComponent(".mindscope/data/icons")
try? FileManager.default.createDirectory(at: iconDir, withIntermediateDirectories: true)

// Scan /Applications and /System/Applications
let dirs = ["/Applications", "/System/Applications", "/System/Applications/Utilities"]
var cached = 0

for dir in dirs {
    guard let apps = try? FileManager.default.contentsOfDirectory(atPath: dir) else { continue }
    for app in apps where app.hasSuffix(".app") {
        let appName = String(app.dropLast(4)) // remove .app
        let iconPath = iconDir.appendingPathComponent("\(appName).png")
        if FileManager.default.fileExists(atPath: iconPath.path) { continue }

        let appPath = "\(dir)/\(app)"
        let icon = NSWorkspace.shared.icon(forFile: appPath)
        icon.size = NSSize(width: 64, height: 64)

        if let tiff = icon.tiffRepresentation,
           let rep = NSBitmapImageRep(data: tiff),
           let png = rep.representation(using: .png, properties: [:]) {
            try? png.write(to: iconPath)
            cached += 1
        }
    }
}

print("Cached \(cached) app icons")

// MindScope topmost_window — frontmost real application window via CGWindowList z-order.
// Replaces the osascript / NSWorkspace fallbacks: works even when MindScope's own
// overlay is frontmost, and reports which display the window lives on so the
// recorder can capture only that display.
//
// Output (stdout): one JSON object, or the literal "NONE" when no window qualifies.
//   {"app":"Safari","owner":"Safari","title":"…","bundle":"com.apple.Safari",
//    "pid":123,"wid":456,"x":0,"y":25,"w":1440,"h":850,"display":1}
//
// Also caches the app icon as PNG under $MINDSCOPE_HOME/data/icons/<app>.png
// (default ~/.mindscope) the first time an app is seen, so /app-icon works.

import AppKit
import CoreGraphics
import Foundation

let skipOwners: Set<String> = [
    "Window Server", "SystemUIServer", "ControlCenter", "Control Center", "Dock",
    "NotificationCenter", "Notification Center", "loginwindow", "WindowManager",
    "Spotlight", "Screenshot", "MindScope", "universalaccessd", "TextInputMenuAgent",
    "CursorUIViewService", "Contexts", "StatusIndicator",
]
let skipTitles: Set<String> = ["Item-0", "App Icon Window", "Menubar", "Menu Bar", "StatusIndicator"]

let parentPid = getppid()

func jsonString(_ s: String) -> String {
    let data = (try? JSONSerialization.data(withJSONObject: [s])) ?? Data()
    let arr = String(data: data, encoding: .utf8) ?? "[\"\"]"
    // strip the surrounding [ ]
    return String(arr.dropFirst().dropLast())
}

func mindscopeHome() -> URL {
    if let env = ProcessInfo.processInfo.environment["MINDSCOPE_HOME"], !env.isEmpty {
        return URL(fileURLWithPath: env)
    }
    return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".mindscope")
}

func cacheIcon(app: NSRunningApplication?, name: String) {
    guard let app = app, let icon = app.icon else { return }
    let safe = name.replacingOccurrences(of: "/", with: "_")
    let iconDir = mindscopeHome().appendingPathComponent("data/icons")
    let iconPath = iconDir.appendingPathComponent("\(safe).png")
    if FileManager.default.fileExists(atPath: iconPath.path) { return }
    try? FileManager.default.createDirectory(at: iconDir, withIntermediateDirectories: true)
    icon.size = NSSize(width: 64, height: 64)
    if let tiff = icon.tiffRepresentation,
       let rep = NSBitmapImageRep(data: tiff),
       let png = rep.representation(using: .png, properties: [:]) {
        try? png.write(to: iconPath)
    }
}

guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
        as? [[String: Any]] else {
    print("NONE")
    exit(0)
}

for w in list {
    let layer = w[kCGWindowLayer as String] as? Int ?? 0
    if layer != 0 { continue }  // menu bar extras, overlays, our own floating bar
    let pid = pid_t(w[kCGWindowOwnerPID as String] as? Int ?? 0)
    if pid == parentPid || pid == getpid() { continue }
    let owner = w[kCGWindowOwnerName as String] as? String ?? ""
    if skipOwners.contains(owner) { continue }
    let alpha = w[kCGWindowAlpha as String] as? Double ?? 1.0
    if alpha < 0.05 { continue }
    guard let b = w[kCGWindowBounds as String] as? [String: CGFloat] else { continue }
    let rect = CGRect(x: b["X"] ?? 0, y: b["Y"] ?? 0, width: b["Width"] ?? 0, height: b["Height"] ?? 0)
    if rect.width < 64 || rect.height < 64 { continue }
    let title = w[kCGWindowName as String] as? String ?? ""
    if skipTitles.contains(title) { continue }
    let wid = w[kCGWindowNumber as String] as? Int ?? 0

    // Display containing the window (by rect intersection; first match wins).
    var displayId: CGDirectDisplayID = CGMainDisplayID()
    var matched: UInt32 = 0
    var found: CGDirectDisplayID = 0
    if CGGetDisplaysWithRect(rect, 1, &found, &matched) == .success, matched > 0 {
        displayId = found
    }

    let app = NSRunningApplication(processIdentifier: pid)
    let name = app?.localizedName ?? owner
    let bundle = app?.bundleIdentifier ?? ""
    cacheIcon(app: app, name: name)

    print("{\"app\":\(jsonString(name)),\"owner\":\(jsonString(owner)),\"title\":\(jsonString(title)),"
          + "\"bundle\":\(jsonString(bundle)),\"pid\":\(pid),\"wid\":\(wid),"
          + "\"x\":\(Int(rect.origin.x)),\"y\":\(Int(rect.origin.y)),\"w\":\(Int(rect.width)),\"h\":\(Int(rect.height)),"
          + "\"display\":\(displayId)}")
    exit(0)
}

print("NONE")

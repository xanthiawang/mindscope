// Get active app name + window title using NSWorkspace + Accessibility API
// More reliable than AppleScript — returns actual app name, not "App"
import AppKit

let workspace = NSWorkspace.shared
guard let app = workspace.frontmostApplication else {
    print("Unknown|")
    exit(0)
}

let appName = app.localizedName ?? app.bundleIdentifier ?? "Unknown"
let bundleId = app.bundleIdentifier ?? ""
var windowTitle = ""

// Get window title via Accessibility API
let pid = app.processIdentifier
let axApp = AXUIElementCreateApplication(pid)
var value: CFTypeRef?
var axWindow: AXUIElement?
if AXUIElementCopyAttributeValue(axApp, kAXFocusedWindowAttribute as CFString, &value) == .success {
    let window = value as! AXUIElement
    axWindow = window
    var titleValue: CFTypeRef?
    if AXUIElementCopyAttributeValue(window, kAXTitleAttribute as CFString, &titleValue) == .success {
        windowTitle = titleValue as? String ?? ""
    }
}

// Browser URL extraction via Accessibility API
var browserURL = ""
let browserBundles = [
    "com.google.Chrome",
    "com.apple.Safari",
    "company.thebrowser.Browser",  // Arc
    "org.mozilla.firefox",
    "com.microsoft.edgemac"
]
if browserBundles.contains(bundleId), let window = axWindow {
    // Recursive search for AXTextField containing URL-like text
    func findURLField(_ element: AXUIElement, depth: Int = 0) -> String? {
        if depth > 5 { return nil }
        var role: CFTypeRef?
        AXUIElementCopyAttributeValue(element, kAXRoleAttribute as CFString, &role)
        let roleStr = role as? String ?? ""
        if roleStr == "AXTextField" || roleStr == "AXComboBox" {
            var urlValue: CFTypeRef?
            if AXUIElementCopyAttributeValue(element, kAXValueAttribute as CFString, &urlValue) == .success {
                let val = urlValue as? String ?? ""
                if val.contains(".") || val.contains("://") {
                    return val
                }
            }
        }
        var children: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXChildrenAttribute as CFString, &children) == .success,
              let kids = children as? [AXUIElement] else { return nil }
        for child in kids {
            if let found = findURLField(child, depth: depth + 1) {
                return found
            }
        }
        return nil
    }
    if let url = findURLField(window) {
        browserURL = url
    }
}

// Also extract and cache app icon if not already cached
let iconDir = FileManager.default.homeDirectoryForCurrentUser
    .appendingPathComponent(".mindscope/data/icons")
try? FileManager.default.createDirectory(at: iconDir, withIntermediateDirectories: true)
let iconPath = iconDir.appendingPathComponent("\(appName).png")
if !FileManager.default.fileExists(atPath: iconPath.path) {
    if let icon = app.icon {
        icon.size = NSSize(width: 64, height: 64)
        if let tiff = icon.tiffRepresentation,
           let rep = NSBitmapImageRep(data: tiff),
           let png = rep.representation(using: .png, properties: [:]) {
            try? png.write(to: iconPath)
        }
    }
}

print("\(appName)|\(windowTitle)|\(bundleId)|\(browserURL)")

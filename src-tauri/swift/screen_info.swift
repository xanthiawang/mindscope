// Return all screens info as JSON for multi-monitor support
// Output: [{"x":0,"y":34,"w":1440,"h":866,"scale":2.0,"is_main":true,"mouse":true}, ...]
import AppKit
import Foundation

var screens: [[String: Any]] = []
let mouseLocation = NSEvent.mouseLocation

for screen in NSScreen.screens {
    let f = screen.frame
    let vf = screen.visibleFrame
    let scale = screen.backingScaleFactor
    let isMain = (screen == NSScreen.main)

    // Convert to top-left origin (AppKit uses bottom-left)
    let topY = f.height - vf.height - vf.origin.y + (f.origin.y == 0 ? 0 : f.origin.y)

    // For multi-monitor: visibleFrame origin is relative to the global coordinate space
    // We need the global position
    let globalX = vf.origin.x
    // Top-left Y in global coords: total height of main screen - (origin.y + height)
    let mainHeight = NSScreen.screens.first?.frame.height ?? f.height
    let globalTopY = mainHeight - vf.origin.y - vf.height

    // Check if mouse is on this screen
    let mouseOnScreen = NSMouseInRect(mouseLocation, f, false)

    screens.append([
        "x": globalX,
        "y": globalTopY,
        "w": vf.width,
        "h": vf.height,
        "scale": scale,
        "is_main": isMain,
        "mouse": mouseOnScreen,
        // Raw frame for fullscreen positioning
        "frame_x": f.origin.x,
        "frame_y": mainHeight - f.origin.y - f.height,
        "frame_w": f.width,
        "frame_h": f.height,
    ])
}

if let data = try? JSONSerialization.data(withJSONObject: screens, options: []),
   let json = String(data: data, encoding: .utf8) {
    print(json)
} else {
    // Fallback single screen
    print("[{\"x\":0,\"y\":34,\"w\":1440,\"h\":866,\"scale\":2.0,\"is_main\":true,\"mouse\":true,\"frame_x\":0,\"frame_y\":0,\"frame_w\":1440,\"frame_h\":900}]")
}

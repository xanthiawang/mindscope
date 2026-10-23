// MindScope is_meeting — "is the user in a call right now?"
//
// Combines two signals so an idle Zoom window does not count as a meeting:
//   1. which processes are actively using an audio INPUT device
//      (macOS 14+: CoreAudio process objects, kAudioProcessPropertyIsRunningInput;
//       older: kAudioDevicePropertyDeviceIsRunningSomewhere on the default input)
//   2. which known meeting apps are running / frontmost / showing a call window
//
// MindScope's own recorder (ffmpeg, spawned by our parent) is excluded by PID so
// our own recording never keeps a "meeting" alive.
//
// Usage: is_meeting [--ignore-pid N]...
// Output (stdout): one JSON object:
//   {"mic_active":true,"mic_precise":true,
//    "mic_users":[{"pid":1,"bundle":"us.zoom.xos","name":"zoom.us"}],
//    "meeting_apps":[{"name":"Zoom","bundle":"us.zoom.xos","pid":1}],
//    "frontmost":{"name":"Safari","bundle":"com.apple.Safari"},
//    "call_window":{"app":"Zoom","title":"Zoom Meeting"},
//    "in_call":true,"in_call_app":"Zoom"}

import AppKit
import CoreAudio
import Foundation

// (bundle id prefix, display name)
let meetingBundles: [(String, String)] = [
    ("us.zoom.xos", "Zoom"),
    ("com.apple.FaceTime", "FaceTime"),
    ("com.microsoft.teams", "Microsoft Teams"),
    ("com.cisco.webex", "Webex"),
    ("Cisco-Systems.Spark", "Webex"),
    ("com.hnc.Discord", "Discord"),
    ("com.tencent.meeting", "Tencent Meeting"),
    ("com.tencent.wemeet", "Tencent Meeting"),
    ("com.alibaba.DingTalkMac", "DingTalk"),
    ("com.bytedance.macos.feishu", "Feishu"),
    ("com.electron.lark", "Lark"),
    ("com.skype.skype", "Skype"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("com.google.meet", "Google Meet"),
]
let meetingNameHints = ["zoom", "腾讯会议", "tencent meeting", "wemeet", "钉钉", "dingtalk",
                        "飞书", "feishu", "lark", "teams", "webex", "facetime", "discord", "skype"]
let browserBundles: [String] = [
    "com.google.Chrome", "com.apple.Safari", "company.thebrowser.Browser",
    "org.mozilla.firefox", "com.microsoft.edgemac", "com.brave.Browser", "com.vivaldi.Vivaldi",
]
func isBrowser(_ bundle: String) -> Bool { browserBundles.contains { bundle.hasPrefix($0) } }
let ignoredMicBundles: Set<String> = ["com.apple.controlcenter", "com.apple.audio.coreaudiod"]

func meetingLabel(bundle: String, name: String) -> String? {
    for (prefix, label) in meetingBundles where bundle.hasPrefix(prefix) { return label }
    let lower = name.lowercased()
    for hint in meetingNameHints where lower.contains(hint) { return name }
    return nil
}

/// Does this window title look like an active call (not just the app's home screen)?
func isCallTitle(app: String, bundle: String, title: String) -> Bool {
    let t = title.lowercased()
    if t.isEmpty { return false }
    if bundle.hasPrefix("us.zoom.xos") { return t.contains("zoom meeting") || t.contains("zoom webinar") || t.contains("会议") }
    if bundle.hasPrefix("com.apple.FaceTime") { return t != "facetime" }
    if bundle.hasPrefix("com.microsoft.teams") { return t.contains("meeting") || t.contains("call") || t.contains("会议") }
    if bundle.hasPrefix("com.tencent") { return t.contains("会议") || t.contains("meeting") }
    if bundle.hasPrefix("com.alibaba.DingTalkMac") || bundle.hasPrefix("com.bytedance") || bundle.hasPrefix("com.electron.lark") {
        return t.contains("会议") || t.contains("meeting") || t.contains("call")
    }
    if isBrowser(bundle) {
        return t.contains("meet - ") || t.contains("google meet") || t.contains("meet.google")
            || t.contains("zoom meeting") || t.contains("microsoft teams") || t.contains("webex")
    }
    return t.contains("meeting") || t.contains("会议") || t.contains("call")
}

func jsonString(_ s: String) -> String {
    let data = (try? JSONSerialization.data(withJSONObject: [s])) ?? Data()
    let arr = String(data: data, encoding: .utf8) ?? "[\"\"]"
    return String(arr.dropFirst().dropLast())
}

func parentPid(of pid: pid_t) -> pid_t? {
    var info = kinfo_proc()
    var size = MemoryLayout<kinfo_proc>.size
    var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, pid]
    guard sysctl(&mib, 4, &info, &size, nil, 0) == 0, size > 0 else { return nil }
    return info.kp_eproc.e_ppid
}

func processName(_ pid: pid_t) -> String {
    var buf = [CChar](repeating: 0, count: 4096)
    let n = proc_pidpath(pid, &buf, UInt32(buf.count))
    if n > 0 {
        let path = String(cString: buf)
        return (path as NSString).lastPathComponent
    }
    return ""
}

// ---- args ----
var ignorePids = Set<pid_t>([getpid(), getppid()])
var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let a = args.removeFirst()
    if a == "--ignore-pid", !args.isEmpty, let p = Int32(args.removeFirst()) { ignorePids.insert(p) }
}
let ourParent = getppid()

// ---- 1. microphone users ----
struct MicUser { let pid: pid_t; let bundle: String; let name: String }
var micUsers: [MicUser] = []
var micPrecise = false
var micActive = false

func audioProp(_ obj: AudioObjectID, _ selector: AudioObjectPropertySelector) -> UInt32? {
    var addr = AudioObjectPropertyAddress(mSelector: selector,
                                          mScope: kAudioObjectPropertyScopeGlobal,
                                          mElement: kAudioObjectPropertyElementMain)
    var value: UInt32 = 0
    var size = UInt32(MemoryLayout<UInt32>.size)
    guard AudioObjectGetPropertyData(obj, &addr, 0, nil, &size, &value) == noErr else { return nil }
    return value
}

if #available(macOS 14.0, *) {
    var addr = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyProcessObjectList,
                                          mScope: kAudioObjectPropertyScopeGlobal,
                                          mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    let sys = AudioObjectID(kAudioObjectSystemObject)
    if AudioObjectGetPropertyDataSize(sys, &addr, 0, nil, &size) == noErr, size > 0 {
        let count = Int(size) / MemoryLayout<AudioObjectID>.size
        var ids = [AudioObjectID](repeating: 0, count: count)
        if AudioObjectGetPropertyData(sys, &addr, 0, nil, &size, &ids) == noErr {
            micPrecise = true
            for obj in ids {
                guard let running = audioProp(obj, kAudioProcessPropertyIsRunningInput), running != 0 else { continue }
                guard let pidRaw = audioProp(obj, kAudioProcessPropertyPID) else { continue }
                let pid = pid_t(bitPattern: pidRaw)
                if ignorePids.contains(pid) { continue }
                if let pp = parentPid(of: pid), pp == ourParent { continue }  // our ffmpeg / helpers
                let app = NSRunningApplication(processIdentifier: pid)
                let bundle = app?.bundleIdentifier ?? ""
                if ignoredMicBundles.contains(bundle) { continue }
                let name = app?.localizedName ?? processName(pid)
                micUsers.append(MicUser(pid: pid, bundle: bundle, name: name))
            }
            micActive = !micUsers.isEmpty
        }
    }
}

if !micPrecise {
    // Fallback: is the default input device running in ANY process (including us)?
    var addr = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice,
                                          mScope: kAudioObjectPropertyScopeGlobal,
                                          mElement: kAudioObjectPropertyElementMain)
    var dev: AudioDeviceID = 0
    var size = UInt32(MemoryLayout<AudioDeviceID>.size)
    if AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &dev) == noErr, dev != 0 {
        if let running = audioProp(dev, kAudioDevicePropertyDeviceIsRunningSomewhere) {
            micActive = running != 0
        }
    }
}

// ---- 2. running meeting apps + frontmost ----
struct MeetingApp { let name: String; let bundle: String; let pid: pid_t }
var meetingApps: [MeetingApp] = []
for app in NSWorkspace.shared.runningApplications {
    if app.activationPolicy != .regular { continue }  // skip XPC services / helpers
    let bundle = app.bundleIdentifier ?? ""
    let name = app.localizedName ?? ""
    if let label = meetingLabel(bundle: bundle, name: name) {
        meetingApps.append(MeetingApp(name: label, bundle: bundle, pid: app.processIdentifier))
    }
}
let front = NSWorkspace.shared.frontmostApplication
let frontName = front?.localizedName ?? ""
let frontBundle = front?.bundleIdentifier ?? ""
let frontIsMeeting = meetingLabel(bundle: frontBundle, name: frontName) != nil

// ---- 3. call windows (meeting apps + browsers) ----
var callWindow: (app: String, title: String, pid: pid_t)? = nil
if let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]] {
    for w in list {
        let layer = w[kCGWindowLayer as String] as? Int ?? 0
        if layer != 0 { continue }
        let pid = pid_t(w[kCGWindowOwnerPID as String] as? Int ?? 0)
        let title = w[kCGWindowName as String] as? String ?? ""
        if title.isEmpty { continue }
        let app = NSRunningApplication(processIdentifier: pid)
        let bundle = app?.bundleIdentifier ?? ""
        let name = app?.localizedName ?? (w[kCGWindowOwnerName as String] as? String ?? "")
        let label = meetingLabel(bundle: bundle, name: name)
        if label == nil && !isBrowser(bundle) { continue }
        if isCallTitle(app: name, bundle: bundle, title: title) {
            callWindow = (label ?? name, title, pid)
            break
        }
    }
}

// ---- 4. decision ----
var inCall = false
var inCallApp = ""
if micPrecise {
    for u in micUsers {
        if let label = meetingLabel(bundle: u.bundle, name: u.name) {
            inCall = true; inCallApp = label; break
        }
        if isBrowser(u.bundle), let cw = callWindow, (cw.pid == u.pid || parentPid(of: u.pid) == cw.pid) {
            inCall = true; inCallApp = cw.app; break
        }
    }
} else if micActive {
    if let cw = callWindow { inCall = true; inCallApp = cw.app }
    else if frontIsMeeting { inCall = true; inCallApp = meetingLabel(bundle: frontBundle, name: frontName) ?? frontName }
}

// ---- output ----
var out = "{"
out += "\"mic_active\":\(micActive),\"mic_precise\":\(micPrecise),"
out += "\"mic_users\":[" + micUsers.map {
    "{\"pid\":\($0.pid),\"bundle\":\(jsonString($0.bundle)),\"name\":\(jsonString($0.name))}"
}.joined(separator: ",") + "],"
out += "\"meeting_apps\":[" + meetingApps.map {
    "{\"name\":\(jsonString($0.name)),\"bundle\":\(jsonString($0.bundle)),\"pid\":\($0.pid)}"
}.joined(separator: ",") + "],"
out += "\"frontmost\":{\"name\":\(jsonString(frontName)),\"bundle\":\(jsonString(frontBundle))},"
if let cw = callWindow {
    out += "\"call_window\":{\"app\":\(jsonString(cw.app)),\"title\":\(jsonString(cw.title))},"
} else {
    out += "\"call_window\":null,"
}
out += "\"in_call\":\(inCall),\"in_call_app\":\(jsonString(inCallApp))}"
print(out)

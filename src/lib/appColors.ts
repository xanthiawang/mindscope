// Map of app names to their brand colors and emoji icons
// Used in the timeline to show colored segments per app

interface AppInfo {
  color: string;
  emoji: string;
  short: string;
}

const APP_MAP: Record<string, AppInfo> = {
  // Browsers
  "Google Chrome": { color: "#4285F4", emoji: "🌐", short: "Chrome" },
  "Safari": { color: "#006CFF", emoji: "🧭", short: "Safari" },
  "Firefox": { color: "#FF7139", emoji: "🦊", short: "Firefox" },
  "Arc": { color: "#5F5DFF", emoji: "🌈", short: "Arc" },
  "Microsoft Edge": { color: "#0078D4", emoji: "🌀", short: "Edge" },

  // Communication
  "Slack": { color: "#4A154B", emoji: "💬", short: "Slack" },
  "WeChat": { color: "#07C160", emoji: "💚", short: "WeChat" },
  "Telegram": { color: "#0088CC", emoji: "✈️", short: "Telegram" },
  "Discord": { color: "#5865F2", emoji: "🎮", short: "Discord" },
  "Messages": { color: "#34C759", emoji: "💬", short: "Messages" },
  "Mail": { color: "#007AFF", emoji: "📧", short: "Mail" },
  "Spark": { color: "#1C86EE", emoji: "⚡", short: "Spark" },

  // Meetings
  "zoom.us": { color: "#2D8CFF", emoji: "📹", short: "Zoom" },
  "Zoom": { color: "#2D8CFF", emoji: "📹", short: "Zoom" },
  "FaceTime": { color: "#34C759", emoji: "📞", short: "FaceTime" },
  "Microsoft Teams": { color: "#6264A7", emoji: "👥", short: "Teams" },
  "Google Meet": { color: "#00897B", emoji: "📹", short: "Meet" },

  // Productivity
  "Finder": { color: "#4A90D9", emoji: "📁", short: "Finder" },
  "Preview": { color: "#8E8E93", emoji: "🖼️", short: "Preview" },
  "Notes": { color: "#FFCC00", emoji: "📝", short: "Notes" },
  "Reminders": { color: "#007AFF", emoji: "☑️", short: "Reminders" },
  "Calendar": { color: "#FF3B30", emoji: "📅", short: "Calendar" },

  // Editors
  "Visual Studio Code": { color: "#007ACC", emoji: "💻", short: "VS Code" },
  "Code": { color: "#007ACC", emoji: "💻", short: "VS Code" },
  "Cursor": { color: "#7C3AED", emoji: "💻", short: "Cursor" },
  "Xcode": { color: "#147EFB", emoji: "🛠️", short: "Xcode" },
  "Terminal": { color: "#000000", emoji: "⬛", short: "Terminal" },
  "iTerm2": { color: "#000000", emoji: "⬛", short: "iTerm" },
  "Warp": { color: "#01A4FF", emoji: "⬛", short: "Warp" },

  // Design
  "Figma": { color: "#F24E1E", emoji: "🎨", short: "Figma" },
  "Sketch": { color: "#FDB300", emoji: "💎", short: "Sketch" },
  "Canva": { color: "#00C4CC", emoji: "🖌️", short: "Canva" },

  // Office
  "Microsoft Word": { color: "#2B579A", emoji: "📄", short: "Word" },
  "Microsoft Excel": { color: "#217346", emoji: "📊", short: "Excel" },
  "Microsoft PowerPoint": { color: "#D24726", emoji: "📊", short: "PPT" },
  "Keynote": { color: "#0070C9", emoji: "📊", short: "Keynote" },
  "Pages": { color: "#FF9500", emoji: "📄", short: "Pages" },
  "Numbers": { color: "#34C759", emoji: "📊", short: "Numbers" },

  // Media
  "Spotify": { color: "#1DB954", emoji: "🎵", short: "Spotify" },
  "Music": { color: "#FC3C44", emoji: "🎵", short: "Music" },
  "YouTube": { color: "#FF0000", emoji: "▶️", short: "YouTube" },

  // System
  "System Settings": { color: "#8E8E93", emoji: "⚙️", short: "Settings" },
  "System Preferences": { color: "#8E8E93", emoji: "⚙️", short: "Settings" },
  "Activity Monitor": { color: "#34C759", emoji: "📊", short: "Activity" },
};

const FALLBACK: AppInfo = { color: "#6B7280", emoji: "📱", short: "App" };

// Screenpipe-style app category colors (6 levels of gray → more readable timeline)
const CATEGORY_COLORS: Record<string, string> = {
  browser: "#1a1a1a",        // darkest
  dev: "#3d3d3d",
  communication: "#666666",
  productivity: "#8a8a8a",
  media: "#ababab",
  other: "#cccccc",          // lightest
};

const APP_CATEGORIES: Record<string, string> = {
  "Google Chrome": "browser", "Safari": "browser", "Firefox": "browser", "Arc": "browser", "Microsoft Edge": "browser",
  "Visual Studio Code": "dev", "Code": "dev", "Cursor": "dev", "Xcode": "dev", "Terminal": "dev", "iTerm2": "dev", "Warp": "dev",
  "Slack": "communication", "WeChat": "communication", "Telegram": "communication", "Discord": "communication", "Messages": "communication", "Mail": "communication",
  "zoom.us": "communication", "Zoom": "communication", "FaceTime": "communication", "Microsoft Teams": "communication",
  "Finder": "productivity", "Notes": "productivity", "Reminders": "productivity", "Calendar": "productivity", "Preview": "productivity",
  "Microsoft Word": "productivity", "Microsoft Excel": "productivity", "Microsoft PowerPoint": "productivity", "Keynote": "productivity", "Pages": "productivity",
  "Figma": "productivity", "Sketch": "productivity", "Canva": "productivity",
  "Spotify": "media", "Music": "media", "YouTube": "media",
};

export function getAppCategoryColor(appName: string): string {
  const category = APP_CATEGORIES[appName] || "other";
  return CATEGORY_COLORS[category] || CATEGORY_COLORS.other;
}

export function getAppInfo(appName: string): AppInfo {
  return APP_MAP[appName] ?? FALLBACK;
}

export function getAppColor(appName: string): string {
  return (APP_MAP[appName] ?? FALLBACK).color;
}

export function getAppEmoji(appName: string): string {
  return (APP_MAP[appName] ?? FALLBACK).emoji;
}

export function getAppShort(appName: string): string {
  return (APP_MAP[appName] ?? FALLBACK).short;
}

// Get unique apps from frames
export function getUniqueApps(frames: { app_name: string }[]): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const f of frames) {
    if (f.app_name && !seen.has(f.app_name)) {
      seen.add(f.app_name);
      result.push(f.app_name);
    }
  }
  return result;
}

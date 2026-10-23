// Light timeline row from SQLite (no OCR text, no path — images are served by id)
export interface TimelineFrame {
  id: number;
  timestamp: number; // microseconds since epoch
  app_name: string;
  window_name: string;
}

// Full frame (search results / detail views)
export interface CapturedFrame extends TimelineFrame {
  ocr_text: string;
  image_path: string;
}

export type SearchResult = CapturedFrame;

// Settings
export interface AppSettings {
  retention_days: number;
  capture_interval_secs: number;
  jpeg_quality: number;
  idle_threshold_secs: number;
  excluded_apps: string[];
  capture_audio: boolean;
  transcription_engine: string | null;
  private_browsing: boolean;
  ffmpeg_path?: string | null;
  claude_path?: string | null;
  storage_budget_mb?: number;
  claude_daily_budget?: number;
  capture_height?: number;
}

export interface StorageInfo {
  total_size_mb: number;
  total_size_display: string;
  frame_count: number;
  oldest_timestamp: number;
  newest_timestamp: number;
}

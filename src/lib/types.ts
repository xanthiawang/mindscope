// Frame from SQLite database
export interface CapturedFrame {
  id: number;
  timestamp: number; // microseconds since epoch
  app_name: string;
  window_name: string;
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
}

export interface StorageInfo {
  total_size_mb: number;
  total_size_display: string;
  frame_count: number;
  oldest_timestamp: number;
  newest_timestamp: number;
}

import { invoke } from "@tauri-apps/api/core";
import type { CapturedFrame, AppSettings, StorageInfo } from "./types";

// Permission (NEVER triggers dialog)
export async function checkPermission(): Promise<boolean> {
  return invoke("check_permission");
}
export async function openPermissionSettings(): Promise<void> {
  return invoke("open_permission_settings");
}

// Recording
export async function startRecording(): Promise<boolean> {
  return invoke("start_recording");
}
export async function stopRecording(): Promise<void> {
  return invoke("stop_recording");
}
export async function isRecording(): Promise<boolean> {
  return invoke("is_recording");
}

// Timeline (from SQLite)
export async function getTimeline(date: string): Promise<CapturedFrame[]> {
  return invoke("get_timeline", { date });
}
export async function getScreenshot(imagePath: string): Promise<string | null> {
  return invoke("get_screenshot", { imagePath });
}

// Load ALL thumbnails for a date in one call — enables instant scrubbing
export async function getAllThumbnails(date: string): Promise<Record<string, string>> {
  return invoke("get_all_thumbnails", { date });
}

// OCR regions (for highlighting on screenshot)
export interface OcrRegion {
  text: string;
  x: number;  // 0-1 normalized
  y: number;
  w: number;
  h: number;
}
export async function getOcrRegions(imagePath: string): Promise<OcrRegion[]> {
  return invoke("get_ocr_regions", { imagePath });
}

// Search (FTS5)
export async function search(query: string): Promise<CapturedFrame[]> {
  return invoke("search", { query });
}

// Storage
export async function getStorageInfo(): Promise<StorageInfo | null> {
  return invoke("get_storage_info");
}
export async function cleanupOldData(retentionDays: number): Promise<number> {
  return invoke("cleanup_old_data", { retentionDays });
}

// Audio / Meeting recording
export async function checkMicPermission(): Promise<boolean> {
  return invoke("check_mic_permission");
}
export async function requestMicPermission(): Promise<boolean> {
  return invoke("request_mic_permission");
}
export async function startAudio(): Promise<boolean> {
  return invoke("start_audio");
}
export async function stopAudio(): Promise<void> {
  return invoke("stop_audio");
}
export async function isAudioRecording(): Promise<boolean> {
  return invoke("is_audio_recording");
}

// Window control
export async function resizeToBar(): Promise<void> {
  return invoke("resize_to_bar");
}
export async function resizeToFullscreen(): Promise<void> {
  return invoke("resize_to_fullscreen");
}
export async function resizeToSearch(): Promise<void> {
  return invoke("resize_to_search");
}

// Window control
export async function hideWindow(): Promise<void> {
  return invoke("hide_window");
}

// Bar expand/collapse (resize window for panels)
export async function expandBar(): Promise<void> {
  return invoke("expand_bar");
}
export async function collapseBar(): Promise<void> {
  return invoke("collapse_bar");
}

// Panel control (NSWindow level + click-through)
export async function setClickthrough(enabled: boolean): Promise<void> {
  return invoke("set_clickthrough", { enabled });
}
// Windows only: adjust interactive zone height (physical px from screen bottom).
// 200 = bar-only, 700 = panels open.
export async function setInteractiveZone(px: number): Promise<void> {
  return invoke("set_interactive_zone", { px });
}
export async function setBarMode(): Promise<void> {
  return invoke("set_bar_mode");
}
export async function setFullscreenMode(): Promise<void> {
  return invoke("set_fullscreen_mode");
}

// Whisper
export async function isWhisperAvailable(): Promise<boolean> {
  return invoke("is_whisper_available");
}
export async function downloadWhisperModel(): Promise<void> {
  return invoke("download_whisper_model");
}

// Pipes (automation)
export interface PipeConfig {
  name: string;
  schedule: string;
  enabled: boolean;
  prompt: string;
  output: string;
  context_query?: string;
  context_hours?: number;
}
export interface PipeInfo {
  id: string;
  config: PipeConfig;
  last_run?: string;
  last_result?: string;
}
export interface PipeResult {
  pipe_id: string;
  timestamp: string;
  output: string;
  success: boolean;
}
export async function listPipes(): Promise<PipeInfo[]> {
  return invoke("list_pipes");
}
export async function createPipe(id: string, config: PipeConfig): Promise<void> {
  return invoke("create_pipe", { id, config });
}
export async function setPipeEnabled(id: string, enabled: boolean): Promise<void> {
  return invoke("set_pipe_enabled", { id, enabled });
}
export async function runPipe(id: string): Promise<PipeResult> {
  return invoke("run_pipe", { id });
}

// Brief
export async function getDailyBrief(): Promise<string> {
  return invoke("get_daily_brief");
}
export async function generateJournal(date: string): Promise<void> {
  return invoke("generate_journal", { date });
}

// Apps
export async function getAllApps(): Promise<string[]> {
  return invoke("get_all_apps");
}

// Meeting status
export async function getMeetingStatus(): Promise<{
  active: boolean;
  app_name: string;
  recent_transcripts: Array<{ time: string; speaker: string; text: string }>;
}> {
  return invoke("get_meeting_status");
}

// Settings
export async function getSettings(): Promise<AppSettings> {
  return invoke("get_settings");
}
export async function updateSettings(settings: Partial<AppSettings>): Promise<AppSettings> {
  return invoke("update_settings", { settings });
}

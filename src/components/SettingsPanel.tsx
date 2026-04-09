import { useState, useEffect } from "react";
import type { AppSettings, StorageInfo } from "../lib/types";
import { getSettings, updateSettings, getStorageInfo, cleanupOldData, listPipes, runPipe, setPipeEnabled, isWhisperAvailable, downloadWhisperModel } from "../lib/commands";
import type { PipeInfo } from "../lib/commands";

interface Props { onClose: () => void; }

type Tab = "general" | "screen" | "audio" | "meetings" | "storage" | "shortcuts" | "pipes";

const COMMON_APPS = [
  "Finder", "Safari", "Google Chrome", "Arc", "Firefox",
  "Visual Studio Code", "Cursor", "Xcode", "Terminal", "iTerm2",
  "Slack", "Discord", "WeChat", "Telegram", "Messages", "Mail",
  "zoom.us", "FaceTime", "Microsoft Teams",
  "Notes", "Calendar", "Music", "Spotify", "Preview",
  "System Settings",
];

const RETENTION_OPTIONS = [
  { label: "1 week", days: 7 }, { label: "1 month", days: 30 },
  { label: "3 months", days: 90 }, { label: "1 year", days: 365 },
  { label: "Forever", days: 0 },
];

const TABS: { id: Tab; label: string }[] = [
  { id: "general", label: "General" }, { id: "screen", label: "Screen" },
  { id: "audio", label: "Audio" }, { id: "meetings", label: "Meetings" },
  { id: "storage", label: "Storage" }, { id: "shortcuts", label: "Shortcuts" },
  { id: "pipes", label: "Pipes" },
];

export default function SettingsPanel({ onClose }: Props) {
  const [tab, setTab] = useState<Tab>("general");
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [storage, setStorage] = useState<StorageInfo | null>(null);
  // saving state removed - inline in handlers
  const [cleaning, setCleaning] = useState(false);
  const [pipes, setPipes] = useState<PipeInfo[]>([]);
  const [runningPipe, setRunningPipe] = useState<string | null>(null);
  const [whisperAvailable, setWhisperAvailable] = useState(false);
  const [downloadingWhisper, setDownloadingWhisper] = useState(false);

  useEffect(() => {
    getSettings().then(setSettings).catch(() => {});
    getStorageInfo().then(setStorage).catch(() => {});
    listPipes().then(setPipes).catch(() => {});
    isWhisperAvailable().then(setWhisperAvailable).catch(() => {});
  }, []);

  const update = async (partial: Partial<AppSettings>) => {
    const updated = await updateSettings(partial);
    setSettings(updated);
    return updated;
  };

  const toggleExcludedApp = async (app: string) => {
    if (!settings) return;
    const current = settings.excluded_apps || [];
    const next = current.includes(app) ? current.filter(a => a !== app) : [...current, app];
    await update({ excluded_apps: next } as Partial<AppSettings>);
  };

  return (
    <div style={{ position: "absolute", bottom: 78, right: 0, zIndex: 50 }}>
      <div className="animate-slide-up" style={{
        width: 540, maxHeight: 500, background: "rgba(255,255,255,0.95)",
        backdropFilter: "blur(40px)", borderRadius: 20,
        boxShadow: "0 12px 48px rgba(0,0,0,0.12), 0 0 0 0.5px rgba(0,0,0,0.06)",
        overflow: "hidden", color: "#1d1d1f", display: "flex", flexDirection: "column",
      }}>
        {/* Header */}
        <div style={{ padding: "16px 24px 0", textAlign: "center", flexShrink: 0 }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <button onClick={onClose} style={{ width: 12, height: 12, borderRadius: 6, background: "#FF5F57", border: "0.5px solid rgba(0,0,0,0.1)", cursor: "pointer" }} />
            <span style={{ fontSize: 14, fontWeight: 600 }}>Settings</span>
            <div style={{ width: 12 }} />
          </div>
          <div style={{ display: "flex", justifyContent: "center", gap: 2, marginTop: 12, flexWrap: "wrap" }}>
            {TABS.map((t) => (
              <button key={t.id} onClick={() => setTab(t.id)} style={{
                padding: "5px 10px", borderRadius: 100, border: "none", cursor: "pointer",
                background: tab === t.id ? "rgba(0,0,0,0.06)" : "transparent",
                color: tab === t.id ? "#1d1d1f" : "#86868b", fontSize: 11, fontWeight: 500,
              }}>{t.label}</button>
            ))}
          </div>
        </div>

        {/* Content */}
        <div style={{ padding: "16px 24px 24px", overflowY: "auto", flex: 1 }}>

          {/* === GENERAL === */}
          {tab === "general" && settings && (
            <div>
              <p style={{ fontSize: 12, color: "#86868b", marginBottom: 14 }}>
                Screen recordings are stored locally and not sent to the cloud.
              </p>

              <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 8 }}>Exclude Apps</div>
              <p style={{ fontSize: 11, color: "#86868b", marginBottom: 8 }}>Select which apps you do not want recorded:</p>
              <div style={{ maxHeight: 200, overflowY: "auto", border: "1px solid rgba(0,0,0,0.06)", borderRadius: 10, background: "white" }}>
                {COMMON_APPS.map((app) => {
                  const excluded = (settings.excluded_apps || []).includes(app);
                  const iconUrl = `http://127.0.0.1:9457/app-icon/${encodeURIComponent(app)}`;
                  return (
                    <label key={app} style={{
                      display: "flex", alignItems: "center", gap: 10, padding: "8px 12px",
                      borderBottom: "1px solid rgba(0,0,0,0.04)", cursor: "pointer",
                    }}>
                      <input type="checkbox" checked={excluded} onChange={() => toggleExcludedApp(app)}
                        style={{ width: 16, height: 16, accentColor: "#007AFF" }} />
                      <img src={iconUrl} alt="" style={{ width: 20, height: 20, borderRadius: 4 }}
                        onError={(e) => { (e.target as HTMLImageElement).style.display = "none"; }} />
                      <span style={{ fontSize: 13 }}>{app}</span>
                    </label>
                  );
                })}
              </div>

              <div style={{ marginTop: 16 }}>
                <SettingRow label="Private Browsing" description="Do not record Incognito/Private windows">
                  <Toggle on={settings.private_browsing ?? true} onChange={() => update({ private_browsing: !settings.private_browsing } as Partial<AppSettings>)} />
                </SettingRow>
              </div>
            </div>
          )}

          {/* === SCREEN === */}
          {tab === "screen" && settings && (
            <div>
              <SettingRow label="Capture interval" description="How often to take a screenshot">
                <div style={{ display: "flex", gap: 4 }}>
                  {[2, 3, 5, 10, 30].map((s) => (
                    <PillBtn key={s} label={`${s}s`} active={settings.capture_interval_secs === s}
                      onClick={() => update({ capture_interval_secs: s })} />
                  ))}
                </div>
              </SettingRow>

              <SettingRow label="Image quality" description="Higher quality = more storage">
                <div style={{ display: "flex", gap: 4 }}>
                  {[{ l: "Low", v: 0.4 }, { l: "Med", v: 0.6 }, { l: "High", v: 0.7 }, { l: "Max", v: 0.9 }].map((q) => (
                    <PillBtn key={q.v} label={q.l} active={Math.abs(settings.jpeg_quality - q.v) < 0.05}
                      onClick={() => update({ jpeg_quality: q.v })} />
                  ))}
                </div>
              </SettingRow>

              <SettingRow label="Idle detection" description="Skip when screen unchanged">
                <div style={{ display: "flex", gap: 4 }}>
                  {[30, 60, 120, 300].map((s) => (
                    <PillBtn key={s} label={s >= 60 ? `${s/60}m` : `${s}s`}
                      active={settings.idle_threshold_secs === s}
                      onClick={() => update({ idle_threshold_secs: s })} />
                  ))}
                </div>
              </SettingRow>

              <SettingRow label="Text Recognition" description="OCR language support">
                <span style={{ fontSize: 12, color: "#1d1d1f" }}>English + Chinese</span>
              </SettingRow>
            </div>
          )}

          {/* === AUDIO === */}
          {tab === "audio" && settings && (
            <div>
              <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 4 }}>Audio Recordings</div>
              <p style={{ fontSize: 12, color: "#86868b", marginBottom: 14 }}>Audio recordings are stored locally and not sent to the cloud.</p>

              <SettingRow label="Audio capture" description="Record microphone for transcription">
                <Toggle on={settings.capture_audio} onChange={() => update({ capture_audio: !settings.capture_audio })} />
              </SettingRow>

              <SettingRow label="Transcription engine" description="Whisper (local, better) or System (macOS Speech)">
                <div style={{ display: "flex", gap: 4 }}>
                  <PillBtn label="Whisper" active={settings.transcription_engine !== "system"}
                    onClick={() => update({ transcription_engine: "whisper" } as Partial<AppSettings>)} />
                  <PillBtn label="System" active={settings.transcription_engine === "system"}
                    onClick={() => update({ transcription_engine: "system" } as Partial<AppSettings>)} />
                </div>
              </SettingRow>

              {!whisperAvailable && (
                <div style={{ marginTop: 12, padding: "10px 14px", borderRadius: 10, background: "rgba(0,0,0,0.03)", display: "flex", alignItems: "center", gap: 8 }}>
                  <span style={{ fontSize: 12, color: "#86868b", flex: 1 }}>Whisper model not installed</span>
                  <button onClick={async () => { setDownloadingWhisper(true); try { await downloadWhisperModel(); setWhisperAvailable(true); } catch {} setDownloadingWhisper(false); }}
                    disabled={downloadingWhisper} style={{ padding: "4px 12px", borderRadius: 100, border: "1px solid #007AFF", background: "white", color: "#007AFF", fontSize: 11, cursor: "pointer" }}>
                    {downloadingWhisper ? "Downloading..." : "Download"}
                  </button>
                </div>
              )}
              {whisperAvailable && (
                <p style={{ fontSize: 11, color: "#34C759", marginTop: 8 }}>Whisper model installed (ggml-base.en)</p>
              )}
            </div>
          )}

          {/* === MEETINGS === */}
          {tab === "meetings" && settings && (
            <div>
              <SettingRow label="Auto-transcribe meetings" description="Automatically record audio when a meeting app is active">
                <Toggle on={settings.capture_audio} onChange={() => update({ capture_audio: !settings.capture_audio })} />
              </SettingRow>

              <p style={{ fontSize: 12, color: "#86868b", marginTop: 8, marginBottom: 16 }}>
                When Zoom, Teams, FaceTime, or other meeting apps are detected, MindScope automatically starts recording and transcribing. Meeting notes are saved to the Vault.
              </p>

              <div style={{ padding: "14px 16px", borderRadius: 10, background: "rgba(0,0,0,0.03)" }}>
                <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 4 }}>Calendar Sync</div>
                <p style={{ fontSize: 12, color: "#86868b" }}>Connect your calendar to auto-join meetings and get summaries ready to email.</p>
                <button style={{ marginTop: 8, padding: "6px 14px", borderRadius: 100, border: "1px solid #007AFF", background: "white", color: "#007AFF", fontSize: 12, cursor: "pointer" }}>
                  Coming soon
                </button>
              </div>

              <div style={{ marginTop: 12, padding: "14px 16px", borderRadius: 10, background: "rgba(0,0,0,0.03)" }}>
                <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 4 }}>Detected meeting apps</div>
                <p style={{ fontSize: 11, color: "#86868b" }}>Zoom, FaceTime, Microsoft Teams, Google Meet, Webex, Slack, Discord, Tencent Meeting, DingTalk, Lark, Skype</p>
              </div>
            </div>
          )}

          {/* === STORAGE === */}
          {tab === "storage" && (
            <div>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "baseline", marginBottom: 14 }}>
                <span style={{ fontSize: 13, fontWeight: 600 }}>Disk space used:</span>
                <span style={{ fontSize: 17, fontWeight: 700 }}>{storage?.total_size_display ?? "..."}</span>
              </div>
              <div style={{ display: "flex", gap: 10, marginBottom: 16 }}>
                <StatBox label="Frames" value={storage?.frame_count?.toLocaleString() ?? "..."} />
                <StatBox label="Total" value={storage?.total_size_display ?? "..."} />
              </div>
              <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 8 }}>Retention period</div>
              <div style={{ display: "flex", gap: 6, marginBottom: 12 }}>
                {RETENTION_OPTIONS.map((opt) => (
                  <PillBtn key={opt.days} label={opt.label}
                    active={settings?.retention_days === opt.days}
                    onClick={() => { update({ retention_days: opt.days }).then(() => getStorageInfo().then(setStorage)); }} />
                ))}
              </div>
              <button onClick={async () => { setCleaning(true); await cleanupOldData(settings?.retention_days ?? 90); setStorage(await getStorageInfo()); setCleaning(false); }}
                disabled={cleaning} style={{ width: "100%", padding: "8px", borderRadius: 100, border: "1px solid rgba(0,0,0,0.08)", background: "white", color: "#FF3B30", fontSize: 12, fontWeight: 500, cursor: "pointer" }}>
                {cleaning ? "Cleaning..." : "Clean up old data"}
              </button>
            </div>
          )}

          {/* === SHORTCUTS === */}
          {tab === "shortcuts" && (
            <div>
              <ShortcutRow keys="⌘ ⇧ Space" desc="Open / close MindScope" />
              <ShortcutRow keys="Esc" desc="Hide to background" />
              <ShortcutRow keys="← →" desc="Navigate frames" />
              <ShortcutRow keys="Scroll / Swipe" desc="Scrub timeline" />
              <ShortcutRow keys="Click time" desc="Jump to date" />
              <ShortcutRow keys="Click search" desc="Open search" />
            </div>
          )}

          {/* === PIPES === */}
          {tab === "pipes" && (
            <div>
              <p style={{ fontSize: 12, color: "#86868b", marginBottom: 12 }}>Automations that analyze your screen history using AI.</p>
              {pipes.map((pipe) => (
                <div key={pipe.id} style={{ padding: "10px 14px", borderRadius: 10, border: "1px solid rgba(0,0,0,0.06)", marginBottom: 8, background: "white" }}>
                  <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                    <div>
                      <div style={{ fontSize: 13, fontWeight: 600 }}>{pipe.config.name}</div>
                      <div style={{ fontSize: 10, color: "#86868b", marginTop: 1 }}>{pipe.config.schedule} | {pipe.config.output}</div>
                    </div>
                    <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                      <button onClick={async () => { setRunningPipe(pipe.id); try { await runPipe(pipe.id); setPipes(await listPipes()); } catch {} setRunningPipe(null); }}
                        disabled={runningPipe === pipe.id} style={{ padding: "3px 10px", borderRadius: 100, border: "1px solid #007AFF", background: "white", color: "#007AFF", fontSize: 11, cursor: "pointer" }}>
                        {runningPipe === pipe.id ? "..." : "Run"}
                      </button>
                      <Toggle on={pipe.config.enabled} onChange={async () => { await setPipeEnabled(pipe.id, !pipe.config.enabled); setPipes(await listPipes()); }} small />
                    </div>
                  </div>
                </div>
              ))}
              {whisperAvailable ? (
                <p style={{ fontSize: 11, color: "#34C759", marginTop: 8 }}>Whisper: installed</p>
              ) : (
                <button onClick={async () => { setDownloadingWhisper(true); try { await downloadWhisperModel(); setWhisperAvailable(true); } catch {} setDownloadingWhisper(false); }}
                  disabled={downloadingWhisper} style={{ marginTop: 8, padding: "4px 12px", borderRadius: 100, border: "1px solid #007AFF", background: "white", color: "#007AFF", fontSize: 11, cursor: "pointer" }}>
                  {downloadingWhisper ? "Downloading Whisper..." : "Download Whisper Model"}
                </button>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// --- Shared components ---

function Toggle({ on, onChange, small }: { on: boolean; onChange: () => void; small?: boolean }) {
  const w = small ? 36 : 44;
  const h = small ? 20 : 24;
  const dot = small ? 16 : 20;
  return (
    <button onClick={onChange} style={{
      width: w, height: h, borderRadius: h/2, border: "none",
      background: on ? "#007AFF" : "#d1d5db", cursor: "pointer", position: "relative", flexShrink: 0,
    }}>
      <div style={{
        width: dot, height: dot, borderRadius: dot/2, background: "white",
        position: "absolute", top: (h-dot)/2, left: on ? w - dot - 2 : 2,
        transition: "left 0.15s", boxShadow: "0 1px 2px rgba(0,0,0,0.2)",
      }} />
    </button>
  );
}

function PillBtn({ label, active, onClick }: { label: string; active: boolean; onClick: () => void }) {
  return (
    <button onClick={onClick} style={{
      padding: "5px 10px", borderRadius: 100, border: "none", cursor: "pointer",
      background: active ? "#007AFF" : "rgba(0,0,0,0.04)",
      color: active ? "white" : "#3a3a3c", fontSize: 11, fontWeight: 500,
    }}>{label}</button>
  );
}

function SettingRow({ label, description, children }: { label: string; description: string; children: React.ReactNode }) {
  return (
    <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "10px 0", borderBottom: "1px solid rgba(0,0,0,0.04)" }}>
      <div style={{ flex: 1 }}>
        <div style={{ fontSize: 13, fontWeight: 500 }}>{label}</div>
        <div style={{ fontSize: 11, color: "#86868b", marginTop: 1 }}>{description}</div>
      </div>
      {children}
    </div>
  );
}

function StatBox({ label, value }: { label: string; value: string }) {
  return (
    <div style={{ flex: 1, background: "rgba(0,0,0,0.03)", borderRadius: 10, padding: "10px", textAlign: "center" }}>
      <div style={{ fontSize: 16, fontWeight: 700 }}>{value}</div>
      <div style={{ fontSize: 10, color: "#86868b", marginTop: 2 }}>{label}</div>
    </div>
  );
}

function ShortcutRow({ keys, desc }: { keys: string; desc: string }) {
  return (
    <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", padding: "8px 0", borderBottom: "1px solid rgba(0,0,0,0.04)" }}>
      <span style={{ fontSize: 13, color: "#3a3a3c" }}>{desc}</span>
      <kbd style={{ background: "rgba(0,0,0,0.04)", padding: "3px 8px", borderRadius: 6, fontSize: 12, fontFamily: "SF Mono, monospace", color: "#3a3a3c" }}>{keys}</kbd>
    </div>
  );
}

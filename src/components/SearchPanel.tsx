import { useState, useEffect, useMemo, useRef } from "react";
import type { CapturedFrame } from "../lib/types";
import { search as searchFrames, getAllApps } from "../lib/commands";
import { invoke } from "@tauri-apps/api/core";
import { getAppColor, getAppShort, getUniqueApps } from "../lib/appColors";

interface Props {
  onClose: () => void;
  onSelectFrame: (frame: CapturedFrame, query?: string, regions?: Array<{text:string,x:number,y:number,w:number,h:number}>) => void;
}

type Tab = "apps" | "meetings" | "starred";

const MEETING_APPS = [
  "zoom", "teams", "facetime", "meet", "webex",
  "tencent meeting", "tencent", "飞书", "钉钉",
];

function isMeetingApp(appName: string): boolean {
  const lower = appName.toLowerCase();
  return MEETING_APPS.some((k) => lower.includes(k));
}

// SVG icon components (no emoji)
function ChevronDownIcon({ size = 10 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
      <path d="M2 3.5L5 6.5L8 3.5" />
    </svg>
  );
}

function SearchIcon({ size = 14 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
      <circle cx="11" cy="11" r="8" />
      <path d="M21 21l-4.35-4.35" />
    </svg>
  );
}

function CloseIcon({ size = 10 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
      <path d="M1 1l8 8M9 1l-8 8" />
    </svg>
  );
}

function StarIcon({ size = 13 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
    </svg>
  );
}

function MeetingIcon({ size = 13 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <polygon points="23 7 16 12 23 17 23 7" />
      <rect x="1" y="5" width="15" height="14" rx="2" ry="2" />
    </svg>
  );
}

function GridIcon({ size = 13 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3" y="3" width="7" height="7" /><rect x="14" y="3" width="7" height="7" />
      <rect x="3" y="14" width="7" height="7" /><rect x="14" y="14" width="7" height="7" />
    </svg>
  );
}

export default function SearchPanel({ onClose, onSelectFrame }: Props) {
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<Tab>("apps");
  const [results, setResults] = useState<CapturedFrame[]>([]);
  const [loading, setLoading] = useState(false);
  const [appFilter, setAppFilter] = useState<string | null>(null);
  const [appsDropdownOpen, setAppsDropdownOpen] = useState(false);
  const [appSearchQuery, setAppSearchQuery] = useState("");
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Search results with regions (from HTTP API, includes OCR coordinates)
  const [searchRegions, setSearchRegions] = useState<Record<number, Array<{text:string,x:number,y:number,w:number,h:number}>>>({});

  // Audio transcript segments for Meetings tab
  interface AudioSeg { timestamp: string; audio_path: string; transcript: string; duration_secs: number; }
  interface MeetingSession {
    startTime: string;
    endTime: string;
    duration: number; // seconds
    fullTranscript: string;
    segmentCount: number;
  }
  const [meetingSessions, setMeetingSessions] = useState<MeetingSession[]>([]);

  // Load audio segments when Meetings tab is active, group by session
  useEffect(() => {
    if (tab !== "meetings") return;
    const now = new Date();
    const today = `${now.getFullYear()}-${String(now.getMonth()+1).padStart(2,"0")}-${String(now.getDate()).padStart(2,"0")}`;
    invoke<AudioSeg[]>("get_audio_segments", { date: today })
      .then((segs) => {
        const valid = segs.filter((s) => s.transcript && s.transcript.trim().length > 0);
        // Group consecutive segments into sessions (gap > 5 min = new session)
        const sessions: MeetingSession[] = [];
        const SESSION_GAP_MS = 5 * 60 * 1000;
        const parseTs = (ts: string) => new Date(ts).getTime();

        for (const seg of valid) {
          const segTime = parseTs(seg.timestamp);
          const last = sessions[sessions.length - 1];
          if (last && segTime - parseTs(last.endTime) < SESSION_GAP_MS) {
            // Extend existing session
            // Dedupe: skip if transcript identical to last few lines
            const lines = last.fullTranscript.split(" | ");
            const segText = seg.transcript.trim();
            if (!lines.slice(-3).some((l) => l === segText)) {
              last.fullTranscript += " | " + segText;
              last.segmentCount += 1;
            }
            last.endTime = seg.timestamp;
            last.duration = Math.round((segTime - parseTs(last.startTime)) / 1000) + seg.duration_secs;
          } else {
            // New session
            sessions.push({
              startTime: seg.timestamp,
              endTime: seg.timestamp,
              duration: seg.duration_secs,
              fullTranscript: seg.transcript.trim(),
              segmentCount: 1,
            });
          }
        }
        // Newest first
        sessions.reverse();
        setMeetingSessions(sessions);
      })
      .catch(() => {});
  }, [tab]);

  // Close dropdown on outside click
  useEffect(() => {
    function handleClick(e: MouseEvent) {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setAppsDropdownOpen(false);
      }
    }
    if (appsDropdownOpen) {
      document.addEventListener("mousedown", handleClick);
      return () => document.removeEventListener("mousedown", handleClick);
    }
  }, [appsDropdownOpen]);

  // Real-time search via HTTP API
  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (!query.trim()) { setResults([]); setSearchRegions({}); setLoading(false); return; }
    setLoading(true);
    debounceRef.current = setTimeout(async () => {
      try {
        const resp = await fetch(`http://127.0.0.1:9457/search?q=${encodeURIComponent(query.trim())}&limit=30`);
        const data = await resp.json();
        const items = data.results || [];
        const frames = items.map((r: any) => ({
          id: r.id, timestamp: r.timestamp, app_name: r.app_name,
          window_name: r.window_name, ocr_text: r.text || "", image_path: r.image_path || "",
        }));
        const regions: Record<number, any[]> = {};
        items.forEach((r: any) => { if (r.regions?.length) regions[r.id] = r.regions; });
        setResults(frames);
        setSearchRegions(regions);
      } catch {
        setResults(await searchFrames(query.trim()));
        setSearchRegions({});
      }
      setLoading(false);
    }, 300);
    return () => { if (debounceRef.current) clearTimeout(debounceRef.current); };
  }, [query]);

  // Load all apps from DB (not just from search results)
  const [allApps, setAllApps] = useState<string[]>([]);
  useEffect(() => {
    getAllApps().then(setAllApps).catch(() => {});
  }, []);
  // Merge: DB apps + any from search results
  const apps = useMemo(() => {
    const fromResults = getUniqueApps(results);
    const merged = [...new Set([...allApps, ...fromResults])];
    return merged.sort();
  }, [allApps, results]);

  const filteredApps = useMemo(() => {
    if (!appSearchQuery.trim()) return apps;
    const q = appSearchQuery.toLowerCase();
    return apps.filter((a) => a.toLowerCase().includes(q) || getAppShort(a).toLowerCase().includes(q));
  }, [apps, appSearchQuery]);

  const filtered = useMemo(() => {
    let r = results;
    if (tab === "apps") {
      if (appFilter) r = r.filter((f) => f.app_name === appFilter);
    } else if (tab === "meetings") {
      r = r.filter((f) => isMeetingApp(f.app_name));
    } else if (tab === "starred") {
      return []; // placeholder
    }
    return r;
  }, [results, appFilter, tab]);

  const groupedByDate = useMemo(() => {
    const groups: Map<string, CapturedFrame[]> = new Map();
    for (const f of filtered) {
      const date = new Date(f.timestamp / 1000).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
      if (!groups.has(date)) groups.set(date, []);
      groups.get(date)!.push(f);
    }
    return groups;
  }, [filtered]);

  function handleTabClick(t: Tab) {
    if (t === "apps") {
      if (tab === "apps") {
        setAppsDropdownOpen((prev) => !prev);
      } else {
        setTab("apps");
        setAppsDropdownOpen(false);
      }
    } else {
      setTab(t);
      setAppsDropdownOpen(false);
      setAppFilter(null);
    }
  }

  return (
    <div style={{
      position: "absolute", bottom: 78, left: "50%", transform: "translateX(-50%)",
      width: 680, maxHeight: 520,
      background: "rgba(255,255,255,0.95)",
      backdropFilter: "blur(40px)",
      WebkitBackdropFilter: "blur(40px)",
      borderRadius: 20,
      boxShadow: "0 12px 48px rgba(0,0,0,0.12), 0 0 0 0.5px rgba(0,0,0,0.06)",
      display: "flex", flexDirection: "column", overflow: "hidden", zIndex: 200, color: "#1f2937",
    }}>
      {/* Search input */}
      <div style={{ display: "flex", alignItems: "center", padding: "14px 18px", gap: 10 }}>
        <button onClick={onClose} style={{
          background: "rgba(0,0,0,0.05)", border: "none", borderRadius: 100,
          width: 26, height: 26, cursor: "pointer", color: "#86868b",
          display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
        }}>
          <CloseIcon />
        </button>
        <div style={{
          flex: 1, display: "flex", alignItems: "center", gap: 8,
          background: "rgba(0,0,0,0.03)", borderRadius: 10, padding: "8px 14px",
        }}>
          <span style={{ color: "#9ca3af", display: "flex", flexShrink: 0 }}>
            <SearchIcon />
          </span>
          <input type="text" value={query} onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Escape") onClose(); }}
            placeholder="Search anything you've seen, said, or heard..."
            autoFocus
            style={{ flex: 1, border: "none", outline: "none", fontSize: 15, color: "#1f2937", background: "transparent" }}
          />
          {query && (
            <button onClick={() => setQuery("")} style={{
              background: "#e5e7eb", border: "none", borderRadius: 10,
              width: 20, height: 20, cursor: "pointer", color: "#6b7280",
              display: "flex", alignItems: "center", justifyContent: "center", fontSize: 10,
            }}>
              <CloseIcon size={8} />
            </button>
          )}
        </div>
      </div>

      {/* Tab pills */}
      <div style={{ display: "flex", alignItems: "center", gap: 6, padding: "4px 18px 8px", position: "relative" }}>
        {/* Apps tab with dropdown */}
        <div ref={dropdownRef} style={{ position: "relative" }}>
          <button onClick={() => handleTabClick("apps")} style={{
            display: "flex", alignItems: "center", gap: 5, padding: "6px 14px",
            borderRadius: 100, border: "none", cursor: "pointer",
            background: tab === "apps" ? "rgba(0,0,0,0.06)" : "transparent",
            color: tab === "apps" ? "#1d1d1f" : "#86868b",
            fontSize: 12, fontWeight: 500, transition: "background 0.15s",
          }}>
            <GridIcon size={12} />
            <span>{appFilter ? getAppShort(appFilter) : "Apps"}</span>
            <ChevronDownIcon size={9} />
          </button>

          {/* Apps dropdown */}
          {appsDropdownOpen && (
            <div style={{
              position: "absolute", top: "calc(100% + 6px)", left: 0,
              width: 220, maxHeight: 280,
              background: "rgba(255,255,255,0.98)",
              backdropFilter: "blur(20px)",
              WebkitBackdropFilter: "blur(20px)",
              borderRadius: 12,
              boxShadow: "0 8px 32px rgba(0,0,0,0.14), 0 0 0 0.5px rgba(0,0,0,0.06)",
              zIndex: 300, overflow: "hidden",
              display: "flex", flexDirection: "column",
            }}>
              {/* Filter input */}
              <div style={{ padding: "8px 10px", borderBottom: "1px solid #f0f0f0" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 6, background: "rgba(0,0,0,0.03)", borderRadius: 8, padding: "5px 8px" }}>
                  <span style={{ color: "#9ca3af", display: "flex" }}><SearchIcon size={11} /></span>
                  <input
                    type="text"
                    value={appSearchQuery}
                    onChange={(e) => setAppSearchQuery(e.target.value)}
                    placeholder="Filter apps"
                    autoFocus
                    style={{ flex: 1, border: "none", outline: "none", fontSize: 12, color: "#1f2937", background: "transparent" }}
                  />
                </div>
              </div>

              {/* Show all option */}
              <div style={{ overflowY: "auto", flex: 1 }}>
                {appFilter && (
                  <button onClick={() => { setAppFilter(null); setAppsDropdownOpen(false); setAppSearchQuery(""); }} style={{
                    display: "flex", alignItems: "center", gap: 8, width: "100%",
                    padding: "7px 12px", border: "none", background: "transparent",
                    cursor: "pointer", fontSize: 12, color: "#6b7280", textAlign: "left",
                  }}
                    onMouseEnter={(e) => { e.currentTarget.style.background = "rgba(0,0,0,0.04)"; }}
                    onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                  >
                    All Apps
                  </button>
                )}

                {/* App list */}
                {filteredApps.length === 0 ? (
                  <p style={{ fontSize: 12, color: "#9ca3af", padding: "12px", textAlign: "center" }}>No apps found</p>
                ) : (
                  filteredApps.map((app) => (
                    <button key={app} onClick={() => { setAppFilter(app); setAppsDropdownOpen(false); setAppSearchQuery(""); setTab("apps"); }} style={{
                      display: "flex", alignItems: "center", gap: 8, width: "100%",
                      padding: "7px 12px", border: "none",
                      background: appFilter === app ? "rgba(0,0,0,0.04)" : "transparent",
                      cursor: "pointer", fontSize: 12, color: "#1d1d1f", textAlign: "left",
                    }}
                      onMouseEnter={(e) => { e.currentTarget.style.background = "rgba(0,0,0,0.04)"; }}
                      onMouseLeave={(e) => { e.currentTarget.style.background = appFilter === app ? "rgba(0,0,0,0.04)" : "transparent"; }}
                    >
                      <img
                        src={`http://127.0.0.1:9457/app-icon/${encodeURIComponent(app)}`}
                        alt=""
                        style={{ width: 20, height: 20, borderRadius: 4, objectFit: "cover" }}
                        onError={(e) => { (e.target as HTMLImageElement).style.display = "none"; }}
                      />
                      <span style={{
                        width: 7, height: 7, borderRadius: "50%",
                        background: getAppColor(app), flexShrink: 0,
                      }} />
                      <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {getAppShort(app)}
                      </span>
                    </button>
                  ))
                )}
              </div>
            </div>
          )}
        </div>

        {/* Meetings tab */}
        <button onClick={() => handleTabClick("meetings")} style={{
          display: "flex", alignItems: "center", gap: 5, padding: "6px 14px",
          borderRadius: 100, border: "none", cursor: "pointer",
          background: tab === "meetings" ? "rgba(0,0,0,0.06)" : "transparent",
          color: tab === "meetings" ? "#1d1d1f" : "#86868b",
          fontSize: 12, fontWeight: 500, transition: "background 0.15s",
        }}>
          <MeetingIcon size={12} />
          <span>Meetings</span>
        </button>

        {/* Starred tab */}
        <button onClick={() => handleTabClick("starred")} style={{
          display: "flex", alignItems: "center", gap: 5, padding: "6px 14px",
          borderRadius: 100, border: "none", cursor: "pointer",
          background: tab === "starred" ? "rgba(0,0,0,0.06)" : "transparent",
          color: tab === "starred" ? "#1d1d1f" : "#86868b",
          fontSize: 12, fontWeight: 500, transition: "background 0.15s",
        }}>
          <StarIcon size={11} />
          <span>Starred</span>
        </button>

        {/* Active app filter badge */}
        {appFilter && tab === "apps" && (
          <button onClick={() => setAppFilter(null)} style={{
            display: "flex", alignItems: "center", gap: 4, padding: "4px 10px",
            borderRadius: 100, border: "none", cursor: "pointer",
            background: "rgba(0,0,0,0.04)", fontSize: 11, color: "#1d1d1f",
            marginLeft: 2,
          }}>
            <span style={{ width: 6, height: 6, borderRadius: "50%", background: getAppColor(appFilter) }} />
            {getAppShort(appFilter)}
            <CloseIcon size={7} />
          </button>
        )}
      </div>

      {/* Results area */}
      <div style={{ flex: 1, overflowY: "auto", padding: "4px 12px 8px" }}>
        {tab === "starred" ? (
          <div style={{ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", padding: "40px 20px", gap: 8 }}>
            <span style={{ color: "#d1d5db" }}><StarIcon size={28} /></span>
            <p style={{ fontSize: 13, color: "#9ca3af", textAlign: "center" }}>No starred items</p>
            <p style={{ fontSize: 11, color: "#c4c4c6", textAlign: "center" }}>Star important moments to find them quickly</p>
          </div>
        ) : tab === "meetings" && !query.trim() ? (
          /* Show meeting sessions — one card per session, full transcript inside */
          meetingSessions.length === 0 ? (
            <p style={{ textAlign: "center", color: "#9ca3af", fontSize: 13, padding: 24 }}>No meeting sessions today</p>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              {meetingSessions.map((session, i) => {
                const fmtTime = (ts: string) => ts.length >= 16 ? ts.slice(11, 16) : ts;
                const durMin = Math.round(session.duration / 60);
                const previewText = session.fullTranscript.length > 400
                  ? session.fullTranscript.slice(0, 400) + "…"
                  : session.fullTranscript;
                return (
                  <div key={i} style={{
                    background: "#fff", borderRadius: 14, border: "1px solid rgba(0,0,0,0.06)",
                    overflow: "hidden", padding: "14px 16px",
                  }}>
                    {/* Session header */}
                    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
                      <span style={{ fontSize: 12, fontWeight: 600, color: "#1d1d1f" }}>
                        {fmtTime(session.startTime)} — {fmtTime(session.endTime)}
                      </span>
                      <span style={{ fontSize: 11, color: "#86868b" }}>·</span>
                      <span style={{ fontSize: 11, color: "#86868b" }}>{durMin} min</span>
                      <span style={{ fontSize: 11, color: "#86868b" }}>·</span>
                      <span style={{ fontSize: 11, color: "#86868b" }}>{session.segmentCount} segments</span>
                    </div>
                    {/* Full transcript */}
                    <div style={{ fontSize: 12, color: "#374151", lineHeight: 1.6, maxHeight: 200, overflowY: "auto" }}>
                      {previewText.split(" | ").map((line, j) => (
                        <div key={j} style={{ marginBottom: 4 }}>{line}</div>
                      ))}
                    </div>
                  </div>
                );
              })}
            </div>
          )
        ) : loading ? (
          <p style={{ textAlign: "center", color: "#9ca3af", fontSize: 13, padding: 24 }}>Searching...</p>
        ) : !query.trim() ? (
          <p style={{ textAlign: "center", color: "#9ca3af", fontSize: 13, padding: 24 }}>Type to search</p>
        ) : filtered.length === 0 ? (
          <p style={{ textAlign: "center", color: "#9ca3af", fontSize: 13, padding: 24 }}>No results</p>
        ) : (
          Array.from(groupedByDate.entries()).map(([date, frames]) => (
            <div key={date} style={{ marginBottom: 12 }}>
              <p style={{ fontSize: 11, fontWeight: 600, color: "#9ca3af", padding: "4px 4px 6px", textTransform: "uppercase", letterSpacing: "0.03em" }}>
                {date}
              </p>
              <div style={{
                display: "grid",
                gridTemplateColumns: tab === "meetings"
                  ? "repeat(auto-fill, minmax(200px, 1fr))"
                  : "repeat(auto-fill, minmax(200px, 1fr))",
                gap: 8,
              }}>
                {frames.map((frame, i) => (
                  tab === "meetings"
                    ? <TranscriptCard key={`${frame.id}-${i}`} frame={frame} onClick={() => onSelectFrame(frame, query, searchRegions[frame.id] || [])} />
                    : <ScreenshotHighlightCard key={`${frame.id}-${i}`} frame={frame} query={query} regions={searchRegions[frame.id] || []} onClick={() => onSelectFrame(frame, query, searchRegions[frame.id] || [])} />
                ))}
              </div>
            </div>
          ))
        )}
      </div>

      {/* Bottom bar */}
      <div style={{
        display: "flex", justifyContent: "space-between", alignItems: "center",
        padding: "6px 16px", borderTop: "1px solid rgba(0,0,0,0.04)",
        fontSize: 11, color: "#9ca3af",
      }}>
        <span>{filtered.length} results</span>
        <span>
          {new Date().toLocaleDateString("en-US", { month: "short", day: "numeric" })}{" "}
          {new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
        </span>
      </div>
    </div>
  );
}

// === Screenshot card with yellow highlight boxes on matching text ===
function ScreenshotHighlightCard({ frame, query, regions = [], onClick }: {
  frame: CapturedFrame;
  query: string;
  regions?: Array<{text:string,x:number,y:number,w:number,h:number}>;
  onClick: () => void;
}) {
  const [thumb, setThumb] = useState<string | null>(null);
  const imgRef = useRef<HTMLImageElement>(null);

  useEffect(() => {
    setThumb(`http://127.0.0.1:9457/frames/${frame.id}`);
  }, [frame.id]);

  // Filter matches, skip boxes >15% of frame, show top 3 smallest
  const matching = query.trim()
    ? regions
        .filter((r) => r.text.toLowerCase().includes(query.toLowerCase()))
        .filter((r) => r.w * r.h < 0.15)
        .sort((a, b) => (a.w * a.h) - (b.w * b.h))
        .slice(0, 3)
    : [];

  const time = new Date(frame.timestamp / 1000);

  return (
    <div onClick={onClick} style={{
      background: "#fff", borderRadius: 12,
      overflow: "hidden", cursor: "pointer", transition: "transform 0.2s, box-shadow 0.2s",
      boxShadow: "0 1px 4px rgba(0,0,0,0.06), 0 0 0 0.5px rgba(0,0,0,0.03)",
    }}
      onMouseEnter={(e) => { e.currentTarget.style.transform = "translateY(-2px)"; e.currentTarget.style.boxShadow = "0 6px 20px rgba(0,0,0,0.12)"; }}
      onMouseLeave={(e) => { e.currentTarget.style.transform = "none"; e.currentTarget.style.boxShadow = "0 1px 4px rgba(0,0,0,0.06), 0 0 0 0.5px rgba(0,0,0,0.03)"; }}
    >
      {/* Screenshot with highlights */}
      <div style={{ position: "relative", aspectRatio: "16/10", overflow: "hidden", background: "#f3f4f6" }}>
        {thumb ? (
          <>
            <img ref={imgRef} src={thumb} alt=""
              style={{ width: "100%", height: "100%", objectFit: "cover" }}
            />
            {matching.map((r, i) => (
              <div key={i} style={{
                position: "absolute",
                left: `${r.x * 100}%`, top: `${r.y * 100}%`,
                width: `${Math.max(r.w * 100, 3)}%`, height: `${Math.max(r.h * 100, 2)}%`,
                background: "rgba(251, 191, 36, 0.5)",
                border: "2.5px solid rgba(251, 191, 36, 1)",
                borderRadius: 3, pointerEvents: "none",
                boxShadow: "0 0 8px rgba(251, 191, 36, 0.6), 0 0 16px rgba(251, 191, 36, 0.3)",
              }} />
            ))}
          </>
        ) : (
          <div style={{ width: "100%", height: "100%", background: "rgba(0,0,0,0.03)" }} />
        )}
      </div>

      {/* Footer */}
      <div style={{ padding: "6px 10px", display: "flex", alignItems: "center", gap: 6 }}>
        <span style={{
          width: 7, height: 7, borderRadius: "50%",
          background: getAppColor(frame.app_name), display: "inline-block", flexShrink: 0,
        }} />
        <span style={{
          fontSize: 11, color: "#374151", fontWeight: 500, flex: 1,
          overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap",
        }}>
          {frame.window_name || getAppShort(frame.app_name)}
        </span>
        <span style={{ fontSize: 10, color: "#9ca3af", whiteSpace: "nowrap" }}>
          {time.toLocaleDateString("en-US", { month: "short", day: "numeric" })}{" "}
          {time.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
        </span>
      </div>
    </div>
  );
}

// === Transcript card for Meetings tab ===
function TranscriptCard({ frame, onClick }: { frame: CapturedFrame; onClick: () => void }) {
  const time = new Date(frame.timestamp / 1000);
  const text = frame.ocr_text.split("\n---REGIONS---\n")[0];

  return (
    <div onClick={onClick} style={{
      background: "#fff", borderRadius: 12,
      overflow: "hidden", cursor: "pointer", transition: "box-shadow 0.15s, transform 0.15s",
      boxShadow: "0 1px 4px rgba(0,0,0,0.06), 0 0 0 0.5px rgba(0,0,0,0.03)",
    }}
      onMouseEnter={(e) => { e.currentTarget.style.boxShadow = "0 4px 16px rgba(0,0,0,0.1)"; e.currentTarget.style.transform = "translateY(-1px)"; }}
      onMouseLeave={(e) => { e.currentTarget.style.boxShadow = "0 1px 4px rgba(0,0,0,0.06), 0 0 0 0.5px rgba(0,0,0,0.03)"; e.currentTarget.style.transform = "none"; }}
    >
      <div style={{
        padding: "10px 12px", minHeight: 80, fontSize: 12,
        color: "#374151", lineHeight: 1.5, overflow: "hidden", maxHeight: 100,
      }}>
        {text.slice(0, 150) || "No transcript"}
      </div>
      <div style={{
        padding: "6px 10px", display: "flex", alignItems: "center", gap: 6,
        borderTop: "1px solid rgba(0,0,0,0.04)",
      }}>
        <span style={{ color: "#9ca3af", display: "flex" }}>
          <MeetingIcon size={10} />
        </span>
        <span style={{ fontSize: 10, color: "#86868b", flex: 1 }}>Transcript</span>
        <span style={{ fontSize: 10, color: "#9ca3af" }}>
          {time.toLocaleDateString("en-US", { month: "short", day: "numeric" })}{" "}
          {time.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
        </span>
      </div>
    </div>
  );
}

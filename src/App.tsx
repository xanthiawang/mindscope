import { useState, useEffect, useCallback, useRef } from "react";
import SearchPanel from "./components/SearchPanel";
// MeetingBar removed — meeting transcript is now integrated into the AI panel
// import DetailView from "./components/DetailView";
import SettingsPanel from "./components/SettingsPanel";
import type { CapturedFrame } from "./lib/types";
import { checkPermission, openPermissionSettings, startRecording, isRecording, getTimeline, getDailyBrief, hideWindow, expandBar, collapseBar } from "./lib/commands";
import { getAppColor, getAppShort } from "./lib/appColors";
import "./styles/globals.css";

// --- SVG Icons (Apple SF-style, no emoji) ---
const SearchIcon = () => (
  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><circle cx="11" cy="11" r="7" /><path d="M21 21l-4.35-4.35" /></svg>
);
const RewindIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><polygon points="11 19 2 12 11 5" /><polygon points="22 19 13 12 22 5" /></svg>
);
const GearIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>
);
const SparkleIcon = () => (
  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M12 2l2.4 7.2L22 12l-7.6 2.8L12 22l-2.4-7.2L2 12l7.6-2.8z" /></svg>
);
const LockIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
);
const BriefIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/>
    <polyline points="14 2 14 8 20 8"/>
    <line x1="16" y1="13" x2="8" y2="13"/>
    <line x1="16" y1="17" x2="8" y2="17"/>
  </svg>
);

const TranscriptIcon = () => (
  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
    <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>
  </svg>
);
const CopyIcon = () => (
  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
    <rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
  </svg>
);
const MicIcon = () => (
  <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
    <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/>
  </svg>
);
const SpeakerSmIcon = () => (
  <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/>
  </svg>
);

type Mode = "bar" | "rewind";

function App() {
  const [mode, setMode] = useState<Mode>("bar");
  const [date, setDate] = useState(() => {
    const now = new Date();
    const y = now.getFullYear();
    const m = String(now.getMonth() + 1).padStart(2, "0");
    const d = String(now.getDate()).padStart(2, "0");
    return `${y}-${m}-${d}`;
  });
  const [frames, setFrames] = useState<CapturedFrame[]>([]);
  const [frameIndex, setFrameIndex] = useState(0);
  const loadingRef = useRef(false); // prevent concurrent date loads
  const [_selectedResult, _setSelectedResult] = useState<CapturedFrame | null>(null);
  const [highlightQuery, setHighlightQuery] = useState("");
  const [highlightRegions, setHighlightRegions] = useState<Array<{text:string,x:number,y:number,w:number,h:number}>>([]);
  const [showSettings, setShowSettings] = useState(false);
  const [aiMessages, setAiMessages] = useState<Array<{role: string, text: string}>>([]);
  const [aiInput, setAiInput] = useState("");
  const [hasPermission, setHasPermission] = useState<boolean | null>(null);
  const [searchText, setSearchText] = useState("");
  const [briefText, setBriefText] = useState("");
  const [showBrief, setShowBrief] = useState(false);
  const [showAI, setShowAI] = useState(false);
  const [showDatePicker, setShowDatePicker] = useState(false);
  const [pickerMonth, setPickerMonth] = useState(() => new Date());
  const [aiTab, setAiTab] = useState<"chat" | "transcript">("chat");
  const [meetingActive, setMeetingActive] = useState(false);
  const [meetingTranscripts, setMeetingTranscripts] = useState<Array<{time: string, speaker: string, text: string}>>([]);
  const [latestTranscript, setLatestTranscript] = useState("");
  const meetingScrollRef = useRef<HTMLDivElement>(null);
  const timelineRef = useRef<HTMLDivElement>(null);

  const currentFrame = frames[frameIndex] ?? null;

  // Load previous day when scrubbing past the left edge
  const loadPrevDay = useCallback(async () => {
    if (loadingRef.current) return;
    loadingRef.current = true;
    const d = new Date(date + "T12:00:00");
    d.setDate(d.getDate() - 1);
    const prevDate = `${d.getFullYear()}-${String(d.getMonth()+1).padStart(2,"0")}-${String(d.getDate()).padStart(2,"0")}`;
    const prevFrames = await getTimeline(prevDate);
    if (prevFrames.length > 0) {
      setDate(prevDate);
      setFrames(prevFrames);
      setFrameIndex(prevFrames.length - 1); // start at the end (most recent of that day)
    }
    loadingRef.current = false;
  }, [date]);

  // Load next day when scrubbing past the right edge
  const loadNextDay = useCallback(async () => {
    if (loadingRef.current) return;
    const now = new Date();
    const today = `${now.getFullYear()}-${String(now.getMonth()+1).padStart(2,"0")}-${String(now.getDate()).padStart(2,"0")}`;
    if (date >= today) { loadingRef.current = false; return; } // can't go past today
    loadingRef.current = true;
    const d = new Date(date + "T12:00:00");
    d.setDate(d.getDate() + 1);
    const nextDate = `${d.getFullYear()}-${String(d.getMonth()+1).padStart(2,"0")}-${String(d.getDate()).padStart(2,"0")}`;
    const nextFrames = await getTimeline(nextDate);
    if (nextFrames.length > 0) {
      setDate(nextDate);
      setFrames(nextFrames);
      setFrameIndex(0); // start at the beginning
    }
    loadingRef.current = false;
  }, [date]);

  const bgImageUrl = (() => {
    if (!currentFrame) return null;
    return `http://127.0.0.1:9457/frames/${currentFrame.id}`;
  })();

  // Permission check
  useEffect(() => {
    const check = () => checkPermission().then(setHasPermission).catch(() => {});
    check();
    const t = setInterval(check, 5000);
    return () => clearInterval(t);
  }, []);

  useEffect(() => {
    if (hasPermission) {
      isRecording().then((r) => { if (!r) startRecording(); });
    }
  }, [hasPermission]);

  useEffect(() => {
    if (!hasPermission) return;
    getTimeline(date).then((f) => {
      setFrames(f);
      if (f.length > 0) setFrameIndex(f.length - 1);
    });
  }, [date, hasPermission]);

  useEffect(() => {
    if (!hasPermission) return;
    const t = setInterval(() => {
      getTimeline(date).then((f) => {
        if (f.length !== frames.length) {
          setFrames(f);
          // Auto-follow latest if user was near the end
          setFrameIndex((prev) => prev >= frames.length - 3 ? f.length - 1 : prev);
        }
      });
    }, 3000);
    return () => clearInterval(t);
  }, [date, hasPermission]);

  const segments = (() => {
    if (frames.length === 0) return [];
    const result: { app: string; startIdx: number; endIdx: number; color: string }[] = [];
    let curApp = frames[0].app_name;
    let start = 0;
    for (let i = 1; i <= frames.length; i++) {
      const app = i < frames.length ? frames[i].app_name : "";
      if (app !== curApp || i === frames.length) {
        result.push({ app: curApp, startIdx: start, endIdx: i - 1, color: getAppColor(curApp) });
        curApp = app;
        start = i;
      }
    }
    return result;
  })();

  const scrubberPos = frames.length > 1 ? (frameIndex / (frames.length - 1)) * 100 : 50;

  const formatFullTime = (ts: number) => {
    const d = new Date(ts / 1000);
    return d.toLocaleDateString("en-US", { month: "short", day: "numeric" }) + " " +
      d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  const timeLabel = (() => {
    if (!currentFrame) return "";
    const diffSec = Math.floor((Date.now() - currentFrame.timestamp / 1000) / 1000);
    if (diffSec < 10) return "Now";
    if (diffSec < 60) return `${diffSec}s ago`;
    if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
    return formatFullTime(currentFrame.timestamp);
  })();

  // Rewind always shows full date+time
  const rewindTimeLabel = currentFrame ? formatFullTime(currentFrame.timestamp) : "";

  const handleTimelineInteraction = useCallback((clientX: number) => {
    if (!timelineRef.current || frames.length === 0) return;
    const rect = timelineRef.current.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
    setFrameIndex(Math.round(ratio * (frames.length - 1)));
    if (mode === "bar") setMode("rewind");
  }, [frames.length, mode]);

  // AI query
  const handleAiQuery = useCallback(async (question: string) => {
    setAiMessages((prev) => [...prev, { role: "user", text: question }]);
    setAiInput("");
    setShowAI(true);
    setAiMessages((prev) => [...prev, { role: "ai", text: "Searching..." }]);

    const keywords = question.toLowerCase()
      .replace(/[?!.,]/g, "").split(" ")
      .filter((w) => w.length > 2 && !["what","was","the","how","when","where","which","that","this","with","from","have","been","doing"].includes(w));
    const sq = keywords.slice(0, 3).join(" ") || question;

    let context = "";
    try {
      const searchResp = await fetch(`http://127.0.0.1:9457/search?q=${encodeURIComponent(sq)}&limit=15`);
      const searchData = await searchResp.json();
      const searchResults = searchData.results || [];
      const timelineResp = await fetch(`http://127.0.0.1:9457/timeline?date=${date}`);
      const timelineData: any[] = await timelineResp.json();
      const recentFrames = timelineData.slice(-30);
      const seen = new Set<number>();
      const allFrames: any[] = [];
      for (const r of searchResults) { if (!seen.has(r.id)) { seen.add(r.id); allFrames.push(r); } }
      for (const f of recentFrames) { if (!seen.has(f.id)) { seen.add(f.id); allFrames.push(f); } }
      allFrames.sort((a, b) => a.timestamp - b.timestamp);
      if (allFrames.length > 0) {
        let lastApp = "";
        const lines: string[] = [];
        for (const r of allFrames.slice(-40)) {
          const time = new Date(r.timestamp / 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
          const text = (r.text || r.ocr_text || "").split("\n---REGIONS---\n")[0].slice(0, 150);
          if (r.app_name !== lastApp) { lines.push(`[${time}] ${r.app_name} — ${r.window_name || ""}`); lastApp = r.app_name; }
          if (text && text.length > 10) lines.push(`  ${text}`);
        }
        context = lines.join("\n");
      }
    } catch {}

    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const prompt = context
        ? `Screen activity log:\n${context}\n\nQ: ${question}\n\nRules: Answer in 1-2 short sentences ONLY. Plain text, no markdown, no bullet points, no bold. Be direct. No filler like "Based on your screen activity". Just state what happened.`
        : `Q: ${question}\n\nNo screen data found. Say so in one short sentence. No markdown.`;
      let response: string = await invoke("ask_ai", { prompt });
      // Strip markdown formatting
      response = response.replace(/\*\*(.*?)\*\*/g, "$1").replace(/^[-*] /gm, "").replace(/^#+\s/gm, "").trim();
      setAiMessages((prev) => {
        const filtered = prev.filter((m) => m.text !== "Searching...");
        return [...filtered, { role: "ai", text: response }];
      });
    } catch {
      if (context) {
        setAiMessages((prev) => {
          const filtered = prev.filter((m) => m.text !== "Searching...");
          return [...filtered, { role: "ai", text: context }];
        });
      } else {
        setAiMessages((prev) => {
          const filtered = prev.filter((m) => m.text !== "Searching...");
          return [...filtered, { role: "ai", text: `No data found.` }];
        });
      }
    }
  }, [date]);

  // Poll meeting status every 5s (always active)
  useEffect(() => {
    const poll = async () => {
      try {
        const resp = await fetch("http://127.0.0.1:9457/meeting/status");
        const data = await resp.json();
        setMeetingActive(!!data.active);
        const transcripts = data.recent_transcripts || [];
        setMeetingTranscripts(transcripts);
        if (transcripts.length > 0) {
          const last = transcripts[transcripts.length - 1];
          setLatestTranscript(`${last.speaker}: ${last.text}`);
        }
      } catch {
        setMeetingActive(false);
      }
    };
    poll();
    const t = setInterval(poll, 5000);
    return () => clearInterval(t);
  }, []);

  // Auto-scroll meeting transcript
  useEffect(() => {
    if (meetingScrollRef.current) {
      meetingScrollRef.current.scrollTop = meetingScrollRef.current.scrollHeight;
    }
  }, [meetingTranscripts]);

  // Meeting-aware AI query helper — prepends transcript context when meeting is active
  const handleMeetingAwareAiQuery = useCallback(async (question: string) => {
    if (meetingActive && meetingTranscripts.length > 0) {
      const transcriptContext = meetingTranscripts
        .map((t) => `[${t.time}] ${t.speaker}: ${t.text}`)
        .join("\n");
      const augmented = `[Meeting transcript context]\n${transcriptContext}\n\n${question}`;
      handleAiQuery(augmented);
    } else {
      handleAiQuery(question);
    }
  }, [meetingActive, meetingTranscripts, handleAiQuery]);

  const [showSearch, setShowSearch] = useState(false);
  const handleSearchSubmit = useCallback(() => { setShowSearch(true); }, []);

  // Resize window: bar=70px at bottom, rewind=fullscreen
  useEffect(() => {
    import("./lib/commands").then(({ resizeToBar, resizeToFullscreen }) => {
      if (mode === "bar") resizeToBar().catch(() => {});
      else resizeToFullscreen().catch(() => {});
    });
  }, [mode]);

  // Expand/collapse window when panels open/close
  const anyPanelOpen = showBrief || showAI || showSearch || showSettings || showDatePicker;
  useEffect(() => {
    if (mode !== "bar") return;
    if (anyPanelOpen) {
      expandBar().catch(() => {});
    } else {
      collapseBar().catch(() => {});
    }
  }, [anyPanelOpen, mode]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        // One press does everything: close panels + reset mode + hide window
        setShowBrief(false); setShowAI(false); setShowSettings(false); setShowSearch(false); setShowDatePicker(false);
        if (mode !== "bar") setMode("bar");
        hideWindow().catch(() => {});
        return;
      }
      if (mode === "bar" || mode === "rewind") {
        if (e.key === "ArrowLeft") {
          setFrameIndex((p) => { if (p <= 0) { loadPrevDay(); return 0; } return p - 1; });
        }
        if (e.key === "ArrowRight") {
          setFrameIndex((p) => { if (p >= frames.length - 1) { loadNextDay(); return frames.length - 1; } return p + 1; });
        }
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [mode, frames.length]);

  // === Permission screen ===
  if (hasPermission === false) {
    return (
      <div style={{ width: "100vw", height: "100vh", background: "transparent", pointerEvents: "none" }}>
        <div style={{
          position: "fixed", bottom: 58, left: 20, right: 20, height: 80,
          background: "rgba(255,255,255,0.92)", backdropFilter: "blur(40px)",
          borderRadius: 20, display: "flex", alignItems: "center", justifyContent: "center", gap: 14,
          boxShadow: "0 4px 24px rgba(0,0,0,0.08), 0 0 0 0.5px rgba(0,0,0,0.06)",
          pointerEvents: "auto",
        }}>
          <LockIcon />
          <div>
            <p style={{ color: "#1d1d1f", fontSize: 13, fontWeight: 600 }}>Screen Recording Permission Required</p>
            <p style={{ color: "#86868b", fontSize: 11 }}>Enable MindScope in System Settings</p>
          </div>
          <button onClick={() => openPermissionSettings()} style={{
            background: "#007AFF", color: "white", border: "none", borderRadius: 100,
            padding: "7px 18px", fontSize: 12, fontWeight: 600, cursor: "pointer",
          }}>Open Settings</button>
        </div>
      </div>
    );
  }

  // Search is inline in bar mode via showSearch

  // === Auto-load brief when toggled ===
  useEffect(() => {
    if (showBrief && !briefText) {
      getDailyBrief().then(setBriefText).catch(() => setBriefText("Unable to load brief."));
    }
  }, [showBrief, briefText]);

  // === AI Chat (resizable from any edge/corner) ===
  // chatBox uses left/top positioning (simpler coordinate math)
  const [chatBox, setChatBox] = useState({ left: window.innerWidth - 400, top: window.innerHeight - 540, w: 380, h: 440 });
  const chatDragRef = useRef<{ edge: string; startX: number; startY: number; startBox: typeof chatBox } | null>(null);

  const clampChat = (b: typeof chatBox) => ({
    left: b.left,
    top: b.top,
    w: Math.max(280, Math.min(700, b.w)),
    h: Math.max(200, Math.min(window.innerHeight * 0.8, b.h)),
  });

  const startChatResize = (edge: string) => (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const start = { edge, startX: e.clientX, startY: e.clientY, startBox: { ...chatBox } };
    chatDragRef.current = start;

    const onMove = (ev: MouseEvent) => {
      if (!chatDragRef.current) return;
      const dx = ev.clientX - start.startX;
      const dy = ev.clientY - start.startY;
      const s = start.startBox;
      const next = { ...s };

      // left edge: move left side, width changes inversely
      if (edge.includes("l")) { next.left = s.left + dx; next.w = s.w - dx; }
      // right edge: just change width
      if (edge.includes("r")) { next.w = s.w + dx; }
      // top edge: move top, height changes inversely
      if (edge.includes("t")) { next.top = s.top + dy; next.h = s.h - dy; }
      // bottom edge: just change height
      if (edge.includes("b")) { next.h = s.h + dy; }

      setChatBox(clampChat(next));
    };
    const onUp = () => { chatDragRef.current = null; window.removeEventListener("mousemove", onMove); window.removeEventListener("mouseup", onUp); };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const edgeStyle = (cursor: string, pos: React.CSSProperties): React.CSSProperties => ({
    position: "absolute", ...pos, zIndex: 50, cursor, pointerEvents: "auto",
  });

  // Dead code — AI is now inline in bar mode via showAI
  if (false) {
    return (
      <div style={{ width: "100vw", height: "100vh", background: "transparent", pointerEvents: "none" }}>
        <div style={{
          position: "fixed", left: chatBox.left, top: chatBox.top,
          width: Math.max(280, Math.min(700, chatBox.w)),
          height: Math.max(200, Math.min(window.innerHeight * 0.8, chatBox.h)),
          background: "rgba(255,255,255,0.95)", backdropFilter: "blur(40px)",
          borderRadius: 20, boxShadow: "0 8px 40px rgba(0,0,0,0.12), 0 0 0 0.5px rgba(0,0,0,0.06)",
          display: "flex", flexDirection: "column", overflow: "hidden",
          pointerEvents: "auto",
        }}>
          {/* Resize edges */}
          <div onMouseDown={startChatResize("l")} style={edgeStyle("ew-resize", { left: -3, top: 10, bottom: 10, width: 6 })} />
          <div onMouseDown={startChatResize("r")} style={edgeStyle("ew-resize", { right: -3, top: 10, bottom: 10, width: 6 })} />
          <div onMouseDown={startChatResize("t")} style={edgeStyle("ns-resize", { top: -3, left: 10, right: 10, height: 6 })} />
          <div onMouseDown={startChatResize("b")} style={edgeStyle("ns-resize", { bottom: -3, left: 10, right: 10, height: 6 })} />
          {/* Corner handles */}
          <div onMouseDown={startChatResize("tl")} style={edgeStyle("nwse-resize", { top: -3, left: -3, width: 14, height: 14 })} />
          <div onMouseDown={startChatResize("tr")} style={edgeStyle("nesw-resize", { top: -3, right: -3, width: 14, height: 14 })} />
          <div onMouseDown={startChatResize("bl")} style={edgeStyle("nesw-resize", { bottom: -3, left: -3, width: 14, height: 14 })} />
          <div onMouseDown={startChatResize("br")} style={edgeStyle("nwse-resize", { bottom: -3, right: -3, width: 14, height: 14 })} />
          {/* Header */}
          <div style={{ padding: "14px 18px", borderBottom: "1px solid rgba(0,0,0,0.06)", display: "flex", alignItems: "center", gap: 8 }}>
            <SparkleIcon />
            <span style={{ fontSize: 14, fontWeight: 600, color: "#1d1d1f", flex: 1 }}>Ask MindScope</span>
            <button onClick={() => setMode("bar")} style={{
              background: "rgba(0,0,0,0.05)", border: "none", borderRadius: 100, width: 24, height: 24,
              cursor: "pointer", fontSize: 11, color: "#86868b", display: "flex", alignItems: "center", justifyContent: "center",
            }}>
              <svg width="10" height="10" viewBox="0 0 10 10" stroke="currentColor" strokeWidth="1.5"><path d="M1 1l8 8M9 1l-8 8" /></svg>
            </button>
          </div>
          {/* Messages */}
          <div style={{ flex: 1, overflowY: "auto", padding: 14, display: "flex", flexDirection: "column", gap: 8 }}>
            {aiMessages.length === 0 && (
              <div style={{ textAlign: "center", padding: 20, color: "#86868b" }}>
                <p style={{ fontSize: 13, marginBottom: 14 }}>Ask about your screen history</p>
                {["What was I working on?", "Summarize my activity", "Find meeting notes"].map((q) => (
                  <button key={q} onClick={() => handleAiQuery(q)} style={{
                    display: "block", width: "100%", textAlign: "left", background: "rgba(0,0,0,0.03)",
                    border: "none", borderRadius: 10, padding: "9px 14px", marginBottom: 6,
                    cursor: "pointer", fontSize: 12, color: "#1d1d1f",
                  }}>{q}</button>
                ))}
              </div>
            )}
            {aiMessages.map((msg, i) => (
              <div key={i} style={{
                alignSelf: msg.role === "user" ? "flex-end" : "flex-start",
                maxWidth: "85%", padding: "9px 14px", borderRadius: 16,
                background: msg.role === "user" ? "#007AFF" : "rgba(0,0,0,0.05)",
                color: msg.role === "user" ? "white" : "#1d1d1f",
                fontSize: 13, lineHeight: 1.5, whiteSpace: "pre-wrap",
              }}>{msg.text}</div>
            ))}
          </div>
          {/* Input */}
          <div style={{ padding: "10px 14px", borderTop: "1px solid rgba(0,0,0,0.06)", display: "flex", gap: 8 }}>
            <input type="text" value={aiInput} onChange={(e) => setAiInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && aiInput.trim()) handleAiQuery(aiInput.trim());
                if (e.key === "Escape") setMode("bar");
              }}
              autoFocus placeholder="Ask anything..."
              style={{ flex: 1, border: "1px solid rgba(0,0,0,0.08)", borderRadius: 100, padding: "7px 14px", fontSize: 13, outline: "none", background: "rgba(0,0,0,0.02)" }}
            />
          </div>
        </div>
      </div>
    );
  }

  // Detail view is now shown inline when selectedResult is set

  // === Rewind mode ===
  if (mode === "rewind") {
    return (
      <div style={{ width: "100vw", height: "100vh", position: "relative", cursor: "default", background: "#000" }}
        onClick={() => { setMode("bar"); setHighlightQuery(""); setHighlightRegions([]); }}
      >
        <div style={{ position: "absolute", inset: 0, background: "#000" }} />
        {bgImageUrl && <img src={bgImageUrl} alt="" style={{ position: "absolute", inset: 0, width: "100%", height: "100%", objectFit: "cover", filter: "brightness(0.9)", zIndex: 1 }} />}

        {/* Search highlight boxes on screenshot */}
        {highlightQuery && highlightRegions.length > 0 && (() => {
          const matching = highlightRegions
            .filter((r) => r.text.toLowerCase().includes(highlightQuery.toLowerCase()))
            .filter((r) => r.w * r.h < 0.15)
            .slice(0, 5);
          return matching.map((r, i) => (
            <div key={i} style={{
              position: "absolute",
              left: `${r.x * 100}%`, top: `${r.y * 100}%`,
              width: `${Math.max(r.w * 100, 2)}%`, height: `${Math.max(r.h * 100, 1.5)}%`,
              background: "rgba(251, 191, 36, 0.35)",
              border: "2px solid rgba(251, 191, 36, 0.9)",
              borderRadius: 4, pointerEvents: "none", zIndex: 3,
              boxShadow: "0 0 12px rgba(251, 191, 36, 0.5)",
            }} />
          ));
        })()}

        {/* Gradient */}
        <div style={{ position: "absolute", bottom: 0, left: 0, right: 0, height: 180, background: "linear-gradient(to top, rgba(0,0,0,0.6), transparent)", pointerEvents: "none", zIndex: 2 }} />

        {/* App info pill */}
        {currentFrame && (
          <div style={{ position: "absolute", bottom: 130, left: "50%", transform: "translateX(-50%)", zIndex: 10 }}>
            <div style={{
              background: "rgba(255,255,255,0.15)", backdropFilter: "blur(20px)",
              borderRadius: 100, padding: "4px 14px", display: "flex", alignItems: "center", gap: 6,
            }}>
              <span style={{ width: 7, height: 7, borderRadius: 4, background: getAppColor(currentFrame.app_name), display: "inline-block" }} />
              <span style={{ color: "rgba(255,255,255,0.9)", fontSize: 12, fontWeight: 500 }}>
                {getAppShort(currentFrame.app_name)}
              </span>
              {currentFrame.window_name && <span style={{ color: "rgba(255,255,255,0.45)", fontSize: 12 }}>{currentFrame.window_name.slice(0, 40)}</span>}
            </div>
          </div>
        )}

        {/* Time pill */}
        <div style={{ position: "absolute", bottom: 108, left: `${scrubberPos}%`, transform: "translateX(-50%)", zIndex: 20 }}>
          <div className="time-bubble">{rewindTimeLabel}</div>
        </div>

        {/* Timeline */}
        <div onClick={(e) => e.stopPropagation()} style={{ position: "absolute", bottom: 58, left: 0, right: 0, padding: "0 50px 16px", zIndex: 20 }}>
          <BottomTimeline timelineRef={timelineRef} segments={segments} frames={frames} scrubberPos={scrubberPos} onInteraction={handleTimelineInteraction}
            onFrameStep={(d) => {
              setFrameIndex((p) => {
                const next = p + d;
                if (next < 0) { loadPrevDay(); return 0; }
                if (next >= frames.length) { loadNextDay(); return frames.length - 1; }
                return next;
              });
            }} />
        </div>
      </div>
    );
  }

  // === Bar mode — white glass, original two-row layout ===
  return (
    <div style={{ width: "100vw", height: "100vh", background: "transparent", pointerEvents: anyPanelOpen ? "auto" : "none" }}
      onClick={() => {
        // Click on transparent area above bar → close all panels
        if (anyPanelOpen) {
          setShowBrief(false); setShowAI(false); setShowSettings(false);
          setShowSearch(false); setShowDatePicker(false);
        }
      }}
    >
    <div onClick={(e) => e.stopPropagation()} style={{
      position: "fixed", bottom: 0, left: 0, right: 0, height: 70,
      background: "rgba(255, 255, 255, 0.78)",
      backdropFilter: "blur(40px) saturate(200%)",
      WebkitBackdropFilter: "blur(40px) saturate(200%)",
      borderRadius: "16px 16px 0 0",
      display: "flex", flexDirection: "column", justifyContent: "center",
      padding: "6px 16px", overflow: "visible",
      pointerEvents: "auto",
      boxShadow: "0 -1px 8px rgba(0,0,0,0.04), inset 0 0.5px 0 rgba(255,255,255,0.6)",
    }}>
      {/* Top row: search + time + controls */}
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
        {/* Search box */}
        <div style={{
          display: "flex", alignItems: "center", gap: 5,
          background: "rgba(0,0,0,0.04)", borderRadius: 100,
          padding: "4px 10px", width: 180,
        }}>
          <span style={{ color: "#aeaeb2", flexShrink: 0 }}><SearchIcon /></span>
          <input type="text" value={searchText} onChange={(e) => setSearchText(e.target.value)}
            onFocus={() => setShowSearch(true)}
            onKeyDown={(e) => { if (e.key === "Enter") handleSearchSubmit(); }}
            placeholder="Search"
            style={{ flex: 1, background: "transparent", border: "none", outline: "none", fontSize: 11, color: "#1d1d1f" }}
          />
        </div>

        {/* Time label — clickable, opens date picker */}
        <button onClick={() => setShowDatePicker(!showDatePicker)} style={{
          fontSize: 12, color: "#3a3a3c", fontWeight: 600, background: showDatePicker ? "rgba(0,0,0,0.06)" : "none",
          border: "none", cursor: "pointer", padding: "3px 8px", borderRadius: 100, display: "flex", alignItems: "center", gap: 4,
        }}>
          {timeLabel}
          <svg width="8" height="5" viewBox="0 0 8 5" fill="none" stroke="#86868b" strokeWidth="1.5" strokeLinecap="round"><path d="M1 1l3 3 3-3" /></svg>
        </button>

        {/* App name */}
        {currentFrame && (
          <div style={{ display: "flex", alignItems: "center", gap: 4, overflow: "hidden", maxWidth: 200 }}>
            <span style={{ width: 6, height: 6, borderRadius: 3, background: getAppColor(currentFrame.app_name), display: "inline-block", flexShrink: 0 }} />
            <span style={{ fontSize: 11, color: "#636366", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {getAppShort(currentFrame.app_name)}
            </span>
          </div>
        )}

        <div style={{ flex: 1 }} />

        {/* Frame count */}
        <span style={{ fontSize: 11, color: "#8e8e93" }}>{frames.length} frames</span>

        {/* Brief */}
        <button onClick={() => setShowBrief(!showBrief)} style={{
          background: showBrief ? "rgba(0,0,0,0.1)" : "rgba(0,0,0,0.04)", border: "none", borderRadius: 100, width: 26, height: 26,
          cursor: "pointer", color: "#aeaeb2", display: "flex", alignItems: "center", justifyContent: "center",
        }}><BriefIcon /></button>

        {/* AI input */}
        <div style={{
          display: "flex", alignItems: "center", gap: 5,
          background: "rgba(0,0,0,0.04)", borderRadius: 100,
          padding: "4px 10px", width: 180,
        }}>
          <span style={{ color: "#aeaeb2", flexShrink: 0 }}><SparkleIcon /></span>
          <input type="text" value={aiInput} onChange={(e) => setAiInput(e.target.value)}
            onFocus={() => setShowAI(true)}
            onKeyDown={(e) => { if (e.key === "Enter" && aiInput.trim()) handleMeetingAwareAiQuery(aiInput.trim()); }}
            placeholder="Ask"
            style={{ flex: 1, background: "transparent", border: "none", outline: "none", fontSize: 11, color: "#1d1d1f" }}
          />
        </div>

        {/* Rewind */}
        <button onClick={() => setMode("rewind")} style={{
          background: "rgba(0,0,0,0.04)", border: "none", borderRadius: 100, width: 26, height: 26,
          cursor: "pointer", color: "#aeaeb2", display: "flex", alignItems: "center", justifyContent: "center",
        }}><RewindIcon /></button>

        {/* Settings */}
        <button onClick={() => setShowSettings(true)} style={{
          background: "rgba(0,0,0,0.04)", border: "none", borderRadius: 100, width: 26, height: 26,
          cursor: "pointer", color: "#aeaeb2", display: "flex", alignItems: "center", justifyContent: "center",
        }}><GearIcon /></button>
      </div>

      {/* Rolling transcript line — shown when meeting is active */}
      {meetingActive && latestTranscript && (
        <div style={{
          fontSize: 11, color: "#3a3a3c", whiteSpace: "nowrap",
          overflow: "hidden", marginBottom: 2,
          animation: "scroll-left 20s linear infinite",
        }}>
          <span style={{ display: "inline-block", paddingLeft: "100%" }}>
            {latestTranscript}
          </span>
        </div>
      )}

      {/* Timeline track */}
      <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
        <BottomTimeline timelineRef={timelineRef} segments={segments} frames={frames} scrubberPos={scrubberPos}
          onInteraction={handleTimelineInteraction}
          onFrameStep={(d) => {
              setFrameIndex((p) => {
                const next = p + d;
                if (next < 0) { loadPrevDay(); return 0; }
                if (next >= frames.length) { loadNextDay(); return frames.length - 1; }
                return next;
              });
            }} />
      </div>

      {/* Date picker — floating above the bar */}
      {showDatePicker && (() => {
        const y = pickerMonth.getFullYear();
        const m = pickerMonth.getMonth();
        const firstDay = new Date(y, m, 1).getDay();
        const daysInMonth = new Date(y, m + 1, 0).getDate();
        const today = new Date();
        const days: (number | null)[] = [];
        for (let i = 0; i < firstDay; i++) days.push(null);
        for (let d = 1; d <= daysInMonth; d++) days.push(d);

        const jumpToDate = async (day: number, hour: number) => {
          const dateStr = `${y}-${String(m + 1).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
          try {
            const newFrames = await getTimeline(dateStr);
            if (newFrames.length > 0) {
              setFrames(newFrames);
              // Find frame closest to selected hour
              const targetTs = new Date(y, m, day, hour).getTime() * 1000;
              let closestIdx = 0;
              let minDiff = Infinity;
              newFrames.forEach((f, i) => {
                const diff = Math.abs(f.timestamp - targetTs);
                if (diff < minDiff) { minDiff = diff; closestIdx = i; }
              });
              setFrameIndex(closestIdx);
            }
          } catch {}
          setShowDatePicker(false);
        };

        return (
          <div style={{
            position: "absolute", bottom: 78, left: 180, width: 340,
            background: "rgba(30,30,32,0.95)", backdropFilter: "blur(40px)",
            borderRadius: 16, boxShadow: "0 8px 32px rgba(0,0,0,0.3)",
            overflow: "hidden", zIndex: 100, color: "white",
          }}>
            {/* Header */}
            <div style={{ padding: "14px 16px 8px", fontSize: 14, fontWeight: 600, textAlign: "center" }}>
              Jump to Date & Time
            </div>
            <div style={{ display: "flex" }}>
              {/* Calendar */}
              <div style={{ flex: 1, padding: "8px 12px 12px" }}>
                {/* Month nav */}
                <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 8 }}>
                  <button onClick={() => setPickerMonth(new Date(y, m - 1))} style={{
                    background: "rgba(255,255,255,0.1)", border: "none", borderRadius: 100, width: 28, height: 28,
                    cursor: "pointer", color: "white", fontSize: 14, display: "flex", alignItems: "center", justifyContent: "center",
                  }}>{'<'}</button>
                  <span style={{ fontSize: 15, fontWeight: 600 }}>
                    {pickerMonth.toLocaleDateString("en-US", { month: "long" })}
                  </span>
                  <button onClick={() => setPickerMonth(new Date(y, m + 1))} style={{
                    background: "rgba(255,255,255,0.1)", border: "none", borderRadius: 100, width: 28, height: 28,
                    cursor: "pointer", color: "white", fontSize: 14, display: "flex", alignItems: "center", justifyContent: "center",
                  }}>{'>'}</button>
                </div>
                {/* Day headers */}
                <div style={{ display: "grid", gridTemplateColumns: "repeat(7, 1fr)", gap: 2, textAlign: "center", marginBottom: 4 }}>
                  {["Su","Mo","Tu","We","Th","Fr","Sa"].map(d => (
                    <div key={d} style={{ fontSize: 10, color: "rgba(255,255,255,0.4)", padding: 2 }}>{d}</div>
                  ))}
                </div>
                {/* Days grid */}
                <div style={{ display: "grid", gridTemplateColumns: "repeat(7, 1fr)", gap: 2, textAlign: "center" }}>
                  {days.map((d, i) => {
                    if (d === null) return <div key={i} />;
                    const isToday = d === today.getDate() && m === today.getMonth() && y === today.getFullYear();
                    return (
                      <button key={i} onClick={() => jumpToDate(d, 12)} style={{
                        width: 30, height: 30, borderRadius: 15, border: "none",
                        background: isToday ? "#007AFF" : "transparent",
                        color: isToday ? "white" : "rgba(255,255,255,0.8)",
                        fontSize: 13, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center",
                        margin: "0 auto",
                      }}>{d}</button>
                    );
                  })}
                </div>
              </div>
              {/* Time selector */}
              <div style={{ width: 70, borderLeft: "1px solid rgba(255,255,255,0.1)", overflowY: "auto", maxHeight: 260 }}>
                {Array.from({ length: 24 }, (_, h) => (
                  <button key={h} onClick={() => {
                    const d = currentFrame ? new Date(currentFrame.timestamp / 1000).getDate() : today.getDate();
                    jumpToDate(d, h);
                  }} style={{
                    display: "block", width: "100%", padding: "6px 8px", border: "none",
                    background: "transparent", color: "rgba(255,255,255,0.6)", fontSize: 12,
                    cursor: "pointer", textAlign: "center",
                  }}
                    onMouseEnter={(e) => { e.currentTarget.style.background = "rgba(255,255,255,0.1)"; }}
                    onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
                  >{`${String(h).padStart(2, "0")}:00`}</button>
                ))}
              </div>
            </div>
          </div>
        );
      })()}

      {/* Search panel — floating above the bar */}
      {showSearch && (
        <SearchPanel onClose={() => { setShowSearch(false); setHighlightQuery(""); setHighlightRegions([]); }} onSelectFrame={async (f, query, regions) => {
          // Jump to this frame's date and position on timeline
          const d = new Date(f.timestamp / 1000);
          const dateStr = `${d.getFullYear()}-${String(d.getMonth()+1).padStart(2,"0")}-${String(d.getDate()).padStart(2,"0")}`;
          // Store search query and regions for highlight in rewind mode
          setHighlightQuery(query || "");
          setHighlightRegions(regions || []);
          try {
            const newFrames = await getTimeline(dateStr);
            if (newFrames.length > 0) {
              setFrames(newFrames);
              let closestIdx = 0;
              let minDiff = Infinity;
              newFrames.forEach((nf, i) => {
                const diff = Math.abs(nf.timestamp - f.timestamp);
                if (diff < minDiff) { minDiff = diff; closestIdx = i; }
              });
              setFrameIndex(closestIdx);
              setMode("rewind");
            }
          } catch {}
          setShowSearch(false);
        }} />
      )}

      {/* AI chat panel — floating above the bar (with Chat/Transcript tabs) */}
      {showAI && (
        <div style={{
          position: "absolute", bottom: 78, right: 0, width: 360, maxHeight: 480,
          background: "rgba(255,255,255,0.95)", backdropFilter: "blur(40px)",
          borderRadius: 16, boxShadow: "0 8px 32px rgba(0,0,0,0.12), 0 0 0 0.5px rgba(0,0,0,0.06)",
          display: "flex", flexDirection: "column", overflow: "hidden", zIndex: 100,
        }}>
          {/* Header with tabs */}
          <div style={{ padding: "10px 16px 0", borderBottom: "1px solid rgba(0,0,0,0.06)" }}>
            <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
              <SparkleIcon />
              <span style={{ fontSize: 13, fontWeight: 600, color: "#1d1d1f", flex: 1 }}>MindScope</span>
              <button onClick={() => setShowAI(false)} style={{
                background: "rgba(0,0,0,0.05)", border: "none", borderRadius: 100, width: 22, height: 22,
                cursor: "pointer", color: "#86868b", display: "flex", alignItems: "center", justifyContent: "center",
              }}>
                <svg width="8" height="8" viewBox="0 0 10 10" stroke="currentColor" strokeWidth="1.5"><path d="M1 1l8 8M9 1l-8 8" /></svg>
              </button>
            </div>
            {/* Tab pills */}
            <div style={{ display: "flex", gap: 4, paddingBottom: 8 }}>
              <button onClick={() => setAiTab("chat")} style={{
                background: aiTab === "chat" ? "rgba(0,0,0,0.08)" : "rgba(0,0,0,0.03)",
                border: "none", borderRadius: 100, padding: "4px 14px",
                fontSize: 12, fontWeight: aiTab === "chat" ? 600 : 500,
                color: aiTab === "chat" ? "#1d1d1f" : "#86868b",
                cursor: "pointer", display: "flex", alignItems: "center", gap: 5,
              }}>
                <SparkleIcon /> Chat
              </button>
              <button onClick={() => setAiTab("transcript")} style={{
                background: aiTab === "transcript" ? "rgba(0,0,0,0.08)" : "rgba(0,0,0,0.03)",
                border: "none", borderRadius: 100, padding: "4px 14px",
                fontSize: 12, fontWeight: aiTab === "transcript" ? 600 : 500,
                color: aiTab === "transcript" ? "#1d1d1f" : "#86868b",
                cursor: "pointer", display: "flex", alignItems: "center", gap: 5, position: "relative",
              }}>
                <TranscriptIcon /> Transcript
                {meetingActive && (
                  <span style={{
                    width: 6, height: 6, borderRadius: "50%", background: "#FF3B30",
                    display: "inline-block", position: "absolute", top: 4, right: 6,
                    animation: "meetingDotPulse 2s ease-in-out infinite",
                  }} />
                )}
              </button>
            </div>
          </div>

          {/* Quick action buttons — only when meeting is active */}
          {meetingActive && aiTab === "chat" && (
            <div style={{ padding: "6px 12px", display: "flex", gap: 4, flexWrap: "wrap", borderBottom: "1px solid rgba(0,0,0,0.04)" }}>
              {[
                { label: "What should I say?", icon: <MicIcon /> },
                { label: "Follow-up questions", icon: <TranscriptIcon /> },
                { label: "Recap", icon: <CopyIcon /> },
              ].map((btn) => (
                <button key={btn.label} onClick={() => handleMeetingAwareAiQuery(btn.label)} style={{
                  background: "rgba(0,0,0,0.04)", border: "none", borderRadius: 100,
                  padding: "3px 10px", fontSize: 11, color: "#3a3a3c",
                  cursor: "pointer", display: "flex", alignItems: "center", gap: 4,
                  whiteSpace: "nowrap",
                }}>
                  {btn.icon} {btn.label}
                </button>
              ))}
            </div>
          )}

          {/* Chat tab content */}
          {aiTab === "chat" && (
            <>
              <div style={{ flex: 1, overflowY: "auto", padding: 12, display: "flex", flexDirection: "column", gap: 8 }}>
                {aiMessages.length === 0 && (
                  <div style={{ textAlign: "center", padding: 16, color: "#86868b" }}>
                    <p style={{ fontSize: 12, marginBottom: 10 }}>Ask about your screen history</p>
                    {["What was I working on?", "Summarize my activity"].map((q) => (
                      <button key={q} onClick={() => handleMeetingAwareAiQuery(q)} style={{
                        display: "block", width: "100%", textAlign: "left", background: "rgba(0,0,0,0.03)",
                        border: "none", borderRadius: 8, padding: "8px 12px", marginBottom: 4,
                        cursor: "pointer", fontSize: 12, color: "#1d1d1f",
                      }}>{q}</button>
                    ))}
                  </div>
                )}
                {aiMessages.map((msg, i) => (
                  <div key={i} style={{
                    alignSelf: msg.role === "user" ? "flex-end" : "flex-start",
                    maxWidth: "85%", padding: "8px 12px", borderRadius: 14,
                    background: msg.role === "user" ? "#007AFF" : "rgba(0,0,0,0.05)",
                    color: msg.role === "user" ? "white" : "#1d1d1f",
                    fontSize: 12, lineHeight: 1.5, whiteSpace: "pre-wrap",
                  }}>{msg.text}</div>
                ))}
              </div>
              <div style={{ padding: "8px 12px", borderTop: "1px solid rgba(0,0,0,0.06)", display: "flex", gap: 6 }}>
                <input type="text" value={aiInput} onChange={(e) => setAiInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && aiInput.trim()) handleMeetingAwareAiQuery(aiInput.trim());
                    if (e.key === "Escape") setShowAI(false);
                  }}
                  autoFocus placeholder={meetingActive ? "Ask about meeting or screen..." : "Ask anything..."}
                  style={{ flex: 1, border: "1px solid rgba(0,0,0,0.08)", borderRadius: 100, padding: "6px 12px", fontSize: 12, outline: "none", background: "rgba(0,0,0,0.02)" }}
                />
              </div>
            </>
          )}

          {/* Transcript tab content */}
          {aiTab === "transcript" && (
            <>
              {meetingActive || meetingTranscripts.length > 0 ? (
                <>
                  {/* Copy All button */}
                  <div style={{ padding: "6px 12px 0", display: "flex", justifyContent: "flex-end" }}>
                    <button onClick={() => {
                      const text = meetingTranscripts.map((t) => `[${t.time}] ${t.speaker}: ${t.text}`).join("\n");
                      navigator.clipboard.writeText(text).catch(() => {});
                    }} style={{
                      background: "rgba(0,0,0,0.04)", border: "none", borderRadius: 100,
                      padding: "3px 10px", fontSize: 11, color: "#3a3a3c",
                      cursor: "pointer", display: "flex", alignItems: "center", gap: 4,
                    }}>
                      <CopyIcon /> Copy All
                    </button>
                  </div>
                  {/* Transcript lines */}
                  <div ref={meetingScrollRef} style={{ flex: 1, overflowY: "auto", padding: 12, display: "flex", flexDirection: "column", gap: 6 }}>
                    {meetingTranscripts.map((line, i) => (
                      <div key={`t-${i}`} style={{ display: "flex", alignItems: "flex-start", gap: 6 }}>
                        <span style={{ color: "#86868b", flexShrink: 0, marginTop: 2 }}>
                          {(line.speaker.toLowerCase().includes("you") || line.speaker.toLowerCase().includes("me"))
                            ? <MicIcon /> : <SpeakerSmIcon />}
                        </span>
                        <div style={{ flex: 1, minWidth: 0 }}>
                          <span style={{ fontSize: 12, lineHeight: 1.5 }}>
                            <span style={{ fontWeight: 600, color: "#1d1d1f" }}>{line.speaker}</span>
                            <span style={{ color: "#48484a", marginLeft: 5 }}>{line.text}</span>
                          </span>
                        </div>
                        <span style={{ fontSize: 10, color: "#aeaeb2", flexShrink: 0, marginTop: 2 }}>{line.time}</span>
                      </div>
                    ))}
                    {meetingTranscripts.length === 0 && (
                      <div style={{ textAlign: "center", padding: 20, color: "#86868b", fontSize: 12 }}>
                        Waiting for transcript lines...
                      </div>
                    )}
                  </div>
                </>
              ) : (
                <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", padding: 20 }}>
                  <div style={{ textAlign: "center", color: "#86868b" }}>
                    <TranscriptIcon />
                    <p style={{ fontSize: 12, marginTop: 8 }}>No active meeting</p>
                    <p style={{ fontSize: 11, marginTop: 4, color: "#aeaeb2" }}>Start a meeting to see the transcript here</p>
                  </div>
                </div>
              )}
            </>
          )}

          {/* Pulse animation for recording dot */}
          <style>{`
            @keyframes meetingDotPulse {
              0%, 100% { opacity: 1; }
              50% { opacity: 0.4; }
            }
          `}</style>
        </div>
      )}

      {/* Brief panel — floating above the bar */}
      {showBrief && (() => {
        const sectionHeaders = ["Focus", "Schedule", "Tasks", "Recent", "People", "Summary", "Highlights", "Notes"];
        return (
          <div style={{
            position: "absolute", bottom: 78, left: 0, width: 340, maxHeight: 400,
            background: "rgba(255,255,255,0.95)", backdropFilter: "blur(40px)",
            borderRadius: 16, boxShadow: "0 8px 32px rgba(0,0,0,0.12), 0 0 0 0.5px rgba(0,0,0,0.06)",
            display: "flex", flexDirection: "column", overflow: "hidden",
          }}>
            <div style={{ padding: "12px 16px", borderBottom: "1px solid rgba(0,0,0,0.06)", display: "flex", alignItems: "center", gap: 8 }}>
              <BriefIcon />
              <span style={{ fontSize: 13, fontWeight: 600, color: "#1d1d1f", flex: 1 }}>Today</span>
              <button onClick={() => setShowBrief(false)} style={{
                background: "rgba(0,0,0,0.05)", border: "none", borderRadius: 100, width: 22, height: 22,
                cursor: "pointer", color: "#86868b", display: "flex", alignItems: "center", justifyContent: "center",
              }}>
                <svg width="8" height="8" viewBox="0 0 10 10" stroke="currentColor" strokeWidth="1.5"><path d="M1 1l8 8M9 1l-8 8" /></svg>
              </button>
            </div>
            <div style={{ flex: 1, overflowY: "auto", padding: "10px 16px" }}>
              {briefText ? briefText.split("\n").map((line, i) => {
                const t = line.trim();
                if (!t) return <div key={i} style={{ height: 6 }} />;
                if (sectionHeaders.some((h) => t.startsWith(h))) {
                  return <div key={i} style={{ fontSize: 11, fontWeight: 700, color: "#1d1d1f", marginTop: i > 0 ? 10 : 0, marginBottom: 4, textTransform: "uppercase", letterSpacing: 0.5 }}>{t}</div>;
                }
                return <div key={i} style={{ fontSize: 12, color: "#3a3a3c", lineHeight: 1.5, marginBottom: 1 }}>{t}</div>;
              }) : (
                <div style={{ textAlign: "center", padding: 20, color: "#86868b", fontSize: 12 }}>Loading...</div>
              )}
            </div>
            <div style={{ padding: "8px 16px", borderTop: "1px solid rgba(0,0,0,0.06)", display: "flex", justifyContent: "flex-end" }}>
              <button onClick={() => { setBriefText(""); getDailyBrief().then(setBriefText).catch(() => setBriefText("Unable to load.")); }} style={{
                background: "rgba(0,0,0,0.05)", border: "none", borderRadius: 100, padding: "4px 12px", fontSize: 11, color: "#1d1d1f", cursor: "pointer",
              }}>Refresh</button>
            </div>
          </div>
        );
      })()}

      {showSettings && <SettingsPanel onClose={() => setShowSettings(false)} />}
    </div>
    </div>
  );
}

// --- Timeline component ---
function BottomTimeline({ timelineRef, segments, frames, scrubberPos, onInteraction, onFrameStep }: {
  timelineRef: React.RefObject<HTMLDivElement | null>;
  segments: { app: string; startIdx: number; endIdx: number; color: string }[];
  frames: CapturedFrame[];
  scrubberPos: number;
  onInteraction: (clientX: number) => void;
  onFrameStep: (delta: number) => void;
}) {
  // Non-passive wheel listener for trackpad swipe
  useEffect(() => {
    const el = timelineRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      e.preventDefault();
      const delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
      const step = Math.sign(delta) * Math.max(1, Math.floor(Math.abs(delta) / 20));
      onFrameStep(step);
    };
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  });

  return (
    <div ref={timelineRef} className="flex-1 relative" style={{ height: 18, cursor: "grab", touchAction: "none" }}
      onClick={(e) => onInteraction(e.clientX)}
      onMouseDown={(e) => {
        const el = e.currentTarget;
        el.style.cursor = "grabbing";
        const move = (ev: MouseEvent) => { ev.preventDefault(); onInteraction(ev.clientX); };
        const up = () => { el.style.cursor = "grab"; window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); };
        window.addEventListener("mousemove", move);
        window.addEventListener("mouseup", up);
        onInteraction(e.clientX);
      }}
    >
      {/* App icons — hidden in bar mode (too small), shown in rewind */}
      <div style={{ position: "absolute", top: -18, left: 0, right: 0, height: 16, pointerEvents: "none", display: "none" }}>
        {segments.map((seg, i) => {
          const startPct = frames.length > 1 ? (seg.startIdx / (frames.length - 1)) * 100 : 0;
          const widthPct = frames.length > 1 ? ((seg.endIdx - seg.startIdx + 1) / frames.length) * 100 : 100;
          if (widthPct < 4) return null;
          const iconUrl = `http://127.0.0.1:9457/app-icon/${encodeURIComponent(seg.app)}`;
          return (
            <div key={i} style={{ position: "absolute", left: `${startPct + widthPct / 2}%`, transform: "translateX(-50%)" }}>
              <div style={{
                width: 18, height: 18, borderRadius: 5, overflow: "hidden",
                display: "flex", alignItems: "center", justifyContent: "center",
                background: "rgba(0,0,0,0.03)",
              }}>
                <img src={iconUrl} alt="" style={{ width: 14, height: 14 }}
                  onError={(e) => {
                    // Replace with colored dot on error
                    const parent = (e.target as HTMLImageElement).parentElement!;
                    parent.innerHTML = "";
                    parent.style.background = seg.color;
                    parent.style.width = "8px";
                    parent.style.height = "8px";
                    parent.style.borderRadius = "4px";
                  }} />
              </div>
            </div>
          );
        })}
      </div>

      {/* Track */}
      <div className="timeline-track" style={{ position: "absolute", bottom: 0, left: 0, right: 0, background: "rgba(0,0,0,0.03)" }}>
        {segments.map((seg, i) => {
          const startPct = frames.length > 1 ? (seg.startIdx / (frames.length - 1)) * 100 : 0;
          const widthPct = frames.length > 1 ? ((seg.endIdx - seg.startIdx + 1) / frames.length) * 100 : 100;
          return <div key={i} className="timeline-segment" style={{ left: `${startPct}%`, width: `${widthPct}%`, background: seg.color, opacity: 0.35 }} />;
        })}
        {frames.length > 0 && <div className="timeline-scrubber" style={{ left: `${scrubberPos}%` }} />}
      </div>
    </div>
  );
}

export default App;

import { useState, useEffect, useRef } from "react";

// --- SVG Icons (Apple SF-style, no emoji) ---
const MicIcon = () => (
  <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
    <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/>
    <path d="M19 10v2a7 7 0 0 1-14 0v-2"/>
    <line x1="12" y1="19" x2="12" y2="23"/>
  </svg>
);
const SpeakerIcon = () => (
  <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/>
    <path d="M15.54 8.46a5 5 0 0 1 0 7.07"/>
  </svg>
);
const ChevronUpIcon = () => (
  <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
    <polyline points="18 15 12 9 6 15"/>
  </svg>
);

export interface TranscriptLine {
  time: string;
  speaker: string;
  text: string;
}

interface MeetingBarProps {
  onExpand: () => void;
}

export default function MeetingBar({ onExpand }: MeetingBarProps) {
  const [active, setActive] = useState(false);
  const [transcripts, setTranscripts] = useState<TranscriptLine[]>([]);
  const [appName, setAppName] = useState("");
  const [hovered, setHovered] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  // Poll meeting status every 3s
  useEffect(() => {
    const poll = async () => {
      try {
        const resp = await fetch("http://127.0.0.1:9457/meeting/status");
        const data = await resp.json();
        setActive(data.active);
        setAppName(data.app_name || "");
        setTranscripts(data.recent_transcripts || []);
      } catch {
        setActive(false);
      }
    };
    poll();
    const t = setInterval(poll, 3000);
    return () => clearInterval(t);
  }, []);

  if (!active) return null;

  // Show max 4 lines, newest at bottom
  const visibleLines = transcripts.slice(-4);

  return (
    <div
      ref={containerRef}
      onClick={onExpand}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      style={{
        position: "absolute",
        bottom: 78,
        right: 0,
        width: 300,
        background: "rgba(255, 255, 255, 0.92)",
        backdropFilter: "blur(40px) saturate(180%)",
        WebkitBackdropFilter: "blur(40px) saturate(180%)",
        borderRadius: 16,
        boxShadow: "0 4px 24px rgba(0,0,0,0.08), 0 0 0 0.5px rgba(0,0,0,0.06)",
        padding: "10px 14px",
        cursor: "pointer",
        zIndex: 90,
        transition: "box-shadow 0.2s ease",
        ...(hovered ? { boxShadow: "0 6px 32px rgba(0,0,0,0.12), 0 0 0 0.5px rgba(0,0,0,0.08)" } : {}),
      }}
    >
      {/* Header row: recording indicator + app name + expand hint */}
      <div style={{
        display: "flex", alignItems: "center", gap: 8,
        marginBottom: visibleLines.length > 0 ? 8 : 0,
      }}>
        {/* Red recording dot */}
        <span style={{
          width: 7, height: 7, borderRadius: "50%", background: "#FF3B30",
          display: "inline-block", flexShrink: 0,
          animation: "meetingDotPulse 2s ease-in-out infinite",
        }} />
        <span style={{ fontSize: 11, fontWeight: 600, color: "#1d1d1f" }}>
          Meeting{appName ? ` \u2014 ${appName}` : ""}
        </span>
        <div style={{ flex: 1 }} />
        {/* Expand hint on hover */}
        <div style={{
          display: "flex", alignItems: "center", gap: 3,
          opacity: hovered ? 1 : 0,
          transition: "opacity 0.2s ease",
        }}>
          <span style={{ fontSize: 10, color: "#86868b" }}>Full transcript</span>
          <span style={{ color: "#86868b" }}><ChevronUpIcon /></span>
        </div>
      </div>

      {/* Transcript lines */}
      {visibleLines.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {visibleLines.map((line, i) => {
            // Older lines fade out
            const lineCount = visibleLines.length;
            const opacity = lineCount <= 1 ? 1 : 0.4 + 0.6 * (i / (lineCount - 1));
            const isMic = line.speaker.toLowerCase().includes("you") || line.speaker.toLowerCase().includes("me");

            return (
              <div key={i} style={{
                display: "flex", alignItems: "flex-start", gap: 6,
                opacity,
                transition: "opacity 0.3s ease",
              }}>
                {/* Speaker icon */}
                <span style={{ color: "#86868b", flexShrink: 0, marginTop: 1 }}>
                  {isMic ? <MicIcon /> : <SpeakerIcon />}
                </span>
                {/* Speaker name + text */}
                <div style={{ flex: 1, minWidth: 0 }}>
                  <span style={{ fontSize: 12, lineHeight: 1.4 }}>
                    <span style={{ fontWeight: 600, color: "#1d1d1f" }}>{line.speaker}</span>
                    <span style={{ color: "#48484a", marginLeft: 5 }}>{line.text}</span>
                  </span>
                </div>
                {/* Time */}
                <span style={{ fontSize: 10, color: "#aeaeb2", flexShrink: 0, marginTop: 1 }}>
                  {line.time}
                </span>
              </div>
            );
          })}
        </div>
      )}

      {/* Pulse animation for recording dot */}
      <style>{`
        @keyframes meetingDotPulse {
          0%, 100% { opacity: 1; }
          50% { opacity: 0.4; }
        }
      `}</style>
    </div>
  );
}

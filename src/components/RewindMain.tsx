import { useState, useEffect, useMemo, useCallback, useRef } from "react";
import type { CapturedFrame } from "../lib/types";
import { getScreenshot } from "../lib/commands";
import { getAppColor, getAppEmoji, getAppShort } from "../lib/appColors";

interface Props {
  frames: CapturedFrame[];
  frameIndex: number;
  currentFrame: CapturedFrame | null;
  date: string;
  loading: boolean;
  onFrameIndexChange: (index: number) => void;
  onSearchSubmit: (query: string) => void;
  onDateNavigate: (delta: number) => void;
  onOpenSettings?: () => void;
}

export default function RewindMain({
  frames,
  frameIndex,
  currentFrame,
  date,
  loading,
  onFrameIndexChange,
  onSearchSubmit,
  onDateNavigate,
  onOpenSettings,
}: Props) {
  const [bgImage, setBgImage] = useState<string | null>(null);
  const [searchText, setSearchText] = useState("");
  const [searchFocused, setSearchFocused] = useState(false);
  const timelineRef = useRef<HTMLDivElement>(null);

  // Load background screenshot
  useEffect(() => {
    if (!currentFrame) { setBgImage(null); return; }
    getScreenshot(currentFrame.image_path)
      .then((b64) => { if (b64) setBgImage(`data:image/webp;base64,${b64}`); })
      .catch(() => setBgImage(null));
  }, [currentFrame?.image_path]);

  // Time ago label (timestamp is microseconds since epoch)
  const timeLabel = useMemo(() => {
    if (!currentFrame) return "";
    const now = Date.now();
    const tsMs = currentFrame.timestamp / 1000; // micros to millis
    const diffSec = Math.floor((now - tsMs) / 1000);

    if (diffSec < 10) return "Now";
    if (diffSec < 60) return `${diffSec} seconds ago`;
    if (diffSec < 3600) return `${Math.floor(diffSec / 60)} minutes ago`;
    if (diffSec < 86400) return `${Math.floor(diffSec / 3600)} hours ago`;
    // Show timestamp for older frames
    const ts = new Date(currentFrame.timestamp / 1000);
    return ts.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }, [currentFrame?.timestamp]);

  // Build timeline segments grouped by app
  const segments = useMemo(() => {
    if (frames.length === 0) return [];
    const result: { app: string; startIdx: number; endIdx: number; color: string }[] = [];
    let currentApp = frames[0].app_name;
    let startIdx = 0;

    for (let i = 1; i <= frames.length; i++) {
      const app = i < frames.length ? frames[i].app_name : "";
      if (app !== currentApp || i === frames.length) {
        result.push({
          app: currentApp,
          startIdx,
          endIdx: i - 1,
          color: getAppColor(currentApp),
        });
        currentApp = app;
        startIdx = i;
      }
    }
    return result;
  }, [frames]);

  // Unique apps for icons (used by segments rendering below)

  // Handle timeline click
  const handleTimelineClick = useCallback(
    (e: React.MouseEvent) => {
      if (!timelineRef.current || frames.length === 0) return;
      const rect = timelineRef.current.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const ratio = Math.max(0, Math.min(1, x / rect.width));
      const idx = Math.round(ratio * (frames.length - 1));
      onFrameIndexChange(idx);
    },
    [frames.length, onFrameIndexChange]
  );

  const handleSearchKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && searchText.trim()) {
      onSearchSubmit(searchText.trim());
    }
  };

  // Scrubber position
  const scrubberPos = frames.length > 1 ? (frameIndex / (frames.length - 1)) * 100 : 50;

  // Format date display
  const dateDisplay = useMemo(() => {
    const d = new Date(date + "T12:00:00");
    return d.toLocaleDateString("en-US", {
      weekday: "short",
      month: "short",
      day: "numeric",
      year: "numeric",
    });
  }, [date]);

  return (
    <div className="w-full h-full relative overflow-hidden">
      {/* Background: Screenshot or gradient */}
      {bgImage ? (
        <img
          src={bgImage}
          alt=""
          className="absolute inset-0 w-full h-full object-cover"
          style={{ filter: "brightness(0.85)" }}
        />
      ) : (
        <div className="absolute inset-0 rewind-bg" />
      )}

      {/* Gradient overlay at bottom for timeline visibility */}
      <div
        className="absolute bottom-0 left-0 right-0"
        style={{
          height: 160,
          background: "linear-gradient(to top, rgba(0,0,0,0.6) 0%, transparent 100%)",
          pointerEvents: "none",
        }}
      />

      {/* Date navigation — top center */}
      <div className="absolute top-4 left-1/2 -translate-x-1/2 flex items-center gap-3 z-20">
        <button className="nav-circle" style={{ width: 28, height: 28, fontSize: 12 }} onClick={() => onDateNavigate(-1)}>
          ‹
        </button>
        <div className="time-bubble" style={{ fontSize: 13, fontWeight: 600 }}>
          {dateDisplay}
        </div>
        <button className="nav-circle" style={{ width: 28, height: 28, fontSize: 12 }} onClick={() => onDateNavigate(1)}>
          ›
        </button>
      </div>

      {/* Recording indicator — top left */}
      <div className="absolute top-4 left-4 flex items-center gap-2 z-20">
        <div
          style={{
            width: 8,
            height: 8,
            borderRadius: 4,
            background: frames.length > 0 ? "#ef4444" : "#6b7280",
            animation: frames.length > 0 ? "pulse 2s infinite" : "none",
            boxShadow: frames.length > 0 ? "0 0 8px #ef4444" : "none",
          }}
        />
        <span style={{ color: "rgba(255,255,255,0.7)", fontSize: 11, fontWeight: 500 }}>
          MindScope
        </span>
        {onOpenSettings && (
          <button
            onClick={onOpenSettings}
            style={{
              background: "rgba(255,255,255,0.15)",
              border: "none",
              borderRadius: 6,
              padding: "3px 8px",
              cursor: "pointer",
              fontSize: 13,
              color: "rgba(255,255,255,0.6)",
              marginLeft: 4,
            }}
          >
            ⚙️
          </button>
        )}
      </div>

      {/* App name + window — above timeline */}
      {currentFrame && (
        <div className="absolute bottom-20 left-1/2 -translate-x-1/2 z-20 animate-fade-in" style={{ textAlign: "center" }}>
          <span style={{ color: "rgba(255,255,255,0.9)", fontSize: 12, fontWeight: 500 }}>
            {getAppEmoji(currentFrame.app_name)} {getAppShort(currentFrame.app_name)}
            {currentFrame.window_name && <span style={{ color: "rgba(255,255,255,0.5)" }}> — {currentFrame.window_name.slice(0, 40)}</span>}
          </span>
        </div>
      )}

      {/* Center Search Bar */}
      <div
        className="absolute left-1/2 top-1/2 -translate-x-1/2 z-30"
        style={{ transform: "translate(-50%, -60%)" }}
      >
        <div
          className="search-glass flex items-center gap-3"
          style={{
            width: searchFocused ? 520 : 480,
            padding: "14px 24px",
            borderRadius: 16,
            transition: "width 0.3s ease",
          }}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#9ca3af" strokeWidth="2.5" strokeLinecap="round">
            <circle cx="11" cy="11" r="8" />
            <path d="M21 21l-4.35-4.35" />
          </svg>
          <input
            type="text"
            value={searchText}
            onChange={(e) => setSearchText(e.target.value)}
            onKeyDown={handleSearchKey}
            onFocus={() => setSearchFocused(true)}
            onBlur={() => setSearchFocused(false)}
            placeholder="Search anything you've seen, said, or heard"
            style={{
              flex: 1,
              background: "transparent",
              border: "none",
              outline: "none",
              fontSize: 16,
              color: "#1f2937",
              fontWeight: 400,
            }}
          />
        </div>
      </div>

      {/* Frame counter — top right corner */}
      {frames.length > 0 && (
        <div className="absolute top-4 right-4 z-20 time-bubble" style={{ fontSize: 11 }}>
          {frames.length} frames
        </div>
      )}

      {/* Empty state */}
      {!loading && frames.length === 0 && (
        <div className="absolute inset-0 flex flex-col items-center justify-center z-10">
          <div style={{ fontSize: 48, opacity: 0.6, marginBottom: 16 }}>🔍</div>
          <p style={{ color: "rgba(255,255,255,0.7)", fontSize: 16, fontWeight: 500 }}>
            Recording started — frames will appear soon
          </p>
          <p style={{ color: "rgba(255,255,255,0.4)", fontSize: 13, marginTop: 8 }}>
            MindScope captures your screen every 5 seconds
          </p>
        </div>
      )}

      {/* Time label bubble — above timeline */}
      {currentFrame && (
        <div
          className="absolute z-20"
          style={{
            bottom: 72,
            left: `calc(${scrubberPos}% - 0px)`,
            transform: "translateX(-50%)",
          }}
        >
          <div className="time-bubble">{timeLabel}</div>
        </div>
      )}

      {/* Bottom Timeline */}
      <div className="absolute bottom-0 left-0 right-0 z-20" style={{ padding: "0 16px 12px" }}>
        {/* Navigation arrows + timeline track */}
        <div className="flex items-center gap-3">
          {/* Left arrow */}
          <button
            className="nav-circle"
            style={{ width: 30, height: 30, fontSize: 14, flexShrink: 0, opacity: frameIndex > 0 ? 1 : 0.3 }}
            onClick={() => onFrameIndexChange(Math.max(0, frameIndex - 1))}
          >
            ‹
          </button>

          {/* Timeline track */}
          <div
            ref={timelineRef}
            className="flex-1 relative cursor-pointer"
            style={{ height: 36, display: "flex", alignItems: "flex-end" }}
            onClick={handleTimelineClick}
          >
            {/* App icon row */}
            <div className="absolute -top-1 left-0 right-0 flex" style={{ height: 24, pointerEvents: "none" }}>
              {segments.map((seg, i) => {
                const startPct = frames.length > 1 ? (seg.startIdx / (frames.length - 1)) * 100 : 0;
                const widthPct = frames.length > 1 ? ((seg.endIdx - seg.startIdx + 1) / frames.length) * 100 : 100;
                // Only show icon if segment is wide enough
                if (widthPct < 5) return null;
                return (
                  <div
                    key={i}
                    className="absolute flex items-center justify-center"
                    style={{
                      left: `${startPct + widthPct / 2}%`,
                      transform: "translateX(-50%)",
                    }}
                  >
                    <div
                      className="app-icon-timeline"
                      style={{ background: seg.color }}
                    >
                      {getAppEmoji(seg.app)}
                    </div>
                  </div>
                );
              })}
            </div>

            {/* Colored track */}
            <div className="timeline-track w-full" style={{ background: "rgba(255,255,255,0.1)" }}>
              {segments.map((seg, i) => {
                const startPct = frames.length > 1 ? (seg.startIdx / (frames.length - 1)) * 100 : 0;
                const widthPct = frames.length > 1 ? ((seg.endIdx - seg.startIdx + 1) / frames.length) * 100 : 100;
                return (
                  <div
                    key={i}
                    className="timeline-segment"
                    style={{
                      left: `${startPct}%`,
                      width: `${widthPct}%`,
                      background: seg.color,
                    }}
                  />
                );
              })}

              {/* Scrubber */}
              {frames.length > 0 && (
                <div
                  className="timeline-scrubber"
                  style={{ left: `${scrubberPos}%` }}
                />
              )}
            </div>

            {/* Thumbnail strip (tiny screenshots behind track) */}
            <div
              className="absolute bottom-0 left-0 right-0 flex overflow-hidden"
              style={{
                height: 8,
                borderRadius: 4,
                opacity: 0.4,
                pointerEvents: "none",
              }}
            >
              {frames.length > 0 && (
                <div className="w-full h-full" style={{
                  background: `repeating-linear-gradient(90deg, ${
                    segments.map((s) => `${s.color} 0%`).join(", ")
                  })`,
                }} />
              )}
            </div>
          </div>

          {/* Right arrow */}
          <button
            className="nav-circle"
            style={{ width: 30, height: 30, fontSize: 14, flexShrink: 0, opacity: frameIndex < frames.length - 1 ? 1 : 0.3 }}
            onClick={() => onFrameIndexChange(Math.min(frames.length - 1, frameIndex + 1))}
          >
            ›
          </button>
        </div>
      </div>
    </div>
  );
}

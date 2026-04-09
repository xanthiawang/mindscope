import { useState, useEffect, useMemo } from "react";
import type { CapturedFrame } from "../lib/types";
import { getScreenshot } from "../lib/commands";
import { getAppColor, getAppEmoji, getAppShort, getUniqueApps } from "../lib/appColors";

interface Props {
  query: string;
  results: CapturedFrame[];
  loading: boolean;
  onQueryChange: (query: string) => void;
  onSelectResult: (frame: CapturedFrame) => void;
  onBack: () => void;
}

export default function SearchResultsView({
  query,
  results,
  loading,
  onQueryChange,
  onSelectResult,
  onBack,
}: Props) {
  const [activeApp, setActiveApp] = useState<string | null>(null);
  const [searchText, setSearchText] = useState(query);

  // Unique apps from results
  const apps = useMemo(() => getUniqueApps(results), [results]);

  // Filtered results
  const filtered = useMemo(() => {
    if (!activeApp) return results;
    return results.filter((r) => r.app_name === activeApp);
  }, [results, activeApp]);

  const handleSearchKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && searchText.trim()) {
      onQueryChange(searchText.trim());
    }
  };

  return (
    <div className="w-full h-full rewind-bg overflow-hidden flex flex-col">
      {/* Top bar: close + search + menu */}
      <div className="flex items-center gap-4 px-6 pt-6 pb-4">
        <button className="nav-circle" onClick={onBack} style={{ width: 32, height: 32 }}>
          ✕
        </button>

        <div
          className="flex-1 flex items-center gap-3"
          style={{
            background: "rgba(255,255,255,0.2)",
            backdropFilter: "blur(10px)",
            borderRadius: 12,
            padding: "10px 18px",
            border: "1px solid rgba(255,255,255,0.15)",
          }}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="rgba(255,255,255,0.7)" strokeWidth="2.5" strokeLinecap="round">
            <circle cx="11" cy="11" r="8" />
            <path d="M21 21l-4.35-4.35" />
          </svg>
          <input
            type="text"
            value={searchText}
            onChange={(e) => setSearchText(e.target.value)}
            onKeyDown={handleSearchKey}
            autoFocus
            style={{
              flex: 1,
              background: "transparent",
              border: "none",
              outline: "none",
              fontSize: 18,
              color: "white",
              fontWeight: 500,
            }}
          />
        </div>

        <button className="nav-circle" style={{ width: 32, height: 32, fontSize: 14 }}>
          ⋯
        </button>
      </div>

      {/* App filter chips */}
      <div className="flex items-center gap-2 px-6 pb-4 overflow-x-auto" style={{ scrollbarWidth: "none" }}>
        <button
          className={`filter-chip ${!activeApp ? "active" : ""}`}
          onClick={() => setActiveApp(null)}
        >
          ★ All
        </button>
        {apps.slice(0, 8).map((app) => (
          <button
            key={app}
            className={`filter-chip ${activeApp === app ? "active" : ""}`}
            onClick={() => setActiveApp(activeApp === app ? null : app)}
          >
            <span
              style={{
                width: 12,
                height: 12,
                borderRadius: 3,
                background: getAppColor(app),
                display: "inline-block",
                flexShrink: 0,
              }}
            />
            {getAppShort(app)}
          </button>
        ))}
      </div>

      {/* Results grid */}
      <div className="flex-1 overflow-y-auto px-6 pb-6">
        {loading ? (
          <div className="flex items-center justify-center h-40">
            <p style={{ color: "rgba(255,255,255,0.6)", fontSize: 15 }}>Searching...</p>
          </div>
        ) : filtered.length === 0 ? (
          <div className="flex items-center justify-center h-40">
            <p style={{ color: "rgba(255,255,255,0.6)", fontSize: 15 }}>No results found</p>
          </div>
        ) : (
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))",
              gap: 16,
            }}
          >
            {filtered.map((frame, i) => (
              <ScreenshotCard
                key={`${frame.timestamp}-${i}`}
                frame={frame}
                onClick={() => onSelectResult(frame)}
                delay={i * 50}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function ScreenshotCard({
  frame,
  onClick,
  delay,
}: {
  frame: CapturedFrame;
  onClick: () => void;
  delay: number;
}) {
  const [thumb, setThumb] = useState<string | null>(null);

  useEffect(() => {
    getScreenshot(frame.image_path)
      .then((b64) => { if (b64) setThumb(`data:image/webp;base64,${b64}`); })
      .catch(() => {});
  }, [frame.image_path]);

  const time = new Date(frame.timestamp / 1000);

  return (
    <div
      className="screenshot-card animate-slide-up"
      style={{ animationDelay: `${delay}ms`, animationFillMode: "backwards" }}
      onClick={onClick}
    >
      {/* Screenshot thumbnail */}
      <div style={{ aspectRatio: "16/10", background: "#1a1a1a", position: "relative", overflow: "hidden" }}>
        {thumb ? (
          <img src={thumb} alt="" className="w-full h-full object-cover" />
        ) : (
          <div className="w-full h-full flex items-center justify-center" style={{ color: "rgba(255,255,255,0.2)" }}>
            📷
          </div>
        )}

        {/* OCR text preview overlay */}
        {frame.ocr_text && (
          <div
            className="absolute bottom-0 left-0 right-0"
            style={{
              background: "linear-gradient(to top, rgba(0,0,0,0.8), transparent)",
              padding: "20px 10px 8px",
              fontSize: 10,
              color: "rgba(255,255,255,0.6)",
              lineHeight: 1.3,
              overflow: "hidden",
              maxHeight: 48,
            }}
          >
            {frame.ocr_text.slice(0, 100)}
          </div>
        )}
      </div>

      {/* Card footer */}
      <div style={{ padding: "8px 12px", display: "flex", alignItems: "center", gap: 8 }}>
        <div
          style={{
            width: 20,
            height: 20,
            borderRadius: 5,
            background: getAppColor(frame.app_name),
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            fontSize: 10,
          }}
        >
          {getAppEmoji(frame.app_name)}
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontSize: 12, fontWeight: 600, color: "white", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
            {frame.window_name || getAppShort(frame.app_name)}
          </div>
          <div style={{ fontSize: 10, color: "rgba(255,255,255,0.5)" }}>
            {time.toLocaleDateString("en-US", { month: "short", day: "numeric" })} {time.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
          </div>
        </div>
      </div>
    </div>
  );
}

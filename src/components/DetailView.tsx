import { useState, useEffect, useRef } from "react";
import type { CapturedFrame } from "../lib/types";
import { getScreenshot, getOcrRegions, type OcrRegion } from "../lib/commands";

interface Props {
  frame: CapturedFrame;
  query: string;
  onBack: () => void;
}

export default function DetailView({ frame, query, onBack }: Props) {
  const [imageSrc, setImageSrc] = useState<string | null>(null);
  const [regions, setRegions] = useState<OcrRegion[]>([]);
  const [imgSize, setImgSize] = useState({ w: 0, h: 0 });
  const imgRef = useRef<HTMLImageElement>(null);

  // Load screenshot
  useEffect(() => {
    getScreenshot(frame.image_path).then((b64) => {
      if (b64) setImageSrc(`data:image/jpeg;base64,${b64}`);
    });
  }, [frame.image_path]);

  // Load OCR regions for highlighting
  useEffect(() => {
    if (frame.image_path.startsWith("video://")) return;
    getOcrRegions(frame.image_path).then(setRegions).catch(() => {});
  }, [frame.image_path]);

  // Track image dimensions for overlay positioning
  const handleImageLoad = () => {
    if (imgRef.current) {
      setImgSize({ w: imgRef.current.clientWidth, h: imgRef.current.clientHeight });
    }
  };

  // Find regions matching the search query
  const matchingRegions = query.trim()
    ? regions.filter((r) => r.text.toLowerCase().includes(query.toLowerCase()))
    : [];

  // Keyboard: Esc to go back
  useEffect(() => {
    const handler = (e: KeyboardEvent) => { if (e.key === "Escape") onBack(); };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onBack]);

  return (
    <div
      style={{
        width: "100vw", height: "100vh",
        background: "#111",
        display: "flex", alignItems: "center", justifyContent: "center",
        position: "relative",
      }}
      onClick={onBack}
    >
      {/* Back button */}
      <button
        onClick={(e) => { e.stopPropagation(); onBack(); }}
        style={{
          position: "absolute", top: 16, left: 16, zIndex: 10,
          background: "rgba(255,255,255,0.15)", border: "none", borderRadius: 8,
          padding: "6px 14px", cursor: "pointer", color: "white", fontSize: 13,
          backdropFilter: "blur(10px)",
        }}
      >
        ← Back
      </button>

      {/* Query badge */}
      {query && (
        <div style={{
          position: "absolute", top: 16, left: "50%", transform: "translateX(-50%)", zIndex: 10,
          background: "rgba(251, 191, 36, 0.9)", color: "#1f2937",
          padding: "4px 14px", borderRadius: 20, fontSize: 13, fontWeight: 600,
        }}>
          🔍 "{query}" — {matchingRegions.length} matches
        </div>
      )}

      {/* Screenshot + highlight overlay */}
      <div
        style={{ position: "relative", maxWidth: "90vw", maxHeight: "85vh" }}
        onClick={(e) => e.stopPropagation()}
      >
        {imageSrc ? (
          <>
            <img
              ref={imgRef}
              src={imageSrc}
              alt=""
              onLoad={handleImageLoad}
              style={{
                maxWidth: "90vw", maxHeight: "85vh",
                objectFit: "contain", borderRadius: 8,
                boxShadow: "0 8px 40px rgba(0,0,0,0.5)",
              }}
            />

            {/* Highlight boxes for matching OCR regions */}
            {imgSize.w > 0 && matchingRegions.map((region, i) => (
              <div
                key={i}
                style={{
                  position: "absolute",
                  left: region.x * imgSize.w,
                  top: region.y * imgSize.h,
                  width: region.w * imgSize.w,
                  height: region.h * imgSize.h,
                  background: "rgba(251, 191, 36, 0.35)",
                  border: "2px solid rgba(251, 191, 36, 0.8)",
                  borderRadius: 3,
                  pointerEvents: "none",
                  transition: "all 0.2s",
                }}
                title={region.text}
              />
            ))}

            {/* All OCR regions (dim, to show text detection) */}
            {imgSize.w > 0 && query && regions.filter((r) => !matchingRegions.includes(r)).map((region, i) => (
              <div
                key={`dim-${i}`}
                style={{
                  position: "absolute",
                  left: region.x * imgSize.w,
                  top: region.y * imgSize.h,
                  width: region.w * imgSize.w,
                  height: region.h * imgSize.h,
                  background: "rgba(0, 0, 0, 0.3)",
                  borderRadius: 2,
                  pointerEvents: "none",
                }}
              />
            ))}
          </>
        ) : (
          <div style={{ color: "rgba(255,255,255,0.3)", fontSize: 15 }}>Loading...</div>
        )}
      </div>

      {/* Frame info at bottom */}
      <div style={{
        position: "absolute", bottom: 16, left: "50%", transform: "translateX(-50%)",
        background: "rgba(0,0,0,0.6)", backdropFilter: "blur(10px)",
        borderRadius: 10, padding: "6px 16px",
        display: "flex", alignItems: "center", gap: 12,
        color: "rgba(255,255,255,0.7)", fontSize: 12,
      }}>
        <span>{new Date(frame.timestamp / 1000).toLocaleString()}</span>
        <span>•</span>
        <span>{frame.app_name}</span>
        {frame.window_name && <><span>•</span><span>{frame.window_name}</span></>}
      </div>
    </div>
  );
}

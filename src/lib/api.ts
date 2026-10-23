// Local HTTP API (127.0.0.1:9457) access. The backend issues a per-launch token;
// every request (fetch or <img src>) must carry it. `initApi()` runs once before
// the app renders so `apiUrl()` can stay synchronous for image URLs.
import { invoke } from "@tauri-apps/api/core";

let port = 9457;
let token = "";

export async function initApi(): Promise<void> {
  try {
    const info = await invoke<{ port: number; token: string }>("get_api_info");
    port = info.port;
    token = info.token;
  } catch {
    // Not running inside Tauri (e.g. vite dev in a browser): requests will 401.
  }
}

/** Build an authenticated URL for the local API, e.g. apiUrl("/frames/12"). */
export function apiUrl(path: string): string {
  const sep = path.includes("?") ? "&" : "?";
  return `http://127.0.0.1:${port}${path}${sep}token=${encodeURIComponent(token)}`;
}

/** fetch() against the local API with the token in the Authorization header. */
export function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const headers = new Headers(init?.headers);
  headers.set("Authorization", `Bearer ${token}`);
  return fetch(`http://127.0.0.1:${port}${path}`, { ...init, headers });
}

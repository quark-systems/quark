// The daemon connection: its URL, request helper and error types.

export interface DaemonEvent<T = unknown> {
  seq: number; project_id?: string | null; type: string; ts: string; payload: T;
}

/** The daemon answered, but does not serve this endpoint yet (404/405/501). */
export class NotAvailable extends Error {
  constructor(public path: string) { super(`${path} is not available on this daemon yet`); }
}
/** Any other non-2xx answer. `message` is the daemon's `error.message` when it sent one. */
export class ApiError extends Error {
  constructor(public status: number, public code: string | null, message: string) { super(message); }
}

export const DEFAULT_DAEMON = "http://127.0.0.1:7380";
const STORAGE_KEY = "quark.daemon";

function initialDaemon(): string {
  const fromQuery = typeof location !== "undefined" ? new URLSearchParams(location.search).get("daemon") : null;
  let saved: string | null = null;
  try { saved = localStorage.getItem(STORAGE_KEY); } catch { /* storage unavailable */ }
  return (fromQuery ?? saved ?? DEFAULT_DAEMON).replace(/\/$/, "");
}

let daemon = initialDaemon();
export function daemonUrl() { return daemon; }
export function wsUrl(path: string) { return daemon.replace(/^http/, "ws") + path; }
/** Point the app at another daemon and remember it for this viewer. */
export function setDaemonUrl(url: string) {
  daemon = url.trim().replace(/\/$/, "") || DEFAULT_DAEMON;
  try { localStorage.setItem(STORAGE_KEY, daemon); } catch { /* storage unavailable */ }
}

export const enc = (s: string) => encodeURIComponent(s);

export async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(daemon + path, {
    method,
    headers: body !== undefined ? { "content-type": "application/json" } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (r.status === 404 || r.status === 405 || r.status === 501) {
    // A 404 with a daemon error body naming a missing entity is a real "not found", not a missing route.
    const err = await r.json().catch(() => null);
    if (r.status === 404 && err?.error?.code && err.error.code !== "route_not_found") {
      throw new ApiError(404, err.error.code, err.error.message ?? "not found");
    }
    throw new NotAvailable(path);
  }
  if (!r.ok) {
    const text = await r.text().catch(() => "");
    let code: string | null = null, message = text || r.statusText;
    try {
      const j = JSON.parse(text);
      code = j?.error?.code ?? null;
      message = j?.error?.message ?? (typeof j?.error === "string" ? j.error : message);
    } catch { /* not JSON */ }
    throw new ApiError(r.status, code, `${message} (${r.status})`);
  }
  if (r.status === 202 || r.status === 204) return undefined as T;
  const ct = r.headers.get("content-type") ?? "";
  if (ct.includes("json")) return (await r.json()) as T;
  return (await r.text()) as unknown as T;
}

const te = new TextEncoder();
export function utf8ToB64(s: string): string {
  const bytes = te.encode(s);
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}
export function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

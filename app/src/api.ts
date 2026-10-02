// Typed client for the quarkd v1 API. Shapes follow api/openapi.json where the daemon
// serves them today, and app/CONTRACT.md for endpoints other Phase 1 workstreams are adding.

export type TaskState =
  | "queued" | "running" | "needs_decision" | "blocked" | "paused"
  | "in_review" | "done" | "failed" | "unknown";
export type TaskKind = "ship" | "scout";

export interface AgentConfig { harness: string; model?: string | null; effort?: string | null }

export interface Project {
  id: string; name: string; goal?: string | null; workspace_path?: string | null;
  created_at: string; updated_at: string;
  // Proposed (CONTRACT.md, workstream 2); absent on today's daemon.
  repos?: string[]; agent_config?: AgentConfig | null; dispatch_preset?: string | null;
  coordinator_state?: "starting" | "running" | "stopped" | "failed" | null;
}

export interface CreateProject {
  name: string; goal?: string | null; workspace_path?: string | null;
  repos?: string[]; agent_config?: AgentConfig; dispatch_preset?: string;
}

export interface Task {
  id: string; project_id: string; title: string; state: TaskState;
  kind?: TaskKind | null; state_note?: string | null; harness?: string | null;
  pull_request_url?: string | null; created_at: string; updated_at: string;
}

export interface Decision {
  id: string; project_id: string; task_id?: string | null; question: string;
  state: "open" | "answered"; answer?: string | null; opened_at: string;
}

export interface Health { status: string; version: string; engine: string; last_seq: number }

export interface Harness {
  id: string; name: string; installed: boolean; version?: string | null;
  models: string[]; efforts: string[]; install_hint?: string | null;
}
export interface DispatchPreset { id: string; label: string; description?: string | null }

export interface ChatMessage { id: string; ts: string; role: "user" | "coordinator"; text: string }
export interface TranscriptEntry {
  id: string; ts: string; role: "user" | "assistant" | "tool" | "system"; text: string; tool?: string | null;
}
export interface TerminalInfo {
  task_id: string; cols: number; rows: number; attached: boolean; seq: number; snapshot_b64?: string | null;
}
export type ChangeStatus = "added" | "modified" | "deleted" | "renamed";
export interface ChangedFile { path: string; old_path?: string | null; status: ChangeStatus; additions: number; deletions: number }
export interface Changes { base?: string | null; head?: string | null; files: ChangedFile[] }

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

const enc = (s: string) => encodeURIComponent(s);

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
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

export const api = {
  health: () => req<Health>("GET", "/v1/health"),
  projects: () => req<Project[]>("GET", "/v1/projects"),
  project: (id: string) => req<Project>("GET", `/v1/projects/${enc(id)}`),
  createProject: (p: CreateProject) => req<Project>("POST", "/v1/projects", p),
  tasks: (pid: string) => req<Task[]>("GET", `/v1/projects/${enc(pid)}/tasks`),
  task: (id: string) => req<Task>("GET", `/v1/tasks/${enc(id)}`),
  decisions: () => req<Decision[]>("GET", "/v1/decisions?state=open"),
  harnesses: () => req<Harness[]>("GET", "/v1/harnesses"),
  dispatchPresets: () => req<DispatchPreset[]>("GET", "/v1/dispatch/presets"),

  chat: (cid: string) => req<ChatMessage[]>("GET", `/v1/coordinators/${enc(cid)}/messages`),
  sendChat: (cid: string, text: string) => req<void>("POST", `/v1/coordinators/${enc(cid)}/messages`, { text }),

  steer: (id: string, text: string) => req<void>("POST", `/v1/tasks/${enc(id)}/messages`, { text }),
  cancel: (id: string) => req<void>("POST", `/v1/tasks/${enc(id)}:cancel`),
  relaunch: (id: string) => req<void>("POST", `/v1/tasks/${enc(id)}:relaunch`),

  transcript: (id: string) => req<TranscriptEntry[]>("GET", `/v1/tasks/${enc(id)}/transcript`),
  changes: (id: string) => req<Changes>("GET", `/v1/tasks/${enc(id)}/changes`),
  diff: (id: string, path?: string) =>
    req<string>("GET", `/v1/tasks/${enc(id)}/diff` + (path ? `?path=${enc(path)}` : "")),

  terminal: (id: string) => req<TerminalInfo>("GET", `/v1/tasks/${enc(id)}/terminal`),
  terminalResize: (id: string, cols: number, rows: number) =>
    req<unknown>("POST", `/v1/tasks/${enc(id)}/terminal/resize`, { cols, rows }),
  /** Ordered input: one request in flight per task; keys typed meanwhile are coalesced. */
  terminalInput: (id: string, data: string) => enqueueInput(id, data),
};

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

// Concurrent fetches can reach the daemon out of order (POC: "echo" arrived as "ecoh"
// when typing fast), so input is serialized per task and coalesced while a POST is in flight.
interface InputQueue { buf: string; busy: boolean; waiters: ((e?: unknown) => void)[] }
const inputQ = new Map<string, InputQueue>();
function enqueueInput(id: string, data: string): Promise<void> {
  let q = inputQ.get(id);
  if (!q) inputQ.set(id, (q = { buf: "", busy: false, waiters: [] }));
  q.buf += data;
  const done = new Promise<void>((resolve, reject) => q!.waiters.push((e) => (e ? reject(e) : resolve())));
  if (!q.busy) void pump(id, q);
  return done;
}
async function pump(id: string, q: InputQueue) {
  q.busy = true;
  while (q.buf) {
    const chunk = q.buf, waiters = q.waiters;
    q.buf = ""; q.waiters = [];
    let err: unknown;
    try {
      await req<void>("POST", `/v1/tasks/${enc(id)}/terminal/input`, { data_b64: utf8ToB64(chunk) });
    } catch (e) { err = e; }
    waiters.forEach((w) => w(err));
  }
  q.busy = false;
}

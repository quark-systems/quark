// Typed client for the stub daemon contract (ui-poc/CONTRACT.md).
// The webview talks to the daemon directly; there is no Rust-side proxy.

export type TaskState = "queued" | "running" | "needs_decision" | "review" | "done" | "failed";

export interface Project { id: string; name: string; repo: string; active_tasks: number }
export interface Task {
  id: string; project_id: string; title: string; state: TaskState;
  harness: string; branch: string; updated_at: string | number;
}
export interface DecisionOption { label: string; consequence: string }
export interface Decision {
  id: string; project_id: string; task_id: string; question: string; context: string;
  options: DecisionOption[]; recommended: number; state: "open" | "answered"; answer: number | null;
}
export interface PullRequest {
  id: string; project_id: string; task_id: string; number: number; title: string; url: string;
  state: "draft" | "open" | "merged"; checks: "pending" | "passing" | "failing";
  additions: number; deletions: number; risk: string;
}
export interface Comment { id: string; path: string; line: number; body: string; author: string; ts: string | number }
export interface ChatMessage { id: string; role: "user" | "coordinator"; text: string; ts: string | number }
export interface Worker { id: string; task_id: string; title: string; cols: number; rows: number }

export interface DaemonEvent<T = unknown> { seq: number; project_id: string; type: string; ts: string | number; payload: T }

const params = new URLSearchParams(location.search);
export const DAEMON = (params.get("daemon") ?? "http://127.0.0.1:7420").replace(/\/$/, "");
export const WS_BASE = DAEMON.replace(/^http/, "ws");

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(DAEMON + path, {
    method,
    headers: body !== undefined ? { "content-type": "application/json" } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${await r.text().catch(() => "")}`);
  const ct = r.headers.get("content-type") ?? "";
  if (r.status === 202 || r.status === 204) return undefined as T;
  if (ct.includes("json")) return (await r.json()) as T;
  return (await r.text()) as unknown as T;
}

export const api = {
  projects: () => req<Project[]>("GET", "/v1/projects"),
  tasks: (pid: string) => req<Task[]>("GET", `/v1/projects/${encodeURIComponent(pid)}/tasks`),
  decisions: () => req<Decision[]>("GET", "/v1/decisions"),
  answer: (id: string, option: number) => req<Decision>("POST", `/v1/decisions/${encodeURIComponent(id)}:answer`, { option }),
  prs: () => req<PullRequest[]>("GET", "/v1/pull-requests"),
  diff: (id: string) => req<string>("GET", `/v1/pull-requests/${encodeURIComponent(id)}/diff`),
  comments: (id: string) => req<Comment[]>("GET", `/v1/pull-requests/${encodeURIComponent(id)}/comments`),
  addComment: (id: string, c: { path: string; line: number; body: string }) =>
    req<Comment>("POST", `/v1/pull-requests/${encodeURIComponent(id)}/comments`, c),
  messages: (cid: string) => req<ChatMessage[]>("GET", `/v1/coordinators/${encodeURIComponent(cid)}/messages`),
  send: (cid: string, text: string) => req<void>("POST", `/v1/coordinators/${encodeURIComponent(cid)}/messages`, { text }),
  workers: () => req<Worker[]>("GET", "/v1/workers"),
  /** Ordered input: one request in flight per worker; keystrokes typed meanwhile are coalesced. */
  input: (wid: string, data: string) => enqueueInput(wid, data),
  resize: (wid: string, cols: number, rows: number) =>
    req<void>("POST", `/v1/workers/${encodeURIComponent(wid)}/resize`, { cols, rows }),
  stress: (wid: string, on: boolean) => req<void>("POST", `/v1/workers/${encodeURIComponent(wid)}/stress`, { on }),
};

const enc = new TextEncoder();
export function utf8ToB64(s: string): string {
  const bytes = enc.encode(s);
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

// Concurrent fetches can reach the daemon out of order (observed: "echo" arriving as "ecoh"
// when typing fast), so input is serialized per worker and coalesced while a POST is in flight.
const inputQ = new Map<string, { buf: string; busy: boolean; waiters: (() => void)[] }>();
function enqueueInput(wid: string, data: string): Promise<void> {
  let q = inputQ.get(wid);
  if (!q) inputQ.set(wid, (q = { buf: "", busy: false, waiters: [] }));
  q.buf += data;
  const done = new Promise<void>((r) => q!.waiters.push(r));
  if (!q.busy) void pump(wid, q);
  return done;
}
async function pump(wid: string, q: { buf: string; busy: boolean; waiters: (() => void)[] }) {
  q.busy = true;
  while (q.buf) {
    const chunk = q.buf, waiters = q.waiters;
    q.buf = ""; q.waiters = [];
    try {
      await req<void>("POST", `/v1/workers/${encodeURIComponent(wid)}/input`, { data_b64: utf8ToB64(chunk) });
    } catch (e) { console.warn("input failed", e); }
    waiters.forEach((w) => w());
  }
  q.busy = false;
}

import { useSyncExternalStore } from "react";
import {
  api, b64ToBytes, ChatMessage, DaemonEvent, Decision, Project, PullRequest, Task, Worker, WS_BASE,
} from "./api";

export interface AppState {
  connected: boolean;
  lastSeq: number;
  events: number;
  reconnects: number;
  error: string | null;
  projects: Project[];
  tasks: Record<string, Task>; // by task id
  decisions: Record<string, Decision>;
  prs: Record<string, PullRequest>;
  chat: Record<string, ChatMessage[]>; // by coordinator (= project) id
  streaming: Record<string, Record<string, string>>; // coordinator id -> message id -> text so far
  workers: Worker[];
}

let state: AppState = {
  connected: false, lastSeq: 0, events: 0, reconnects: 0, error: null,
  projects: [], tasks: {}, decisions: {}, prs: {}, chat: {}, streaming: {}, workers: [],
};

const listeners = new Set<() => void>();
let notifyScheduled = false;
function set(patch: Partial<AppState>) {
  state = { ...state, ...patch };
  // Coalesce React notifications to one per frame: the event stream can deliver
  // hundreds of frames per second under stress.
  if (!notifyScheduled) {
    notifyScheduled = true;
    requestAnimationFrame(() => {
      notifyScheduled = false;
      listeners.forEach((l) => l());
    });
  }
}
export function getState() { return state; }
export function useStore<T>(sel: (s: AppState) => T): T {
  return useSyncExternalStore(
    (l) => { listeners.add(l); return () => listeners.delete(l); },
    () => sel(state),
  );
}

// ---- worker output: a separate, non-React pub/sub so terminals never re-render ----
type OutputListener = (bytes: Uint8Array) => void;
const outputSubs = new Map<string, Set<OutputListener>>();
const outputBacklog = new Map<string, { chunks: Uint8Array[]; size: number }>();
const BACKLOG_CAP = 2 * 1024 * 1024;

export function subscribeOutput(wid: string, fn: OutputListener, replay = true): () => void {
  if (replay) {
    const b = outputBacklog.get(wid);
    if (b) for (const c of b.chunks) fn(c);
  }
  let s = outputSubs.get(wid);
  if (!s) outputSubs.set(wid, (s = new Set()));
  s.add(fn);
  return () => s!.delete(fn);
}
function pushOutput(wid: string, bytes: Uint8Array) {
  let b = outputBacklog.get(wid);
  if (!b) outputBacklog.set(wid, (b = { chunks: [], size: 0 }));
  b.chunks.push(bytes);
  b.size += bytes.length;
  while (b.size > BACKLOG_CAP && b.chunks.length > 1) b.size -= b.chunks.shift()!.length;
  outputSubs.get(wid)?.forEach((fn) => fn(bytes));
}

// ---- generic raw-event taps (used by perf instrumentation) ----
const eventTaps = new Set<(e: DaemonEvent) => void>();
export function tapEvents(fn: (e: DaemonEvent) => void) { eventTaps.add(fn); return () => eventTaps.delete(fn); }

function upsertChat(cid: string, m: ChatMessage) {
  const list = state.chat[cid] ?? [];
  const i = list.findIndex((x) => x.id === m.id);
  const next = i >= 0 ? list.map((x, j) => (j === i ? m : x)) : [...list, m];
  const stream = state.streaming[cid];
  let streaming = state.streaming;
  if (stream && m.id in stream) {
    const { [m.id]: _drop, ...rest } = stream;
    streaming = { ...state.streaming, [cid]: rest };
  }
  set({ chat: { ...state.chat, [cid]: next }, streaming });
}

function apply(e: DaemonEvent) {
  const p = e.payload as any;
  switch (e.type) {
    case "task.created":
    case "task.state_changed": {
      set({ tasks: { ...state.tasks, [p.id]: p as Task } });
      // keep the sidebar's active count roughly live without a refetch
      const active = Object.values(state.tasks).filter(
        (t) => t.project_id === p.project_id && t.state !== "done" && t.state !== "failed",
      ).length;
      set({ projects: state.projects.map((pr) => (pr.id === p.project_id ? { ...pr, active_tasks: active } : pr)) });
      break;
    }
    case "decision.opened":
    case "decision.answered":
      set({ decisions: { ...state.decisions, [p.id]: p as Decision } });
      break;
    case "pr.updated":
      set({ prs: { ...state.prs, [p.id]: p as PullRequest } });
      break;
    case "coordinator.message":
      upsertChat(e.project_id, p as ChatMessage);
      break;
    case "coordinator.delta": {
      const cid = e.project_id;
      if ((state.chat[cid] ?? []).some((m) => m.id === p.message_id)) break; // already complete (replay)
      const cur = state.streaming[cid] ?? {};
      set({ streaming: { ...state.streaming, [cid]: { ...cur, [p.message_id]: (cur[p.message_id] ?? "") + p.text } } });
      break;
    }
    case "worker.output":
      pushOutput(p.worker_id, b64ToBytes(p.data_b64));
      break;
  }
}

let ws: WebSocket | null = null;
let backoff = 250;
function connect() {
  const sock = new WebSocket(`${WS_BASE}/v1/events?cursor=${state.lastSeq}`);
  ws = sock;
  sock.onopen = () => { backoff = 250; set({ connected: true, error: null }); };
  sock.onmessage = (m) => {
    let e: DaemonEvent;
    try { e = JSON.parse(m.data); } catch { return; }
    if (e.seq <= state.lastSeq) return; // duplicate across reconnect
    state.lastSeq = e.seq; // mutate in place: cheap, read by reconnect
    state.events++;
    apply(e);
    eventTaps.forEach((t) => t(e));
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    set({ connected: false, reconnects: state.reconnects + 1 });
    setTimeout(() => { connect(); refreshSnapshots(); }, backoff);
    backoff = Math.min(backoff * 2, 5000);
  };
  sock.onerror = () => set({ error: "event stream error" });
}

export async function refreshSnapshots() {
  try {
    const [projects, decisions, prs, workers] = await Promise.all([api.projects(), api.decisions(), api.prs(), api.workers()]);
    const taskLists = await Promise.all(projects.map((p) => api.tasks(p.id)));
    const tasks: Record<string, Task> = { ...state.tasks };
    for (const l of taskLists) for (const t of l) tasks[t.id] = t;
    const chatLists = await Promise.all(projects.map((p) => api.messages(p.id).catch(() => [] as ChatMessage[])));
    const chat = { ...state.chat };
    projects.forEach((p, i) => {
      const merged = new Map((chat[p.id] ?? []).map((m) => [m.id, m]));
      for (const m of chatLists[i]) merged.set(m.id, m);
      chat[p.id] = [...merged.values()];
    });
    set({
      projects, tasks, chat, workers,
      decisions: Object.fromEntries(decisions.map((d) => [d.id, d])),
      prs: Object.fromEntries(prs.map((x) => [x.id, x])),
      error: null,
    });
  } catch (err) {
    set({ error: String(err) });
    setTimeout(refreshSnapshots, 2000);
  }
}

let started = false;
export function start() {
  if (started) return;
  started = true;
  refreshSnapshots();
  connect();
}
/** For testing reconnect: drop the socket; onclose resumes from lastSeq. */
export function dropConnection() { ws?.close(); }

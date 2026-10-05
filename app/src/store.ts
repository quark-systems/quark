// App state fed by REST snapshots plus the event stream. Every event is applied through
// `applyEvent`, a pure function, so replays after a reconnect are idempotent.
import { useSyncExternalStore } from "react";
import {
  Account, AccountQuotaChanged, api, CheckUpdated, DaemonEvent, Decision, Health, NotAvailable, Project, PullRequest, ReviewUpdated, Task, TerminalOutput, TranscriptEntry,
  TranscriptItem, wsUrl,
} from "./api";

export interface AppState {
  connected: boolean;
  lastSeq: number;
  error: string | null;
  health: Health | null;
  projects: Record<string, Project>;
  tasks: Record<string, Task>;
  decisions: Record<string, Decision>;
  /** Coordinator transcripts by coordinator id (= Project id). Absent until loaded. */
  chat: Record<string, TranscriptItem[]>;
  /** Worker transcripts by task id. Absent until loaded. */
  transcripts: Record<string, TranscriptItem[]>;
  /** Count of `task.event` events per task, so views can refetch what a task changed. */
  taskActivity: Record<string, number>;
  /** PR center: pull requests across Projects by id. */
  pullRequests: Record<string, PullRequest>;
  /** Whether the daemon serves the PR center; null until the first load answers. */
  prsAvailable: boolean | null;
  /** Count of `pr.updated`, `check.updated` and `review.updated` events per PR, so views can refetch it. */
  prActivity: Record<string, number>;
  /** Accounts per harness by id, in the daemon's order (`order`). */
  accounts: Record<string, Account>;
  accountOrder: string[];
  /** Whether the daemon serves accounts; null until the first load answers. */
  accountsAvailable: boolean | null;
}

export const initialState: AppState = {
  connected: false, lastSeq: 0, error: null, health: null,
  projects: {}, tasks: {}, decisions: {}, chat: {}, transcripts: {}, taskActivity: {},
  pullRequests: {}, prsAvailable: null, prActivity: {},
  accounts: {}, accountOrder: [], accountsAvailable: null,
};

function upsertById<T extends { id: string | number }>(list: T[] | undefined, item: T): T[] {
  const cur = list ?? [];
  const i = cur.findIndex((x) => x.id === item.id);
  return i >= 0 ? cur.map((x, j) => (j === i ? item : x)) : [...cur, item];
}

function upsertBy<T>(list: T[], item: T, key: keyof T): T[] {
  const i = list.findIndex((x) => x[key] === item[key]);
  return i >= 0 ? list.map((x, j) => (j === i ? item : x)) : [...list, item];
}

/** Applies one event. Output bytes are routed separately (see `onOutput`), not kept in state. */
export function applyEvent(s: AppState, e: DaemonEvent): AppState {
  const p = e.payload as any;
  switch (e.type) {
    case "project.updated":
      return { ...s, projects: { ...s.projects, [p.id]: { ...s.projects[p.id], ...p } } };
    case "task.created":
      return { ...s, tasks: { ...s.tasks, [p.id]: { ...s.tasks[p.id], ...p } } };
    case "task.state_changed": {
      // quarkd sends `{ task, previous_state }`.
      const t = p?.task as Task | undefined;
      if (typeof t?.id !== "string") return s;
      return { ...s, tasks: { ...s.tasks, [t.id]: { ...s.tasks[t.id], ...t } } };
    }
    case "decision.opened":
    case "decision.answered":
      return { ...s, decisions: { ...s.decisions, [p.id]: p as Decision } };
    case "coordinator.message": {
      // Transcript entries are identified by the seq of the event that carried them.
      const cid = p.coordinator_id ?? e.project_id;
      if (!cid || !(cid in s.chat) || !p.entry) return s; // loaded on demand; the fetch includes it
      const item: TranscriptItem = { ...(p.entry as TranscriptEntry), id: e.seq };
      return { ...s, chat: { ...s.chat, [cid]: upsertById(s.chat[cid], item) } };
    }
    case "worker.transcript": {
      const tid = p.task_id as string;
      if (!(tid in s.transcripts) || !p.entry) return s;
      const item: TranscriptItem = { ...(p.entry as TranscriptEntry), id: e.seq };
      return { ...s, transcripts: { ...s.transcripts, [tid]: upsertById(s.transcripts[tid], item) } };
    }
    case "task.event": {
      const tid = p.task_id as string;
      if (!tid) return s;
      return { ...s, taskActivity: { ...s.taskActivity, [tid]: (s.taskActivity[tid] ?? 0) + 1 } };
    }
    case "pr.updated": {
      if (typeof p?.id !== "string") return s;
      return {
        ...s,
        pullRequests: { ...s.pullRequests, [p.id]: { ...s.pullRequests[p.id], ...(p as PullRequest) } },
        prActivity: { ...s.prActivity, [p.id]: (s.prActivity[p.id] ?? 0) + 1 },
      };
    }
    case "check.updated":
    case "review.updated": {
      // Merge the one check or review into a loaded PR; views also refetch it for the rolled-up states.
      const id = p?.pull_request_id as string | undefined;
      if (!id) return s;
      const next = { ...s, prActivity: { ...s.prActivity, [id]: (s.prActivity[id] ?? 0) + 1 } };
      const cur = s.pullRequests[id];
      if (cur && e.type === "check.updated" && p.check?.name) {
        const c = p as CheckUpdated;
        const stale = c.head_sha && cur.head_sha && c.head_sha !== cur.head_sha;
        if (!stale) next.pullRequests = { ...s.pullRequests, [id]: { ...cur, checks: upsertBy(cur.checks, c.check, "name") } };
      } else if (cur && e.type === "review.updated" && p.review?.id) {
        next.pullRequests = { ...s.pullRequests, [id]: { ...cur, reviews: upsertBy(cur.reviews, (p as ReviewUpdated).review, "id") } };
      }
      return next;
    }
    case "account.quota_changed": {
      const q = p as AccountQuotaChanged;
      const cur = q?.account_id ? s.accounts[q.account_id] : undefined;
      if (!cur || !q.quota) return s; // not loaded yet; the next load includes it
      return { ...s, accounts: { ...s.accounts, [cur.id]: { ...cur, quota: q.quota } } };
    }
    default:
      return s;
  }
}

let state: AppState = initialState;
const listeners = new Set<() => void>();
let notifyScheduled = false;
const raf: (cb: () => void) => void =
  typeof requestAnimationFrame === "function" ? (cb) => requestAnimationFrame(cb) : (cb) => setTimeout(cb, 16);

function commit(next: AppState) {
  if (next === state) return;
  state = next;
  // One React notification per frame: terminal-heavy streams deliver hundreds of events a second.
  if (!notifyScheduled) {
    notifyScheduled = true;
    raf(() => { notifyScheduled = false; listeners.forEach((l) => l()); });
  }
}
function set(patch: Partial<AppState>) { commit({ ...state, ...patch }); }

export function getState() { return state; }
export function useStore<T>(sel: (s: AppState) => T): T {
  return useSyncExternalStore(
    (l) => { listeners.add(l); return () => listeners.delete(l); },
    () => sel(state),
  );
}

// ---- terminal output: a non-React pub/sub so terminals never re-render ----
export type OutputListener = (chunk: TerminalOutput, seq: number) => void;
const outputSubs = new Map<string, Set<OutputListener>>();
const backlog = new Map<string, { chunks: { chunk: TerminalOutput; seq: number }[]; size: number }>();
const BACKLOG_CAP = 2 * 1024 * 1024;

/** Subscribe to a terminal's `worker.output` chunks. `replay` first delivers what arrived before. */
export function onOutput(terminalId: string, fn: OutputListener, replay = true): () => void {
  if (replay) for (const c of backlog.get(terminalId)?.chunks ?? []) fn(c.chunk, c.seq);
  let subs = outputSubs.get(terminalId);
  if (!subs) outputSubs.set(terminalId, (subs = new Set()));
  subs.add(fn);
  return () => subs!.delete(fn);
}
function pushOutput(terminalId: string, chunk: TerminalOutput, seq: number) {
  let b = backlog.get(terminalId);
  if (!b) backlog.set(terminalId, (b = { chunks: [], size: 0 }));
  // A snapshot redraws the whole screen, so nothing before it is worth keeping.
  if (chunk.kind === "snapshot") { b.chunks = []; b.size = 0; }
  b.chunks.push({ chunk, seq });
  b.size += chunk.data_b64.length;
  while (b.size > BACKLOG_CAP && b.chunks.length > 1) b.size -= b.chunks.shift()!.chunk.data_b64.length;
  outputSubs.get(terminalId)?.forEach((fn) => fn(chunk, seq));
}

export function handleEvent(e: DaemonEvent) {
  if (e.seq <= state.lastSeq) return; // duplicate across a reconnect
  if (e.type === "worker.output") {
    const p = e.payload as TerminalOutput;
    if (p.terminal_id) pushOutput(p.terminal_id, p, e.seq);
  }
  const applied = applyEvent(state, e);
  // The cursor always advances; listeners only hear about changes they can see.
  if (applied === state) state = { ...state, lastSeq: e.seq };
  else commit({ ...applied, lastSeq: e.seq });
}

// ---- loading ----
export async function refreshSnapshots() {
  const [projects, decisions] = await Promise.all([
    api.projects(),
    api.decisions().catch(() => [] as Decision[]),
  ]);
  const lists = await Promise.all(projects.map((p) => api.tasks(p.id).catch(() => [] as Task[])));
  const tasks: Record<string, Task> = {};
  for (const l of lists) for (const t of l) tasks[t.id] = t;
  set({
    projects: Object.fromEntries(projects.map((p) => [p.id, p])),
    tasks,
    decisions: Object.fromEntries(decisions.map((d) => [d.id, d])),
    error: null,
  });
  await loadPullRequests().catch(() => undefined);
}

export async function refreshTask(id: string) {
  const t = await api.task(id);
  set({ tasks: { ...state.tasks, [t.id]: t } });
}

/** Loads a coordinator's chat; later `coordinator.message` events append to it. */
export async function loadChat(cid: string): Promise<"ok" | "unavailable"> {
  try {
    const msgs = await api.chat(cid);
    const merged = new Map((state.chat[cid] ?? []).map((m) => [m.id, m]));
    for (const m of msgs) merged.set(m.id, m);
    set({ chat: { ...state.chat, [cid]: [...merged.values()] } });
    return "ok";
  } catch (e) {
    if (e instanceof NotAvailable) return "unavailable";
    throw e;
  }
}

/** Loads a worker transcript; later `worker.transcript` events append to it. */
export async function loadTranscript(taskId: string): Promise<"ok" | "unavailable"> {
  try {
    const entries = await api.transcript(taskId);
    const merged = new Map((state.transcripts[taskId] ?? []).map((m) => [m.id, m]));
    for (const m of entries) merged.set(m.id, m);
    set({ transcripts: { ...state.transcripts, [taskId]: [...merged.values()] } });
    return "ok";
  } catch (e) {
    if (e instanceof NotAvailable) return "unavailable";
    throw e;
  }
}

/** Records a decision the daemon returned, e.g. from answering it; the event may arrive before or after. */
export function upsertDecision(d: Decision) {
  set({ decisions: { ...state.decisions, [d.id]: d } });
}

/** Loads every PR across Projects. Answers "unavailable" while the daemon has no PR center. */
export async function loadPullRequests(): Promise<"ok" | "unavailable"> {
  try {
    const prs = await api.pullRequests();
    set({ pullRequests: Object.fromEntries(prs.map((x) => [x.id, x])), prsAvailable: true });
    return "ok";
  } catch (e) {
    if (e instanceof NotAvailable) { set({ prsAvailable: false }); return "unavailable"; }
    throw e;
  }
}

export async function refreshPullRequest(id: string) {
  const pr = await api.pullRequest(id);
  set({ pullRequests: { ...state.pullRequests, [pr.id]: pr } });
  return pr;
}

export function setPullRequest(pr: PullRequest) {
  set({ pullRequests: { ...state.pullRequests, [pr.id]: pr } });
}

/** Loads every account. Answers "unavailable" while the daemon has no accounts. */
export async function loadAccounts(refresh = false): Promise<"ok" | "unavailable"> {
  try {
    setAccounts(await api.accounts(refresh));
    return "ok";
  } catch (e) {
    if (e instanceof NotAvailable) { set({ accountsAvailable: false }); return "unavailable"; }
    throw e;
  }
}

export function setAccounts(list: Account[]) {
  set({ accounts: Object.fromEntries(list.map((a) => [a.id, a])), accountOrder: list.map((a) => a.id), accountsAvailable: true });
}

export function addProject(p: Project) {
  set({ projects: { ...state.projects, [p.id]: p } });
}

// ---- event stream ----
let ws: WebSocket | null = null;
let backoff = 250;
let generation = 0;

function connect(cursor: number | null) {
  const gen = generation;
  const sock = new WebSocket(wsUrl("/v1/events" + (cursor === null ? "" : `?cursor=${cursor}`)));
  ws = sock;
  sock.onopen = () => { backoff = 250; set({ connected: true, error: null }); };
  sock.onmessage = (m) => {
    let e: DaemonEvent;
    try { e = JSON.parse(m.data); } catch { return; }
    handleEvent(e);
  };
  sock.onclose = () => {
    if (ws !== sock || gen !== generation) return;
    set({ connected: false });
    setTimeout(() => {
      if (gen !== generation) return;
      if (!state.health) { void start(); return; } // never reached it: start over
      // Resume from the cursor; refetch snapshots too, since retention is bounded.
      connect(state.lastSeq);
      refreshSnapshots().catch((err) => set({ error: String(err.message ?? err) }));
    }, backoff);
    backoff = Math.min(backoff * 2, 5000);
  };
  sock.onerror = () => set({ error: "Lost the connection to the daemon" });
}

/** (Re)connects to the current daemon: snapshot cursor first, then REST snapshots, then events. */
export async function start() {
  generation++;
  const gen = generation;
  ws?.close();
  ws = null;
  state = { ...initialState };
  set({});
  backlog.clear();
  let cursor: number | null = null;
  try {
    const health = await api.health();
    set({ health });
    cursor = health.last_seq;
  } catch (e) {
    set({ error: `Cannot reach the daemon: ${String((e as Error).message ?? e)}` });
  }
  if (gen !== generation) return;
  state = { ...state, lastSeq: cursor ?? 0 };
  try { await refreshSnapshots(); } catch (e) { set({ error: String((e as Error).message ?? e) }); }
  if (gen !== generation) return;
  connect(cursor);
}

// One terminal emulator per daemon terminal (a worker's task id, or a Project id for its
// coordinator), created on first view and kept outside React. Leaving the worker view only
// detaches the host element, so coming back never replays scrollback.
//
// Opening follows quarkd's terminal contract: with the event stream already subscribed, ask
// for a snapshot (an appended `worker.output` event of kind "snapshot"), apply it, then apply
// only that terminal's events with a greater `seq`. A snapshot arriving later (a resync)
// resets the emulator to its size and redraws.
import { api, ApiError, b64ToBytes, NotAvailable, Terminal, TerminalOutput } from "./api";
import { onOutput } from "./store";
import { AdapterOptions, TermAdapter, TERM_FONT } from "./term/types";

/** xterm.js DOM renderer on Linux until WebGL is measured on a GPU (ADR-3); WebGL elsewhere. */
export function defaultRenderer(): string {
  const q = new URLSearchParams(location.search).get("renderer");
  if (q) return q;
  return /linux/i.test(navigator.userAgent) && !/android/i.test(navigator.userAgent) ? "dom" : "webgl";
}

export type TermStatus = "connecting" | "live" | "unavailable" | "error";

export interface TermHandle {
  id: string;
  host: HTMLDivElement;
  adapter: TermAdapter | null;
  status: TermStatus;
  info: Terminal | null;
  /** Why the terminal is unavailable or failed, for display. */
  error: string | null;
  onStatus: Set<() => void>;
  unsub: (() => void) | null;
}

const terms = new Map<string, TermHandle>();

export function getTerm(id: string): TermHandle {
  let h = terms.get(id);
  if (h) return h;
  const host = document.createElement("div");
  host.className = "term-host";
  h = { id, host, adapter: null, status: "connecting", info: null, error: null, onStatus: new Set(), unsub: null };
  terms.set(id, h);
  void open(h);
  return h;
}

function setStatus(h: TermHandle, status: TermStatus, error: string | null = null) {
  h.status = status;
  h.error = error;
  h.onStatus.forEach((f) => f());
}

/** Writes one chunk; a snapshot first resets the emulator to the snapshot's size. */
export function applyChunk(a: TermAdapter, c: TerminalOutput) {
  if (c.kind === "snapshot") a.reset(c.cols ?? undefined, c.rows ?? undefined);
  a.write(b64ToBytes(c.data_b64));
}

async function open(h: TermHandle) {
  // Buffer chunks (with their seq) until the snapshot says where to start.
  const pending: { chunk: TerminalOutput; seq: number }[] = [];
  let minSeq = -1; // apply only seq > minSeq; -1 until the snapshot is in
  h.unsub = onOutput(h.id, (chunk, seq) => {
    if (!h.adapter || minSeq < 0) pending.push({ chunk, seq });
    else if (seq > minSeq) { applyChunk(h.adapter, chunk); minSeq = seq; }
  }, false);

  let info: Terminal;
  try {
    info = await api.terminal(h.id);
  } catch (e) {
    h.unsub?.();
    if (e instanceof NotAvailable) setStatus(h, "unavailable", "Live terminals are not available from this daemon yet");
    else if (e instanceof ApiError && e.code === "unavailable") setStatus(h, "unavailable", "The daemon cannot reach tmux, so live terminals are off");
    else setStatus(h, "error", String((e as Error).message ?? e));
    return;
  }
  h.info = info;

  const { createXterm } = await import("./term/xterm");
  const opts: AdapterOptions = {
    cols: info.cols || 120, rows: info.rows || 36, fontFamily: TERM_FONT, fontSize: 13, lineHeight: 1.15,
    renderer: defaultRenderer(),
  };
  let resizeTimer = 0;
  const adapter = createXterm(opts, {
    onData: (d) => { api.terminalInput(h.id, d).catch((e) => console.warn("terminal input failed", e)); },
    onResize: (cols, rows) => {
      clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => api.terminalResize(h.id, cols, rows).catch(() => {}), 100);
    },
  });
  // xterm measures fonts on open, so wait for the host to be in the document.
  await whenAttached(h.host);
  await adapter.open(h.host);

  try {
    const snap = await api.terminalSnapshot(h.id);
    applyChunk(adapter, snap.payload);
    minSeq = snap.seq;
  } catch (e) {
    // No snapshot: show whatever streams from here on.
    console.warn("terminal snapshot failed", e);
    minSeq = 0;
  }
  for (const c of pending.splice(0)) if (c.seq > minSeq) { applyChunk(adapter, c.chunk); minSeq = c.seq; }
  h.adapter = adapter;
  adapter.fit();
  setStatus(h, "live");
}

function whenAttached(el: HTMLElement): Promise<void> {
  if (el.isConnected) return Promise.resolve();
  return new Promise((resolve) => {
    const t = setInterval(() => { if (el.isConnected) { clearInterval(t); resolve(); } }, 30);
  });
}

/** Drops a terminal so the next view opens it afresh (e.g. after an error). */
export function resetTerm(id: string) {
  const h = terms.get(id);
  if (!h) return;
  terms.delete(id);
  h.unsub?.();
  h.host.remove();
  (h.adapter?.raw() as { dispose?: () => void } | undefined)?.dispose?.();
}

export function attach(h: TermHandle, container: HTMLElement) {
  if (h.host.parentElement !== container) container.appendChild(h.host);
  h.adapter?.fit();
}

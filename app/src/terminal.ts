// One terminal per task, created on first view and kept outside React. Leaving the worker
// view only detaches the host element, so coming back never replays scrollback.
//
// Opening a terminal asks the daemon for the pane's current screen (`snapshot_b64` with the
// `seq` it reflects) and then applies only newer `worker.output` events. Without a snapshot
// it falls back to the output buffered since the app connected.
import { api, b64ToBytes, NotAvailable, TerminalInfo } from "./api";
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
  taskId: string;
  host: HTMLDivElement;
  adapter: TermAdapter | null;
  status: TermStatus;
  info: TerminalInfo | null;
  error: string | null;
  onStatus: Set<() => void>;
  unsub: (() => void) | null;
}

const terms = new Map<string, TermHandle>();

export function getTerm(taskId: string): TermHandle {
  let h = terms.get(taskId);
  if (h) return h;
  const host = document.createElement("div");
  host.className = "term-host";
  h = { taskId, host, adapter: null, status: "connecting", info: null, error: null, onStatus: new Set(), unsub: null };
  terms.set(taskId, h);
  void open(h);
  return h;
}

function setStatus(h: TermHandle, status: TermStatus, error: string | null = null) {
  h.status = status;
  h.error = error;
  h.onStatus.forEach((f) => f());
}

async function open(h: TermHandle) {
  // Buffer output (with its seq) until we know where the snapshot ends.
  const pending: { bytes: Uint8Array; seq: number }[] = [];
  let minSeq = -1; // apply only seq > minSeq; -1 until the snapshot question is settled
  const unsub = onOutput(h.taskId, (bytes, seq) => {
    if (!h.adapter || minSeq < 0) pending.push({ bytes, seq });
    else if (seq > minSeq) h.adapter.write(bytes);
  });
  h.unsub = unsub;

  let info: TerminalInfo | null = null;
  try {
    info = await api.terminal(h.taskId);
  } catch (e) {
    if (e instanceof NotAvailable) { unsub(); setStatus(h, "unavailable"); return; }
    setStatus(h, "error", String((e as Error).message ?? e));
    // Keep going: live output may still arrive.
  }
  h.info = info;

  const { createXterm } = await import("./term/xterm");
  const opts: AdapterOptions = {
    cols: info?.cols || 120, rows: info?.rows || 36, fontFamily: TERM_FONT, fontSize: 13, lineHeight: 1.15,
    renderer: defaultRenderer(),
  };
  let resizeTimer = 0;
  const adapter = createXterm(opts, {
    onData: (d) => { api.terminalInput(h.taskId, d).catch((e) => console.warn("terminal input failed", e)); },
    onResize: (cols, rows) => {
      clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => api.terminalResize(h.taskId, cols, rows).catch(() => {}), 100);
    },
  });
  // xterm measures fonts on open, so wait for the host to be in the document.
  await whenAttached(h.host);
  await adapter.open(h.host);

  if (info?.snapshot_b64) {
    adapter.write(b64ToBytes(info.snapshot_b64));
    minSeq = info.seq;
  } else {
    minSeq = 0;
  }
  for (const c of pending.splice(0)) if (c.seq > minSeq) adapter.write(c.bytes);
  h.adapter = adapter;
  adapter.fit();
  if (h.status !== "error") setStatus(h, "live");
}

function whenAttached(el: HTMLElement): Promise<void> {
  if (el.isConnected) return Promise.resolve();
  return new Promise((resolve) => {
    const t = setInterval(() => { if (el.isConnected) { clearInterval(t); resolve(); } }, 30);
  });
}

/** Drops a task's terminal so the next view opens it afresh (e.g. after an error). */
export function resetTerm(taskId: string) {
  const h = terms.get(taskId);
  if (!h) return;
  terms.delete(taskId);
  h.unsub?.();
  h.host.remove();
  (h.adapter?.raw() as { dispose?: () => void } | undefined)?.dispose?.();
}

export function attach(h: TermHandle, container: HTMLElement) {
  if (h.host.parentElement !== container) container.appendChild(h.host);
  h.adapter?.fit();
}

// Terminal instances live outside React: one per worker, created once and re-parented
// into whatever container the Terminals screen renders, so switching screens never
// tears down a terminal or replays its scrollback.
//
// The implementation is chosen by ?renderer=:
//   webgl (default) / dom  -> xterm.js with the WebGL addon / DOM renderer
//   wterm                  -> @wterm/dom DOM renderer + @wterm/ghostty core (WASM)
//   wterm-lite             -> @wterm/dom with its built-in Zig core (WASM)
//   ghostty-web            -> ghostty-web (Ghostty WASM core, canvas2d renderer)
// Each one is a separate dynamically imported chunk, so the bundle cost per renderer is visible.
import { api, Worker } from "./api";
import { subscribeOutput } from "./store";
import { latency } from "./perf";
import type { AdapterCallbacks, AdapterOptions, TermAdapter } from "./term/types";
import { TERM_FONT } from "./term/types";

export const RENDERER = new URLSearchParams(location.search).get("renderer") ?? "webgl";

export interface TermHandle {
  id: string;
  host: HTMLDivElement;
  adapter: TermAdapter | null;
  renderer: string;
  probe: boolean;
  ready: Promise<void>;
  /** load+open duration in ms (includes WASM fetch/compile for the WASM renderers) */
  openMs: number;
}

const terms = new Map<string, TermHandle>();
let probeWorker: string | null = new URLSearchParams(location.search).get("probe");
/** Test hook: every onData string from any terminal. */
export const dataTaps = new Set<(wid: string, d: string) => void>();

export function chooseProbe(workers: Worker[]): string | null {
  if (probeWorker && workers.some((w) => w.id === probeWorker)) return probeWorker;
  const bash = workers.find((w) => /bash|shell/i.test(w.title)) ?? workers[0];
  probeWorker = bash?.id ?? null;
  return probeWorker;
}
export function getProbeWorker() { return probeWorker; }
export function allTerms() { return [...terms.values()]; }

async function createAdapter(o: AdapterOptions, cb: AdapterCallbacks): Promise<TermAdapter> {
  switch (o.renderer) {
    case "wterm":
    case "wterm-lite":
      return (await import("./term/wterm")).createWterm(o, cb);
    case "ghostty-web":
      return (await import("./term/ghostty-web")).createGhosttyWeb(o, cb);
    default:
      return (await import("./term/xterm")).createXterm(o, cb);
  }
}

export function getTerm(w: Worker): TermHandle {
  let h = terms.get(w.id);
  if (h) return h;
  const host = document.createElement("div");
  host.className = "xterm-host";
  let resizeTimer = 0;
  const cb: AdapterCallbacks = {
    onData: (d) => { dataTaps.forEach((f) => f(w.id, d)); api.input(w.id, d).catch(() => {}); },
    onResize: (cols, rows) => {
      clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => api.resize(w.id, cols, rows).catch(() => {}), 80);
    },
  };
  let opened!: () => void;
  const handle: TermHandle = {
    id: w.id, host, adapter: null, renderer: RENDERER, probe: w.id === probeWorker,
    ready: new Promise<void>((r) => (opened = r)), openMs: NaN,
  };
  h = handle;
  terms.set(w.id, handle);

  // Keystroke timestamps for the latency probe: capture phase on the host, before the
  // terminal's own key handling, identical for every implementation.
  host.addEventListener("keydown", (ev) => {
    if (handle.probe && ev.key.length === 1 && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
      latency.keydown(ev.key, ev.timeStamp || performance.now());
    }
  }, true);

  // Output that arrives before the (async) terminal is open is queued.
  const queue: Uint8Array[] = [];
  const dec = new TextDecoder();
  subscribeOutput(w.id, (bytes) => {
    const a = handle.adapter;
    if (!a) { queue.push(bytes); return; }
    if (!handle.probe || latency.pending.length === 0) { a.write(bytes); return; }
    const hits = latency.match(dec.decode(bytes, { stream: true }));
    if (!hits.length) { a.write(bytes); return; }
    const tArrive = performance.now();
    for (const k of hits) latency.recordNet(tArrive - k.t0);
    a.write(bytes, () => {
      const t1 = performance.now();
      for (const k of hits) latency.record(t1 - k.t0);
    });
  });

  // Opening needs the host in the document (font measurement), so it starts on first attach.
  (handle as any)._start = async () => {
    const t0 = performance.now();
    const a = await createAdapter(
      { cols: w.cols || 80, rows: w.rows || 24, fontFamily: TERM_FONT, fontSize: 13, lineHeight: 1.1, renderer: RENDERER },
      cb,
    );
    await a.open(host);
    handle.openMs = performance.now() - t0;
    handle.renderer = a.kind;
    for (const b of queue.splice(0)) a.write(b);
    handle.adapter = a;
    a.fit();
    opened();
  };
  return handle;
}

export function attach(h: TermHandle, container: HTMLElement) {
  if (h.host.parentElement !== container) container.appendChild(h.host);
  const start = (h as any)._start as (() => Promise<void>) | undefined;
  if (start) {
    delete (h as any)._start;
    start().catch((e) => { console.error("terminal open failed", e); h.renderer = "error: " + String(e); });
  }
  h.adapter?.fit();
}

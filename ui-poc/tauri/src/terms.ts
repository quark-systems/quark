// xterm.js instances live outside React: one per worker, created once and re-parented
// into whatever container the Terminals screen renders, so switching screens never
// tears down a terminal or replays its scrollback.
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { api, Worker } from "./api";
import { subscribeOutput } from "./store";
import { latency } from "./perf";

export interface TermHandle {
  id: string;
  term: Terminal;
  fit: FitAddon;
  host: HTMLDivElement;
  renderer: "webgl" | "dom";
  probe: boolean;
}

const terms = new Map<string, TermHandle>();
const RENDERER = new URLSearchParams(location.search).get("renderer") ?? "webgl";
let probeWorker: string | null = new URLSearchParams(location.search).get("probe");

export function chooseProbe(workers: Worker[]): string | null {
  if (probeWorker && workers.some((w) => w.id === probeWorker)) return probeWorker;
  const bash = workers.find((w) => /bash|shell/i.test(w.title)) ?? workers[0];
  probeWorker = bash?.id ?? null;
  return probeWorker;
}
export function getProbeWorker() { return probeWorker; }
export function allTerms() { return [...terms.values()]; }

export const TERM_THEME = {
  background: "#0d0e12",
  foreground: "#d7dae0",
  cursor: "#9d8cff",
  cursorAccent: "#0d0e12",
  selectionBackground: "#3a3560",
  black: "#1b1d23", red: "#f2777a", green: "#99cc99", yellow: "#ffcc66",
  blue: "#6699cc", magenta: "#cc99cc", cyan: "#66cccc", white: "#d3d0c8",
  brightBlack: "#5c6370", brightRed: "#ff8b8e", brightGreen: "#b5e8b5", brightYellow: "#ffe08a",
  brightBlue: "#8fb8ff", brightMagenta: "#e2b3ff", brightCyan: "#8ff0f0", brightWhite: "#ffffff",
};

export function getTerm(w: Worker): TermHandle {
  let h = terms.get(w.id);
  if (h) return h;
  const host = document.createElement("div");
  host.className = "xterm-host";
  const term = new Terminal({
    cols: w.cols || 80,
    rows: w.rows || 24,
    fontFamily: '"JetBrains Mono", "DejaVu Sans Mono", "Liberation Mono", "Noto Color Emoji", monospace',
    fontSize: 13,
    lineHeight: 1.1,
    scrollback: 5000,
    theme: TERM_THEME,
    allowProposedApi: true,
    cursorBlink: false,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  const isProbe = w.id === probeWorker;
  h = { id: w.id, term, fit, host, renderer: "dom", probe: isProbe };
  terms.set(w.id, h);

  // Keystroke timestamps for the latency probe: taken at keydown, before xterm processes it.
  term.attachCustomKeyEventHandler((ev) => {
    if (ev.type === "keydown" && h!.probe && ev.key.length === 1 && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
      latency.keydown(ev.key, ev.timeStamp || performance.now());
    }
    return true;
  });
  term.onData((d) => { api.input(w.id, d).catch(() => {}); });
  term.onBinary((d) => { api.input(w.id, d).catch(() => {}); });

  let resizeTimer = 0;
  term.onResize(({ cols, rows }) => {
    clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(() => api.resize(w.id, cols, rows).catch(() => {}), 80);
  });

  const dec = new TextDecoder();
  subscribeOutput(w.id, (bytes) => {
    if (!h!.probe || latency.pending.length === 0) {
      term.write(bytes);
      return;
    }
    const hits = latency.match(dec.decode(bytes, { stream: true }));
    if (!hits.length) { term.write(bytes); return; }
    const tArrive = performance.now();
    for (const k of hits) latency.recordNet(tArrive - k.t0);
    term.write(bytes, () => {
      // Parsed into the buffer; the glyph is on screen after xterm's next render pass.
      let done = false;
      const finish = () => {
        if (done) return;
        done = true;
        disp.dispose();
        const t1 = performance.now();
        for (const k of hits) latency.record(t1 - k.t0);
      };
      const disp = term.onRender(finish);
      // fallback if no render is scheduled (e.g. echo landed off-viewport)
      requestAnimationFrame(() => requestAnimationFrame(finish));
    });
  });
  return h;
}

export function attach(h: TermHandle, container: HTMLElement) {
  if (h.host.parentElement !== container) container.appendChild(h.host);
  if (!h.term.element) {
    h.term.open(h.host);
    if (RENDERER !== "dom") try {
      const gl = new WebglAddon();
      gl.onContextLoss(() => { gl.dispose(); h.renderer = "dom"; });
      h.term.loadAddon(gl);
      h.renderer = "webgl";
    } catch (e) {
      console.warn("webgl addon failed, using DOM renderer", e);
      h.renderer = "dom";
    }
  }
  try { h.fit.fit(); } catch { /* not laid out yet */ }
}

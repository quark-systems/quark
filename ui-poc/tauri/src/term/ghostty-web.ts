// ghostty-web: Ghostty's VT core in WASM with an xterm.js-compatible API and a canvas2d renderer.
import { init, Terminal, FitAddon } from "ghostty-web";
import { AdapterCallbacks, AdapterOptions, TermAdapter, TERM_THEME } from "./types";

let ready: Promise<void> | null = null;

export function createGhosttyWeb(o: AdapterOptions, cb: AdapterCallbacks): TermAdapter {
  let term: Terminal | null = null;
  let fit: FitAddon | null = null;
  return {
    kind: "ghostty-web",
    async open(host) {
      ready ??= init();
      await ready;
      term = new Terminal({
        cols: o.cols, rows: o.rows, fontFamily: o.fontFamily, fontSize: o.fontSize,
        scrollback: 5000, theme: TERM_THEME, cursorBlink: false,
      } as any);
      fit = new FitAddon();
      term.loadAddon(fit);
      term.onData(cb.onData);
      term.onResize(({ cols, rows }) => cb.onResize(cols, rows));
      term.open(host);
    },
    write(bytes, rendered) {
      // write() is synchronous; the callback runs in a rAF registered after the render loop's,
      // i.e. after this frame's canvas paint.
      term!.write(bytes, rendered);
    },
    fit() { try { fit?.fit(); } catch { /* hidden */ } },
    focus() { term?.focus(); },
    simulateInput(d) { term!.input(d, true); },
    getSelection() { return term?.getSelection() ?? ""; },
    async cursorLine() {
      if (!term) return "";
      const b = term.buffer.active;
      return b.getLine(b.cursorY + b.viewportY)?.translateToString(true) ?? "";
    },
    inputElement() { return term?.textarea ?? null; },
    raw() { return term; },
  };
}

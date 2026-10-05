import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { AdapterCallbacks, AdapterOptions, TermAdapter, TERM_THEME } from "./types";

export function createXterm(o: AdapterOptions, cb: AdapterCallbacks): TermAdapter {
  const term = new Terminal({
    cols: o.cols, rows: o.rows, fontFamily: o.fontFamily, fontSize: o.fontSize, lineHeight: o.lineHeight,
    scrollback: 5000, theme: TERM_THEME, allowProposedApi: true, cursorBlink: false,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.onData(cb.onData);
  term.onBinary(cb.onData);
  term.onResize(({ cols, rows }) => cb.onResize(cols, rows));
  let kind = o.renderer === "dom" ? "xterm-dom" : "xterm-webgl";
  return {
    get kind() { return kind; },
    async open(host) {
      term.open(host);
      if (o.renderer !== "dom") {
        try {
          const gl = new WebglAddon();
          gl.onContextLoss(() => { gl.dispose(); kind = "xterm-dom"; });
          term.loadAddon(gl);
        } catch (e) { console.warn("webgl addon failed", e); kind = "xterm-dom"; }
      }
    },
    write(bytes) { term.write(bytes); },
    reset(cols, rows) {
      term.reset();
      if (cols && rows && (cols !== term.cols || rows !== term.rows)) term.resize(cols, rows);
    },
    fit() { try { fit.fit(); } catch { /* not laid out */ } },
    focus() { term.focus(); },
    text() {
      const b = term.buffer.active, lines: string[] = [];
      for (let i = 0; i < b.length; i++) lines.push(b.getLine(i)?.translateToString(true) ?? "");
      return lines.join("\n");
    },
    raw() { return term; },
  };
}

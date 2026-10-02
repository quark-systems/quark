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
  term.onBinary((d) => (cb.onBinary ?? cb.onData)(d));
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
    write(bytes, rendered) {
      if (!rendered) { term.write(bytes); return; }
      term.write(bytes, () => {
        // parsed; the glyph is on screen after xterm's next render pass
        let done = false;
        const finish = () => { if (done) return; done = true; disp.dispose(); rendered(); };
        const disp = term.onRender(finish);
        requestAnimationFrame(() => requestAnimationFrame(finish)); // nothing to render (off-viewport)
      });
    },
    fit() { try { fit.fit(); } catch { /* not laid out */ } },
    focus() { term.focus(); },
    simulateInput(d) { term.input(d, true); },
    getSelection() { return term.getSelection(); },
    async cursorLine() { const b = term.buffer.active; return b.getLine(b.cursorY + b.viewportY)?.translateToString(true) ?? ""; },
    inputElement() { return term.textarea ?? null; },
    raw() { return term; },
  };
}

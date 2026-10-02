// wterm: DOM renderer with either the Ghostty core (renderer=wterm) or its built-in Zig core (renderer=wterm-lite).
import { WTerm } from "@wterm/dom";
import "@wterm/dom/css";
import { AdapterCallbacks, AdapterOptions, TermAdapter, TERM_THEME, ANSI16 } from "./types";

export function createWterm(o: AdapterOptions, cb: AdapterCallbacks): TermAdapter {
  let t: WTerm | null = null;
  let host: HTMLElement | null = null;
  const lite = o.renderer === "wterm-lite";
  return {
    kind: lite ? "wterm-lite" : "wterm-ghostty",
    async open(h) {
      host = h;
      // same palette and font as xterm, through wterm's CSS custom properties
      const s = h.style;
      s.setProperty("--term-bg", TERM_THEME.background);
      s.setProperty("--term-fg", TERM_THEME.foreground);
      s.setProperty("--term-cursor", TERM_THEME.cursor);
      s.setProperty("--term-selection-bg", TERM_THEME.selectionBackground);
      ANSI16.forEach((c, i) => s.setProperty(`--term-color-${i}`, c));
      s.setProperty("--term-font-family", o.fontFamily);
      s.setProperty("--term-font-size", `${o.fontSize}px`);
      s.setProperty("--term-line-height", String(o.lineHeight));
      s.setProperty("--term-row-height", `${Math.round(o.fontSize * o.lineHeight)}px`);
      s.padding = "0"; s.borderRadius = "0"; s.boxShadow = "none";
      let core: any;
      if (!lite) {
        const { GhosttyCore } = await import("@wterm/ghostty");
        core = await GhosttyCore.load();
      }
      t = new WTerm(h, {
        ...(core ? { core } : {}),
        autoResize: true,
        cursorBlink: false,
        onData: cb.onData,
        onResize: cb.onResize,
      } as any);
      await t.init();
      cb.onResize(t.cols, t.rows);
    },
    write(bytes, rendered) {
      t!.write(bytes);
      // WTerm schedules its paint with requestAnimationFrame during write(); a rAF registered
      // now runs after it in the same frame, i.e. once the rows are in the DOM.
      if (rendered) requestAnimationFrame(() => rendered());
    },
    fit() { try { t?.fit(); } catch { /* hidden */ } },
    focus() { t?.focus(); },
    simulateInput(d) { cb.onData(d); },
    getSelection() { return t?.getSelectionText() ?? window.getSelection()?.toString() ?? ""; },
    async cursorLine() {
      if (!t) return "";
      const lines = (await t.readText()).split("\n").filter((l) => l.trim().length);
      return lines[lines.length - 1] ?? "";
    },
    inputElement() { return host?.querySelector("textarea") ?? null; },
    raw() { return t; },
  };
}

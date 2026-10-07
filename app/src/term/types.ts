// The surface the app needs from a terminal emulator. xterm.js implements it today; wterm
// is the tracked upgrade (ADR-3) and would slot in behind the same interface.
export interface AdapterCallbacks {
  onData(data: string): void;
  onResize(cols: number, rows: number): void;
}
export interface AdapterOptions { cols: number; rows: number; fontFamily: string; fontSize: number; lineHeight: number; renderer: string }
export interface TermAdapter {
  readonly kind: string;
  open(host: HTMLElement): Promise<void>;
  write(bytes: Uint8Array): void;
  /** Clears the screen and scrollback, optionally resizing, before a snapshot redraw. */
  reset(cols?: number, rows?: number): void;
  fit(): void;
  focus(): void;
  /** The buffer's text, scrollback included, one line per row, whatever the renderer. */
  text(): string;
  /** The underlying library object, for debugging. */
  raw(): unknown;
}

/** Fallback when the stylesheet's `--font-terminal` stack isn't available (tests, early paint). */
export const TERM_FONT = '"JetBrains Mono", "DejaVu Sans Mono", "Liberation Mono", "Noto Color Emoji", monospace';

// ANSI palettes and the transparent, CSS-driven theme are adapted from MonoCode
// (https://github.com/hardbeat920/monocode), MIT, Copyright (c) 2026 Nick.
export const ANSI_DARK = {
  black: "#1d2428", red: "#f87171", green: "#4ade80", yellow: "#fbbf24",
  blue: "#60a5fa", magenta: "#c084fc", cyan: "#22d3ee", white: "#e8eef2",
  brightBlack: "#64748b", brightRed: "#fca5a5", brightGreen: "#86efac", brightYellow: "#fde68a",
  brightBlue: "#93c5fd", brightMagenta: "#d8b4fe", brightCyan: "#67e8f9", brightWhite: "#f8fafc",
};
/** One Light family, tuned for a near-white page. */
export const ANSI_LIGHT = {
  black: "#383a42", red: "#e45649", green: "#50a14f", yellow: "#c18401",
  blue: "#4078f2", magenta: "#a626a4", cyan: "#0184bc", white: "#fafafa",
  brightBlack: "#7c8591", brightRed: "#df6b60", brightGreen: "#68b567", brightYellow: "#d19a2f",
  brightBlue: "#5c89f5", brightMagenta: "#b54bb3", brightCyan: "#1f9cc9", brightWhite: "#ffffff",
};

/**
 * The terminal theme for a scheme. The background is fully transparent so the terminal sits on
 * the same surface (and glass) as the rest of the window; foreground and cursor come from the
 * page's tokens through `css`, which resolves a CSS color expression (or returns null).
 */
export function terminalTheme(light: boolean, css: (expr: string) => string | null = () => null) {
  return {
    background: "#00000000",
    foreground: css("var(--content)") ?? (light ? "#2e2e2e" : "#e2e4e9"),
    cursor: css("var(--accent)") ?? (light ? "#4078f2" : "#4da3f5"),
    cursorAccent: light ? "#ffffff" : "#000000",
    selectionBackground: light ? "rgba(0,0,0,0.18)" : "rgba(255,255,255,0.18)",
    selectionInactiveBackground: light ? "rgba(0,0,0,0.08)" : "rgba(255,255,255,0.08)",
    ...(light ? ANSI_LIGHT : ANSI_DARK),
  };
}

const A = ANSI_DARK;
export const ANSI16 = [
  A.black, A.red, A.green, A.yellow, A.blue, A.magenta, A.cyan, A.white,
  A.brightBlack, A.brightRed, A.brightGreen, A.brightYellow, A.brightBlue, A.brightMagenta, A.brightCyan, A.brightWhite,
];

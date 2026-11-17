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
  /** Resizes the emulator, keeping its scrollback, before a snapshot redraw. */
  resize(cols: number, rows: number): void;
  fit(): void;
  focus(): void;
  /** Whether typed keys are passed on (`onData`); off shows the terminal view only. */
  setInput(on: boolean): void;
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
/**
 * Catppuccin Latte (https://github.com/catppuccin/catppuccin, MIT, Copyright (c) 2021 Catppuccin).
 * Kept as is: its colors are meant to read on a near-white page.
 */
export const ANSI_LIGHT = {
  black: "#5c5f77", red: "#d20f39", green: "#40a02b", yellow: "#df8e1d",
  blue: "#1e66f5", magenta: "#ea76cb", cyan: "#179299", white: "#acb0be",
  brightBlack: "#6c6f85", brightRed: "#d20f39", brightGreen: "#40a02b", brightYellow: "#df8e1d",
  brightBlue: "#1e66f5", brightMagenta: "#ea76cb", brightCyan: "#179299", brightWhite: "#bcc0cc",
};

/**
 * The contrast xterm enforces per cell, adjusting any color (ANSI, 256 or truecolor) that falls
 * short. Harnesses such as Kiro and Bob pick pale truecolors for a dark terminal, which wash out
 * on the light theme (under 2:1). The light floor is 3: it lifts those, and leaves Latte's own
 * colors nearly untouched, since xterm's adjustment darkens toward grey and a higher floor turns
 * them muddy. xterm halves it for dim (faint) text.
 */
export function minimumContrast(light: boolean) { return light ? 3 : 4.5; }

/**
 * The terminal theme for a scheme. The background is the page color, opaque: the WebGL renderer
 * rasterizes glyphs onto its background, and on a transparent one it can only use coarse
 * grayscale edges, which read as jagged text (worst dark-on-light). Foreground, cursor and
 * background come from the page's tokens through `css`, which resolves a CSS color expression
 * (or returns null).
 */
export function terminalTheme(light: boolean, css: (expr: string) => string | null = () => null) {
  return {
    background: css("var(--background)") ?? (light ? "#f6f6f8" : "#131418"),
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

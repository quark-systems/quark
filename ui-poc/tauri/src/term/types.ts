// Common surface over the three terminal implementations so input, resize, the echo
// probe and the bench are wired identically for each.
export interface AdapterCallbacks {
  onData(data: string): void;
  onBinary?(data: string): void;
  onResize(cols: number, rows: number): void;
}
export interface AdapterOptions { cols: number; rows: number; fontFamily: string; fontSize: number; lineHeight: number; renderer: string }
export interface TermAdapter {
  /** Label shown in the pane header and bench JSON. */
  readonly kind: string;
  open(host: HTMLElement): Promise<void>;
  /** Write output; `rendered` fires once the bytes are painted into the DOM/canvas (next render pass). */
  write(bytes: Uint8Array, rendered?: () => void): void;
  fit(): void;
  focus(): void;
  /** Same path as a keystroke after key handling: emits onData. Used by the scripted bench. */
  simulateInput(data: string): void;
  getSelection(): string;
  /** Text of the cursor row (tests). */
  cursorLine(): Promise<string>;
  /** The element that receives keyboard focus. */
  inputElement(): HTMLElement | null;
  /** The underlying library object, for debugging. */
  raw(): unknown;
}

export const TERM_FONT = '"JetBrains Mono", "DejaVu Sans Mono", "Liberation Mono", "Noto Color Emoji", monospace';
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
export const ANSI16 = [
  TERM_THEME.black, TERM_THEME.red, TERM_THEME.green, TERM_THEME.yellow, TERM_THEME.blue, TERM_THEME.magenta, TERM_THEME.cyan, TERM_THEME.white,
  TERM_THEME.brightBlack, TERM_THEME.brightRed, TERM_THEME.brightGreen, TERM_THEME.brightYellow, TERM_THEME.brightBlue, TERM_THEME.brightMagenta, TERM_THEME.brightCyan, TERM_THEME.brightWhite,
];

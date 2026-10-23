// quarkd snapshots repaint the whole screen and start with a full reset (RIS, `ESC c`), which in
// xterm.js also drops the scrollback. quarkd sends one whenever it prunes a terminal's stored
// output (every 512 KB or so of output) and whenever a view opens the terminal, so applied as-is a
// busy worker's history kept shrinking to a single screen and the view stopped scrolling.

const ESC = 0x1b;

/**
 * Repaints from here without touching the scrollback: leave the alternate screen (the snapshot
 * enters it again when the program uses it), soft-reset modes, margins and attributes (DECSTR),
 * turn off what RIS would have (mouse reporting, focus events, bracketed paste, synchronized output,
 * application keypad), then clear the screen. The snapshot turns back on whatever the program uses.
 */
const SOFT_RESET = new TextEncoder().encode(
  "\x1b[?1049l\x1b[!p" +
    "\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\x1b[?1015l" +
    "\x1b[?2004l\x1b[?2026l\x1b>\x1b[H\x1b[2J",
);

/** A snapshot's bytes with its leading full reset swapped for a repaint that keeps the scrollback. */
export function keepScrollback(bytes: Uint8Array): Uint8Array {
  if (bytes[0] !== ESC || bytes[1] !== 0x63) return bytes;
  const out = new Uint8Array(SOFT_RESET.length + bytes.length - 2);
  out.set(SOFT_RESET);
  out.set(bytes.subarray(2), SOFT_RESET.length);
  return out;
}

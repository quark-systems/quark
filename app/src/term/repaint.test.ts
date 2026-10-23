import { describe, expect, it } from "vitest";
import { keepScrollback } from "./repaint";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

describe("keepScrollback", () => {
  it("swaps a snapshot's full reset for a repaint that keeps the scrollback", () => {
    const out = dec(keepScrollback(enc("\x1bc\x1b[?1049h\x1b[1;1Hhello")));
    expect(out).not.toContain("\x1bc");
    expect(out.startsWith("\x1b[?1049l\x1b[!p")).toBe(true);
    expect(out).toContain("\x1b[?1000l");
    expect(out.endsWith("\x1b[H\x1b[2J\x1b[?1049h\x1b[1;1Hhello")).toBe(true);
  });

  it("leaves bytes without a leading reset alone", () => {
    const b = enc("\x1b[2J\x1b[Hscreen \x1bc later");
    expect(keepScrollback(b)).toBe(b);
    expect(keepScrollback(new Uint8Array())).toHaveLength(0);
  });
});

import { describe, expect, it } from "vitest";
import { dur } from "./Metrics";

describe("dur", () => {
  it("reads durations at the right scale", () => {
    expect(dur(null)).toBe("–");
    expect(dur(42)).toBe("42s");
    expect(dur(600)).toBe("10m");
    expect(dur(3600)).toBe("1h");
    expect(dur(2 * 3600 + 900)).toBe("2h 15m");
    expect(dur(86400 + 3 * 3600)).toBe("1d 3h");
  });
});

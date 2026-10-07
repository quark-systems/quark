import { describe, expect, it } from "vitest";
import { dur, hasTurns, tokens } from "./Metrics";

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

describe("tokens", () => {
  it("reads token counts compactly", () => {
    expect(tokens(null)).toBe("–");
    expect(tokens(950)).toBe("950");
    expect(tokens(1500)).toBe("1.5k");
    expect(tokens(48_200)).toBe("48k");
    expect(tokens(2_340_000)).toBe("2.3M");
  });
});

describe("hasTurns", () => {
  const none = { turns: 0, ack_turns: 0, input_tokens: 0, output_tokens: 0, cache_read_tokens: 0 };
  it("hides the coordinator section until a coordinator turn is known", () => {
    expect(hasTurns({ tasks: 3, baseline: none, native: none, would_wake_turns: 0 })).toBe(false);
    expect(hasTurns({ tasks: 3, baseline: { ...none, turns: 2 }, native: none, would_wake_turns: 0 })).toBe(true);
  });
});

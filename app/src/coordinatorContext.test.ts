import { describe, expect, it } from "vitest";
import type { CoordinatorContext } from "./api";
import { agentLine, compactionLine, contextLine, fullness, tokens } from "./coordinatorContext";

const ctx = (c: Partial<CoordinatorContext> = {}): CoordinatorContext => ({
  coordinator_id: "p",
  agent_config: { harness: "claude-code", model: "opus", effort: "high", pool: null },
  context_tokens: 46_000, window_tokens: 200_000,
  compaction: { auto: true, at_tokens: 140_000 }, compacting: "idle",
  ...c,
});

describe("coordinator context", () => {
  it("writes token counts short", () => {
    expect(tokens(800)).toBe("800");
    expect(tokens(46_400)).toBe("46k");
    expect(tokens(1_000_000)).toBe("1M");
    expect(tokens(1_250_000)).toBe("1.3M");
  });
  it("names the agent, falling back to what the harness picks", () => {
    expect(agentLine(ctx())).toEqual({ harness: "Claude Code", model: "opus", effort: "high effort" });
    const a = agentLine(ctx({ agent_config: { harness: "claude-code" }, model_seen: "claude-opus-5-5" }));
    expect(a.model).toBe("claude-opus-5-5");
    expect(a.effort).toBe("default effort");
  });
  it("says how full the context is", () => {
    expect(fullness(ctx())).toBe("23%");
    expect(fullness(ctx({ window_tokens: null }))).toBe("46k");
    expect(fullness(ctx({ context_tokens: null }))).toBe("–");
    expect(contextLine(ctx())).toBe("46k of 200k tokens");
    expect(contextLine(ctx({ context_tokens: null, last_compaction: { text: "x" } }))).toMatch(/since it was compacted/);
  });
  it("says what happens about compaction", () => {
    expect(compactionLine(ctx())).toBe("Compacts at 140k tokens, after a wrap up.");
    expect(compactionLine(ctx({ compaction: { auto: false, at_tokens: 140_000 } }))).toMatch(/^Automatic compaction is off\. Claude Code compacts/);
    expect(compactionLine(ctx({ compacting: "wrapping_up" }))).toBe("Wrapping up, then compacting.");
  });
});

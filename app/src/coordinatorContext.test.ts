import { describe, expect, it } from "vitest";
import type { CoordinatorContext, CoordinatorSwitch } from "./api";
import { agentLine, compactionLine, contextLine, fullness, switching, switchLine, tokens } from "./coordinatorContext";

const ctx = (c: Partial<CoordinatorContext> = {}): CoordinatorContext => ({
  coordinator_id: "p",
  agent_config: { harness: "claude-code", model: "opus", effort: "high", pool: null },
  context_tokens: 46_000, window_tokens: 200_000,
  compaction: { auto: true, at_percent: 70 }, compact_at_tokens: 140_000, compacting: "idle",
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
    expect(compactionLine(ctx())).toBe("Compacts when the context is 70% full (140k tokens), after a wrap up.");
    expect(compactionLine(ctx({ compaction: { auto: false, at_percent: 70 }, compact_at_tokens: null }))).toMatch(/^Automatic compaction is off\. Claude Code compacts/);
    expect(compactionLine(ctx({ compacting: "wrapping_up" }))).toBe("Wrapping up, then compacting.");
  });
  it("says how far a change of agent got", () => {
    const sw = (c: Partial<CoordinatorSwitch>): CoordinatorSwitch => ({
      id: "sw_1", project_id: "p", from: { harness: "claude-code", model: "sonnet" }, to: { harness: "claude-code", model: "opus", effort: "high" },
      kind: "resume", state: "requested", requested_at: "t", updated_at: "t", ...c,
    });
    expect(switching(sw({}))).toBe(true);
    expect(switching(sw({ state: "failed" }))).toBe(false);
    expect(switchLine(sw({ state: "wrapping_up", kind: "handoff" }))).toBe("Changing to Claude Code, opus, high effort. The coordinator is saving its work and writing a handoff.");
    expect(switchLine(sw({ state: "done" }))).toBe("Changed from Claude Code, sonnet, default effort to Claude Code, opus, high effort, in the same conversation.");
    expect(switchLine(sw({ state: "failed", error: "no account" }))).toBe("Could not change to Claude Code, opus, high effort: no account. The coordinator kept Claude Code, sonnet, default effort.");
  });
});

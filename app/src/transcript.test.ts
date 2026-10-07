import { describe, expect, it } from "vitest";
import { duration, groupTurns, newDay, stepTitle, summarize } from "./transcript";
import type { ToolInfo, TranscriptItem, TranscriptRole } from "./api";

let next = 0;
const item = (role: TranscriptRole, extra: Partial<TranscriptItem> = {}): TranscriptItem =>
  ({ id: ++next, role, text: role, is_error: false, truncated: false, ts: null, ...extra });
const tool = (kind: ToolInfo["kind"], title: string): ToolInfo => ({ kind, title });

describe("groupTurns", () => {
  it("splits on prompts and pairs results with calls by id", () => {
    const g = groupTurns([
      item("user", { ts: "2026-10-07T10:00:00Z" }),
      item("thinking"),
      item("tool_call", { tool_call_id: "a" }),
      item("tool_call", { tool_call_id: "b" }),
      item("tool_result", { tool_call_id: "b", is_error: true }),
      item("tool_result", { tool_call_id: "a" }),
      item("assistant", { ts: "2026-10-07T10:02:39Z" }),
      item("user"),
      item("tool_call", { tool_call_id: "c" }),
    ]);
    expect(g).toHaveLength(2);
    const [first, second] = g;
    expect(first.parts.map((p) => p.kind)).toEqual(["work", "text"]);
    const steps = first.parts[0].kind === "work" ? first.parts[0].steps : [];
    expect(steps.map((s) => s.status)).toEqual(["ok", "ok", "error"]);
    expect(first.endedAt! - first.startedAt!).toBe(159_000);
    expect(first.live).toBe(false);
    expect(second.live).toBe(true);
    expect(second.parts[0].kind === "work" && second.parts[0].steps[0].status).toBe("running");
  });

  it("pairs a result without an id with the latest waiting call", () => {
    const [t] = groupTurns([item("user"), item("tool_call"), item("tool_result"), item("assistant")]);
    expect(t.parts[0].kind === "work" && t.parts[0].steps).toHaveLength(1);
  });

  it("keeps entries before the first prompt, and results whose call is not loaded", () => {
    const [t] = groupTurns([item("tool_result", { tool_call_id: "gone" }), item("assistant")]);
    expect(t.prompt).toBeUndefined();
    expect(t.parts.map((p) => p.kind)).toEqual(["work", "text"]);
  });

  it("a prompt with no answer yet is live", () => {
    expect(groupTurns([item("user")])[0].live).toBe(true);
  });
});

describe("labels", () => {
  it("titles fall back to the tool name and input", () => {
    expect(stepTitle({ key: 1, call: item("tool_call", { tool: tool("read", "Read a.rs") }), status: "ok" })).toBe("Read a.rs");
    expect(stepTitle({ key: 1, call: item("tool_call", { tool_name: "bash", text: "ls\nmore" }), status: "ok" })).toBe("bash: ls");
  });

  it("summarizes a run of steps", () => {
    const s = (kind: ToolInfo["kind"]) => ({ key: 1, call: item("tool_call", { tool: tool(kind, "x") }), status: "ok" as const });
    expect(summarize([s("read"), s("read"), s("shell"), s("other")])).toBe("Read 2 files, ran 1 command, 1 other step");
    expect(summarize([{ key: 1, thinking: item("thinking"), status: "ok" }])).toBe("Thought");
  });

  it("formats durations and day breaks", () => {
    expect(duration(159_000)).toBe("2m 39s");
    expect(duration(55_000)).toBe("55s");
    expect(duration(3_840_000)).toBe("1h 4m");
    const a = Date.parse("2026-10-06T10:00:00");
    expect(newDay(a, undefined)).toBe(true);
    expect(newDay(a + 3_600_000, a)).toBe(false);
    expect(newDay(a + 86_400_000, a)).toBe(true);
  });
});

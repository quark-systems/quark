import { describe, expect, it } from "vitest";
import { groupForChat } from "./screens/ProjectBoard";
import type { TranscriptItem, TranscriptRole } from "./api";

const item = (id: number, role: TranscriptRole): TranscriptItem =>
  ({ id, role, text: role, is_error: false, truncated: false, ts: null });

describe("groupForChat", () => {
  it("keeps user and assistant entries and collapses runs of the rest", () => {
    const g = groupForChat([item(1, "user"), item(2, "thinking"), item(3, "tool_call"), item(4, "tool_result"), item(5, "assistant")]);
    expect(g.map((x) => (x.kind === "msg" ? x.item.role : `activity:${x.items.length}`))).toEqual(["user", "activity:3", "assistant"]);
  });
});

import { describe, expect, it } from "vitest";
import type { Decision } from "./api";
import { nextAfterAnswer, step } from "./inbox";

const d = (id: string, over: Partial<Decision> = {}): Decision => ({
  id, project_id: "p1", question: `Q ${id}`, state: "open", opened_at: "2026-10-02T10:00:00Z", ...over,
});

describe("step", () => {
  const list = [d("a"), d("b"), d("c")];
  it("moves and clamps", () => {
    expect(step(list, "a", 1)).toBe("b");
    expect(step(list, "c", 1)).toBe("c");
    expect(step(list, "a", -1)).toBe("a");
  });
  it("starts at the first row", () => {
    expect(step(list, undefined, 1)).toBe("a");
    expect(step([], "a", 1)).toBeUndefined();
  });
});

describe("nextAfterAnswer", () => {
  const list = [d("a"), d("b"), d("c")];
  it("moves to the next open decision, or the previous one at the end", () => {
    expect(nextAfterAnswer(list, "a")).toBe("b");
    expect(nextAfterAnswer(list, "c")).toBe("b");
    expect(nextAfterAnswer([d("a")], "a")).toBeUndefined();
  });
});

import { describe, expect, it } from "vitest";
import { afterClose, withWorker } from "./projectPanel";

describe("project panel", () => {
  it("opens a worker once, newest last", () => {
    expect(withWorker(["a"], "b")).toEqual(["a", "b"]);
    const same = ["a", "b"];
    expect(withWorker(same, "a")).toBe(same);
  });

  it("drops the oldest worker past eight open", () => {
    const eight = ["1", "2", "3", "4", "5", "6", "7", "8"];
    expect(withWorker(eight, "9")).toEqual(["2", "3", "4", "5", "6", "7", "8", "9"]);
  });

  it("shows the right neighbour after closing, else the left, else none", () => {
    expect(afterClose(["a", "b", "c"], "b")).toBe("c");
    expect(afterClose(["a", "b", "c"], "c")).toBe("b");
    expect(afterClose(["a"], "a")).toBeNull();
    expect(afterClose(["a"], "x")).toBeNull();
  });
});

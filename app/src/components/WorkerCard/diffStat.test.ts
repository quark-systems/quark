import { describe, expect, it } from "vitest";
import { statOf } from "./diffStat";

describe("statOf", () => {
  it("sums additions and deletions, counting binary files as zero", () => {
    expect(statOf({ task_id: "t", base_ref: "main", base: "a", head: "b", files: [
      { path: "a.rs", status: "modified", additions: 3, deletions: 1 },
      { path: "logo.png", status: "added", additions: null, deletions: null },
    ] })).toEqual({ files: 2, adds: 3, dels: 1 });
  });
});

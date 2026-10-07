import { describe, expect, it } from "vitest";
import type { OverviewDigest } from "../../api";
import { summarize } from "./summary";

const digest = (d: Partial<OverviewDigest>): OverviewDigest => ({
  since: 0, events: 0, spawned: 0, done: 0, pull_requests: 0, failed: 0, decisions_opened: 0, decisions_resolved: 0,
  highlights: [], truncated: false, ...d,
});

describe("summarize", () => {
  it("says when nothing happened", () => {
    expect(summarize(digest({}))).toBe("Nothing new.");
  });
  it("puts what needs you first and joins the rest", () => {
    expect(summarize(digest({ events: 9, decisions_opened: 1, done: 2, pull_requests: 1, spawned: 3 })))
      .toBe("1 decision raised, 2 tasks done (1 pull request) and 3 workers started.");
  });
  it("counts routine updates when nothing notable happened", () => {
    expect(summarize(digest({ events: 4 }))).toBe("4 updates, nothing that needs you.");
  });
});

import { describe, expect, it } from "vitest";
import type { PullRequest } from "./api";
import { applyEvent, initialState } from "./store";
import { caseCounts, countByState, evidenceOutcome, filterPrs, formatBytes, formatMs, mergeBlocker, sortCases, sortChecks, traceViewerUrl } from "./prs";
import { anchorOf } from "./components/FileDiff";

const pr = (over: Partial<PullRequest> = {}): PullRequest => ({
  id: "pr-1", project_id: "p1", provider: "github", repo: "o/r", number: 1, url: "https://github.com/o/r/pull/1", title: "T",
  state: "open", head_ref: "h", base_ref: "main", head_sha: "abc", mergeable: "mergeable", checks_state: "passing",
  review_decision: "none", checks: [], reviews: [],
  opened_at: "2026-10-02T10:00:00Z", updated_at: "2026-10-02T10:00:00Z", ...over,
});
const ev = (seq: number, type: string, payload: unknown) => ({ seq, type, payload, project_id: "p1", ts: "" });

describe("PR events", () => {
  it("upserts a full PR from pr.updated and counts activity", () => {
    let s = applyEvent(initialState, ev(1, "pr.updated", pr()));
    s = applyEvent(s, ev(2, "pr.updated", pr({ state: "merged" })));
    expect(s.pullRequests["pr-1"].state).toBe("merged");
    expect(s.prActivity["pr-1"]).toBe(2);
  });
  it("counts activity for checks and reviews of PRs not loaded", () => {
    const s = applyEvent(initialState, ev(1, "check.updated", { pull_request_id: "pr-9", check: { name: "ci", status: "success" } }));
    expect(s.pullRequests).toEqual({});
    expect(s.prActivity["pr-9"]).toBe(1);
  });
  it("merges a check or review into a loaded PR, skipping checks for an older head", () => {
    let s = applyEvent(initialState, ev(1, "pr.updated", pr({ checks: [{ name: "ci", status: "pending" }] })));
    s = applyEvent(s, ev(2, "check.updated", { pull_request_id: "pr-1", head_sha: "abc", check: { name: "ci", status: "success" } }));
    s = applyEvent(s, ev(3, "check.updated", { pull_request_id: "pr-1", head_sha: "old", check: { name: "ci", status: "failure" } }));
    s = applyEvent(s, ev(4, "review.updated", { pull_request_id: "pr-1", review: { id: "r1", state: "approved", body: "" } }));
    expect(s.pullRequests["pr-1"].checks).toEqual([{ name: "ci", status: "success" }]);
    expect(s.pullRequests["pr-1"].reviews.map((r) => r.id)).toEqual(["r1"]);
    expect(s.prActivity["pr-1"]).toBe(4);
  });
  it("ignores events without a PR id", () => {
    expect(applyEvent(initialState, ev(1, "review.updated", {}))).toBe(initialState);
  });
});

describe("PR helpers", () => {
  const list = [
    pr({ id: "a", updated_at: "2026-10-02T10:00:00Z" }), pr({ id: "b", updated_at: "2026-10-02T11:00:00Z" }),
    pr({ id: "c", state: "merged" }), pr({ id: "d", project_id: "p2" }),
  ];
  it("filters by state and Project, newest first", () => {
    expect(filterPrs(list, "open", null).map((p) => p.id)).toEqual(["b", "a", "d"]);
    expect(filterPrs(list, "open", "p2").map((p) => p.id)).toEqual(["d"]);
    expect(countByState(list, "p1")).toEqual({ open: 2, draft: 0, merged: 1, closed: 0 });
  });
  it("names what blocks a merge", () => {
    expect(mergeBlocker(pr())).toBeNull();
    expect(mergeBlocker(pr({ checks_state: "failing" }))).toEqual({ reason: "Checks are failing.", hard: true });
    expect(mergeBlocker(pr({ mergeable: "conflicting" }))?.reason).toMatch(/conflicts/);
    expect(mergeBlocker(pr({ state: "draft" }))?.hard).toBe(true);
    expect(mergeBlocker(pr({ review_decision: "changes_requested" }))?.hard).toBe(false);
  });
  it("puts failing checks first", () => {
    const checks = sortChecks([
      { name: "a", status: "success" },
      { name: "b", status: "pending" },
      { name: "c", status: "failure", conclusion: "timed_out" },
    ]);
    expect(checks.map((c) => c.name)).toEqual(["c", "b", "a"]);
  });
  it("anchors comments on the new side unless the line was deleted", () => {
    expect(anchorOf({ kind: "add", text: "", new: 5 })).toEqual({ line: 5, side: "new" });
    expect(anchorOf({ kind: "ctx", text: "", old: 3, new: 4 })).toEqual({ line: 4, side: "new" });
    expect(anchorOf({ kind: "del", text: "", old: 7 })).toEqual({ line: 7, side: "old" });
    expect(anchorOf({ kind: "hunk", text: "@@" })).toBeNull();
  });
});

describe("evidence helpers", () => {
  it("colors states, treating unknown ones as in progress", () => {
    expect(evidenceOutcome("passed").cls).toBe("green");
    expect(evidenceOutcome("failed").cls).toBe("red");
    expect(evidenceOutcome("skipped").cls).toBe("");
    expect(evidenceOutcome("running")).toEqual({ cls: "yellow", glyph: "●", label: "running" });
  });
  it("counts and orders cases, failures first", () => {
    const cases = [{ name: "a", state: "passed" }, { name: "b", state: "running" }, { name: "c", state: "failed" }, { name: "d", state: "passed" }];
    expect(caseCounts(cases)).toEqual({ passed: 2, failed: 1, other: 1 });
    expect(sortCases(cases).map((c) => c.name)).toEqual(["c", "b", "a", "d"]);
  });
  it("formats sizes, durations and trace viewer links", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MB");
    expect(formatMs(450)).toBe("450 ms");
    expect(formatMs(30000)).toBe("30 s");
    expect(formatMs(95000)).toBe("1m 35s");
    expect(traceViewerUrl("http://127.0.0.1:7380/a b.zip")).toBe("https://trace.playwright.dev/?trace=http%3A%2F%2F127.0.0.1%3A7380%2Fa%20b.zip");
  });
});

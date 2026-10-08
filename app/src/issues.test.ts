import { describe, expect, it } from "vitest";
import type { BeadsStatus, DraftIssue, Issue, IssueDraft } from "./api";
import {
  beadsWhere, chipCounts, chipList, draftBlockers, githubNumber, issueMeta, openDraft, parseLabels, refState, scopeChoices, startWorkerMessage,
} from "./issues";

const issue = (id: string, over: Partial<Issue> = {}): Issue => ({
  id, title: `Title ${id}`, description: "", status: "open", priority: 2, issue_type: "feature", labels: [],
  created_at: "2026-10-01T00:00:00Z", updated_at: "2026-10-01T00:00:00Z", blocked_by: [], ready: true, blocked: false, ...over,
});
const all = [
  issue("qk-41", { priority: 1 }),
  issue("qk-9", { priority: 1 }),
  issue("qk-44", { priority: 1, blocked_by: ["qk-d14", "qk-30"], ready: false, blocked: true, external_ref: "https://github.com/o/r/issues/61" }),
  issue("qk-d14", { issue_type: "decision" }),
  issue("qk-39", { status: "in_progress", ready: false, assignee: "transcript-search" }),
  issue("qk-30", { status: "closed", ready: false, closed_at: "2026-10-02T00:00:00Z" }),
  issue("qk-31", { status: "closed", ready: false, closed_at: "2026-10-03T00:00:00Z" }),
];
const byId = Object.fromEntries(all.map((i) => [i.id, i]));

describe("chipList", () => {
  it("lists ready work by priority, then issue number; decisions wait on a person, so they are not ready", () => {
    expect(chipList(all, "ready").map((i) => i.id)).toEqual(["qk-9", "qk-41"]);
  });
  it("puts each open issue under one chip and closed ones newest first", () => {
    expect(chipList(all, "blocked").map((i) => i.id)).toEqual(["qk-44"]);
    expect(chipList(all, "in_progress").map((i) => i.id)).toEqual(["qk-39"]);
    expect(chipList(all, "closed").map((i) => i.id)).toEqual(["qk-31", "qk-30"]);
    expect(chipCounts(all)).toEqual({ ready: 2, in_progress: 1, blocked: 1, closed: 2 });
  });
});

describe("issueMeta", () => {
  it("names only the blockers still open, and the GitHub issue", () => {
    expect(issueMeta(byId["qk-44"], byId)).toBe("feature · blocked by qk-d14 · GitHub #61");
    expect(issueMeta(byId["qk-39"], byId)).toBe("feature · in progress · transcript-search");
    expect(issueMeta(byId["qk-41"], byId)).toBe("feature · ready");
    expect(issueMeta(byId["qk-d14"], byId)).toBe("decision · waiting on you");
  });
});

describe("githubNumber", () => {
  it("reads GitHub issue URLs only", () => {
    expect(githubNumber("https://github.com/quark-systems/quark/issues/61")).toBe("#61");
    expect(githubNumber("https://example.com/issues/61")).toBeNull();
    expect(githubNumber(null)).toBeNull();
  });
});

describe("refState", () => {
  it("shows an open decision as waiting on you", () => {
    expect(refState({ id: "qk-d14", title: "x", status: "open", issue_type: "decision" })).toMatchObject({ label: "waiting on you", decision: true });
    expect(refState({ id: "qk-35", title: "x", status: "closed", issue_type: "decision" })).toMatchObject({ label: "done", decision: false });
    expect(refState({ id: "qk-43", title: "x", status: "in_progress", issue_type: "task" }).label).toBe("in progress");
  });
});

describe("drafts", () => {
  const draft = (id: string, over: Partial<IssueDraft> = {}): IssueDraft => ({
    id, project_id: "p1", state: "open", messages: [], issues: [], related: [], waiting: false, created: {},
    created_at: "2026-10-01T00:00:00Z", updated_at: "2026-10-01T00:00:00Z", ...over,
  });
  it("reopens the Project's most recently updated open draft", () => {
    const list = [draft("a"), draft("b", { updated_at: "2026-10-02T00:00:00Z" }), draft("c", { state: "accepted", updated_at: "2026-10-03T00:00:00Z" }),
      draft("d", { project_id: "p2", updated_at: "2026-10-04T00:00:00Z" })];
    expect(openDraft(list, "p1")?.id).toBe("b");
    expect(openDraft(list, "p3")).toBeUndefined();
  });
  it("labels a blocker that is another draft as new", () => {
    const d = (key: string, blocked_by: string[] = []): DraftIssue => ({ key, title: key, issue_type: "bug", priority: 1, labels: [], description: "", blocked_by });
    const drafts = [d("1"), d("2", ["1", "qk-44"])];
    expect(draftBlockers(drafts[1], drafts)).toEqual(["new · 1", "qk-44"]);
  });
  it("parses labels typed with commas or spaces", () => {
    expect(parseLabels(" attention, ui  attention ")).toEqual(["attention", "ui"]);
  });
  it("asks the coordinator for a worker by id and title", () => {
    expect(startWorkerMessage({ id: "qk-41", title: "Next-attention shortcut" })).toBe("Start a worker on qk-41: Next-attention shortcut");
  });
});

describe("where memory and issues live", () => {
  const ready: BeadsStatus = { project_id: "p1", state: "ready", github_repo: "quark-systems/quark", dir: "/x" };
  it("says where the Beads database is and whether it mirrors GitHub", () => {
    expect(beadsWhere(ready)).toBe("Beads in quark-systems/quark · mirrored both ways with GitHub Issues");
    expect(beadsWhere({ ...ready, github_repo: null })).toBe("Beads in /x");
  });
  it("offers the Project's Beads, or memory/ without it", () => {
    expect(scopeChoices(ready).map((s) => `${s.scope}: ${s.hint}`)).toEqual([
      "project: Beads in quark-systems/quark", "user: your own memory", "repo: opens a PR to AGENTS.md",
    ]);
    expect(scopeChoices({ ...ready, state: "missing" })[0].hint).toBe("memory/ in the Project repo");
    expect(scopeChoices(undefined)[0].hint).toBe("memory/ in the Project repo");
  });
});

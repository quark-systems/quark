import { describe, expect, it } from "vitest";
import type { Decision, StandingRule, Task } from "./api";
import { answeredVia, askedBy, blockLabels, decisionLabel, joinAnd, lifecycleStep, logList, ruleDraft, rowSummary, rowTone, usesOf } from "./decisions";

const d = (id: string, over: Partial<Decision> = {}): Decision => ({
  id, project_id: "p1", question: `Q ${id}`, state: "open", opened_at: "2026-10-02T10:00:00Z", ...over,
});
const tasks = { t1: { id: "t1", title: "attention-model" } as Task };

describe("decision words", () => {
  it("numbers decisions, falling back to the id's tail", () => {
    expect(decisionLabel(d("x", { number: 14 }))).toBe("D-14");
    expect(decisionLabel(d("abcdef123456"))).toBe("123456");
  });
  it("names who asked and what waits", () => {
    expect(askedBy(d("a"), tasks)).toBe("the coordinator");
    expect(askedBy(d("a", { brief: { asked_by: "quarkd" } }), tasks)).toBe("Quark");
    expect(askedBy(d("a", { brief: { asked_by: "t1" } }), tasks)).toBe("attention-model");
    expect(blockLabels(d("a", { brief: { blocks: ["https://github.com/o/r/pull/95", "t1", "qk-44"] } }), tasks))
      .toEqual(["PR #95", "attention-model", "qk-44"]);
    expect(joinAnd(["a", "b", "c"])).toBe("a, b and c");
    expect(joinAnd(["a"])).toBe("a");
  });
  it("places a decision on its lifecycle", () => {
    expect(lifecycleStep(d("a"))).toBe(0);
    expect(lifecycleStep(d("a", { state: "answered" }))).toBe(1);
    expect(lifecycleStep(d("a", { state: "acted" }))).toBe(3);
  });
});

describe("answeredVia", () => {
  it("says where an answer came from", () => {
    expect(answeredVia("app")).toBe("in the app");
    expect(answeredVia("phone")).toBe("in the phone");
    expect(answeredVia("beads")).toBe("in Beads");
    expect(answeredVia("rule")).toBe("under a standing rule");
    expect(answeredVia(null)).toBeNull();
  });
});

describe("logList", () => {
  const all = [
    d("o2", { opened_at: "2026-10-02T11:00:00Z" }),
    d("o1", { opened_at: "2026-10-02T10:00:00Z", brief: { context: "about the flag" } }),
    d("r", { state: "answered", answered_at: "2026-10-02T09:00:00Z", made_rule_id: "rule-1" }),
    d("u", { state: "acted", answered_at: "2026-10-02T12:00:00Z", rule_id: "rule-1", answered_via: "rule" }),
  ];
  it("lists open ones oldest first and the rest newest first", () => {
    expect(logList(all, "open").map((x) => x.id)).toEqual(["o1", "o2"]);
    expect(logList(all, "all").map((x) => x.id)).toEqual(["u", "o2", "o1", "r"]);
  });
  it("filters to rules and agent decisions, and searches the brief", () => {
    expect(logList(all, "rules").map((x) => x.id)).toEqual(["r"]);
    expect(logList(all, "agents").map((x) => x.id)).toEqual(["u"]);
    expect(logList(all, "all", "FLAG").map((x) => x.id)).toEqual(["o1"]);
  });
  it("finds the uses of a rule", () => {
    expect(usesOf(all[2], all).map((x) => x.id)).toEqual(["u"]);
    expect(usesOf(all[0], all)).toEqual([]);
  });
});

describe("rows", () => {
  const rule = { id: "rule-1", decision_id: "r" } as StandingRule;
  const r = d("r", { number: 12, state: "answered", answer: "Yes, always.\nMore.", answered_by: "matt", made_rule_id: "rule-1" });
  const u = d("u", { state: "acted", answer: "Yes", answered_by: "coordinator", rule_id: "rule-1" });
  it("summarises open and answered decisions in the mock's words", () => {
    expect(rowSummary(d("o", { brief: { blocks: ["https://github.com/o/r/pull/95"] } }), tasks, {}, {})).toBe("Open · blocks PR #95");
    expect(rowSummary(d("o"), tasks, {}, {})).toBe("Open · asked by the coordinator");
    expect(rowSummary(r, tasks, { "rule-1": rule }, { r })).toBe("Yes, always. · matt · standing rule");
    expect(rowSummary(u, tasks, { "rule-1": rule }, { r })).toBe("Yes · coordinator, under rule D-12");
  });
  it("colours rows by what they need", () => {
    expect(rowTone(d("o"))).toBe("needs-you");
    expect(rowTone(r)).toBe("accent");
    expect(rowTone(u)).toBe("plain");
  });
  it("drafts a rule from the answer", () => {
    expect(ruleDraft(d("a", { question: "Rebase first?" }), "Yes, always. ")).toBe('For questions like "Rebase first?": Yes, always.');
    expect(ruleDraft(d("a"), "  ")).toBe("");
  });
});

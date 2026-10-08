import { describe, expect, it } from "vitest";
import { leftListGroups, swatchFor } from "./leftList";

const p = (id: string, name: string) => ({ id, name, created_at: "", updated_at: "" });
const t = (id: string, project_id: string, state: any, updated_at = "2026-10-08T00:00:00Z", state_note?: string) =>
  ({ id, project_id, title: id, state, created_at: "", updated_at, state_note });

describe("leftListGroups", () => {
  it("groups unfinished workers under their project, newest first, and counts open decisions", () => {
    const g = leftListGroups(
      { b: p("b", "Beta"), a: p("a", "Alpha") },
      { t1: t("t1", "a", "running", "2026-10-08T01:00:00Z"), t2: t("t2", "a", "needs_decision", "2026-10-08T02:00:00Z", "Asked a question"), t3: t("t3", "a", "done") },
      { d1: { id: "d1", project_id: "a", question: "q", state: "open", opened_at: "" }, d2: { id: "d2", project_id: "a", question: "q", state: "answered", opened_at: "" } },
    );
    expect(g.map((x) => x.project.name)).toEqual(["Alpha", "Beta"]);
    expect(g[0].workers.map((w) => [w.id, w.tone, w.needsYou, w.sub])).toEqual([
      ["t2", "needs-you", true, "Asked a question"], ["t1", "busy", false, "Running"],
    ]);
    expect(g[0].waiting).toBe(1);
    expect(g[0].coordSub).toBe("1 decision · 2 workers");
    expect(g[1].coordSub).toBe("Nothing waiting");
  });
  it("keeps a failed worker for three days, then drops it", () => {
    const now = Date.parse("2026-10-08T00:00:00Z");
    const g = leftListGroups({ a: p("a", "A") }, { f1: t("f1", "a", "failed", "2026-10-07T00:00:00Z"), f2: t("f2", "a", "failed", "2026-10-01T00:00:00Z") }, {}, now);
    expect(g[0].workers.map((w) => [w.id, w.tone])).toEqual([["f1", "failed"]]);
  });
  it("gives each project a stable color", () => {
    expect(swatchFor("quark")).toBe(swatchFor("quark"));
  });
});

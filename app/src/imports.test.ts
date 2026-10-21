import { describe, expect, it } from "vitest";
import type { FirstmateImportReport, ImportedProject } from "./api";
import { leftBehind, pendingProjects, projectSummary, totals, unroutedGroups } from "./imports";

const project = (o: Partial<ImportedProject> = {}): ImportedProject => ({
  name: "p", project: "created", outcome: "imported", dispatch: "imported",
  tasks: { imported: 2, present: 0 }, holds: { imported: 1, present: 0 }, ...o,
});
const report = (o: Partial<FirstmateImportReport> = {}): FirstmateImportReport =>
  ({ home: "/h", dry_run: true, inventory: [], projects: [], unrouted: [], notes: [], ...o });

describe("unroutedGroups", () => {
  it("groups by id prefix, biggest first, with each reason once", () => {
    const g = unroutedGroups(["a-1: its id fits x, y", "a-2: its id fits x, y", "b-1: no project name matches its id", "lone: nothing"]);
    expect(g.map((x) => [x.prefix, x.ids])).toEqual([["a-", ["a-1", "a-2"]], ["b-", ["b-1"]], ["lone", ["lone"]]]);
    expect(g[0].reasons).toEqual(["its id fits x, y"]);
  });
  it("is empty for nothing", () => expect(unroutedGroups([])).toEqual([]));
});

describe("projectSummary", () => {
  it("says what happens in a preview and what happened after", () => {
    expect(projectSummary(project(), true)).toBe("New project");
    expect(projectSummary(project({ project: "existing" }), true)).toBe("Already in Quark");
    expect(projectSummary(project(), false)).toBe("Created");
    expect(projectSummary(project({ project: "existing" }), false)).toBe("Updated");
    expect(projectSummary(project({ outcome: "pending" }), false)).toBe("Waiting");
  });
});

describe("report helpers", () => {
  it("totals, pending and left-behind", () => {
    const r = report({
      projects: [project(), project({ project: "existing", outcome: "pending" })],
      inventory: [
        { category: "secrets", source: ".env", target: "", outcome: "not_imported", count: 1, detail: "never read" },
        { category: "tasks", source: "x", target: "y", outcome: "imported", count: 3, detail: "" },
      ],
    });
    expect(totals(r)).toEqual({ projects: 2, created: 1, issues: 4, decisions: 2 });
    expect(pendingProjects(r)).toHaveLength(1);
    expect(leftBehind(r).map((i) => i.category)).toEqual(["secrets"]);
  });
});

import { describe, expect, it } from "vitest";
import { collectionSections, dropped, moved, UNGROUPED } from "./collections";

const g = (id: string) => ({ project: { id, name: id, created_at: "", updated_at: "" }, swatch: "", waiting: 0, coordSub: "", workers: [] }) as any;
const c = (id: string, name: string, projects: string[]) =>
  ({ id, name, position: 0, collapsed: false, projects, created_at: "", updated_at: "" });

describe("collectionSections", () => {
  it("puts each project under every collection it is in, then the rest under Ungrouped", () => {
    const s = collectionSections(
      [c("c1", "Work", ["a", "b"]), c("c2", "Clients", ["b"])],
      [g("a"), g("b"), g("z")],
    );
    expect(s.map((x) => [x.name, x.groups.map((y) => y.project.id)])).toEqual([
      ["Work", ["a", "b"]], ["Clients", ["b"]], [UNGROUPED, ["z"]],
    ]);
  });

  it("keeps an empty collection's heading and drops Ungrouped when nothing is left", () => {
    const s = collectionSections([c("c1", "Work", ["a"]), c("c2", "Empty", [])], [g("a")]);
    expect(s.map((x) => x.name)).toEqual(["Work", "Empty"]);
    expect(s[1].groups).toEqual([]);
  });

  it("skips a member the list is not showing but keeps it in the membership", () => {
    const s = collectionSections([c("c1", "Work", ["a", "hidden", "b"])], [g("a"), g("b")]);
    expect(s[0].groups.map((x) => x.project.id)).toEqual(["a", "b"]);
    expect(s[0].members).toEqual(["a", "hidden", "b"]);
  });
});

describe("reordering", () => {
  it("moves within the list and ignores a move off either end", () => {
    expect(moved(["a", "b", "c"], "c", -1)).toEqual(["a", "c", "b"]);
    expect(moved(["a", "b", "c"], "a", -1)).toEqual(["a", "b", "c"]);
    expect(moved(["a", "b", "c"], "c", 1)).toEqual(["a", "b", "c"]);
    expect(moved(["a", "b"], "z", 1)).toEqual(["a", "b"]);
  });

  it("drops before another project, or at the end", () => {
    expect(dropped(["a", "b", "c"], "c", "a")).toEqual(["c", "a", "b"]);
    expect(dropped(["a", "b", "c"], "a", null)).toEqual(["b", "c", "a"]);
    expect(dropped(["a", "b", "c"], "a", "zz")).toEqual(["a", "b", "c"]);
  });

  it("reorders the whole membership, so a member that is not shown keeps its place", () => {
    expect(dropped(["a", "hidden", "b"], "b", "a")).toEqual(["b", "a", "hidden"]);
  });
});

import { describe, expect, it } from "vitest";
import type { DiffFile, FileStatus } from "./model";
import { buildFileTree, changeSignature, extensions, filterFiles, treeOrder, TreeNode, visibleRows } from "./fileTree";

const file = (path: string, status: FileStatus = "modified"): DiffFile =>
  ({ path, oldPath: path, lines: [], adds: 0, dels: 0, status, binary: false });
const names = (ns: TreeNode[]): unknown[] => ns.map((n) => (n.kind === "dir" ? { [n.name]: names(n.children) } : n.name));

describe("buildFileTree", () => {
  it("joins single-folder chains, puts folders first and sorts by name without case", () => {
    const files = ["uv.lock", "README.md", "src/agentic_work/system1/train.py", "src/agentic_work/system1/__init__.py",
      "docs/architecture.md", "tests/test_system1.py", "AGENTS.md", "pyproject.toml"].map((p) => file(p));
    expect(names(buildFileTree(files))).toEqual([
      { docs: ["architecture.md"] },
      { "src/agentic_work/system1": ["__init__.py", "train.py"] },
      { tests: ["test_system1.py"] },
      "AGENTS.md", "pyproject.toml", "README.md", "uv.lock",
    ]);
  });

  it("stops joining where a folder holds a file or several folders", () => {
    const files = ["a/b/x.ts", "a/b/c/y.ts", "a/b/d/z.ts"].map((p) => file(p));
    expect(names(buildFileTree(files))).toEqual([{ "a/b": [{ c: ["y.ts"] }, { d: ["z.ts"] }, "x.ts"] }]);
  });

  it("sorts numbers by value", () => {
    expect(names(buildFileTree([file("f10.ts"), file("f2.ts")]))).toEqual(["f2.ts", "f10.ts"]);
  });

  it("gives the diff the tree's order", () => {
    const files = ["z.md", "a/b.ts", "A.md"].map((p) => file(p));
    expect(treeOrder(buildFileTree(files)).map((f) => f.path)).toEqual(["a/b.ts", "A.md", "z.md"]);
  });
});

describe("filterFiles", () => {
  const files = ["app/src/DiffView.tsx", "crates/quarkd/src/diff.rs", "docs/README.md"].map((p) => file(p));
  it("matches every word anywhere in the path, in any case", () => {
    expect(filterFiles(files, "DIFF").map((f) => f.path)).toEqual(["app/src/DiffView.tsx", "crates/quarkd/src/diff.rs"]);
    expect(filterFiles(files, "src rs").map((f) => f.path)).toEqual(["crates/quarkd/src/diff.rs"]);
    expect(filterFiles(files, "  ")).toBe(files);
  });
});

describe("extensions", () => {
  it("counts files per extension", () => {
    expect(extensions([file("a.ts"), file("b/c.TS"), file("Makefile"), file(".gitignore")]))
      .toEqual([{ ext: ".ts", n: 2 }, { ext: "No extension", n: 2 }]);
  });
});

describe("visibleRows", () => {
  it("leaves out what is under a closed folder", () => {
    const tree = buildFileTree(["a/x.ts", "a/y.ts", "b.ts"].map((p) => file(p)));
    expect(visibleRows(tree, new Set(["a"])).map((r) => r.node.path)).toEqual(["a", "b.ts"]);
    expect(visibleRows(tree, new Set()).map((r) => [r.node.path, r.depth, r.parent])).toEqual([["a", 0, null], ["a/x.ts", 1, "a"], ["a/y.ts", 1, "a"], ["b.ts", 0, null]]);
  });
});

describe("changeSignature", () => {
  it("changes when the file's lines change", () => {
    const a = { ...file("a.ts"), lines: [{ kind: "add" as const, text: "one", new: 1 }] };
    const b = { ...file("a.ts"), lines: [{ kind: "add" as const, text: "two", new: 1 }] };
    expect(changeSignature(a)).toBe(changeSignature({ ...a }));
    expect(changeSignature(a)).not.toBe(changeSignature(b));
  });
});

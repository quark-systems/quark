// The changed-files tree beside a pull request's diff, after GitHub's "Files changed" tree: only
// the files the diff touches, folders before files, each level sorted by name, and a folder
// whose only child is a folder shown as one row ("src/agentic_work/system1").
import type { DiffFile, FileStatus } from "./model";

/** A file's state in the tree: its change, or a merge conflict with the base branch. */
export type TreeStatus = FileStatus | "conflicted";

export interface TreeDir { kind: "dir"; name: string; path: string; children: TreeNode[] }
export interface TreeFile { kind: "file"; name: string; path: string; file: DiffFile }
export type TreeNode = TreeDir | TreeFile;

const byName = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: "base", numeric: true }) || (a < b ? -1 : a > b ? 1 : 0);

/** Builds the tree of `files`: folders first, then files, each sorted by name; chains of
 *  single-folder folders are joined into one row. */
export function buildFileTree(files: DiffFile[]): TreeNode[] {
  const root: TreeDir = { kind: "dir", name: "", path: "", children: [] };
  for (const f of files) {
    const parts = f.path.split("/").filter(Boolean);
    let dir = root;
    for (let i = 0; i < parts.length - 1; i++) {
      const path = parts.slice(0, i + 1).join("/");
      let next = dir.children.find((c): c is TreeDir => c.kind === "dir" && c.path === path);
      if (!next) { next = { kind: "dir", name: parts[i], path, children: [] }; dir.children.push(next); }
      dir = next;
    }
    dir.children.push({ kind: "file", name: parts[parts.length - 1] ?? f.path, path: f.path, file: f });
  }
  return finish(root.children);
}

function finish(nodes: TreeNode[]): TreeNode[] {
  const out = nodes.map((n) => {
    if (n.kind === "file") return n;
    let d: TreeDir = { ...n, children: finish(n.children) };
    // Join a folder whose only child is a folder.
    while (d.children.length === 1 && d.children[0].kind === "dir") {
      const c = d.children[0];
      d = { kind: "dir", name: d.name + "/" + c.name, path: c.path, children: c.children };
    }
    return d;
  });
  return out.sort((a, b) => (a.kind === b.kind ? byName(a.name, b.name) : a.kind === "dir" ? -1 : 1));
}

/** The files in the order the tree shows them, so the diff below reads in the same order. */
export function treeOrder(nodes: TreeNode[]): DiffFile[] {
  const out: DiffFile[] = [];
  const walk = (ns: TreeNode[]) => { for (const n of ns) n.kind === "file" ? out.push(n.file) : walk(n.children); };
  walk(nodes);
  return out;
}

/** Files whose path holds every word of `query`, in any case; all files for an empty query. */
export function filterFiles(files: DiffFile[], query: string): DiffFile[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return files;
  return files.filter((f) => { const p = f.path.toLowerCase(); return words.every((w) => p.includes(w)); });
}

/** A file's extension as GitHub's filter lists it (".ts"), or "No extension". */
export function extensionOf(path: string): string {
  const name = path.split("/").pop() ?? "";
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot).toLowerCase() : NO_EXTENSION;
}
export const NO_EXTENSION = "No extension";

/** The extensions among `files`, with how many files have each, sorted by extension. */
export function extensions(files: DiffFile[]): { ext: string; n: number }[] {
  const m = new Map<string, number>();
  for (const f of files) { const e = extensionOf(f.path); m.set(e, (m.get(e) ?? 0) + 1); }
  return [...m].map(([ext, n]) => ({ ext, n })).sort((a, b) => byName(a.ext, b.ext));
}

/** The rows a person can reach with the keyboard: every node under an open folder, in order,
 *  with its depth and parent folder. */
export interface VisibleRow { node: TreeNode; depth: number; parent: string | null }
export function visibleRows(nodes: TreeNode[], closed: ReadonlySet<string>): VisibleRow[] {
  const out: VisibleRow[] = [];
  const walk = (ns: TreeNode[], depth: number, parent: string | null) => {
    for (const n of ns) {
      out.push({ node: n, depth, parent });
      if (n.kind === "dir" && !closed.has(n.path)) walk(n.children, depth + 1, n.path);
    }
  };
  walk(nodes, 0, null);
  return out;
}

/** A short fingerprint of a file's change, so a file marked viewed shows as changed once a new
 *  commit changes it again. */
export function changeSignature(f: DiffFile): string {
  let h = 5381;
  const feed = (s: string) => { for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0; };
  feed(f.status); feed(f.oldPath);
  for (const l of f.lines) { feed(l.kind); feed(l.text); feed("\n"); }
  return (h >>> 0).toString(36) + ":" + f.lines.length;
}

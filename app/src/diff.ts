// Minimal unified-diff parser for the worker view (from the POC's diff screen).

export interface DiffLine { kind: "add" | "del" | "ctx" | "hunk"; text: string; old?: number; new?: number }
export interface DiffFile { path: string; oldPath: string; lines: DiffLine[]; adds: number; dels: number }

export function parseUnifiedDiff(src: string): DiffFile[] {
  const files: DiffFile[] = [];
  let f: DiffFile | null = null;
  let o = 0, n = 0;
  for (const raw of src.split("\n")) {
    if (raw.startsWith("diff --git ")) {
      const m = /^diff --git a\/(.*) b\/(.*)$/.exec(raw);
      f = { path: m?.[2] ?? raw.slice(11), oldPath: m?.[1] ?? "", lines: [], adds: 0, dels: 0 };
      files.push(f);
    } else if (raw.startsWith("--- ") && (!f || !f.lines.length)) {
      if (!f) { f = { path: "", oldPath: "", lines: [], adds: 0, dels: 0 }; files.push(f); }
      f.oldPath = raw.slice(4).replace(/^a\//, "");
    } else if (raw.startsWith("+++ ") && f && !f.lines.length) {
      const p = raw.slice(4).replace(/^b\//, "");
      f.path = p === "/dev/null" ? f.oldPath : p;
    } else if (raw.startsWith("@@")) {
      const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
      o = m ? +m[1] : 0; n = m ? +m[2] : 0;
      f?.lines.push({ kind: "hunk", text: raw });
    } else if (f && f.lines.length) {
      if (raw.startsWith("+")) { f.lines.push({ kind: "add", text: raw.slice(1), new: n++ }); f.adds++; }
      else if (raw.startsWith("-")) { f.lines.push({ kind: "del", text: raw.slice(1), old: o++ }); f.dels++; }
      else if (raw.startsWith(" ")) f.lines.push({ kind: "ctx", text: raw.slice(1), old: o++, new: n++ });
      // "\ No newline at end of file", blank trailing lines and index/mode lines are skipped
    }
  }
  return files;
}

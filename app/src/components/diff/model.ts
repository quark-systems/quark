// Unified-diff model for the diff view: git patches parsed into files and lines, the gaps between
// hunks, word-level change ranges inside paired lines, and highlight-safe marking of those ranges.
// Shaped after MonoCode's unified diff model (https://github.com/hardbeat920/monocode, MIT,
// Copyright (c) 2026 Nick); Quark only has git patches, not both file texts, so lines come from the
// patch and folds are the unchanged runs git left out between hunks.

export interface DiffLine {
  kind: "add" | "del" | "ctx" | "hunk";
  text: string;
  old?: number;
  new?: number;
  /** On a hunk line: unchanged lines git left out above it, and the enclosing function git names. */
  hidden?: number;
  section?: string;
  /** On a changed line: the [start, end) range of characters that differ from its paired line. */
  word?: [number, number];
}
export type FileStatus = "added" | "deleted" | "renamed" | "modified";
export interface DiffFile {
  path: string; oldPath: string; lines: DiffLine[]; adds: number; dels: number;
  status: FileStatus; binary: boolean;
}

function newFile(path: string, oldPath: string): DiffFile {
  return { path, oldPath, lines: [], adds: 0, dels: 0, status: "modified", binary: false };
}

export function parseUnifiedDiff(src: string): DiffFile[] {
  const files: DiffFile[] = [];
  let f: DiffFile | null = null;
  let o = 0, n = 0;
  let oldEnd = 1; // the first old line after the previous hunk
  for (const raw of src.split("\n")) {
    if (raw.startsWith("diff --git ")) {
      const m = /^diff --git a\/(.*) b\/(.*)$/.exec(raw);
      f = newFile(m?.[2] ?? raw.slice(11), m?.[1] ?? "");
      files.push(f);
      oldEnd = 1;
    } else if (f && !f.lines.length && raw.startsWith("new file mode")) {
      f.status = "added";
    } else if (f && !f.lines.length && raw.startsWith("deleted file mode")) {
      f.status = "deleted";
    } else if (f && !f.lines.length && raw.startsWith("rename from ")) {
      f.status = "renamed"; f.oldPath = raw.slice(12);
    } else if (f && !f.lines.length && raw.startsWith("rename to ")) {
      f.path = raw.slice(10);
    } else if (f && !f.lines.length && raw.startsWith("Binary files ")) {
      f.binary = true;
    } else if (raw.startsWith("--- ") && (!f || !f.lines.length)) {
      if (!f) { f = newFile("", ""); files.push(f); oldEnd = 1; }
      const p = raw.slice(4).replace(/^a\//, "");
      if (p === "/dev/null") f.status = "added"; else f.oldPath = p;
    } else if (raw.startsWith("+++ ") && f && !f.lines.length) {
      const p = raw.slice(4).replace(/^b\//, "");
      if (p === "/dev/null") { f.status = "deleted"; f.path = f.oldPath; } else f.path = p;
    } else if (raw.startsWith("@@")) {
      const m = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,\d+)? @@ ?(.*)$/.exec(raw);
      o = m ? +m[1] : 0; n = m ? +m[3] : 0;
      const oldCount = m?.[2] != null ? +m[2] : 1;
      // A hunk that starts at line 0 (an empty side) leaves nothing out above it.
      const hidden = m && o > 0 ? Math.max(0, o - oldEnd) : 0;
      oldEnd = o + oldCount;
      f?.lines.push({ kind: "hunk", text: raw, hidden, section: m?.[4] || undefined });
    } else if (f && f.lines.length) {
      if (raw.startsWith("+")) { f.lines.push({ kind: "add", text: raw.slice(1), new: n++ }); f.adds++; }
      else if (raw.startsWith("-")) { f.lines.push({ kind: "del", text: raw.slice(1), old: o++ }); f.dels++; }
      else if (raw.startsWith(" ")) f.lines.push({ kind: "ctx", text: raw.slice(1), old: o++, new: n++ });
      // "\ No newline at end of file", blank trailing lines and index/mode lines are skipped
    }
  }
  for (const file of files) markWordChanges(file.lines);
  return files;
}

/** Pairs each run of deleted lines with the added run after it, line by line, and records on both
 *  lines the range that differs, when the lines share enough to make the range worth showing. */
export function markWordChanges(lines: DiffLine[]): void {
  let i = 0;
  while (i < lines.length) {
    if (lines[i].kind !== "del") { i++; continue; }
    const delStart = i;
    while (i < lines.length && lines[i].kind === "del") i++;
    const addStart = i;
    while (i < lines.length && lines[i].kind === "add") i++;
    const pairs = Math.min(addStart - delStart, i - addStart);
    for (let k = 0; k < pairs; k++) {
      const r = changedRange(lines[delStart + k].text, lines[addStart + k].text);
      if (r) { lines[delStart + k].word = r.a; lines[addStart + k].word = r.b; }
    }
  }
}

/** The differing middle of two strings after their common prefix and suffix, or null when the
 *  lines are identical or have too little in common for a word highlight to help. */
export function changedRange(a: string, b: string): { a: [number, number]; b: [number, number] } | null {
  if (a === b) return null;
  let p = 0;
  const max = Math.min(a.length, b.length);
  while (p < max && a[p] === b[p]) p++;
  let s = 0;
  while (s < max - p && a[a.length - 1 - s] === b[b.length - 1 - s]) s++;
  const shared = p + s;
  // Mostly rewritten lines read better whole than with one long highlight.
  if (shared < 3 || shared < Math.max(a.length, b.length) * 0.4) return null;
  return { a: [p, a.length - s], b: [p, b.length - s] };
}

/** Wraps characters [start, end) of the text inside `html` (escaped, with tags from a highlighter)
 *  in `<span class="cls">`, closing and reopening the span around tags so the markup stays valid. */
export function markRange(html: string, start: number, end: number, cls: string): string {
  if (end <= start) return html;
  const open = `<span class="${cls}">`;
  let out = "";
  let pos = 0; // characters of text seen
  let inMark = false;
  let i = 0;
  while (i < html.length) {
    if (html[i] === "<") {
      const close = html.indexOf(">", i);
      const tag = html.slice(i, close < 0 ? html.length : close + 1);
      if (inMark) out += "</span>" + tag + open; else out += tag;
      i += tag.length;
      continue;
    }
    if (pos === start && !inMark) { out += open; inMark = true; }
    if (pos === end && inMark) { out += "</span>"; inMark = false; }
    let ch = html[i];
    if (ch === "&") {
      const semi = html.indexOf(";", i);
      if (semi > i && semi - i <= 10) ch = html.slice(i, semi + 1);
    }
    out += ch;
    i += ch.length;
    pos++;
  }
  if (inMark) out += "</span>";
  // A mark opened just before a tag and closed right after it leaves an empty span; drop those.
  return out.split(open + "</span>").join("");
}

export function splitPath(path: string): { dir: string; base: string } {
  const i = path.lastIndexOf("/");
  return i < 0 ? { dir: "", base: path } : { dir: path.slice(0, i + 1), base: path.slice(i + 1) };
}

export function totals(files: DiffFile[]): { adds: number; dels: number } {
  let adds = 0, dels = 0;
  for (const f of files) { adds += f.adds; dels += f.dels; }
  return { adds, dels };
}

export type CommentSide = "new" | "old";

/** The side and line a comment on this diff line anchors to: the new side unless the line was deleted. */
export function anchorOf(l: DiffLine): { line: number; side: CommentSide } | null {
  if (l.kind === "del") return l.old != null ? { line: l.old, side: "old" } : null;
  if (l.kind === "add" || l.kind === "ctx") return l.new != null ? { line: l.new, side: "new" } : null;
  return null;
}

/** Files with more rows than this start collapsed, so one generated file doesn't bury the rest. */
export const LARGE_FILE_LINES = 600;

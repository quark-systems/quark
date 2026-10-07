// The diff view: files of a unified diff with syntax highlighting, word-level change marks, folded
// gaps between hunks and collapsible file sections. Shared by the worker view's changes, the PR
// center and memory commits; when `onComment` is given, clicking a line number opens a comment box.
// File header, counts and fold rows follow MonoCode's UnifiedDiffView
// (https://github.com/hardbeat920/monocode, MIT, Copyright (c) 2026 Nick).
import React, { memo, useEffect, useMemo, useState } from "react";
import { highlight, langForPath } from "../../markdown";
import { anchorOf, CommentSide, DiffFile, DiffLine, LARGE_FILE_LINES, markRange, splitPath, totals } from "./model";
import { errText } from "../../util";
import "./diff.css";

export { anchorOf };
export type { CommentSide };

export interface LineComment { path: string; line: number; side: CommentSide; body: string; pending?: boolean }
export type OnComment = (c: { path: string; line: number; side: CommentSide; body: string }) => Promise<void>;

/** Several files with a summary bar and expand/collapse all. */
export function DiffView({ files, comments, onComment }: {
  files: DiffFile[]; comments?: LineComment[]; onComment?: OnComment;
}) {
  // Bumping `all` re-applies "everything open" or "everything closed" to each file.
  const [all, setAll] = useState<{ open: boolean; n: number } | null>(null);
  const t = useMemo(() => totals(files), [files]);
  return (
    <div className="dv" data-testid="diff-view">
      {files.length > 1 && (
        <div className="dv-bar">
          <span>{files.length} files</span>
          <DiffCounts adds={t.adds} dels={t.dels} />
          <span className="spacer" />
          <button className="dv-icon" title="Expand all files" aria-label="Expand all files"
            onClick={() => setAll({ open: true, n: (all?.n ?? 0) + 1 })}><Unfold /></button>
          <button className="dv-icon" title="Collapse all files" aria-label="Collapse all files"
            onClick={() => setAll({ open: false, n: (all?.n ?? 0) + 1 })}><Fold /></button>
        </div>
      )}
      {files.map((f, i) => <FileDiff key={f.path + i} f={f} comments={comments} onComment={onComment} force={all} />)}
    </div>
  );
}

export const FileDiff = memo(function FileDiff({ f, comments, onComment, force }: {
  f: DiffFile; comments?: LineComment[]; onComment?: OnComment; force?: { open: boolean; n: number } | null;
}) {
  const large = f.lines.length > LARGE_FILE_LINES;
  const [open, setOpen] = useState(!large && !f.binary);
  useEffect(() => { if (force) setOpen(force.open); }, [force]);
  const { dir, base } = splitPath(f.path);

  return (
    <div className={"dv-file" + (open ? " open" : "")} data-testid="diff-file">
      <div className="file-head">
        <button className="dv-toggle" aria-expanded={open} onClick={() => setOpen(!open)}
          title={open ? "Collapse file" : "Expand file"}>
          <Chevron open={open} />
          <span className={"dv-status st-" + f.status} title={f.status}>{STATUS_LETTER[f.status]}</span>
          <span className="dv-path" title={f.path}><span className="dv-dir">{dir}</span><span className="dv-base">{base}</span></span>
          {f.status === "renamed" && f.oldPath && <span className="dv-from" title={f.oldPath}>from {f.oldPath}</span>}
        </button>
        {f.binary ? <span className="faint">binary</span> : <><DiffCounts adds={f.adds} dels={f.dels} /><DiffBar adds={f.adds} dels={f.dels} /></>}
      </div>
      {open && (f.binary ? <div className="dv-note">Binary file not shown.</div>
        : <FileBody f={f} comments={comments} onComment={onComment} />)}
      {!open && large && !f.binary && (
        <button className="dv-note dv-load" onClick={() => setOpen(true)}>
          Large diff: {f.lines.length.toLocaleString()} lines. Show it
        </button>
      )}
    </div>
  );
});

function FileBody({ f, comments, onComment }: { f: DiffFile; comments?: LineComment[]; onComment?: OnComment }) {
  const lang = langForPath(f.path);
  const html = useMemo(() => f.lines.map((l) => {
    if (l.kind === "hunk") return "";
    const h = highlight(l.text, lang);
    return l.word ? markRange(h, l.word[0], l.word[1], "dv-word") : h;
  }), [f, lang]);
  const [open, setOpen] = useState<number | null>(null); // index of the line with an open comment box
  const byAnchor = useMemo(() => {
    const m = new Map<string, LineComment[]>();
    for (const c of comments ?? []) if (c.path === f.path) {
      const k = `${c.side}:${c.line}`;
      m.set(k, [...(m.get(k) ?? []), c]);
    }
    return m;
  }, [comments, f.path]);

  if (!f.lines.length) return <div className="dv-note">{f.status === "renamed" ? "Renamed without changes." : "No textual changes."}</div>;
  return (
    <div className="dv-scroll"><table className="diff"><tbody>
      {f.lines.map((l, j) => {
        if (l.kind === "hunk") return <HunkRow key={j} l={l} first={j === 0} />;
        const a = anchorOf(l);
        const thread = a ? byAnchor.get(`${a.side}:${a.line}`) : undefined;
        const commentable = !!onComment && !!a;
        const click = commentable ? () => setOpen(j) : undefined;
        const tip = commentable ? "Comment on this line" : undefined;
        return (
          <React.Fragment key={j}>
            <tr className={l.kind + (commentable ? " commentable" : "")}>
              <td className="ln" onClick={click} title={tip}>{l.old ?? ""}</td>
              <td className="ln" onClick={click} title={tip}>{l.new ?? ""}</td>
              <td className="sign">
                {l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}
                {l.kind !== "ctx" && <span className="sr-only">{l.kind === "add" ? "Added: " : "Removed: "}</span>}
              </td>
              <td className="code" dangerouslySetInnerHTML={{ __html: html[j] || " " }} />
            </tr>
            {(thread || open === j) && a && (
              <tr className="comment-row"><td colSpan={4}>
                {thread?.map((c, k) => (
                  <div key={k} className={"line-comment" + (c.pending ? " pending" : "")} data-testid="line-comment">{c.body}</div>
                ))}
                {open === j && onComment && (
                  <CommentBox onCancel={() => setOpen(null)}
                    onSubmit={async (body) => { await onComment({ path: f.path, ...a, body }); setOpen(null); }} />
                )}
              </td></tr>
            )}
          </React.Fragment>
        );
      })}
    </tbody></table></div>
  );
}

/** A hunk boundary: how many unchanged lines git left out, and the function it is in. */
function HunkRow({ l, first }: { l: DiffLine; first: boolean }) {
  const hidden = l.hidden ?? 0;
  if (first && !hidden && !l.section) return null;
  return (
    <tr className="hunk" title={l.text}>
      <td colSpan={2} className="hunk-gap">{hidden > 0 ? <Dots /> : null}</td>
      <td colSpan={2}>
        {hidden > 0 && <span className="hunk-hidden">{hidden.toLocaleString()} unchanged {hidden === 1 ? "line" : "lines"}</span>}
        {l.section && <span className="hunk-section">{l.section}</span>}
      </td>
    </tr>
  );
}

export function DiffCounts({ adds, dels }: { adds: number; dels: number }) {
  return (
    <span className="dv-counts">
      {adds > 0 && <span className="adds">+{adds.toLocaleString()}</span>}
      {dels > 0 && <span className="dels">−{dels.toLocaleString()}</span>}
      {adds === 0 && dels === 0 && <span className="faint">0</span>}
    </span>
  );
}

/** Five blocks split between additions and deletions, as GitHub draws them. */
function DiffBar({ adds, dels }: { adds: number; dels: number }) {
  const total = adds + dels;
  const a = total ? Math.round((adds / total) * 5) : 0;
  const d = total ? Math.min(5 - a, Math.round((dels / total) * 5)) : 0;
  return (
    <span className="dv-bar-blocks" aria-hidden>
      {Array.from({ length: 5 }, (_, i) => <i key={i} className={i < a ? "a" : i < a + d ? "d" : ""} />)}
    </span>
  );
}

function CommentBox({ onSubmit, onCancel }: { onSubmit: (body: string) => Promise<void>; onCancel: () => void }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const send = async () => {
    const t = text.trim();
    if (!t || busy) return;
    setBusy(true); setErr(null);
    try { await onSubmit(t); } catch (e) { setErr(errText(e)); setBusy(false); }
  };
  return (
    <div className="comment-box">
      <textarea autoFocus rows={3} value={text} aria-label="Line comment"
        placeholder="Comment for the worker (Ctrl+Enter to send, Esc to cancel)"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") { e.preventDefault(); onCancel(); }
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void send(); }
        }} />
      <div className="comment-actions">
        {err && <span className="bad small-text" role="alert">{err}</span>}
        <span className="spacer" />
        <button className="btn small" onClick={onCancel}>Cancel</button>
        <button className="btn small on" disabled={busy || !text.trim()} onClick={() => void send()}>{busy ? "Sending…" : "Comment"}</button>
      </div>
    </div>
  );
}

const STATUS_LETTER: Record<string, string> = { added: "A", deleted: "D", renamed: "R", modified: "M" };

const svg = { width: 14, height: 14, viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", strokeWidth: 1.6, strokeLinecap: "round" as const, strokeLinejoin: "round" as const };
function Chevron({ open }: { open: boolean }) {
  return <svg {...svg} className={"dv-chevron" + (open ? " open" : "")} aria-hidden><path d="M6 4l4 4-4 4" /></svg>;
}
function Unfold() { return <svg {...svg} aria-hidden><path d="M8 1.5v4M5.5 3.5 8 1l2.5 2.5M8 14.5v-4M5.5 12.5 8 15l2.5-2.5M2.5 8h11" /></svg>; }
function Fold() { return <svg {...svg} aria-hidden><path d="M8 1v4M5.5 3 8 5.5 10.5 3M8 15v-4M5.5 13 8 10.5l2.5 2.5M2.5 8h11" /></svg>; }
function Dots() { return <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor" aria-hidden><circle cx="3" cy="8" r="1.3" /><circle cx="8" cy="8" r="1.3" /><circle cx="13" cy="8" r="1.3" /></svg>; }

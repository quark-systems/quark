// One file of a unified diff with syntax highlighting. Shared by the worker view's changes
// and the PR center; when `onComment` is given, clicking a line number opens a comment box.
import React, { memo, useMemo, useState } from "react";
import { highlight, langForPath } from "../markdown";
import type { DiffFile, DiffLine } from "../diff";
import { errText } from "../util";

export type CommentSide = "new" | "old";
export interface LineComment { path: string; line: number; side: CommentSide; body: string; pending?: boolean }
export type OnComment = (c: { path: string; line: number; side: CommentSide; body: string }) => Promise<void>;

/** The side and line a comment on this diff line anchors to: the new side unless the line was deleted. */
export function anchorOf(l: DiffLine): { line: number; side: CommentSide } | null {
  if (l.kind === "del") return l.old != null ? { line: l.old, side: "old" } : null;
  if (l.kind === "add" || l.kind === "ctx") return l.new != null ? { line: l.new, side: "new" } : null;
  return null;
}

export const FileDiff = memo(function FileDiff({ f, comments, onComment }: {
  f: DiffFile; comments?: LineComment[]; onComment?: OnComment;
}) {
  const lang = langForPath(f.path);
  const html = useMemo(() => f.lines.map((l) => (l.kind === "hunk" ? "" : highlight(l.text, lang))), [f, lang]);
  const [open, setOpen] = useState<number | null>(null); // index of the line with an open comment box
  const byAnchor = useMemo(() => {
    const m = new Map<string, LineComment[]>();
    for (const c of comments ?? []) if (c.path === f.path) {
      const k = `${c.side}:${c.line}`;
      m.set(k, [...(m.get(k) ?? []), c]);
    }
    return m;
  }, [comments, f.path]);

  return (
    <div className="file" data-testid="diff-file">
      <div className="file-head"><span>{f.path}</span><span className="adds">+{f.adds}</span><span className="dels">−{f.dels}</span></div>
      <table className="diff"><tbody>
        {f.lines.map((l, j) => {
          if (l.kind === "hunk") return <tr className="hunk" key={j}><td colSpan={4}>{l.text}</td></tr>;
          const a = anchorOf(l);
          const thread = a ? byAnchor.get(`${a.side}:${a.line}`) : undefined;
          const commentable = !!onComment && !!a;
          return (
            <React.Fragment key={j}>
              <tr className={l.kind + (commentable ? " commentable" : "")}>
                <td className="ln" onClick={commentable ? () => setOpen(j) : undefined}
                  title={commentable ? "Comment on this line" : undefined}>{l.old ?? ""}</td>
                <td className="ln" onClick={commentable ? () => setOpen(j) : undefined}
                  title={commentable ? "Comment on this line" : undefined}>{l.new ?? ""}</td>
                <td className="sign">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}</td>
                <td dangerouslySetInnerHTML={{ __html: html[j] || " " }} />
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
      </tbody></table>
    </div>
  );
});

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

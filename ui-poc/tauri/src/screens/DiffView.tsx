import React, { memo, useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "../store";
import { useNav, setNav } from "../nav";
import { api, Comment } from "../api";
import { highlight, langForPath } from "../markdown";

interface DLine { kind: "add" | "del" | "ctx" | "hunk"; text: string; old?: number; new?: number }
interface DFile { path: string; oldPath: string; lines: DLine[]; adds: number; dels: number }

export function parseUnifiedDiff(src: string): DFile[] {
  const files: DFile[] = [];
  let f: DFile | null = null;
  let o = 0, n = 0;
  for (const raw of src.split("\n")) {
    if (raw.startsWith("diff --git ")) {
      const m = /^diff --git a\/(.*) b\/(.*)$/.exec(raw);
      f = { path: m?.[2] ?? raw.slice(11), oldPath: m?.[1] ?? "", lines: [], adds: 0, dels: 0 };
      files.push(f);
    } else if (raw.startsWith("--- ")) {
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
      else if (raw.startsWith(" ") || raw === "") { if (raw === "" ) continue; f.lines.push({ kind: "ctx", text: raw.slice(1), old: o++, new: n++ }); }
      // "\ No newline at end of file" and index/mode lines are skipped
    }
  }
  return files;
}

const lineKey = (path: string, line: number) => `${path}:${line}`;

export function DiffView() {
  const nav = useNav();
  const prs = useStore((s) => s.prs);
  const list = Object.values(prs).filter((p) => !nav.project || p.project_id === nav.project).sort((a, b) => b.number - a.number);
  const prId = nav.pr && prs[nav.pr] ? nav.pr : list[0]?.id ?? null;
  const pr = prId ? prs[prId] : null;
  const [diff, setDiff] = useState<string | null>(null);
  const [comments, setComments] = useState<Comment[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null); // lineKey with composer open
  const body = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!prId) return;
    setDiff(null); setErr(null); setOpen(null);
    let live = true;
    Promise.all([api.diff(prId), api.comments(prId).catch(() => [])]).then(([d, c]) => {
      if (live) { setDiff(d); setComments(c); }
    }).catch((e) => live && setErr(String(e)));
    return () => { live = false; };
  }, [prId]);

  const files = useMemo(() => (diff ? parseUnifiedDiff(diff) : []), [diff]);
  const byLine = useMemo(() => {
    const m = new Map<string, Comment[]>();
    for (const c of comments) {
      const k = lineKey(c.path, c.line);
      m.set(k, [...(m.get(k) ?? []), c]);
    }
    return m;
  }, [comments]);

  if (!pr) return <div className="empty">No pull requests{nav.project ? " for this project" : ""}.</div>;

  const post = async (path: string, line: number, text: string) => {
    const c = await api.addComment(pr.id, { path, line, body: text });
    setComments((cs) => [...cs, c]);
    setOpen(null);
  };

  return (
    <div className="diff-wrap">
      <div className="diff-files">
        <div className="side-title">Pull request</div>
        <select value={pr.id} onChange={(e) => setNav({ pr: e.target.value })}
          style={{ width: "100%", background: "var(--bg-2)", color: "var(--fg)", border: "1px solid var(--line-2)", borderRadius: 5, padding: 4, font: "12px var(--mono)", marginBottom: 8 }}>
          {list.map((p) => <option key={p.id} value={p.id}>#{p.number} {p.title}</option>)}
        </select>
        <div style={{ padding: "0 8px 8px", fontSize: 12 }}>
          <div style={{ fontWeight: 500, marginBottom: 4 }}>{pr.title}</div>
          <div className="meta" style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            <span className={"pill " + (pr.checks === "passing" ? "green" : pr.checks === "failing" ? "red" : "yellow")}>checks {pr.checks}</span>
            <span className="pill">{pr.state}</span>
            <span className="pill">risk {pr.risk}</span>
            <span className="pill green">+{pr.additions}</span><span className="pill red">−{pr.deletions}</span>
          </div>
        </div>
        <div className="side-title">Files ({files.length})</div>
        {files.map((f, i) => (
          <button key={i} title={f.path} onClick={() => document.getElementById("file-" + i)?.scrollIntoView({ block: "start" })}>
            <span style={{ color: "var(--green)" }}>+{f.adds}</span> <span style={{ color: "var(--red)" }}>−{f.dels}</span> {f.path}
          </button>
        ))}
      </div>
      <div className="diff-body" ref={body}>
        {err && <div className="empty" style={{ color: "var(--red)" }}>{err}</div>}
        {!diff && !err && <div className="empty">Loading diff…</div>}
        {files.map((f, i) => (
          <FileDiff key={pr.id + i} idx={i} f={f} byLine={byLine} open={open} setOpen={setOpen} post={post} />
        ))}
      </div>
    </div>
  );
}

const FileDiff = memo(function FileDiff({ f, idx, byLine, open, setOpen, post }: {
  f: DFile; idx: number; byLine: Map<string, Comment[]>; open: string | null;
  setOpen: (k: string | null) => void; post: (path: string, line: number, text: string) => Promise<void>;
}) {
  const lang = langForPath(f.path);
  const html = useMemo(() => f.lines.map((l) => (l.kind === "hunk" ? "" : highlight(l.text, lang))), [f, lang]);
  return (
    <div className="file" id={"file-" + idx}>
      <div className="file-head"><span>{f.path}</span><span style={{ color: "var(--green)" }}>+{f.adds}</span><span style={{ color: "var(--red)" }}>−{f.dels}</span></div>
      <table className="diff"><tbody>
        {f.lines.map((l, j) => {
          if (l.kind === "hunk") return <tr className="hunk" key={j}><td colSpan={4}>{l.text}</td></tr>;
          const line = l.new ?? l.old!;
          const k = lineKey(f.path, line);
          const cs = byLine.get(k);
          const isOpen = open === k;
          return (
            <React.Fragment key={j}>
              <tr className={l.kind + (isOpen ? " sel" : "")} onClick={() => setOpen(isOpen ? null : k)}>
                <td className="ln">{l.old ?? ""}</td>
                <td className="ln">{l.new ?? ""}</td>
                <td className="sign">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}</td>
                <td dangerouslySetInnerHTML={{ __html: html[j] || " " }} />
              </tr>
              {(cs || isOpen) && (
                <tr><td colSpan={4} style={{ padding: 0, whiteSpace: "normal" }}>
                  <CommentThread comments={cs ?? []} composing={isOpen}
                    onCancel={() => setOpen(null)} onSubmit={(t) => post(f.path, line, t)} onReply={() => setOpen(k)} />
                </td></tr>
              )}
            </React.Fragment>
          );
        })}
      </tbody></table>
    </div>
  );
});

function CommentThread({ comments, composing, onCancel, onSubmit, onReply }: {
  comments: Comment[]; composing: boolean; onCancel: () => void; onSubmit: (t: string) => Promise<void>; onReply: () => void;
}) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const ta = useRef<HTMLTextAreaElement>(null);
  useEffect(() => { if (composing) ta.current?.focus(); }, [composing]);
  const submit = async () => {
    if (!text.trim() || busy) return;
    setBusy(true);
    try { await onSubmit(text.trim()); setText(""); } catch (e) { alert(String(e)); } finally { setBusy(false); }
  };
  return (
    <div className="comment-box" onClick={(e) => e.stopPropagation()}>
      {comments.map((c) => (
        <div className="comment" key={c.id}><span className="who">{c.author}</span>{c.body}</div>
      ))}
      {composing ? (
        <>
          <textarea ref={ta} value={text} placeholder="Leave a comment — Ctrl+Enter to post, Esc to cancel" onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submit(); }
              if (e.key === "Escape") onCancel();
            }} />
          <div className="comment-actions">
            <button className="btn" onClick={onCancel}>Cancel</button>
            <button className="btn on" onClick={submit} disabled={busy}>{busy ? "Posting…" : "Comment"}</button>
          </div>
        </>
      ) : (
        <div className="comment-actions"><button className="btn" onClick={onReply}>Reply</button></div>
      )}
    </div>
  );
}

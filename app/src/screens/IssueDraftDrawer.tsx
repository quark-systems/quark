// The New issue side chat: describe the work, the coordinator drafts issues (checking for duplicates
// and linking blockers), refine them by message or by editing in place, and nothing is created until
// the draft is accepted. An open draft comes back when the drawer opens again.
import React, { useEffect, useMemo, useRef, useState } from "react";
import { api, BeadsStatus, DraftIssue, Issue, IssueDraft } from "../api";
import { loadIssueDrafts, upsertIssueDraft, useStore } from "../store";
import { draftBlockers, openDraft, parseLabels, pushedTo } from "../issues";
import { errText } from "../util";

export function IssueDraftDrawer({ pid, beads, issues, seed, onClose, onCreated }: {
  pid: string; beads: BeadsStatus; issues: Issue[]; seed: string | null;
  onClose: () => void; onCreated: (issueId: string | undefined) => void;
}) {
  const drafts = useStore((s) => s.issueDrafts);
  // The draft this drawer works on: the one it started, else the Project's open one.
  const [draftId, setDraftId] = useState<string | null>(null);
  const draft: IssueDraft | undefined = draftId ? drafts[draftId] : openDraft(Object.values(drafts), pid);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => { loadIssueDrafts(pid).catch((e) => setErr(errText(e))); }, [pid]);
  useEffect(() => { input.current?.focus(); }, []);

  // Edits made here apply to the drafts as shown; the coordinator's next version replaces them.
  const [edits, setEdits] = useState<{ at: string; issues: DraftIssue[] } | null>(null);
  const edited = !!draft && edits?.at === draft.updated_at;
  const shown = draft ? (edited ? edits!.issues : draft.issues) : [];
  const edit = (key: string, patch: Partial<DraftIssue>) => {
    if (draft) setEdits({ at: draft.updated_at, issues: shown.map((d) => (d.key === key ? { ...d, ...patch } : d)) });
  };

  const send = async (message: string) => {
    const t = message.trim();
    if (!t || busy) return;
    setBusy(true); setErr(null);
    try {
      const d = draft && draft.state === "open" ? await api.refineIssueDraft(draft.id, t) : await api.startIssueDraft(pid, t);
      upsertIssueDraft(d);
      setDraftId(d.id);
      setText("");
    } catch (e) { setErr(errText(e)); }
    finally { setBusy(false); }
  };

  // Started from elsewhere with text ("Turn into an issue"): send it as the first message, once.
  const seeded = useRef(false);
  useEffect(() => {
    if (!seed || seeded.current) return;
    seeded.current = true;
    setBusy(true);
    api.startIssueDraft(pid, seed).then((d) => { upsertIssueDraft(d); setDraftId(d.id); })
      .catch((e) => setErr(errText(e))).finally(() => setBusy(false));
  }, [seed, pid]);

  const accept = async (start_worker: boolean) => {
    if (!draft || busy) return;
    setBusy(true); setErr(null);
    try {
      const d = await api.acceptIssueDraft(draft.id, { issues: edited ? shown : null, start_worker });
      upsertIssueDraft(d);
      onCreated(shown[0] ? d.created[shown[0].key] : Object.values(d.created)[0]);
    } catch (e) { setErr(errText(e)); setBusy(false); }
  };
  const discard = async () => {
    if (!draft || busy) { onClose(); return; }
    setBusy(true);
    try { upsertIssueDraft(await api.discardIssueDraft(draft.id)); onClose(); }
    catch (e) { setErr(errText(e)); setBusy(false); }
  };

  useEffect(() => {
    const k = (e: KeyboardEvent) => { if (e.key === "Escape" && !e.defaultPrevented) { e.preventDefault(); onClose(); } };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);

  const titles = useMemo(() => Object.fromEntries(issues.map((i) => [i.id, i.title])), [issues]);
  const messages = draft?.messages ?? [];
  // The drafts sit inside the coordinator's latest turn, which they belong to.
  let latest = -1;
  messages.forEach((m, n) => { if (m.role === "coordinator") latest = n; });
  const group = shown.length > 0 && (
    <DraftGroup key="drafts" drafts={shown} beads={beads} related={draft?.related ?? []} titles={titles} onEdit={edit} />
  );
  const n = shown.length;
  const waiting = !!draft?.waiting || (busy && !draft);

  return (
    <>
      <div className="drawer-scrim" aria-hidden="true" onMouseDown={onClose} />
      <section className="drawer" role="dialog" aria-label="New issue" data-testid="issue-draft">
        <div className="drawer-head">
          <span className="drawer-title">New issue</span>
          <span className="faint small-text">Nothing is created until you accept</span>
          <button type="button" className="icon-btn" aria-label="Close" onClick={onClose}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
          </button>
        </div>
        <div className="drawer-body" data-testid="draft-conversation">
          {!messages.length && !waiting && (
            <div className="faint">Describe the work. Quark drafts the issues, checks what is already open and links what each one waits for.</div>
          )}
          {messages.map((m, i) => (
            <div key={i} className={"draft-msg " + m.role}>
              <span className="who">{m.role === "user" ? "You" : "Quark"}</span>
              <span className="text">{m.text}</span>
              {i === latest && group}
            </div>
          ))}
          {latest < 0 && group}
          {waiting && <div className="faint small-text drafting" data-testid="draft-waiting">Quark is drafting…</div>}
          {edited && !waiting && <div className="faint small-text">Edited here. A new message asks Quark to redraft from its own copy.</div>}
        </div>
        <div className="drawer-foot">
          <form className="draft-composer" onSubmit={(e) => { e.preventDefault(); void send(text); }}>
            <label htmlFor="issue-draft-input" className="sr-only">Refine the issues</label>
            <input id="issue-draft-input" ref={input} value={text} onChange={(e) => setText(e.target.value)} autoComplete="off"
              placeholder="Refine: split, merge, change priority, add detail…" />
            <button type="submit" className="icon-btn" aria-label="Send" disabled={busy || !text.trim()}>
              <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path d="M8 13V3M4 7l4-4 4 4" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" /></svg>
            </button>
          </form>
          {err && <div className="bad small-text">{err}</div>}
          <div className="drawer-actions">
            <button type="button" className="btn big primary" disabled={!draft || !n || busy || draft.waiting} onClick={() => void accept(false)}>
              Create {n === 1 ? "1 issue" : `${n} issues`}
            </button>
            <button type="button" className="btn big" disabled={!draft || !n || busy || draft.waiting} onClick={() => void accept(true)}>Create and start a worker</button>
            <span className="spacer" />
            <button type="button" className="btn big quiet" disabled={!draft || busy} onClick={() => void discard()}>Discard</button>
          </div>
        </div>
      </section>
    </>
  );
}

/** The drafts as one bordered group, each editable in place, with what was judged related but kept apart. */
function DraftGroup({ drafts, beads, related, titles, onEdit }: {
  drafts: DraftIssue[]; beads: BeadsStatus; related: string[]; titles: Record<string, string>;
  onEdit: (key: string, patch: Partial<DraftIssue>) => void;
}) {
  return (
    <>
      <div className="draft-group">
        {drafts.map((d) => {
          const blockers = draftBlockers(d, drafts);
          const pushed = pushedTo(beads, d.labels);
          return (
            <div key={d.key} className="draft-card" data-testid="draft-issue">
              <div className="draft-facts">
                <span className="mono">new · {d.key}</span>
                <span>
                  <Editable value={d.issue_type} label={`Type of new ${d.key}`} onChange={(v) => onEdit(d.key, { issue_type: v })} />
                  {" · "}
                  <select className="inline-select" aria-label={`Priority of new ${d.key}`} value={d.priority}
                    onChange={(e) => onEdit(d.key, { priority: Number(e.target.value) })}>
                    {[0, 1, 2, 3, 4].map((p) => <option key={p} value={p}>P{p}</option>)}
                  </select>
                </span>
                {blockers.length ? <span className="violet">blocked by {blockers.join(", ")}</span> : pushed.length > 0 && <span>syncs to {pushed.join(", ")}</span>}
                <Editable value={d.labels.join(", ")} label={`Labels of new ${d.key}`} placeholder="add labels"
                  className="draft-labels" onChange={(v) => onEdit(d.key, { labels: parseLabels(v) })} />
              </div>
              <Editable value={d.title} label={`Title of new ${d.key}`} className="draft-title" onChange={(v) => v && onEdit(d.key, { title: v })} />
              <Editable value={d.description} label={`Description of new ${d.key}`} className="draft-desc" multiline placeholder="add a description"
                onChange={(v) => onEdit(d.key, { description: v })} />
            </div>
          );
        })}
      </div>
      {related.length > 0 && (
        <div className="faint small-text">Related, not merged in: {related.map((id) => (titles[id] ? `${id} ${titles[id]}` : id)).join(", ")}.</div>
      )}
    </>
  );
}

/** Text that turns into a box when clicked; Enter (or leaving the box) keeps the change, Esc drops it. */
function Editable({ value, label, onChange, multiline, className, placeholder }: {
  value: string; label: string; onChange: (v: string) => void; multiline?: boolean; className?: string; placeholder?: string;
}) {
  const [editing, setEditing] = useState(false);
  const [v, setV] = useState(value);
  // Esc closes the box, and the blur that follows must not keep the change after all.
  const open = useRef(false);
  if (!editing) {
    return (
      <button type="button" className={"editable " + (className ?? "") + (value ? "" : " placeholder")} title={`Edit: ${label}`}
        onClick={() => { setV(value); setEditing(true); open.current = true; }}>{value || placeholder}</button>
    );
  }
  const done = (keep: boolean) => {
    if (!open.current) return;
    open.current = false;
    setEditing(false);
    if (keep && v.trim() !== value) onChange(v.trim());
  };
  const props = {
    autoFocus: true, "aria-label": label, value: v, className: "editable-box " + (className ?? ""),
    onBlur: () => done(true),
    onKeyDown: (e: React.KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(false); }
      if (e.key === "Enter" && (!multiline || e.metaKey || e.ctrlKey)) { e.preventDefault(); done(true); }
    },
  };
  return multiline
    ? <textarea rows={3} {...props} onChange={(e) => setV(e.target.value)} />
    : <input {...props} onChange={(e) => setV(e.target.value)} />;
}

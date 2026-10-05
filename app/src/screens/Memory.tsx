// J8: a Project's memory. Learnings from finished tasks are reviewed here as proposals; accepted
// entries are browsed with the commit that added them, and any entry can be promoted to the
// user-level memory every Project's coordinator reads.
//
// Keys: j/k or ↑/↓ move, p and a switch between proposed and accepted, t opens the task.
// Proposed: Enter or e edits, ⌘/Ctrl+Enter accepts, x twice rejects, Esc leaves the box.
// Accepted: u promotes to user-level memory, c shows the commit.
import React, { useEffect, useMemo, useRef, useState } from "react";
import { api, MemoryCommit, MemoryEntry, MemoryEvidence, MemoryProposal, NotAvailable, UserMemoryEntry } from "../api";
import { go, href } from "../nav";
import { upsertMemoryProposal, useStore } from "../store";
import { nextAfterAnswer, step } from "../inbox";
import { pendingProposals, prLabel, promotedCopy, sortEntries } from "../memory";
import { parseUnifiedDiff } from "../diff";
import { ago, errText, savedUser, saveUser } from "../util";
import { FileDiff } from "../components/FileDiff";
import { Unavailable } from "../components/Unavailable";

type Tab = "proposed" | "accepted";
/** What the keyboard can do to the selected proposal or entry. */
interface Actions { edit?: () => void; accept?: () => void; reject?: () => void; promote?: () => void; commit?: () => void }

export function Memory({ project: pid, id }: { project: string; id?: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);
  const proposals = useStore((s) => s.memoryProposals);
  const proposed = useMemo(() => pendingProposals(Object.values(proposals), pid), [proposals, pid]);
  const accepted = useMemo(() => Object.values(proposals).filter((m) => m.project_id === pid && m.state === "accepted").length, [proposals, pid]);

  // Entries live in the Project repo, so they are read again whenever a proposal is accepted.
  const [entries, setEntries] = useState<MemoryEntry[] | null>(null);
  const [shared, setShared] = useState<UserMemoryEntry[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    api.memory(pid).then((list) => { if (live) { setEntries(sortEntries(list)); setErr(null); } })
      .catch((e) => { if (!live) return; if (e instanceof NotAvailable) setUnavailable(true); else setErr(errText(e)); });
    return () => { live = false; };
  }, [pid, accepted, connected]);
  useEffect(() => {
    let live = true;
    api.userMemory().then((list) => { if (live) setShared(list); }).catch(() => undefined);
    return () => { live = false; };
  }, [pid, connected]);

  const [tab, setTab] = useState<Tab>("proposed");
  const list: { id: string }[] = tab === "proposed" ? proposed : entries ?? [];
  // A selection in the URL wins; otherwise the first row of the current list.
  const selected = id && list.some((x) => x.id === id) ? id : list[0]?.id;
  const proposal = tab === "proposed" ? proposed.find((m) => m.id === selected) : undefined;
  const entry = tab === "accepted" ? entries?.find((e) => e.id === selected) : undefined;
  const select = (next: string | undefined) => go({ name: "memory", project: pid, id: next });
  const show = (t: Tab) => { setTab(t); select(undefined); };

  // Arriving with an entry in the URL shows the list it is in.
  const entriesLoaded = entries !== null;
  useEffect(() => {
    if (!id) return;
    if (proposed.some((m) => m.id === id)) setTab("proposed");
    else if (entries?.some((e) => e.id === id)) setTab("accepted");
    // Only when the URL changes or the entries arrive, so switching lists by hand is not undone.
  }, [id, entriesLoaded]);

  const rowRefs = useRef(new Map<string, HTMLElement>());
  useEffect(() => { if (selected) rowRefs.current.get(selected)?.scrollIntoView({ block: "nearest" }); }, [selected]);

  const actions = useRef<Actions>({});
  // Rejecting is final, so x asks once more before it does.
  const [armed, setArmed] = useState(false);
  useEffect(() => { setArmed(false); }, [selected, tab]);
  const reject = () => { if (armed) actions.current.reject?.(); else setArmed(true); };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.altKey || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.closest("input, textarea, select, [contenteditable]") || t.closest("[cmdk-root]"))) return;
      if (e.metaKey || e.ctrlKey) {
        if (e.key === "Enter" && proposal) { e.preventDefault(); actions.current.accept?.(); }
        return;
      }
      const move = (delta: number) => { e.preventDefault(); select(step(list, selected, delta)); };
      const task = (proposal ?? entry)?.evidence.task_id;
      switch (e.key) {
        case "j": case "ArrowDown": return move(1);
        case "k": case "ArrowUp": return move(-1);
        case "p": e.preventDefault(); show("proposed"); return;
        case "a": e.preventDefault(); show("accepted"); return;
        case "t": if (task) { e.preventDefault(); go({ name: "task", id: task }); } return;
        case "Enter": case "e": if (proposal) { e.preventDefault(); actions.current.edit?.(); } return;
        case "x": if (proposal) { e.preventDefault(); reject(); } return;
        case "Escape": setArmed(false); return;
        case "u": if (entry) { e.preventDefault(); actions.current.promote?.(); } return;
        case "c": if (entry) { e.preventDefault(); actions.current.commit?.(); } return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const onDecided = (m: MemoryProposal) => {
    const next = nextAfterAnswer(proposed, m.id);
    upsertMemoryProposal(m);
    select(next);
  };
  const onPromoted = (u: UserMemoryEntry) => setShared((cur) => [...cur.filter((x) => x.path !== u.path), u]);

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }

  const rowRef = (rid: string) => (el: HTMLElement | null) => { if (el) rowRefs.current.set(rid, el); else rowRefs.current.delete(rid); };
  return (
    <>
      <div className="header">
        <h1>Memory</h1>
        <a className="crumb" href={href({ name: "project", id: pid })}>{project.name}</a>
        <span className="spacer" />
        <div className="seg" role="tablist" aria-label="Memory">
          <button role="tab" aria-selected={tab === "proposed"} className={tab === "proposed" ? "on" : ""} onClick={() => show("proposed")}>
            Proposed <span className="n">{proposed.length}</span>
          </button>
          <button role="tab" aria-selected={tab === "accepted"} className={tab === "accepted" ? "on" : ""} onClick={() => show("accepted")}>
            Accepted{entries && <> <span className="n">{entries.length}</span></>}
          </button>
        </div>
      </div>
      <div className="screen inbox">
        <div className="inbox-list" data-testid="memory-list">
          {tab === "proposed" && proposed.map((m) => (
            <a key={m.id} ref={rowRef(m.id)} className={"decision-row" + (m.id === selected ? " on" : "")}
              href={href({ name: "memory", project: pid, id: m.id })} data-testid="memory-row" aria-current={m.id === selected ? "true" : undefined}>
              <div className="q">{m.text}</div>
              <div className="meta">
                <span className="pill">{m.source}</span>
                <span className="ellipsis">{m.evidence.task_title ?? ""}</span>
                <span className="spacer" />
                <span>{ago(m.proposed_at)}</span>
              </div>
            </a>
          ))}
          {tab === "accepted" && (entries ?? []).map((e) => (
            <a key={e.id} ref={rowRef(e.id)} className={"decision-row" + (e.id === selected ? " on" : "")}
              href={href({ name: "memory", project: pid, id: e.id })} data-testid="memory-row" aria-current={e.id === selected ? "true" : undefined}>
              <div className="q">{e.text}</div>
              <div className="meta">
                {promotedCopy(shared, e) && <span className="pill accent">user-level</span>}
                {e.accepted_by && <span className="faint ellipsis">accepted by {e.accepted_by}</span>}
                <span className="spacer" />
                <span>{ago(e.accepted_at ?? e.date)}</span>
              </div>
            </a>
          ))}
          {!list.length && (
            <div className="empty">
              {!connected ? "Waiting for the daemon…"
                : tab === "proposed" ? "Nothing to review. Learnings from finished tasks arrive here."
                : unavailable || (err && !entries) ? "" : entries === null ? "Loading…" : "No entries yet."}
            </div>
          )}
        </div>
        <div className="inbox-detail">
          {tab === "accepted" && unavailable ? <Unavailable what="Project memory" endpoint={`GET /v1/projects/${pid}/memory`} />
            : tab === "accepted" && err && !entries ? <div className="empty bad">{err}</div>
            : proposal ? <ProposalDetail key={proposal.id} m={proposal} actions={actions} armed={armed} onReject={reject} onDecided={onDecided} />
            : entry ? <EntryDetail key={entry.id} e={entry} promoted={promotedCopy(shared, entry)} actions={actions} onPromoted={onPromoted} />
            : <div className="empty">{tab === "proposed" ? "Select a proposal." : "Select an entry."}</div>}
          <div className="inbox-keys faint">
            <span className="kbd">j</span><span className="kbd">k</span> move ·{" "}
            {tab === "proposed" ? (
              <><span className="kbd">e</span> edit · <span className="kbd">⌘/Ctrl ↵</span> accept · <span className="kbd">x</span><span className="kbd">x</span> reject ·{" "}</>
            ) : (
              <><span className="kbd">u</span> promote · <span className="kbd">c</span> commit ·{" "}</>
            )}
            <span className="kbd">p</span><span className="kbd">a</span> proposed/accepted · <span className="kbd">t</span> task
          </div>
        </div>
      </div>
    </>
  );
}

/** What a learning rests on: its task, pull request and files, each a link where there is something to open. */
function EvidenceLinks({ ev }: { ev: MemoryEvidence }) {
  const task = useStore((s) => (ev.task_id ? s.tasks[ev.task_id] : undefined));
  const prs = useStore((s) => s.pullRequests);
  const pr = ev.pull_request_url ? Object.values(prs).find((p) => p.url === ev.pull_request_url) : undefined;
  return (
    <div className="m-evidence" data-testid="memory-evidence">
      <div className="m-label">Evidence</div>
      {ev.task_id && <div><span className="faint">Task</span> <a href={href({ name: "task", id: ev.task_id })}>{task?.title ?? ev.task_title ?? ev.task_id}</a></div>}
      {!ev.task_id && ev.task_title && <div><span className="faint">Task</span> {ev.task_title}</div>}
      {ev.pull_request_url && (
        <div>
          <span className="faint">Pull request</span>{" "}
          {pr ? <a href={href({ name: "pr", id: pr.id })}>{pr.repo}#{pr.number}</a>
            : <a href={ev.pull_request_url} target="_blank" rel="noreferrer">{prLabel(ev.pull_request_url)}</a>}
        </div>
      )}
      {ev.files.length > 0 && (
        <div className="m-files"><span className="faint">Files</span>{ev.files.map((f) => <span className="pill" key={f}>{f}</span>)}</div>
      )}
      {!ev.task_id && !ev.task_title && !ev.pull_request_url && !ev.files.length && <div className="faint">None recorded.</div>}
    </div>
  );
}

function ProposalDetail({ m, actions, armed, onReject, onDecided }: {
  m: MemoryProposal; actions: React.MutableRefObject<Actions>; armed: boolean; onReject: () => void; onDecided: (m: MemoryProposal) => void;
}) {
  const [text, setText] = useState(m.text);
  const [user, setUser] = useState(savedUser);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const box = useRef<HTMLTextAreaElement>(null);
  const edited = text.trim() !== m.text.trim();

  const decide = async (accept: boolean) => {
    if (busy || (accept && !text.trim())) return;
    setBusy(true); setErr(null);
    const by = user.trim();
    saveUser(by);
    try {
      onDecided(accept
        ? await api.acceptMemoryProposal(m.project_id, m.id, { decided_by: by || null, ...(edited ? { text: text.trim() } : {}) })
        : await api.rejectMemoryProposal(m.project_id, m.id, { decided_by: by || null }));
    } catch (e) {
      if (e instanceof NotAvailable) setUnavailable(true);
      else setErr(errText(e));
      setBusy(false);
    }
  };
  useEffect(() => {
    actions.current = { edit: () => box.current?.focus(), accept: () => void decide(true), reject: () => void decide(false) };
    return () => { actions.current = {}; };
  });

  return (
    <div className="decision" data-testid="memory-detail">
      <div className="d-context">
        <span className="pill">{m.source}</span> <span className="faint">proposed {ago(m.proposed_at)}</span>
      </div>
      <EvidenceLinks ev={m.evidence} />
      {unavailable ? (
        <Unavailable what="Reviewing memory" endpoint={`POST /v1/projects/${m.project_id}/memory/proposals/${m.id}:accept`} />
      ) : (
        <form className="d-form" onSubmit={(e) => { e.preventDefault(); void decide(true); }}>
          <textarea ref={box} aria-label="Memory entry" value={text} rows={6}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void decide(true); }
              if (e.key === "Escape") { e.preventDefault(); e.currentTarget.blur(); }
            }} />
          <div className="d-actions">
            <label className="faint small-text">Reviewing as{" "}
              <input aria-label="Reviewing as" placeholder="you" value={user} onChange={(e) => setUser(e.target.value)} />
            </label>
            {edited && <button className="link" type="button" onClick={() => setText(m.text)}>Undo edit</button>}
            {err && <span className="bad small-text">{err}</span>}
            <span className="spacer" />
            <button className={"btn" + (armed ? " danger" : "")} type="button" disabled={busy} onClick={onReject} data-testid="memory-reject">
              {armed ? "Reject? Press x again" : "Reject"}
            </button>
            <button className="btn on" type="submit" disabled={busy || !text.trim()}>{busy ? "Saving…" : edited ? "Accept edited" : "Accept"}</button>
          </div>
        </form>
      )}
    </div>
  );
}

function EntryDetail({ e, promoted, actions, onPromoted }: {
  e: MemoryEntry; promoted?: UserMemoryEntry; actions: React.MutableRefObject<Actions>; onPromoted: (u: UserMemoryEntry) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [open, setOpen] = useState(false);
  const [commit, setCommit] = useState<MemoryCommit | null>(null);
  const files = useMemo(() => (commit ? parseUnifiedDiff(commit.patch) : []), [commit]);

  const promote = async () => {
    if (busy || promoted) return;
    setBusy(true); setErr(null);
    try { onPromoted(await api.promoteMemoryEntry(e.project_id, e.id, savedUser().trim() || null)); }
    catch (x) { if (x instanceof NotAvailable) setUnavailable(true); else setErr(errText(x)); }
    finally { setBusy(false); }
  };
  const showCommit = () => {
    if (!e.commit) return;
    setOpen((o) => !o);
    if (!commit) api.memoryCommit(e.project_id, e.commit).then(setCommit).catch((x) => setErr(errText(x)));
  };
  useEffect(() => {
    actions.current = { promote: () => void promote(), commit: showCommit };
    return () => { actions.current = {}; };
  });

  return (
    <div className="decision" data-testid="memory-detail">
      <div className="d-context">
        {e.source && <><span className="pill">{e.source}</span>{" "}</>}
        <span className="faint">
          {e.accepted_at ? <>accepted{e.accepted_by ? <> by <b>{e.accepted_by}</b></> : ""} {ago(e.accepted_at)}</> : "added by hand"}
        </span>
      </div>
      <div className="d-question">{e.text}</div>
      <EvidenceLinks ev={e.evidence} />
      <div className="m-evidence">
        <div className="m-label">Project repo</div>
        <div className="mono small-text">{e.path}</div>
        {e.commit ? (
          <div>
            <span className="faint">Commit</span>{" "}
            <a className="mono" href={href({ name: "memory", project: e.project_id, id: e.id })} aria-expanded={open} data-testid="memory-commit"
              onClick={(x) => { x.preventDefault(); showCommit(); }}>{e.commit.slice(0, 10)}</a>
          </div>
        ) : <div className="faint">No commit recorded.</div>}
      </div>
      {open && commit && (
        <div className="m-commit" data-testid="memory-commit-view">
          <div className="small-text"><b>{commit.subject}</b> <span className="faint">{commit.author ?? ""}{commit.date ? ` · ${ago(commit.date)}` : ""}</span></div>
          {files.map((f) => <FileDiff key={f.path} f={f} />)}
          {!files.length && <div className="faint small-text">This commit changed nothing under memory/.</div>}
        </div>
      )}
      <div className="m-shared" data-testid="memory-shared">
        {promoted ? (
          <>
            <div><span className="pill accent">user-level</span> Every Project's coordinator reads this entry.</div>
            <div className="mono small-text faint">{promoted.path}</div>
            {promoted.promoted_at && <div className="faint small-text">promoted{promoted.promoted_by ? ` by ${promoted.promoted_by}` : ""} {ago(promoted.promoted_at)}</div>}
          </>
        ) : unavailable ? (
          <Unavailable what="Promoting memory" endpoint={`POST /v1/projects/${e.project_id}/memory/${e.id}:promote`} />
        ) : (
          <div className="d-actions">
            <span className="faint small-text">Only this Project's coordinator reads this entry.</span>
            <span className="spacer" />
            <button className="btn" type="button" disabled={busy} onClick={() => void promote()}>{busy ? "Promoting…" : "Promote to user-level memory"}</button>
          </div>
        )}
        {err && <div className="bad small-text">{err}</div>}
      </div>
    </div>
  );
}

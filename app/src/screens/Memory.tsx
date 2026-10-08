// J8: a Project's memory. Learnings from finished tasks are reviewed here as proposals, and each is
// kept where the people who need it will find it: this Project (its Beads memories, or `memory/` in
// the Project repo without Beads), all of the user's Projects (user-level memory), or anyone working
// in the repo (a PR to AGENTS.md). The other lists browse what is kept, and the Project's decisions.
//
// Keys: j/k or ↑/↓ move, p shows what is to review, a what this Project keeps, t opens the task.
// To review: Enter or e edits, ⌘/Ctrl+Enter accepts, x twice rejects, Esc leaves the box.
// This project: x twice forgets a Beads memory; on a `memory/` entry, u promotes it to user-level
// memory and c shows the commit.
import React, { useEffect, useMemo, useRef, useState } from "react";
import {
  api, BeadsMemory, Issue, MemoryCommit, MemoryEntry, MemoryEvidence, MemoryProposal, MemoryScope, NotAvailable, UserMemoryEntry,
} from "../api";
import { go, href } from "../nav";
import { loadBeads, setIssueSeed, upsertMemoryProposal, useStore } from "../store";
import { nextAfterAnswer, step } from "../inbox";
import { pendingProposals, prLabel, promotedCopy, sortEntries } from "../memory";
import { isDecision, scopeChoices, statusLabel } from "../issues";
import { parseUnifiedDiff } from "../components/diff/model";
import { ago, errText, savedUser, saveUser } from "../util";
import { DiffView } from "../components/diff/DiffView";
import { Unavailable } from "../components/Unavailable";
import "./issues.css";

type Tab = "proposed" | "project" | "user" | "decisions";
/** What the keyboard can do to the selected row. */
interface Actions { edit?: () => void; accept?: () => void; reject?: () => void; promote?: () => void; commit?: () => void }

export function Memory({ project: pid, id }: { project: string; id?: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);
  const proposals = useStore((s) => s.memoryProposals);
  const beads = useStore((s) => s.beads[pid]);
  const activity = useStore((s) => s.beadsActivity[pid] ?? 0);
  const proposed = useMemo(() => pendingProposals(Object.values(proposals), pid), [proposals, pid]);
  const accepted = useMemo(() => Object.values(proposals).filter((m) => m.project_id === pid && m.state === "accepted").length, [proposals, pid]);
  useEffect(() => { loadBeads(pid).catch(() => undefined); }, [pid, connected]);
  const beadsReady = beads?.state === "ready";

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
  }, [pid, accepted, connected]);

  // With Beads, this Project's memory and its decisions are Beads records.
  const [memories, setMemories] = useState<BeadsMemory[] | null>(null);
  const [decisions, setDecisions] = useState<Issue[]>([]);
  useEffect(() => {
    if (!beadsReady) return;
    let live = true;
    const t = setTimeout(() => {
      api.memories(pid).then((list) => { if (live) setMemories(sortMemories(list)); }).catch((e) => { if (live) setErr(errText(e)); });
      api.issues(pid, "all").then((list) => { if (live) setDecisions(list.filter(isDecision)); }).catch(() => undefined);
    }, activity ? 250 : 0);
    return () => { live = false; clearTimeout(t); };
  }, [pid, beadsReady, accepted, activity, connected]);

  const [tab, setTab] = useState<Tab>("proposed");
  const projectList: { id: string }[] = beadsReady ? (memories ?? []).map((m) => ({ id: m.key })) : entries ?? [];
  const list: { id: string }[] = tab === "proposed" ? proposed : tab === "project" ? projectList : tab === "user" ? shared : decisions;
  // A selection in the URL wins; otherwise the first row of the current list.
  const selected = id && list.some((x) => x.id === id) ? id : list[0]?.id;
  const proposal = tab === "proposed" ? proposed.find((m) => m.id === selected) : undefined;
  const memory = tab === "project" && beadsReady ? memories?.find((m) => m.key === selected) : undefined;
  const entry = tab === "project" && !beadsReady ? entries?.find((e) => e.id === selected) : undefined;
  const userEntry = tab === "user" ? shared.find((u) => u.id === selected) : undefined;
  const decision = tab === "decisions" ? decisions.find((d) => d.id === selected) : undefined;
  const select = (next: string | undefined) => go({ name: "memory", project: pid, id: next });
  const show = (t: Tab) => { setTab(t); select(undefined); };

  // Arriving with a row in the URL shows the list it is in.
  const loaded = `${entries !== null}${memories !== null}${shared.length}${decisions.length}`;
  useEffect(() => {
    if (!id) return;
    if (proposed.some((m) => m.id === id)) setTab("proposed");
    else if (projectList.some((e) => e.id === id)) setTab("project");
    else if (shared.some((u) => u.id === id)) setTab("user");
    else if (decisions.some((d) => d.id === id)) setTab("decisions");
    // Only when the URL changes or a list arrives, so switching lists by hand is not undone.
  }, [id, loaded]);

  const rowRefs = useRef(new Map<string, HTMLElement>());
  useEffect(() => { if (selected) rowRefs.current.get(selected)?.scrollIntoView({ block: "nearest" }); }, [selected]);

  const actions = useRef<Actions>({});
  // Rejecting and forgetting are final, so x asks once more before it does.
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
      const task = (proposal ?? entry ?? memory ?? userEntry)?.evidence?.task_id;
      switch (e.key) {
        case "j": case "ArrowDown": return move(1);
        case "k": case "ArrowUp": return move(-1);
        case "p": e.preventDefault(); show("proposed"); return;
        case "a": e.preventDefault(); show("project"); return;
        case "t": if (task) { e.preventDefault(); go({ name: "task", id: task }); } return;
        case "Enter": case "e": if (proposal) { e.preventDefault(); actions.current.edit?.(); } return;
        case "x": if (proposal || memory) { e.preventDefault(); reject(); } return;
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
  const onForgotten = (key: string) => {
    const rest = (memories ?? []).filter((m) => m.key !== key);
    select(nextAfterAnswer((memories ?? []).map((m) => ({ id: m.key })), key));
    setMemories(rest);
  };

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }

  const chips: { tab: Tab; label: string; n: number | null }[] = [
    { tab: "proposed", label: "To review", n: proposed.length },
    { tab: "project", label: "This project", n: beadsReady ? memories?.length ?? null : entries?.length ?? null },
    { tab: "user", label: "All projects", n: shared.length },
    { tab: "decisions", label: "Decisions", n: decisions.length },
  ];
  const rowRef = (rid: string) => (el: HTMLElement | null) => { if (el) rowRefs.current.set(rid, el); else rowRefs.current.delete(rid); };
  const row = (rid: string, body: React.ReactNode, key = rid) => (
    <a key={key} ref={rowRef(rid)} className={"mem-row" + (rid === selected ? " on" : "")}
      href={href({ name: "memory", project: pid, id: rid })} data-testid="memory-row" aria-current={rid === selected ? "true" : undefined}>{body}</a>
  );
  const projectUnavailable = tab === "project" && !beadsReady && unavailable;
  return (
    <>
      <div className="header">
        <h1>Memory</h1>
        <a className="crumb" href={href({ name: "project", id: pid })}>{project.name}</a>
        <span className="spacer" />
        <a className="btn" href={href({ name: "issues", project: pid })} data-testid="nav-issues">Issues</a>
      </div>
      <div className="screen issues-layout">
        <section className="mem-list" aria-label="Memory" data-testid="memory-list">
          <div className="chips" role="group" aria-label="Show">
            {chips.map((c) => (
              <button key={c.tab} type="button" className="chip" aria-pressed={tab === c.tab} onClick={() => show(c.tab)}>
                {c.label}{c.n !== null && <> {c.n}</>}
              </button>
            ))}
          </div>
          {tab === "proposed" && proposed.map((m) => row(m.id, (
            <>
              <span className="q">{m.text}</span>
              <span className="meta">
                <span className="ellipsis">proposed by {m.source}{m.evidence.task_title ? ` · ${m.evidence.task_title}` : ""}</span>
                <span className="spacer" />
                <span>{ago(m.proposed_at)}</span>
              </span>
            </>
          )))}
          {tab === "project" && beadsReady && (memories ?? []).map((m) => row(m.key, (
            <>
              <span className="q">{m.value}</span>
              <span className="meta">
                <span className="mono ellipsis">{m.key}</span>
                {m.accepted_by && <span className="ellipsis">· accepted by {m.accepted_by}</span>}
                <span className="spacer" />
                <span>{ago(m.accepted_at)}</span>
              </span>
            </>
          )))}
          {tab === "project" && !beadsReady && (entries ?? []).map((e) => row(e.id, (
            <>
              <span className="q">{e.text}</span>
              <span className="meta">
                {promotedCopy(shared, e) && <span className="pill accent">user-level</span>}
                {e.accepted_by && <span className="ellipsis">accepted by {e.accepted_by}</span>}
                <span className="spacer" />
                <span>{ago(e.accepted_at ?? e.date)}</span>
              </span>
            </>
          )))}
          {tab === "user" && shared.map((u) => row(u.id, (
            <>
              <span className="q">{u.text}</span>
              <span className="meta">
                <span className="ellipsis">{u.project_name ? `from ${u.project_name}` : "written by hand"}</span>
                <span className="spacer" />
                <span>{ago(u.promoted_at ?? u.date)}</span>
              </span>
            </>
          )))}
          {tab === "decisions" && decisions.map((d) => row(d.id, (
            <>
              <span className="q">{d.title}</span>
              <span className="meta"><span className="mono">{d.id}</span><span>· {d.status === "closed" ? "decided" : "waiting on you"}</span></span>
            </>
          )))}
          {!list.length && (
            <div className="empty">
              {!connected ? "Waiting for the daemon…"
                : tab === "proposed" ? "Nothing to review. Learnings from finished tasks arrive here."
                : tab === "user" ? "Nothing in your own memory yet."
                : tab === "decisions" ? (beadsReady ? "No decisions recorded yet." : "Decisions are recorded once this project has Beads.")
                : projectUnavailable || (err && !entries && !memories) ? "" : (beadsReady ? memories : entries) === null ? "Loading…" : "Nothing kept yet."}
            </div>
          )}
          {beadsReady && (
            <div className="mem-note">
              Accepted memory is a Beads record in the repo. Every worker gets the relevant entries in its brief; the coordinator reads all of them.
            </div>
          )}
        </section>
        <div className="issue-detail">
          {projectUnavailable ? <Unavailable what="Project memory" endpoint={`GET /v1/projects/${pid}/memory`} />
            : tab === "project" && err && !entries && !memories ? <div className="empty bad">{err}</div>
            : proposal ? <ProposalDetail key={proposal.id} m={proposal} beadsReady={beadsReady} scopes={scopeChoices(beads)} actions={actions} armed={armed} onReject={reject} onDecided={onDecided} />
            : memory ? <BeadsMemoryDetail key={memory.key} pid={pid} m={memory} actions={actions} armed={armed} onForget={reject} onForgotten={onForgotten} />
            : entry ? <EntryDetail key={entry.id} e={entry} promoted={promotedCopy(shared, entry)} actions={actions} onPromoted={onPromoted} />
            : userEntry ? <UserEntryDetail key={userEntry.id} u={userEntry} />
            : decision ? <DecisionDetail key={decision.id} pid={pid} d={decision} />
            : <div className="empty">Select a row.</div>}
          <div className="inbox-keys faint">
            <span className="kbd">j</span><span className="kbd">k</span> move ·{" "}
            {tab === "proposed" && <><span className="kbd">e</span> edit · <span className="kbd">⌘/Ctrl ↵</span> accept · <span className="kbd">x</span><span className="kbd">x</span> reject ·{" "}</>}
            {tab === "project" && beadsReady && <><span className="kbd">x</span><span className="kbd">x</span> forget ·{" "}</>}
            {tab === "project" && !beadsReady && <><span className="kbd">u</span> promote · <span className="kbd">c</span> commit ·{" "}</>}
            <span className="kbd">p</span><span className="kbd">a</span> to review/this project · <span className="kbd">t</span> task
          </div>
        </div>
      </div>
    </>
  );
}

/** Newest first; memories set with `bd remember` by hand carry no date and come last by key. */
function sortMemories(list: BeadsMemory[]): BeadsMemory[] {
  return [...list].sort((a, b) => (b.accepted_at ?? "").localeCompare(a.accepted_at ?? "") || a.key.localeCompare(b.key));
}

/** What a learning rests on: its task, pull request and files, each a link where there is something to open. */
function EvidenceLinks({ ev }: { ev: MemoryEvidence }) {
  const task = useStore((s) => (ev.task_id ? s.tasks[ev.task_id] : undefined));
  const prs = useStore((s) => s.pullRequests);
  const pr = ev.pull_request_url ? Object.values(prs).find((p) => p.url === ev.pull_request_url) : undefined;
  return (
    <div className="issue-section" data-testid="memory-evidence">
      <div className="issue-label">Evidence</div>
      {ev.task_id && <div><a href={href({ name: "task", id: ev.task_id })}>{task?.title ?? ev.task_title ?? ev.task_id}</a> <span className="faint">· task</span></div>}
      {!ev.task_id && ev.task_title && <div>{ev.task_title} <span className="faint">· task</span></div>}
      {ev.pull_request_url && (
        <div>
          {pr ? <a href={href({ name: "pr", id: pr.id })}>{pr.repo}#{pr.number}</a>
            : <a href={ev.pull_request_url} target="_blank" rel="noreferrer">{prLabel(ev.pull_request_url)}</a>}
          <span className="faint"> · pull request</span>
        </div>
      )}
      {ev.files.length > 0 && (
        <div className="m-files">{ev.files.map((f) => <span className="pill" key={f}>{f}</span>)}<span className="faint">· files</span></div>
      )}
      {!ev.task_id && !ev.task_title && !ev.pull_request_url && !ev.files.length && <div className="faint">None recorded.</div>}
    </div>
  );
}

function ProposalDetail({ m, beadsReady, scopes, actions, armed, onReject, onDecided }: {
  m: MemoryProposal; beadsReady: boolean; scopes: ReturnType<typeof scopeChoices>; actions: React.MutableRefObject<Actions>; armed: boolean;
  onReject: () => void; onDecided: (m: MemoryProposal) => void;
}) {
  const [text, setText] = useState(m.text);
  const [scope, setScope] = useState<MemoryScope>("project");
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
        ? await api.acceptMemoryProposal(m.project_id, m.id, { decided_by: by || null, scope, ...(edited ? { text: text.trim() } : {}) })
        : await api.rejectMemoryProposal(m.project_id, m.id, { decided_by: by || null }));
    } catch (e) {
      if (e instanceof NotAvailable) setUnavailable(true);
      else setErr(errText(e));
      setBusy(false);
    }
  };
  // Work rather than knowledge: draft it as an issue instead. The proposal stays to review.
  const toIssue = () => {
    setIssueSeed({ project_id: m.project_id, text: text.trim() });
    go({ name: "issues", project: m.project_id });
  };
  useEffect(() => {
    actions.current = { edit: () => box.current?.focus(), accept: () => void decide(true), reject: () => void decide(false) };
    return () => { actions.current = {}; };
  });

  return (
    <div className="issue" data-testid="memory-detail">
      <div className="issue-head">
        <div><span className="mem-kind">Proposed · learning</span> <span className="faint small-text">· from the {m.source} · {ago(m.proposed_at)}</span></div>
      </div>
      {unavailable ? (
        <Unavailable what="Reviewing memory" endpoint={`POST /v1/projects/${m.project_id}/memory/proposals/${m.id}:accept`} />
      ) : (
        <form className="issue" style={{ padding: 0 }} onSubmit={(e) => { e.preventDefault(); void decide(true); }}>
          <textarea ref={box} className="mem-text" aria-label="Memory entry" value={text} rows={4}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void decide(true); }
              if (e.key === "Escape") { e.preventDefault(); e.currentTarget.blur(); }
            }} />
          <EvidenceLinks ev={m.evidence} />
          <fieldset className="mem-scope">
            <legend className="issue-label">Who should know</legend>
            {scopes.map((s) => (
              <label key={s.scope}>
                <input type="radio" name={`scope-${m.id}`} value={s.scope} checked={scope === s.scope} onChange={() => setScope(s.scope)} />
                <span>{s.label} <span className="faint">· {s.hint}</span></span>
              </label>
            ))}
          </fieldset>
          <div className="mem-actions">
            <button className="btn big primary" type="submit" disabled={busy || !text.trim()}>{busy ? "Saving…" : edited ? "Accept edited" : "Accept"}</button>
            {beadsReady && <button className="btn big" type="button" disabled={busy || !text.trim()} onClick={toIssue}>Turn into an issue</button>}
            <button className={"btn big" + (armed ? " danger" : " quiet")} type="button" disabled={busy} onClick={onReject} data-testid="memory-reject">
              {armed ? "Reject? Press x again" : "Reject"}
            </button>
            <span className="spacer" />
            {edited && <button className="link" type="button" onClick={() => setText(m.text)}>Undo edit</button>}
            <label className="faint small-text">Reviewing as{" "}
              <input aria-label="Reviewing as" placeholder="you" value={user} onChange={(e) => setUser(e.target.value)} />
            </label>
          </div>
          {err && <div className="bad small-text">{err}</div>}
        </form>
      )}
    </div>
  );
}

function BeadsMemoryDetail({ pid, m, actions, armed, onForget, onForgotten }: {
  pid: string; m: BeadsMemory; actions: React.MutableRefObject<Actions>; armed: boolean; onForget: () => void; onForgotten: (key: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const forget = async () => {
    if (busy) return;
    setBusy(true); setErr(null);
    try { await api.forgetMemory(pid, m.key); onForgotten(m.key); }
    catch (e) { setErr(errText(e)); setBusy(false); }
  };
  useEffect(() => {
    actions.current = { reject: () => void forget() };
    return () => { actions.current = {}; };
  });
  return (
    <div className="issue" data-testid="memory-detail">
      <div className="issue-head">
        <div className="issue-facts"><span className="mono">{m.key}</span><span>Beads memory</span></div>
        <div className="mem-value">{m.value}</div>
        <div className="faint small-text">
          {m.accepted_at ? <>accepted{m.accepted_by ? <> by <b>{m.accepted_by}</b></> : ""}{m.source ? ` from the ${m.source}` : ""} · {ago(m.accepted_at)}</> : "remembered by hand"}
        </div>
      </div>
      {m.evidence && <EvidenceLinks ev={m.evidence} />}
      <div className="mem-actions">
        <span className="faint small-text">Workers get it in their brief when it is relevant; the coordinator reads every one.</span>
        <span className="spacer" />
        <button className={"btn big" + (armed ? " danger" : "")} type="button" disabled={busy} onClick={() => (armed ? void forget() : onForget())} data-testid="memory-forget">
          {armed ? "Forget? Press again" : "Forget"}
        </button>
      </div>
      {err && <div className="bad small-text">{err}</div>}
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
          {files.length > 0 && <DiffView files={files} />}
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

/** A user-level entry: every Project's coordinator reads it, so it is shown, not changed, here. */
function UserEntryDetail({ u }: { u: UserMemoryEntry }) {
  return (
    <div className="issue" data-testid="memory-detail">
      <div className="issue-head">
        <div className="issue-facts"><span>Your own memory</span>{u.project_name && <span>· from {u.project_name}</span>}</div>
        <div className="mem-value">{u.text}</div>
        <div className="faint small-text">
          {u.promoted_at ? <>kept{u.promoted_by ? ` by ${u.promoted_by}` : ""} · {ago(u.promoted_at)}</> : "written by hand"}
        </div>
      </div>
      <EvidenceLinks ev={u.evidence} />
      <div className="issue-section">
        <div className="issue-label">File</div>
        <div className="mono small-text">{u.path}</div>
        <div className="faint small-text">Every Project's coordinator reads this.</div>
      </div>
    </div>
  );
}

/** A decision bead, read-only here; the Issues tab shows what waits for it. */
function DecisionDetail({ pid, d }: { pid: string; d: Issue }) {
  return (
    <div className="issue" data-testid="memory-detail">
      <div className="issue-head">
        <div className="issue-facts"><span className="mono">{d.id}</span><span>decision · {d.status === "closed" ? "decided" : statusLabel(d.status)}</span></div>
        <h2>{d.title}</h2>
        {d.description && <div className="issue-desc">{d.description}</div>}
      </div>
      <div><a className="btn big" href={href({ name: "issues", project: pid, id: d.id })}>Open in Issues</a></div>
    </div>
  );
}

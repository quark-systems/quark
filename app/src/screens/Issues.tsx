// The Project's issues, from its Beads database, which quarkd mirrors both ways with GitHub Issues.
// The list filters by where an issue stands; the detail shows what it waits for and what waits for it,
// and starts a worker on it once nothing it waits for is open. New issues are drafted in a side chat
// with the coordinator (IssueDraftDrawer) and created only when accepted.
//
// Keys: j/k or ↑/↓ move, n opens New issue, Esc closes it.
import React, { useEffect, useMemo, useRef, useState } from "react";
import { api, BeadsStatus, Issue, IssueDetail, IssueRef } from "../api";
import { go, href } from "../nav";
import { loadBeads, setBeads, setIssueSeed, useStore } from "../store";
import { step } from "../inbox";
import {
  beadsWhere, chipCounts, chipList, CHIPS, githubNumber, inChip, IssueChip, issueMeta, openBlockers, refState, startWorkerMessage, statusLabel,
} from "../issues";
import { ago, errText } from "../util";
import { Unavailable } from "../components/Unavailable";
import { CoordinatorChat } from "./ProjectBoard";
import { IssueDraftDrawer } from "./IssueDraftDrawer";
import "./issues.css";

export function Issues({ project: pid, id }: { project: string; id?: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);
  const beads = useStore((s) => s.beads[pid]);
  const seed = useStore((s) => (s.issueSeed?.project_id === pid ? s.issueSeed.text : null));
  const [beadsUnavailable, setBeadsUnavailable] = useState(false);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  useEffect(() => {
    loadBeads(pid).then((r) => setBeadsUnavailable(r === "unavailable")).catch((e) => setLoadErr(errText(e)));
  }, [pid, connected]);
  const ready = beads?.state === "ready";

  // `beads.changed` can come in bursts (a sync touches many issues), so refetch once it settles.
  const activity = useStore((s) => s.beadsActivity[pid] ?? 0);
  const [settled, setSettled] = useState(activity);
  useEffect(() => {
    const t = setTimeout(() => setSettled(activity), 250);
    return () => clearTimeout(t);
  }, [activity]);
  const [reloads, setReloads] = useState(0);

  const [issues, setIssues] = useState<Issue[] | null>(null);
  useEffect(() => {
    if (!ready) return;
    let live = true;
    api.issues(pid, "all").then((list) => { if (live) { setIssues(list); setLoadErr(null); } })
      .catch((e) => { if (live) setLoadErr(errText(e)); });
    return () => { live = false; };
  }, [pid, ready, settled, reloads]);

  const all = issues ?? [];
  const known = useMemo(() => Object.fromEntries(all.map((i) => [i.id, i])), [all]);
  const counts = useMemo(() => chipCounts(all), [all]);
  const [chip, setChip] = useState<IssueChip>("ready");
  const list = useMemo(() => chipList(all, chip), [all, chip]);

  // A selection in the URL wins, even outside the current chip (an open decision is under none);
  // otherwise the first row.
  const selected = id && known[id] ? id : list[0]?.id;
  const select = (next: string | undefined) => go({ name: "issues", project: pid, id: next });
  const show = (c: IssueChip) => { setChip(c); select(undefined); };
  // Arriving with an issue in the URL (or creating one) shows the chip it is under.
  const urlKnown = !!(id && known[id]);
  useEffect(() => {
    const i = id ? known[id] : undefined;
    if (!i || inChip(i, chip)) return;
    const c = CHIPS.find((x) => inChip(i, x.id));
    if (c) setChip(c.id);
    // Only when the URL changes or its issue arrives, so switching chips by hand is not undone.
  }, [id, urlKnown]);

  const [detail, setDetail] = useState<IssueDetail | null>(null);
  useEffect(() => {
    if (!selected || !ready) { setDetail(null); return; }
    let live = true;
    api.issue(pid, selected).then((d) => { if (live) setDetail(d); }).catch((e) => { if (live) setLoadErr(errText(e)); });
    return () => { live = false; };
  }, [pid, selected, ready, settled, reloads]);

  const [drawer, setDrawer] = useState(false);
  // "Turn into an issue" on the Memory screen opens the drawer started with the learning.
  const [drawerSeed, setDrawerSeed] = useState<string | null>(null);
  useEffect(() => {
    if (seed === null || !ready) return;
    setDrawerSeed(seed);
    setDrawer(true);
    setIssueSeed(null);
  }, [seed, ready]);
  const [chatOpen, setChatOpen] = useState(false);

  const rowRefs = useRef(new Map<string, HTMLElement>());
  useEffect(() => { if (selected) rowRefs.current.get(selected)?.scrollIntoView({ block: "nearest" }); }, [selected]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (drawer || e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.closest("input, textarea, select, [contenteditable]") || t.closest("[cmdk-root]"))) return;
      const move = (delta: number) => { e.preventDefault(); select(step(list, selected, delta)); };
      switch (e.key) {
        case "j": case "ArrowDown": return move(1);
        case "k": case "ArrowUp": return move(-1);
        case "n": if (ready) { e.preventDefault(); setDrawerSeed(null); setDrawer(true); } return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }

  const onCreated = (created: string | undefined) => {
    setDrawer(false);
    setReloads((n) => n + 1);
    if (created) select(created);
  };

  return (
    <>
      <div className="header">
        <h1>Issues</h1>
        <a className="crumb" href={href({ name: "project", id: pid })}>{project.name}</a>
        <span className="spacer" />
        <a className="btn" href={href({ name: "memory", project: pid })} data-testid="nav-memory">Memory</a>
      </div>
      {beadsUnavailable ? <Unavailable what="Beads" endpoint={`GET /v1/projects/${pid}/beads`} />
        : !beads ? <div className="empty">{loadErr ?? (connected ? "Loading…" : "Waiting for the daemon…")}</div>
        : !ready ? <BeadsSetup b={beads} />
        : (
          <>
            <BeadsStrip b={beads} onNew={() => { setDrawerSeed(null); setDrawer(true); }} />
            <div className={"screen issues-layout" + (chatOpen ? " with-chat" : "")}>
              <section className="issue-list" aria-label="Issues" data-testid="issue-list">
                <div className="chips" role="group" aria-label="Filter issues">
                  {CHIPS.map((c) => (
                    <button key={c.id} type="button" className="chip" aria-pressed={chip === c.id} onClick={() => show(c.id)}>
                      {c.label}{c.id !== "closed" && <> {counts[c.id]}</>}
                    </button>
                  ))}
                </div>
                {list.map((i) => (
                  <a key={i.id} ref={(el) => { if (el) rowRefs.current.set(i.id, el); else rowRefs.current.delete(i.id); }}
                    className={"issue-row" + (i.id === selected ? " on" : "")} href={href({ name: "issues", project: pid, id: i.id })}
                    data-testid="issue-row" aria-current={i.id === selected ? "true" : undefined}>
                    <span className="issue-id mono">{i.id}</span>
                    <span className="issue-main">
                      <span className="issue-title">{i.title}</span>
                      <span className="issue-meta">{issueMeta(i, known)}</span>
                    </span>
                    <span className="prio">P{i.priority}</span>
                  </a>
                ))}
                {!list.length && (
                  <div className="empty">
                    {issues === null ? (loadErr ?? "Loading…") : chip === "ready" ? "Nothing ready. Issues with nothing open to wait for show here."
                      : chip === "in_progress" ? "Nothing in progress." : chip === "blocked" ? "Nothing is blocked." : "Nothing closed yet."}
                  </div>
                )}
              </section>
              <article className="issue-detail" data-testid="issue-detail">
                {detail && detail.id === selected ? (
                  <IssueView key={detail.id} pid={pid} i={detail} asking={chatOpen} onAsk={() => setChatOpen((o) => !o)} />
                ) : <div className="empty">{selected ? "Loading…" : "Select an issue."}</div>}
                <div className="inbox-keys faint">
                  <span className="kbd">j</span><span className="kbd">k</span> move · <span className="kbd">n</span> new issue
                </div>
              </article>
              {chatOpen && <ChatPanel pid={pid} />}
            </div>
            {drawer && (
              <IssueDraftDrawer pid={pid} beads={beads} issues={all} seed={drawerSeed}
                onClose={() => setDrawer(false)} onCreated={onCreated} />
            )}
          </>
        )}
    </>
  );
}

/** Where the issues live and when they last synced with GitHub, with Sync now and New issue. */
function BeadsStrip({ b, onNew }: { b: BeadsStatus; onNew: () => void }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const sync = async () => {
    setBusy(true); setErr(null);
    try { setBeads(await api.syncBeads(b.project_id)); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  const failed = b.last_sync && !b.last_sync.ok;
  return (
    <div className="beads-strip" data-testid="beads-strip">
      <span className={"beads-dot" + (b.last_sync?.ok ? " ok" : "")} aria-hidden="true" />
      <span className="grow">
        {beadsWhere(b)}
        {b.last_sync && <> · last sync {ago(b.last_sync.at)}</>}
        {failed && <span className="bad"> · {b.last_sync!.message}</span>}
        {err && <span className="bad"> · {err}</span>}
      </span>
      {b.github_repo && <button type="button" className="btn" onClick={() => void sync()} disabled={busy}>{busy ? "Syncing…" : "Sync now"}</button>}
      <button type="button" className="btn primary" onClick={onNew} title="New issue (n)">
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
        New issue
      </button>
    </div>
  );
}

/** No database yet, being set up, or unusable: what to do about it. */
function BeadsSetup({ b }: { b: BeadsStatus }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const setup = async () => {
    setBusy(true); setErr(null);
    try { setBeads(await api.setupBeads(b.project_id)); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  if (b.state === "unavailable" || b.state === "failed") {
    return (
      <div className="unavailable" data-testid="beads-setup">
        <div className="u-title">{b.state === "failed" ? "Setting up Beads failed" : "Beads is not available on this machine"}</div>
        {b.detail && <div className="faint">{b.detail}</div>}
        {b.state === "failed" && <div><button type="button" className="btn" onClick={() => void setup()} disabled={busy}>Try again</button></div>}
        {err && <div className="bad">{err}</div>}
      </div>
    );
  }
  return (
    <div className="beads-empty" data-testid="beads-setup">
      <div className="u-title">{b.state === "setting_up" ? "Setting up Beads…" : "This project has no issues yet"}</div>
      {b.state === "setting_up" ? <div className="faint">{b.detail ?? "Starting"}</div> : (
        <>
          <div className="faint">Quark keeps this project's issues and memory in Beads, in the Project repo, mirrored both ways with GitHub Issues.</div>
          <div><button type="button" className="btn big primary" onClick={() => void setup()} disabled={busy}>{busy ? "Setting up…" : "Set up Beads"}</button></div>
        </>
      )}
      {err && <div className="bad">{err}</div>}
    </div>
  );
}

/** A linked issue as a row: its status dot, id and title, and where it stands. */
function LinkRow({ pid, r, note }: { pid: string; r: IssueRef; note?: string }) {
  const st = refState(r);
  return (
    <a className={"link-row" + (st.decision ? " on-you" : "")} href={href({ name: "issues", project: pid, id: r.id })} data-testid="issue-link">
      <span className="dot" style={{ background: st.color }} />
      <span className="grow ellipsis">{r.id} · {r.title}</span>
      <span className="small-text" style={{ color: st.decision ? st.color : undefined }}>{note ?? st.label}</span>
    </a>
  );
}

function IssueView({ pid, i, asking, onAsk }: { pid: string; i: IssueDetail; asking: boolean; onAsk: () => void }) {
  const waits = openBlockers(i.blocked_by_issues);
  const gh = githubNumber(i.external_ref);
  const [busy, setBusy] = useState(false);
  const [sent, setSent] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const start = async () => {
    setBusy(true); setErr(null);
    try { await api.sendChat(pid, startWorkerMessage(i)); setSent(true); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  const closed = i.status === "closed";
  return (
    <div className="issue">
      <div className="issue-head">
        <div className="issue-facts">
          <span className="mono">{i.id}</span><span>{i.issue_type} · P{i.priority}</span>
          {i.status !== "open" && <span>{statusLabel(i.status)}</span>}
          {gh && <span>GitHub <a href={i.external_ref!} target="_blank" rel="noreferrer">{gh}</a></span>}
          {i.labels.map((l) => <span key={l} className="pill">{l}</span>)}
        </div>
        <h2>{i.title}</h2>
        {i.description && <div className="issue-desc">{i.description}</div>}
      </div>
      {i.blocked_by_issues.length > 0 && (
        <section className="issue-section" aria-label="Blocked by">
          <div className="issue-label">Blocked by</div>
          {i.blocked_by_issues.map((r) => <LinkRow key={r.id} pid={pid} r={r} />)}
          {i.blocks_issues.length > 0 && <Unblocks refs={i.blocks_issues} />}
        </section>
      )}
      {!i.blocked_by_issues.length && i.blocks_issues.length > 0 && <Unblocks refs={i.blocks_issues} />}
      {i.related_issues.length > 0 && (
        <section className="issue-section" aria-label="Related">
          <div className="issue-label">Related</div>
          {i.related_issues.map((r) => <LinkRow key={r.kind + r.id} pid={pid} r={r} note={r.kind.replace(/[-_]/g, " ")} />)}
        </section>
      )}
      {i.acceptance_criteria && (
        <section className="issue-section">
          <div className="issue-label">Done when</div>
          <div className="issue-desc">{i.acceptance_criteria}</div>
        </section>
      )}
      <section className="issue-section" aria-label="History">
        <div className="issue-label">History</div>
        <div>Filed{i.created_by ? ` by ${i.created_by}` : ""} · {ago(i.created_at)}</div>
        {i.comments.map((c, n) => <div key={n}><span className="faint">{c.author}:</span> {c.text} <span className="faint">· {ago(c.created_at)}</span></div>)}
        {i.closed_at && <div>Closed · {ago(i.closed_at)}</div>}
      </section>
      {!closed && (
        <div className="issue-actions">
          <button type="button" className="btn big" disabled={busy || sent || waits.length > 0} onClick={() => void start()} data-testid="start-worker">
            {waits.length ? `Start a worker · waits for ${waits.map((r) => r.id).join(", ")}` : sent ? "Sent to the coordinator" : "Start a worker"}
          </button>
          <button type="button" className={"btn big" + (asking ? " on" : "")} aria-pressed={asking} onClick={onAsk}>Ask the coordinator</button>
          {err && <span className="bad small-text">{err}</span>}
        </div>
      )}
    </div>
  );
}

function Unblocks({ refs }: { refs: IssueRef[] }) {
  return <div className="faint small-text">Unblocks {refs.map((r) => `${r.id} (${r.title})`).join(", ")}.</div>;
}

/** The project's coordinator chat beside the issue, as the board shows it, with the message box focused. */
function ChatPanel({ pid }: { pid: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { ref.current?.querySelector("textarea")?.focus(); }, []);
  return (
    <div className="issue-chat" ref={ref}>
      <CoordinatorChat cid={pid} />
    </div>
  );
}

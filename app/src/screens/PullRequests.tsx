// J6 PR center: every Project's pull requests by state, with checks and reviews at a glance.
// j/k (or arrows) move the selection, Enter opens it; standing approval is set per Project.
import React, { useEffect, useMemo, useRef, useState } from "react";
import type { PullRequest, PullRequestState } from "../api";
import { go, href } from "../nav";
import { loadPullRequests, useStore } from "../store";
import { ago, errText } from "../util";
import { checksMeta, countByState, filterPrs, PR_STATES, prStateMeta, prTitle, reviewMeta } from "../prs";
import { Unavailable } from "../components/Unavailable";
import { StandingApproval } from "../components/StandingApproval";

export function PullRequests() {
  const prsMap = useStore((s) => s.pullRequests);
  const available = useStore((s) => s.prsAvailable);
  const projects = useStore((s) => s.projects);
  const connected = useStore((s) => s.connected);
  const [state, setState] = useState<PullRequestState>("open");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [sel, setSel] = useState(0);
  const [err, setErr] = useState<string | null>(null);
  const list = useRef<HTMLDivElement>(null);

  useEffect(() => { loadPullRequests().catch((e) => setErr(errText(e))); }, [connected]);

  const prs = useMemo(() => Object.values(prsMap), [prsMap]);
  const counts = useMemo(() => countByState(prs, projectId), [prs, projectId]);
  const rows = useMemo(() => filterPrs(prs, state, projectId), [prs, state, projectId]);
  const sortedProjects = useMemo(() => Object.values(projects).sort((a, b) => a.name.localeCompare(b.name)), [projects]);
  const idx = Math.min(sel, Math.max(0, rows.length - 1));

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (e.metaKey || e.ctrlKey || e.altKey || t?.closest("input, textarea, select, [cmdk-root]")) return;
      if (e.key === "j" || e.key === "ArrowDown") { e.preventDefault(); setSel(Math.min(idx + 1, rows.length - 1)); }
      else if (e.key === "k" || e.key === "ArrowUp") { e.preventDefault(); setSel(Math.max(idx - 1, 0)); }
      else if (e.key === "Enter" && rows[idx]) { e.preventDefault(); go({ name: "pr", id: rows[idx].id }); }
      else {
        const n = Number(e.key);
        if (n >= 1 && n <= PR_STATES.length) { setState(PR_STATES[n - 1].id); setSel(0); }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [idx, rows]);

  useEffect(() => {
    list.current?.querySelector(".pr-row.sel")?.scrollIntoView({ block: "nearest" });
  }, [idx]);

  return (
    <>
      <div className="header">
        <h1>Pull requests</h1>
        <div className="seg" role="tablist">
          {PR_STATES.map((s, i) => (
            <button key={s.id} role="tab" aria-selected={state === s.id} className={state === s.id ? "on" : ""}
              onClick={() => { setState(s.id); setSel(0); }} title={`${s.label} (${i + 1})`} data-testid={`prs-tab-${s.id}`}>
              {s.label} <span className="n">{counts[s.id]}</span>
            </button>
          ))}
        </div>
        <span className="spacer" />
        <select className="select" value={projectId ?? ""} aria-label="Project"
          onChange={(e) => { setProjectId(e.target.value || null); setSel(0); }}>
          <option value="">All projects</option>
          {sortedProjects.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        </select>
      </div>
      {available === false ? (
        <div className="screen scroll"><Unavailable what="The PR center" endpoint="GET /v1/pull-requests" /></div>
      ) : (
        <div className="screen prs-layout">
          <div className="pr-list" ref={list} data-testid="pr-list">
            {err && <div className="form-error">{err}</div>}
            {rows.map((pr, i) => (
              <PrRow key={pr.id} pr={pr} selected={i === idx} projectName={projects[pr.project_id]?.name ?? pr.project_id}
                onHover={() => setSel(i)} />
            ))}
            {!rows.length && (
              <div className="empty">
                {available === null ? (connected ? "Loading…" : "Waiting for the daemon…") : `No ${state} pull requests.`}
              </div>
            )}
            <div className="pr-keys faint">
              <span className="kbd">j</span>/<span className="kbd">k</span> move · <span className="kbd">Enter</span> open · <span className="kbd">1</span>–<span className="kbd">4</span> state
            </div>
          </div>
          <aside className="pr-side">
            <div className="panel-head">Standing approval</div>
            <div className="side-note faint">Merges a Project's pull requests as soon as their checks pass.</div>
            {sortedProjects.map((p) => <StandingApproval key={p.id} project={p} compact />)}
            {!sortedProjects.length && <div className="empty">No projects yet.</div>}
          </aside>
        </div>
      )}
    </>
  );
}

function PrRow({ pr, selected, projectName, onHover }: {
  pr: PullRequest; selected: boolean; projectName: string; onHover: () => void;
}) {
  const ch = checksMeta[pr.checks_state];
  const rv = reviewMeta[pr.review_decision];
  const st = prStateMeta[pr.state];
  return (
    <a className={"pr-row" + (selected ? " sel" : "")} href={href({ name: "pr", id: pr.id })} onMouseMove={selected ? undefined : onHover}
      data-testid="pr-row">
      <span className={"pr-glyph " + ch.cls} title={ch.label}>{ch.glyph}</span>
      <div className="pr-main">
        <div className="pr-title ellipsis">{prTitle(pr)}</div>
        <div className="meta">
          <span className="mono">{pr.repo}#{pr.number}</span>
          <span>·</span>
          <span>{projectName}</span>
          {pr.head_ref && <><span>·</span><span className="mono ellipsis">{pr.head_ref} → {pr.base_ref ?? "?"}</span></>}
        </div>
      </div>
      <div className="pr-pills">
        {pr.state !== "open" && <span className={"pill " + st.cls}>{st.label}</span>}
        {pr.mergeable === "conflicting" && pr.state === "open" && <span className="pill red">conflicts</span>}
        {pr.sync_error && <span className="pill yellow" title={pr.sync_error}>out of date</span>}
        <span className={"pill " + ch.cls}>{ch.label}</span>
        <span className={"pill " + rv.cls}>{rv.label}</span>
        {pr.additions != null && <span className="adds">+{pr.additions}</span>}
        {pr.deletions != null && <span className="dels">−{pr.deletions}</span>}
        <span className="faint small-text when">{ago(pr.updated_at)}</span>
      </div>
    </a>
  );
}

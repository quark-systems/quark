// All projects, the app's home: what needs you across every project (oldest first), open PRs,
// and one card per project. The decisions inbox and PR center open from here.
import React, { useMemo, useState } from "react";
import { href } from "../nav";
import { useStore } from "../store";
import { ago, isActive } from "../util";
import { CountBadge, SectionLabel, StatusDot } from "../ui";
import { useAttention } from "../shell/NextAttention";
import { AttentionKind } from "../shell/attention";
import { swatchFor } from "../shell/groups";

const KIND: Record<AttentionKind, { label: string; tone: "needs-you" | "failed" }> = {
  decision: { label: "Decision", tone: "needs-you" },
  pr: { label: "Check failed", tone: "failed" },
  worker: { label: "Worker stuck", tone: "failed" },
};
type Filter = "all" | AttentionKind;

export function Projects() {
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const prs = useStore((s) => s.pullRequests);
  const connected = useStore((s) => s.connected);
  const queue = useAttention();
  const [filter, setFilter] = useState<Filter>("all");
  const pname = (id: string) => projects[id]?.name ?? id;

  const counts = useMemo(() => {
    const c: Record<Filter, number> = { all: queue.length, decision: 0, pr: 0, worker: 0 };
    for (const i of queue) c[i.kind]++;
    return c;
  }, [queue]);
  const shown = filter === "all" ? queue : queue.filter((i) => i.kind === filter);
  const openPrs = useMemo(() => Object.values(prs).filter((p) => p.state === "open" || p.state === "draft")
    .sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? "")), [prs]);
  const rows = useMemo(() => Object.values(projects).map((p) => {
    const ts = Object.values(tasks).filter((t) => t.project_id === p.id);
    return { p, active: ts.filter((t) => isActive(t.state)).length, waiting: queue.filter((i) => i.projectId === p.id).length };
  }).sort((a, b) => b.p.updated_at.localeCompare(a.p.updated_at)), [projects, tasks, queue]);

  return (
    <>
      <div className="header">
        <h1>All projects</h1>
        <span className="spacer" />
        <a className="btn" href={href({ name: "new" })}>New project</a>
      </div>
      <div className="screen scroll">
        <div className="home">
          {rows.length === 0 ? (
            <div className="empty">
              {connected ? <>No projects yet. <a href={href({ name: "new" })}>Create one</a> to get started.</> : "Waiting for the daemon…"}
            </div>
          ) : (
            <>
              <section className="home-section" aria-label="Needs you">
                <SectionLabel right={
                  <div className="home-filters" role="group" aria-label="Show">
                    {(["all", "decision", "pr", "worker"] as Filter[]).map((f) => (
                      <button key={f} type="button" className={"home-chip" + (filter === f ? " on" : "")} aria-pressed={filter === f} onClick={() => setFilter(f)}
                        data-testid={`home-filter-${f}`}>
                        {f === "all" ? "Everything" : f === "decision" ? "Decisions" : f === "pr" ? "PRs" : "Workers"} {counts[f]}
                      </button>
                    ))}
                  </div>
                }>Needs you · oldest first</SectionLabel>
                {shown.length ? (
                  <div className="home-list">
                    {shown.map((i) => (
                      <a key={i.key} className="home-row" href={href(i.route)} data-testid="home-needs">
                        <StatusDot tone={KIND[i.kind].tone} />
                        <span className={"home-kind " + KIND[i.kind].tone}>{KIND[i.kind].label}</span>
                        <span className="home-title">{i.title}</span>
                        <span className="home-meta">{pname(i.projectId)} · {ago(i.since)}</span>
                      </a>
                    ))}
                  </div>
                ) : <p className="faint">Nothing needs you.</p>}
                <a className="home-more" href={href({ name: "inbox" })} data-testid="nav-inbox">All decisions{counts.decision > 0 && <span className="ui-count needs-you" data-testid="inbox-count">{counts.decision}</span>}</a>
              </section>

              <section className="home-section" aria-label="Open PRs">
                <SectionLabel>Open PRs</SectionLabel>
                {openPrs.length ? (
                  <div className="home-list">
                    {openPrs.map((p) => (
                      <a key={p.id} className="home-row" href={href({ name: "pr", id: p.id })} data-testid="home-pr">
                        <span className="home-num mono">#{p.number}</span>
                        <span className="home-title">{p.title ?? `${p.repo}#${p.number}`}</span>
                        <span className={"home-checks " + p.checks_state}>{p.state === "draft" ? "draft" : p.checks_state === "failing" ? "checks failing" : p.checks_state === "passing" ? "green" : p.checks_state === "pending" ? "checks running" : "no checks"}</span>
                        <span className="home-meta">{pname(p.project_id)}</span>
                      </a>
                    ))}
                  </div>
                ) : <p className="faint">No open PRs.</p>}
                <a className="home-more" href={href({ name: "prs" })} data-testid="nav-prs">All pull requests</a>
              </section>

              <section className="home-cards" aria-label="Projects">
                {rows.map(({ p, active, waiting }) => (
                  <a className="project-card" key={p.id} href={href({ name: "project", id: p.id })} data-testid="project-card">
                    <div className="pc-name"><span className="ll-swatch" style={{ background: swatchFor(p.id) }} />{p.name}<span className="spacer" /><CountBadge n={waiting} tone="needs-you" /></div>
                    {p.goal && <div className="pc-goal">{p.goal}</div>}
                    <div className="meta">
                      <span>{active} {active === 1 ? "worker" : "workers"}</span>
                      {p.status && p.status !== "ready" && <span className={"pill " + (p.status === "failed" ? "red" : "yellow")}>{p.status}</span>}
                      <span className="spacer" />
                      <span>{ago(p.updated_at)}</span>
                    </div>
                  </a>
                ))}
              </section>
            </>
          )}
        </div>
      </div>
    </>
  );
}

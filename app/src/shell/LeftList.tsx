// The left list: All projects, then each project with its coordinator pinned on top and its
// workers under it, then Accounts and Hosts in the footer (plans/ui-shell-design.md).
import React, { useMemo } from "react";
import { href, Route, useRoute } from "../nav";
import { useStore } from "../store";
import { CoordinatorMark, CountBadge, Row, StatusDot } from "../ui";
import { useLabels } from "../persona";
import { leftListGroups, ProjectGroup } from "./leftList";
import "./shell.css";

export function LeftList({ top }: { top?: React.ReactNode }) {
  const route = useRoute();
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const decisions = useStore((s) => s.decisions);
  const prs = useStore((s) => s.pullRequests);
  const groups = useMemo(() => leftListGroups(projects, tasks, decisions), [projects, tasks, decisions]);
  const openPrs = useMemo(() => Object.values(prs).filter((p) => p.state === "open").length, [prs]);
  const openDecisions = useMemo(() => Object.values(decisions).filter((d) => d.state === "open").length, [decisions]);

  return (
    <nav className="left-list sidebar" aria-label="Projects">
      <div className="drag-strip" data-tauri-drag-region />
      <div className="ll-head">
        <span className="ll-logo" aria-hidden="true">Q</span>
        <a className="ll-brand" href={href({ name: "projects" })}>Quark</a>
        <a className="ll-new" href={href({ name: "new" })} aria-label="New project" title="New project" data-testid="nav-new-project">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M12 5v14M5 12h14" /></svg>
        </a>
      </div>
      {top}
      <div className="ll-top">
        <Row href={href({ name: "projects" })} current={route.name === "projects"} title="All projects"
          lead={<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><rect x="3" y="3" width="7" height="7" rx="1.5" /><rect x="14" y="3" width="7" height="7" rx="1.5" /><rect x="3" y="14" width="7" height="7" rx="1.5" /><rect x="14" y="14" width="7" height="7" rx="1.5" /></svg>} />
        <Row href={href({ name: "inbox" })} current={route.name === "inbox"} testid="nav-inbox" title="Decisions"
          lead={<StatusDot tone="needs-you" label="" />}
          trail={openDecisions > 0 ? <span className="ui-count needs-you" data-testid="inbox-count">{openDecisions}</span> : undefined} />
        <Row href={href({ name: "prs" })} current={route.name === "prs" || route.name === "pr"} testid="nav-prs" title="Pull requests"
          lead={<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="18" r="2.5" /><path d="M6 8.5v7a3 3 0 0 0 3 3h6.5" /></svg>}
          trail={<span className="ll-meta">{openPrs}</span>} />
      </div>
      <div className="ll-projects">
        {groups.map((g) => <ProjectSection key={g.project.id} g={g} route={route} />)}
        {!groups.length && <div className="ll-empty">No projects yet. <a href={href({ name: "new" })}>Create one</a>.</div>}
      </div>
      <div className="ll-foot">
        <Row href={href({ name: "accounts" })} current={route.name === "accounts"} testid="nav-accounts" title="Accounts"
          lead={<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><circle cx="12" cy="8" r="4" /><path d="M4 21c1.5-4 4.5-6 8-6s6.5 2 8 6" /></svg>} />
        <Row href={href({ name: "hosts" })} current={route.name === "hosts"} testid="nav-hosts" title="Hosts"
          lead={<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><rect x="3" y="4" width="18" height="7" rx="2" /><rect x="3" y="13" width="18" height="7" rx="2" /></svg>} />
      </div>
    </nav>
  );
}

function ProjectSection({ g, route }: { g: ProjectGroup; route: Route }) {
  const l = useLabels(g.project.id);
  const coordCurrent = route.name === "project" && route.id === g.project.id;
  return (
    <section className="ll-project" aria-label={g.project.name} data-testid="ll-project">
      <div className="ll-project-name"><span className="ll-swatch" style={{ background: g.swatch }} />{g.project.name}</div>
      <Row href={href({ name: "project", id: g.project.id })} current={coordCurrent} testid="ll-coordinator"
        lead={<CoordinatorMark />} title={l.Role("coordinator")} sub={g.coordSub}
        trail={<CountBadge n={g.waiting} tone="needs-you" label={`${g.waiting} waiting on you`} />} />
      {g.workers.map((w) => (
        <Row key={w.id} indent dim={w.tone === "parked"} href={href({ name: "task", id: w.id })} testid="ll-worker"
          current={route.name === "task" && route.id === w.id}
          lead={<StatusDot tone={w.tone} />} title={w.title} sub={w.sub}
          trail={w.needsYou ? <span className="ll-needs-you" role="img" aria-label="Needs you" /> : undefined} />
      ))}
    </section>
  );
}

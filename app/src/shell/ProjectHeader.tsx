// The project's header: its name, the tabs (Conversation, Overview, Work, Issues, Decisions,
// Memory, Metrics) and Settings. Every project screen sits under it.
import React, { useMemo } from "react";
import { href, Route } from "../nav";
import { useStore } from "../store";
import { useLabels } from "../persona";
import { CountBadge } from "../ui";
import { swatchFor } from "./groups";

export type ProjectTab = "conversation" | "overview" | "work" | "issues" | "decisions" | "memory" | "metrics" | "settings";

/** The tab a project route belongs to; Dispatch and Automation are sections of Settings. */
export function tabOf(route: Route): ProjectTab | null {
  switch (route.name) {
    case "project": return "conversation";
    case "overview": case "work": case "issues": case "decisions": case "memory": case "metrics": return route.name;
    case "settings": case "dispatch": case "automation": return "settings";
    default: return null;
  }
}

const tabRoute = (tab: Exclude<ProjectTab, "settings">, project: string): Route =>
  tab === "conversation" ? { name: "project", id: project } : { name: tab, project };

export function ProjectHeader({ project: pid, current }: { project: string; current: ProjectTab }) {
  const project = useStore((s) => s.projects[pid]);
  const decisions = useStore((s) => s.decisions);
  const proposals = useStore((s) => s.memoryProposals);
  const l = useLabels(pid);
  const open = useMemo(() => Object.values(decisions).filter((d) => d.project_id === pid && d.state === "open").length, [decisions, pid]);
  const toReview = useMemo(() => Object.values(proposals).filter((m) => m.project_id === pid && m.state === "proposed").length, [proposals, pid]);
  const tabs: { tab: Exclude<ProjectTab, "settings">; label: string; count?: React.ReactNode; testid?: string }[] = [
    { tab: "conversation", label: "Conversation" },
    { tab: "overview", label: "Overview" },
    { tab: "work", label: "Work" },
    { tab: "issues", label: "Issues" },
    { tab: "decisions", label: "Decisions", count: <span data-testid="decisions-count"><CountBadge n={open} tone="needs-you" label={`${open} open`} /></span> },
    { tab: "memory", label: l.ui("memory", "Memory"), count: toReview > 0 ? <span className="ui-count accent" data-testid="memory-count">{toReview}</span> : null },
    { tab: "metrics", label: "Metrics" },
  ];
  if (!project) return null;
  return (
    <header className="project-header" data-testid="project-header">
      <div className="ph-name">
        <span className="ll-swatch" style={{ background: swatchFor(pid) }} />
        <span className="ph-title">{project.name}</span>
        {project.status && project.status !== "ready" && (
          <span className={"pill " + (project.status === "failed" ? "red" : "yellow")} data-testid="project-status">{project.status}</span>
        )}
      </div>
      <nav className="ph-tabs" aria-label="Project">
        {tabs.map((t) => (
          <a key={t.tab} href={href(tabRoute(t.tab, pid))} className={"ph-tab" + (t.tab === current ? " on" : "")}
            aria-current={t.tab === current ? "page" : undefined} data-testid={`nav-${t.tab}`}>{t.label}{t.count}</a>
        ))}
      </nav>
      <a href={href({ name: "settings", project: pid })} className={"ph-settings" + (current === "settings" ? " on" : "")}
        aria-current={current === "settings" ? "page" : undefined} data-testid="nav-settings">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12" /><circle cx="16" cy="6" r="2" /><circle cx="10" cy="12" r="2" /><circle cx="18" cy="18" r="2" /></svg>
        Settings
      </a>
    </header>
  );
}

const SECTIONS = [
  { name: "settings", label: "General" },
  { name: "dispatch", label: "Dispatch" },
  { name: "automation", label: "Automation" },
] as const;

/** Settings is one page; General, Dispatch and Automation are its sections. */
export function SettingsSections({ project, current }: { project: string; current: (typeof SECTIONS)[number]["name"] }) {
  return (
    <nav className="settings-sections" aria-label="Settings">
      <a className="settings-back" href={href({ name: "project", id: project })}><span aria-hidden="true">‹ </span>Back to Conversation</a>
      <span className="settings-title">Settings</span>
      {SECTIONS.map((s) => (
        <a key={s.name} href={href({ name: s.name, project })} className={"settings-section" + (s.name === current ? " on" : "")}
          aria-current={s.name === current ? "page" : undefined} data-testid={`nav-${s.name === "settings" ? "general" : s.name}`}>{s.label}</a>
      ))}
    </nav>
  );
}

/** A project screen under its header; Settings is a page with its sections listed on the left. */
export function ProjectFrame({ route, project, children }: { route: Route; project: string; children: React.ReactNode }) {
  const tab = tabOf(route)!;
  return (
    <>
      <ProjectHeader project={project} current={tab} />
      {tab === "settings" ? (
        <div className="settings-page">
          <SettingsSections project={project} current={route.name as "settings" | "dispatch" | "automation"} />
          <div className="settings-body">{children}</div>
        </div>
      ) : children}
    </>
  );
}

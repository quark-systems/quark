import React, { useEffect } from "react";
import { Command } from "cmdk";
import { go } from "./nav";
import { useStore } from "./store";
import { stateMeta } from "./util";

export function Palette({ onClose }: { onClose: () => void }) {
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const decisions = useStore((s) => s.decisions);
  const prs = useStore((s) => s.pullRequests);

  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    return () => { prev?.focus?.(); };
  }, []);

  const pick = (fn: () => void) => { fn(); onClose(); };
  const pname = (id: string) => projects[id]?.name ?? id;
  const open = Object.values(decisions).filter((d) => d.state === "open").sort((a, b) => a.opened_at.localeCompare(b.opened_at));
  const openPrs = Object.values(prs).filter((p) => p.state === "open" || p.state === "draft")
    .sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? ""));
  const sortedTasks = Object.values(tasks).sort((a, b) => b.updated_at.localeCompare(a.updated_at));

  return (
    <div className="palette-overlay" onMouseDown={onClose}>
      <div onMouseDown={(e) => e.stopPropagation()}>
        <Command label="Command palette" loop onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); onClose(); } }}>
          <Command.Input autoFocus placeholder="Jump to a project, task, decision or pull request…" />
          <Command.List>
            <Command.Empty>No results.</Command.Empty>
            <Command.Group heading="Go to">
              <Command.Item value="all projects" onSelect={() => pick(() => go({ name: "projects" }))}>All projects</Command.Item>
              <Command.Item value="new project create" onSelect={() => pick(() => go({ name: "new" }))}>New project</Command.Item>
              <Command.Item value="decisions inbox answer" onSelect={() => pick(() => go({ name: "inbox" }))}>
                Decisions inbox<span className="sub">{open.length} open</span>
              </Command.Item>
              <Command.Item value="pull requests pr center" onSelect={() => pick(() => go({ name: "prs" }))}>Pull requests</Command.Item>
            </Command.Group>
            {open.length > 0 && (
              <Command.Group heading="Open decisions">
                {open.map((d) => (
                  <Command.Item key={d.id} value={`decision ${d.question} ${pname(d.project_id)} ${d.id}`} onSelect={() => pick(() => go({ name: "inbox", id: d.id }))}>
                    <span className="ellipsis">{d.question}</span>
                    <span className="sub">{pname(d.project_id)}</span>
                  </Command.Item>
                ))}
              </Command.Group>
            )}
            <Command.Group heading="Projects">
              {Object.values(projects).map((p) => (
                <Command.Item key={p.id} value={`project ${p.name} ${p.id}`} onSelect={() => pick(() => go({ name: "project", id: p.id }))}>
                  <span className="mono faint">#</span>{p.name}<span className="sub">{p.goal ?? ""}</span>
                </Command.Item>
              ))}
            </Command.Group>
            {openPrs.length > 0 && (
              <Command.Group heading="Pull requests">
                {openPrs.map((p) => (
                  <Command.Item key={p.id} value={`pr ${p.title ?? ""} ${p.repo}#${p.number} ${pname(p.project_id)}`} onSelect={() => pick(() => go({ name: "pr", id: p.id }))}>
                    <span className="mono faint">⇄</span><span className="ellipsis">{p.title ?? `${p.repo}#${p.number}`}</span>
                    <span className="sub">{p.repo}#{p.number}</span>
                  </Command.Item>
                ))}
              </Command.Group>
            )}
            <Command.Group heading="Tasks">
              {sortedTasks.map((t) => (
                <Command.Item key={t.id} value={`task ${t.title} ${pname(t.project_id)} ${t.id}`} onSelect={() => pick(() => go({ name: "task", id: t.id }))}>
                  <span className="ellipsis">{t.title}</span>
                  <span className="sub">{pname(t.project_id)} · {stateMeta(t.state).label}</span>
                </Command.Item>
              ))}
            </Command.Group>
          </Command.List>
        </Command>
      </div>
    </div>
  );
}

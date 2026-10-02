import React, { useEffect } from "react";
import { Command } from "cmdk";
import { go } from "./nav";
import { useStore } from "./store";
import { stateMeta } from "./util";

export function Palette({ onClose }: { onClose: () => void }) {
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);

  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    return () => { prev?.focus?.(); };
  }, []);

  const pick = (fn: () => void) => { fn(); onClose(); };
  const pname = (id: string) => projects[id]?.name ?? id;
  const sortedTasks = Object.values(tasks).sort((a, b) => b.updated_at.localeCompare(a.updated_at));

  return (
    <div className="palette-overlay" onMouseDown={onClose}>
      <div onMouseDown={(e) => e.stopPropagation()}>
        <Command label="Command palette" loop onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); onClose(); } }}>
          <Command.Input autoFocus placeholder="Jump to a project or task…" />
          <Command.List>
            <Command.Empty>No results.</Command.Empty>
            <Command.Group heading="Go to">
              <Command.Item value="all projects" onSelect={() => pick(() => go({ name: "projects" }))}>All projects</Command.Item>
              <Command.Item value="new project create" onSelect={() => pick(() => go({ name: "new" }))}>New project</Command.Item>
            </Command.Group>
            <Command.Group heading="Projects">
              {Object.values(projects).map((p) => (
                <Command.Item key={p.id} value={`project ${p.name} ${p.id}`} onSelect={() => pick(() => go({ name: "project", id: p.id }))}>
                  <span className="mono faint">#</span>{p.name}<span className="sub">{p.goal ?? ""}</span>
                </Command.Item>
              ))}
            </Command.Group>
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

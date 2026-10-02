import React, { useMemo } from "react";
import { href } from "../nav";
import { useStore } from "../store";
import { ago, isActive } from "../util";

export function Projects() {
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const connected = useStore((s) => s.connected);

  const rows = useMemo(() => {
    return Object.values(projects)
      .map((p) => {
        const ts = Object.values(tasks).filter((t) => t.project_id === p.id);
        return {
          p,
          active: ts.filter((t) => isActive(t.state)).length,
          decide: ts.filter((t) => t.state === "needs_decision").length,
          done: ts.filter((t) => t.state === "done").length,
        };
      })
      .sort((a, b) => b.p.updated_at.localeCompare(a.p.updated_at));
  }, [projects, tasks]);

  return (
    <>
      <div className="header">
        <h1>Projects</h1>
        <span className="spacer" />
        <a className="btn on" href={href({ name: "new" })}>New project</a>
      </div>
      <div className="screen scroll">
        {rows.length === 0 ? (
          <div className="empty">
            {connected ? <>No projects yet. <a href={href({ name: "new" })}>Create one</a> to get started.</> : "Waiting for the daemon…"}
          </div>
        ) : (
          <div className="project-grid">
            {rows.map(({ p, active, decide, done }) => (
              <a className="project-card" key={p.id} href={href({ name: "project", id: p.id })} data-testid="project-card">
                <div className="pc-name">{p.name}</div>
                {p.goal && <div className="pc-goal">{p.goal}</div>}
                <div className="meta">
                  <span className="pill blue">{active} active</span>
                  {decide > 0 && <span className="pill accent">{decide} need a decision</span>}
                  <span className="pill">{done} done</span>
                  {p.status && p.status !== "ready" && <span className={"pill " + (p.status === "failed" ? "red" : "yellow")}>{p.status}</span>}
                  <span className="spacer" />
                  <span>{ago(p.updated_at)}</span>
                </div>
                {!!p.repos?.length && <div className="pc-repos mono faint">{p.repos.map((r) => r.name ?? r.url).join(" · ")}</div>}
              </a>
            ))}
          </div>
        )}
      </div>
    </>
  );
}

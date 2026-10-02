import React, { useMemo, useRef } from "react";
import { useStore } from "../store";
import { useNav } from "../nav";
import { Task, TaskState } from "../api";

const COLS: { id: TaskState; label: string; color: string }[] = [
  { id: "queued", label: "Queued", color: "var(--fg-faint)" },
  { id: "running", label: "Running", color: "var(--blue)" },
  { id: "needs_decision", label: "Needs decision", color: "var(--accent)" },
  { id: "review", label: "Review", color: "var(--yellow)" },
  { id: "done", label: "Done", color: "var(--green)" },
  { id: "failed", label: "Failed", color: "var(--red)" },
];

function ago(ts: string | number): string {
  const t = typeof ts === "number" ? (ts < 1e12 ? ts * 1000 : ts) : Date.parse(ts);
  if (!isFinite(t)) return "";
  const s = Math.max(0, Math.round((Date.now() - t) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  return `${Math.round(s / 3600)}h`;
}

export function Board() {
  const nav = useNav();
  const tasks = useStore((s) => s.tasks);
  const projects = useStore((s) => s.projects);
  const prev = useRef<Record<string, string>>({});
  const byCol = useMemo(() => {
    const m: Record<string, Task[]> = {};
    for (const c of COLS) m[c.id] = [];
    for (const t of Object.values(tasks)) {
      if (nav.project && t.project_id !== nav.project) continue;
      (m[t.state] ??= []).push(t);
    }
    for (const k in m) m[k].sort((a, b) => String(b.updated_at).localeCompare(String(a.updated_at)));
    return m;
  }, [tasks, nav.project]);
  const pname = (id: string) => projects.find((p) => p.id === id)?.name ?? id;

  // flash cards whose state changed since last render
  const changed = new Set<string>();
  for (const t of Object.values(tasks)) {
    if (prev.current[t.id] !== undefined && prev.current[t.id] !== t.state) changed.add(t.id);
  }
  prev.current = Object.fromEntries(Object.values(tasks).map((t) => [t.id, t.state]));

  return (
    <div className="board">
      {COLS.map((c) => (
        <div className="col" key={c.id}>
          <div className="col-head"><span className="swatch" style={{ background: c.color }} />{c.label}<span className="n">{byCol[c.id].length}</span></div>
          <div className="col-body">
            {byCol[c.id].map((t) => (
              <div className={"card" + (changed.has(t.id) ? " flash" : "")} key={t.id + (changed.has(t.id) ? ":" + t.state : "")}>
                <div className="title">{t.title}</div>
                <div className="meta">
                  {!nav.project && <span className="pill">{pname(t.project_id)}</span>}
                  <span className="pill accent">{t.harness}</span>
                  <span>{ago(t.updated_at)}</span>
                </div>
                <div className="meta" style={{ marginTop: 4 }}><span className="branch">⎇ {t.branch}</span></div>
              </div>
            ))}
            {byCol[c.id].length === 0 && <div className="faint" style={{ padding: 8, fontSize: 12 }}>—</div>}
          </div>
        </div>
      ))}
    </div>
  );
}

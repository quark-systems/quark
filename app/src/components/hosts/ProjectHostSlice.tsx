// The Project's slice of each host it runs on, for the Project dashboard's Overview and Metrics tabs:
// its CPU, memory and worktree disk now and over the window, beside the host's own readings.
import React, { useEffect, useState } from "react";
import { api, NotAvailable, ProjectHosts, ProjectHostSlice as Slice } from "../../api";
import { href } from "../../nav";
import { errText } from "../../util";
import { Spark } from "./Spark";
import { bytes, HEALTH, pct, share, SLOT, tail } from "./format";
import "./hosts.css";

/** How often the slice re-reads; hosts are sampled once a minute. */
const REFRESH_MS = 30_000;

export function ProjectHostSlice({ project, hours = 6, detail = false }: { project: string; hours?: number; detail?: boolean }) {
  const [h, setH] = useState<ProjectHosts | null>(null);
  const [gone, setGone] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    const load = () => api.projectHosts(project, hours).then((r) => { if (live) { setH(r); setErr(null); } })
      .catch((e) => { if (!live) return; if (e instanceof NotAvailable) setGone(true); else setErr(errText(e)); });
    void load();
    const t = setInterval(load, REFRESH_MS);
    return () => { live = false; clearInterval(t); };
  }, [project, hours]);
  if (gone) return null;
  return (
    <div className="hs-slice" data-testid="project-hosts">
      {err && <div className="hs-error" role="alert">{err}</div>}
      {h?.error && <div className="hs-error" role="alert">The event log could not be read: {h.error}</div>}
      {h && h.hosts.length === 0 && !h.error && <p className="faint">Nothing of this Project has run on a sampled host in the last {h.hours} hours.</p>}
      {h?.hosts.map((s) => <HostSlice key={s.host_id} s={s} detail={detail} />)}
      <a className="btn" href={href({ name: "hosts" })}>All hosts</a>
    </div>
  );
}

function HostSlice({ s, detail }: { s: Slice; detail: boolean }) {
  const hl = HEALTH[s.health.status];
  const total = s.host?.memory_total_bytes ?? s.capacity.memory_bytes;
  const now = s.now;
  return (
    <div className="hs-slice-host" data-testid="project-host" data-host={s.host_id}>
      <div className="hs-host-head">
        <strong>{s.name}</strong>
        <span className="hs-health" title={s.health.reason ?? undefined}><i className="hs-dot" style={{ background: hl.color }} />{hl.label}</span>
        <span className="faint hs-meta">
          {now ? `${pct(now.cpu)} CPU, ${bytes(now.memory_bytes)} memory (${pct(share(now.memory_bytes, total))} of the host), ${bytes(now.disk_bytes)} in worktrees`
            : "using nothing here right now"}
        </span>
      </div>
      {detail && (
        <div className="hs-sparks">
          <Spark label="CPU" values={s.series.map((p) => p.cpu)} max={1} now={pct(now?.cpu ?? 0)} testId="project-host-cpu" />
          <Spark label="Memory" values={s.series.map((p) => p.memory_bytes)} max={total || undefined} now={bytes(now?.memory_bytes ?? 0)} color="var(--blue)" testId="project-host-memory" />
          <Spark label="Worktree disk" values={s.series.map((p) => p.disk_bytes)} now={bytes(now?.disk_bytes ?? 0)} color="var(--violet)" />
        </div>
      )}
      {now && now.parts.length > 0 && (
        <table className="hs-table">
          <thead><tr><th>Running</th><th>CPU</th><th>Memory</th><th>Disk</th></tr></thead>
          <tbody>{now.parts.map((p) => (
            <tr key={p.engine_task ?? "coordinator"}>
              <td>{p.engine_task == null ? "Coordinator"
                : p.task_id ? <a href={href({ name: "task", id: p.task_id })}>{p.title || p.engine_task}</a>
                : <span className="mono">{p.engine_task}</span>}</td>
              <td>{pct(p.cpu)}</td><td>{bytes(p.memory_bytes)}</td><td>{bytes(p.disk_bytes)}</td>
            </tr>
          ))}</tbody>
        </table>
      )}
      {detail && s.worktrees.length > 0 && (
        <details className="hs-details">
          <summary className="faint">{s.worktrees.length} worktree{s.worktrees.length === 1 ? "" : "s"} in this host's pools</summary>
          <table className="hs-table">
            <thead><tr><th>Worktree</th><th>State</th><th>Held by</th></tr></thead>
            <tbody>{s.worktrees.map((w) => <tr key={w.path}><td className="mono" title={w.path}>{tail(w.path)}</td><td>{SLOT[w.state]}</td><td className="mono">{w.holder ?? ""}</td></tr>)}</tbody>
          </table>
        </details>
      )}
    </div>
  );
}

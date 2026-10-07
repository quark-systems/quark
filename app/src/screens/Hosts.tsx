// Hosts: every machine Quark runs work on, with its health, sampled CPU, memory, pressure and disk,
// each Project's share of it, and its worktree pools. Read from the daemon's event log.
import React, { useEffect, useState } from "react";
import { api, HostView, HostsView, NotAvailable } from "../api";
import { href } from "../nav";
import { ago, errText } from "../util";
import { Unavailable } from "../components/Unavailable";
import { Spark } from "../components/hosts/Spark";
import { bytes, HEALTH, pct, RUNTIME, share, SLOT, tail } from "../components/hosts/format";
import "../components/hosts/hosts.css";

const WINDOWS = [1, 6, 24, 48] as const;
/** Hosts are sampled once a minute. */
const REFRESH_MS = 30_000;

export function Hosts() {
  const [hours, setHours] = useState<number>(6);
  const [v, setV] = useState<HostsView | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    const load = () => api.hosts(hours).then((r) => { if (live) { setV(r); setErr(null); } })
      .catch((e) => { if (!live) return; if (e instanceof NotAvailable) setUnavailable(true); else setErr(errText(e)); });
    void load();
    const t = setInterval(load, REFRESH_MS);
    return () => { live = false; clearInterval(t); };
  }, [hours]);
  return (
    <>
      <div className="header">
        <h1>Hosts</h1>
        <span className="spacer" />
        <div className="hs-hours" role="group" aria-label="Window">
          {WINDOWS.map((h) => (
            <button key={h} className={"btn" + (hours === h ? " on" : "")} aria-pressed={hours === h}
              data-testid={`hosts-hours-${h}`} onClick={() => setHours(h)}>{h}h</button>
          ))}
        </div>
      </div>
      <div className="screen hosts">
        <div className="hs-body">
          {unavailable && <Unavailable what="The Hosts view" endpoint="GET /v1/hosts" />}
          {err && <div className="hs-error" role="alert">{err}</div>}
          {v?.error && <div className="hs-error" role="alert">The event log could not be read: {v.error}</div>}
          {v && v.hosts.length === 0 && !v.error && (
            <p className="faint" data-testid="hosts-empty">No host has registered yet. The daemon registers this machine and samples it once a minute while host telemetry is on.</p>
          )}
          {v?.hosts.map((h) => <Host key={h.id} h={h} />)}
        </div>
      </div>
    </>
  );
}

function Host({ h }: { h: HostView }) {
  const hl = HEALTH[h.health.status];
  const r = h.latest;
  const memTotal = r?.memory_total_bytes ?? h.capacity.memory_bytes;
  const qd = r?.quark_disk;
  const quark = qd ? qd.worktrees_bytes + qd.logs_bytes + qd.caches_bytes + qd.event_log_bytes : 0;
  const wt = h.worktrees;
  return (
    <section className="hs-host" data-testid="host" data-host={h.id}>
      <div className="hs-host-head">
        <h2>{h.name}</h2>
        <span className="hs-health" data-testid="host-health" title={h.health.reason ?? undefined}>
          <i className="hs-dot" style={{ background: hl.color }} />{hl.label}
          {h.health.reason && <span className="faint">: {h.health.reason}</span>}
        </span>
        <span className="spacer" />
        <span className="faint hs-meta">
          {RUNTIME[h.runtime]}{h.os && ` · ${h.os} ${h.arch}`}{h.capacity.cpus > 0 && ` · ${h.capacity.cpus} CPUs`}
          {memTotal > 0 && ` · ${bytes(memTotal)}`}{h.capacity.max_workers > 0 && ` · up to ${h.capacity.max_workers} workers`}
        </span>
      </div>
      {r ? <span className="faint hs-meta">Sampled {ago(r.at)}</span> : <span className="faint hs-meta">Not sampled yet.</span>}
      <div className="hs-sparks">
        <Spark label="CPU" values={h.series.map((p) => p.cpu)} max={1} now={pct(r?.cpu)} testId="host-cpu" />
        <Spark label="Memory" values={h.series.map((p) => p.memory_used_bytes)} max={memTotal || undefined}
          now={r ? `${bytes(r.memory_used_bytes)} (${pct(share(r.memory_used_bytes, memTotal))})` : "–"} color="var(--blue)" testId="host-memory" />
        <Spark label="Pressure" values={h.series.map((p) => p.memory_pressure)} max={1} now={pct(r?.memory_pressure)} color="var(--yellow)" />
        <Spark label="Disk free" values={h.series.map((p) => p.disk_free_bytes)} now={bytes(r?.disk_free_bytes)} color="var(--green)" testId="host-disk" />
      </div>
      {qd && (
        <div className="hs-counts faint" data-testid="host-quark-disk">
          <span>Quark uses <b>{bytes(quark)}</b>:</span>
          <span>worktrees {bytes(qd.worktrees_bytes)}</span><span>logs {bytes(qd.logs_bytes)}</span>
          <span>caches {bytes(qd.caches_bytes)}</span><span>event log {bytes(qd.event_log_bytes)}</span>
        </div>
      )}

      <h3 className="hs-sub">Projects here</h3>
      {h.projects.length === 0 ? <p className="faint">No Project is using this host right now.</p> : (
        <table className="hs-table" data-testid="host-projects">
          <thead><tr><th>Project</th><th>CPU</th><th>Memory</th><th>Worktree disk</th></tr></thead>
          <tbody>{h.projects.flatMap((p) => [
            <tr key={p.project_id} data-testid="host-project">
              <td><a href={href({ name: "overview", project: p.project_id })}>{p.project_name ?? p.project_id}</a></td>
              <td>{pct(p.cpu)}</td><td>{bytes(p.memory_bytes)}</td><td>{bytes(p.disk_bytes)}</td>
            </tr>,
            ...p.parts.map((t) => (
              <tr key={`${p.project_id}/${t.engine_task ?? ""}`} className="faint">
                <td className="hs-part">{t.engine_task == null ? "Coordinator"
                  : t.task_id ? <a href={href({ name: "task", id: t.task_id })}>{t.title || t.engine_task}</a>
                  : <span className="mono">{t.engine_task}</span>}</td>
                <td>{pct(t.cpu)}</td><td>{bytes(t.memory_bytes)}</td><td>{bytes(t.disk_bytes)}</td>
              </tr>
            )),
          ])}</tbody>
        </table>
      )}

      <h3 className="hs-sub">Worktrees</h3>
      {!wt ? <p className="faint">No worktree pool has been reported for this host.</p> : (
        <>
          <div className="hs-counts" data-testid="host-worktrees">
            <span><b>{wt.in_use}</b> in use</span><span><b>{wt.idle}</b> idle</span><span><b>{wt.dirty}</b> dirty</span>
            <span><b>{wt.leased}</b> leased</span><span><b>{wt.quarantined}</b> quarantined</span>
            <span className="faint">as of {ago(wt.at)}</span>
          </div>
          {wt.error && <div className="hs-error">Some pools could not be read: {wt.error}</div>}
          {wt.slots.length > 0 && (
            <details className="hs-details">
              <summary className="faint">Every worktree</summary>
              <table className="hs-table">
                <thead><tr><th>Worktree</th><th>Repo</th><th>State</th><th>Held by</th></tr></thead>
                <tbody>{wt.slots.map((s) => (
                  <tr key={s.path}><td className="mono" title={s.path}>{tail(s.path)}</td><td className="mono" title={s.repo}>{s.repo ? tail(s.repo) : ""}</td>
                    <td>{SLOT[s.state]}</td><td className="mono">{s.holder ?? ""}</td></tr>
                ))}</tbody>
              </table>
            </details>
          )}
        </>
      )}
    </section>
  );
}

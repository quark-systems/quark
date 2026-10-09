// Project dashboard, Metrics tab: how the Project's work has gone over a window of days, computed by
// the daemon from the event log. Throughput per day, lead time, how often work got through on its
// own, interventions, failovers, the quota of the accounts its tasks ran under, what the agents'
// tokens cost and the coordinator's turns and tokens.
import React, { useEffect, useState } from "react";
import { api, CoordinatorMetrics, CoordinatorTurns, DayCount, NotAvailable, ProjectMetrics, SpendMetrics, TokenSpend } from "../../api";
import { href } from "../../nav";
import { useStore } from "../../store";
import { errText } from "../../util";
import { Unavailable } from "../../components/Unavailable";
import { ProjectHostSlice } from "../../components/hosts/ProjectHostSlice";
import "./overview.css";
import "./metrics.css";

const WINDOWS = [7, 30, 90] as const;

export function Metrics({ project: pid }: { project: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);
  const [days, setDays] = useState<number>(7);
  const [m, setM] = useState<ProjectMetrics | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const load = () => {
    setLoadErr(null);
    return api.projectMetrics(pid, days).then(setM)
      .catch((e) => { if (e instanceof NotAvailable) setUnavailable(true); else setLoadErr(errText(e)); });
  };
  useEffect(() => { void load(); }, [pid, days, connected]);

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }
  const finished = m ? m.throughput.done + m.throughput.failed : 0;
  return (
    <>
      <div className="header">
        <h1>Metrics</h1>
        <a className="crumb" href={href({ name: "project", id: pid })}>{project.name}</a>
        <span className="spacer" />
        <div className="met-window" role="group" aria-label="Window">
          {WINDOWS.map((d) => (
            <button key={d} className={"btn" + (days === d ? " on" : "")} aria-pressed={days === d}
              data-testid={`metrics-days-${d}`} onClick={() => setDays(d)}>{d} days</button>
          ))}
        </div>
        <button className="btn" onClick={() => void load()} title="Compute again">Refresh</button>
      </div>
      <div className="screen metrics">
        {unavailable && <Unavailable what="Project metrics" endpoint={`GET /v1/projects/${pid}/metrics`} />}
        {loadErr && <div className="met-error" role="alert">{loadErr} <button className="btn" onClick={() => void load()}>Retry</button></div>}
        {m && <div className="met-body">
          <p className="faint" data-testid="metrics-coverage">
            {m.log_started_at
              ? <>Last {m.days} days. The event log has this Project's work since {when(m.log_started_at)}; earlier work is not counted.</>
              : <>Nothing from this Project is in the event log yet.</>}
          </p>

          <div className="met-tiles">
            <Tile label="Finished" value={String(finished)} sub={`${m.throughput.done} done, ${m.throughput.failed} failed`} testId="metrics-finished" />
            <Tile label="Lead time" value={dur(m.lead_time.median_s)}
              sub={m.lead_time.tasks ? `median of ${plural(m.lead_time.tasks, "task")}; p90 ${dur(m.lead_time.p90_s)}` : "no task done yet"} testId="metrics-lead" />
            <Tile label="Pass rate" value={pct(m.gates.pass_rate)} sub="finished tasks that ended done" testId="metrics-pass" />
            <Tile label="First-time green" value={pct(m.gates.first_time_green_rate)}
              sub={`${m.gates.first_time_green} done with no failure, blocker or relaunch`} testId="metrics-ftg" />
            <Tile label="Interventions" value={String(m.interventions.decisions + m.interventions.blockers)}
              sub={`${plural(m.interventions.decisions, "decision")}, ${plural(m.interventions.blockers, "blocker")}` +
                (m.interventions.per_finished_task != null ? `; ${m.interventions.per_finished_task.toFixed(1)} per finished task` : "")} testId="metrics-interventions" />
            <Tile label="Relaunches" value={String(m.interventions.relaunches)} sub="workers started again" testId="metrics-relaunches" />
            {m.spend && hasSpend(m.spend) && (
              <Tile label="Spend" value={usd(total(m.spend).usd)}
                sub={m.spend.usd_per_done_task != null
                  ? `${usd(m.spend.usd_per_done_task)} per done task`
                  : `${tokens(total(m.spend).input_tokens + total(m.spend).output_tokens)} tokens`} testId="metrics-spend" />
            )}
            <Tile label="Failovers" value={String(m.failovers.relaunched + m.failovers.no_healthy_account + m.failovers.relaunch_failed)}
              sub={`${m.failovers.relaunched} moved accounts, ${m.failovers.no_healthy_account + m.failovers.relaunch_failed} needed a decision`} testId="metrics-failovers" />
          </div>

          <section className="met-section" data-testid="metrics-throughput">
            <h2>Finished per day</h2>
            <Throughput days={m.throughput.per_day} />
          </section>

          {m.spend && hasSpend(m.spend) && <Spend s={m.spend} />}

          <section className="met-section" data-testid="metrics-accounts">
            <h2>Accounts and quota</h2>
            {m.accounts.length === 0 ? <p className="faint">No task has recorded an account yet.</p> : (
              <table className="met-table">
                <thead><tr><th>Account</th><th>Harness</th><th>Tasks</th><th>Quota left</th></tr></thead>
                <tbody>{m.accounts.map((a) => (
                  <tr key={a.account_id}>
                    <td>{a.label}</td><td className="mono">{a.harness}</td><td>{a.tasks}</td>
                    <td>{a.quota.remaining_percent != null
                      ? <span className="met-quota"><span className="met-quota-bar"><span style={{ width: `${a.quota.remaining_percent}%` }} /></span>{Math.round(a.quota.remaining_percent)}%</span>
                      : <span className="faint">{a.quota.detail ?? a.quota.state}</span>}</td>
                  </tr>
                ))}</tbody>
              </table>
            )}
            <a className="btn" href={href({ name: "accounts" })}>Accounts and pools</a>
          </section>

          <section className="met-section" data-testid="metrics-hosts">
            <h2>Hosts, last 48 hours</h2>
            <ProjectHostSlice project={pid} hours={48} detail />
          </section>

          {m.coordinator && hasTurns(m.coordinator) && <Coordinator c={m.coordinator} />}

          {m.unavailable.length > 0 && (
            <section className="met-section" data-testid="metrics-unavailable">
              <h2>Not measured yet</h2>
              <ul className="met-notes">{m.unavailable.map((u) => <li key={u.metric}><span className="mono">{LABELS[u.metric] ?? u.metric}</span> <span className="faint">{u.reason}</span></li>)}</ul>
            </section>
          )}
        </div>}
      </div>
    </>
  );
}

const LABELS: Record<string, string> = { spend: "Spend", coordinator_tokens: "Coordinator token efficiency" };

export function hasTurns(c: CoordinatorMetrics) { return c.baseline.turns + c.native.turns + c.would_wake_turns > 0; }

/** Turns and tokens per task for firstmate's coordinator and the native one, and how many turns only acknowledged status. */
function Coordinator({ c }: { c: CoordinatorMetrics }) {
  const row = (name: string, t: CoordinatorTurns, testId: string) => (
    <tr data-testid={testId}>
      <td>{name}</td><td>{t.turns}</td><td>{tokens(t.input_tokens + t.output_tokens)}</td><td>{num(t.turns_per_task)}</td><td>{tokens(t.tokens_per_task)}</td>
      <td>{pct(t.ack_share)}</td>
    </tr>
  );
  return (
    <section className="met-section" data-testid="metrics-coordinator">
      <h2>Coordinator</h2>
      <p className="faint">
        {c.tasks > 0 && <>Over {c.tasks === 1 ? "1 task" : `${c.tasks} tasks`}. </>}A turn that changed nothing only acknowledged status; the native
        coordinator is woken only for judgment, so its share should stay near zero.
        {c.native.turns === 0 && c.would_wake_turns > 0 && <> Running beside today's coordinator, it would have taken {c.would_wake_turns} {c.would_wake_turns === 1 ? "turn" : "turns"}.</>}
      </p>
      <table className="met-table">
        <thead><tr><th>Coordinator</th><th>Turns</th><th>Tokens</th><th>Turns per task</th><th>Tokens per task</th><th>Status-only turns</th></tr></thead>
        <tbody>
          {row("Today's (firstmate)", c.baseline, "metrics-coordinator-baseline")}
          {c.native.turns > 0 && row("Native", c.native, "metrics-coordinator-native")}
        </tbody>
      </table>
    </section>
  );
}

export function hasSpend(s: SpendMetrics) { return s.workers.turns + s.coordinator.turns > 0; }

function total(s: SpendMetrics): TokenSpend {
  const a = s.workers, b = s.coordinator;
  return {
    turns: a.turns + b.turns, input_tokens: a.input_tokens + b.input_tokens, output_tokens: a.output_tokens + b.output_tokens,
    cache_read_tokens: a.cache_read_tokens + b.cache_read_tokens,
    usd: a.usd == null && b.usd == null ? null : (a.usd ?? 0) + (b.usd ?? 0),
    unpriced_tokens: a.unpriced_tokens + b.unpriced_tokens,
  };
}

/** Tokens and cost of worker and coordinator turns, and per model. */
function Spend({ s }: { s: SpendMetrics }) {
  const all = total(s);
  const row = (name: string, t: TokenSpend, testId: string) => (
    <tr data-testid={testId}>
      <td>{name}</td><td>{t.turns}</td><td>{tokens(t.input_tokens)}</td><td>{tokens(t.cache_read_tokens)}</td><td>{tokens(t.output_tokens)}</td><td>{usd(t.usd)}</td>
    </tr>
  );
  return (
    <section className="met-section" data-testid="metrics-spend-detail">
      <h2>Tokens and spend</h2>
      <p className="faint">
        Read from the agents' session logs. Cost is at API list price (or the harness's own figure), so work under a
        subscription shows what it would cost through the API.
        {all.unpriced_tokens > 0 && <> {tokens(all.unpriced_tokens)} tokens are from models with no known price and are not in the cost.</>}
        {s.usd_per_done_task != null && <> Tasks done in this window cost {usd(s.usd_per_done_task)} each in worker turns, over {plural(s.done_tasks, "task")}.</>}
      </p>
      <table className="met-table">
        <thead><tr><th>Agent</th><th>Turns</th><th>Input</th><th>Of it cached</th><th>Output</th><th>Cost</th></tr></thead>
        <tbody>
          {row("Workers", s.workers, "metrics-spend-workers")}
          {row("Coordinator", s.coordinator, "metrics-spend-coordinator")}
        </tbody>
      </table>
      {s.by_model.length > 0 && (
        <table className="met-table" data-testid="metrics-spend-models">
          <thead><tr><th>Model</th><th>Input</th><th>Output</th><th>Cost</th></tr></thead>
          <tbody>{s.by_model.map((m) => (
            <tr key={m.model}><td className="mono">{m.model || "unknown"}</td><td>{tokens(m.input_tokens)}</td><td>{tokens(m.output_tokens)}</td><td>{usd(m.usd)}</td></tr>
          ))}</tbody>
        </table>
      )}
    </section>
  );
}

/** US dollars: cents under $100, whole dollars above. */
export function usd(n?: number | null): string {
  if (n == null) return "–";
  if (n > 0 && n < 0.01) return "<$0.01";
  return n < 100 ? `$${n.toFixed(2)}` : `$${Math.round(n).toLocaleString("en-US")}`;
}

function num(n?: number | null) { return n == null ? "–" : n.toFixed(1); }

export function tokens(n?: number | null): string {
  if (n == null) return "–";
  if (n < 1000) return String(Math.round(n));
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

function Tile({ label, value, sub, testId }: { label: string; value: string; sub: string; testId: string }) {
  return (
    <div className="met-tile" data-testid={testId}>
      <div className="met-tile-label">{label}</div>
      <div className="met-tile-value">{value}</div>
      <div className="met-tile-sub faint">{sub}</div>
    </div>
  );
}

/** Done and failed per day as stacked bars on one count axis, with a table for the exact numbers. */
function Throughput({ days }: { days: DayCount[] }) {
  const [hover, setHover] = useState<number | null>(null);
  const max = Math.max(1, ...days.map((d) => d.done + d.failed));
  const total = days.reduce((n, d) => n + d.done + d.failed, 0);
  if (total === 0) return <p className="faint">No task finished in this window.</p>;
  const h = hover != null ? days[hover] : null;
  return (
    <div className="met-chart">
      <div className="met-legend">
        <span><i className="met-key done" />Done</span>
        <span><i className="met-key failed" />Failed</span>
        <span className="met-readout faint" aria-live="polite">{h ? `${h.date}: ${h.done} done, ${h.failed} failed` : `peak ${max} a day`}</span>
      </div>
      <div className="met-bars" style={{ gridTemplateColumns: `repeat(${days.length}, 1fr)` }} onMouseLeave={() => setHover(null)}>
        {days.map((d, i) => (
          <div key={d.date} className={"met-bar" + (hover === i ? " on" : "")} onMouseEnter={() => setHover(i)}
            title={`${d.date}: ${d.done} done, ${d.failed} failed`} data-testid="metrics-day">
            {d.failed > 0 && <span className="failed" style={{ height: `${(d.failed / max) * 100}%` }} />}
            {d.done > 0 && <span className="done" style={{ height: `${(d.done / max) * 100}%` }} />}
          </div>
        ))}
      </div>
      <div className="met-axis faint"><span>{days[0].date}</span><span>{days[days.length - 1].date}</span></div>
      <details className="met-details">
        <summary className="faint">Table</summary>
        <table className="met-table">
          <thead><tr><th>Day</th><th>Done</th><th>Failed</th></tr></thead>
          <tbody>{days.filter((d) => d.done + d.failed > 0).map((d) => <tr key={d.date}><td className="mono">{d.date}</td><td>{d.done}</td><td>{d.failed}</td></tr>)}</tbody>
        </table>
      </details>
    </div>
  );
}

function pct(r?: number | null) { return r == null ? "–" : `${Math.round(r * 100)}%`; }

export function dur(s?: number | null): string {
  if (s == null) return "–";
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) { const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60); return m ? `${h}h ${m}m` : `${h}h`; }
  const d = Math.floor(s / 86400), h = Math.round((s % 86400) / 3600);
  return h ? `${d}d ${h}h` : `${d}d`;
}

function when(iso: string) { return new Date(iso).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }); }

function plural(n: number, one: string, many = one + "s") { return `${n} ${n === 1 ? one : many}`; }

// Project dashboard, Overview tab: the project's goal and four numbers up top, then what happened
// since you last looked beside where it runs, then every worker now, folded from the daemon's event log. The last visit's place in the log is remembered per viewer;
// the digest stays anchored there for the whole visit until "Mark as read".
import React, { useEffect, useMemo, useRef, useState } from "react";
import { api, DigestItem, DigestKind, NotAvailable, ProjectOverview, PulseState, TaskPulse } from "../../api";
import { href } from "../../nav";
import { useStore } from "../../store";
import { ago, errText } from "../../util";
import { Unavailable } from "../../components/Unavailable";
import { HarnessLogo } from "../../components/WorkerCard/HarnessLogo";
import { ProjectHostSlice } from "../../components/hosts/ProjectHostSlice";
import { lastSeen, PULSE, saveSeen, summarize } from "./summary";
import "./settings.css";
import "./overview.css";

/** How often the tab re-reads while open; the daemon folds new log entries on each read. */
const REFRESH_MS = 5000;

const KIND: Record<DigestKind, { label: string; color: string }> = {
  decision_opened: { label: "Decision", color: "var(--violet)" },
  decision_resolved: { label: "Resolved", color: "var(--fg-dim)" },
  done: { label: "Done", color: "var(--green)" },
  failed: { label: "Failed", color: "var(--red)" },
  spawned: { label: "Started", color: "var(--blue)" },
};

export function Overview({ project: pid }: { project: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);
  // First visit: digest the whole log.
  const [since, setSince] = useState(() => lastSeen(pid) ?? 0);
  const firstVisit = useRef(lastSeen(pid) === null);
  const saved = useRef(false);
  const [o, setO] = useState<ProjectOverview | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [loadErr, setLoadErr] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    const load = () => api.projectOverview(pid, since).then((r) => {
      if (!live) return;
      setO(r); setLoadErr(null);
      // The next visit digests from here; this one keeps its anchor.
      if (!saved.current) { saved.current = true; saveSeen(pid, r.head); }
    }).catch((e) => {
      if (!live) return;
      if (e instanceof NotAvailable) setUnavailable(true); else setLoadErr(errText(e));
    });
    void load();
    const t = setInterval(load, REFRESH_MS);
    return () => { live = false; clearInterval(t); };
  }, [pid, since, connected]);

  const markRead = () => {
    if (!o) return;
    saveSeen(pid, o.head);
    firstVisit.current = false;
    setSince(o.head);
  };

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }
  const d = o?.digest;
  const counts = o?.live.counts;
  return (
    <>
      <div className="screen settings overview">
        {unavailable && <Unavailable what="The Project overview" endpoint={`GET /v1/projects/${pid}/overview`} />}
        {loadErr && <div className="set-error" role="alert">{loadErr}</div>}
        {o?.error && <div className="set-error" role="alert" data-testid="overview-error">The event log could not be read: {o.error}</div>}
        <div className="set-body ov-body">
          {project.goal && (
            <div className="ov-goal" data-testid="overview-goal">
              <span className="faint">Goal</span>
              <p>{project.goal}</p>
            </div>
          )}
          <Cards pid={pid} counts={counts} />
          <div className="ov-columns">
          <section className="set-section" data-testid="overview-digest">
            <div className="ov-head">
              <h2>{firstVisit.current ? "Since the log began" : "Since you last looked"}</h2>
              {d?.from && <span className="faint">{ago(d.from)}</span>}
              <span className="spacer" />
              {d && d.events > 0 && <button className="btn" onClick={markRead} data-testid="overview-mark-read">Mark as read</button>}
            </div>
            {d && <p className="ov-summary" data-testid="overview-summary">{summarize(d)}</p>}
            {d && d.highlights.length > 0 && (
              <ul className="ov-list" data-testid="overview-highlights">
                {d.highlights.map((h) => <Highlight key={h.seq} h={h} />)}
              </ul>
            )}
            {d?.truncated && <p className="faint">Showing the newest {d.highlights.length}.</p>}
          </section>

          <section className="set-section" data-testid="overview-hosts">
            <div className="ov-head"><h2>Where it runs</h2></div>
            <ProjectHostSlice project={pid} />
          </section>
          </div>

          <section className="set-section" data-testid="overview-now">
            <div className="ov-head">
              <h2>Workers now</h2>
              {o?.live.last_activity && <span className="faint" data-testid="overview-last-activity">last activity {ago(o.live.last_activity)}</span>}
            </div>
            {o && o.live.tasks.length === 0 && <p className="faint">No task has reported anything yet.</p>}
            {o && o.live.tasks.length > 0 && (
              <ul className="ov-list" data-testid="overview-tasks">
                {o.live.tasks.map((t) => <Pulse key={t.engine_task} t={t} />)}
              </ul>
            )}
          </section>

        </div>
      </div>
    </>
  );
}

const ACTIVE: PulseState[] = ["working", "needs_decision", "blocked", "paused"];
const usd = (n: number) => `$${n < 10 ? n.toFixed(2) : Math.round(n)}`;
const sameDay = (iso: string | null | undefined) => !!iso && new Date(iso).toDateString() === new Date().toDateString();

/** The four numbers at the top: workers, issues ready, PRs merged today, spend today. */
function Cards({ pid, counts }: { pid: string; counts?: Record<PulseState, number> }) {
  const prs = useStore((s) => s.pullRequests);
  const beadsActivity = useStore((s) => s.beadsActivity[pid] ?? 0);
  const [issues, setIssues] = useState<{ ready: number; blocked: number } | null | "none">(null);
  const [spend, setSpend] = useState<number | null | "none">(null);

  useEffect(() => {
    let live = true;
    Promise.all([api.issues(pid, "ready"), api.issues(pid, "blocked")])
      .then(([r, b]) => { if (live) setIssues({ ready: r.length, blocked: b.length }); })
      .catch(() => { if (live) setIssues("none"); });
    return () => { live = false; };
  }, [pid, beadsActivity]);
  useEffect(() => {
    let live = true;
    api.projectMetrics(pid, 1).then((m) => {
      if (!live) return;
      const sp = m.spend;
      setSpend(sp && (sp.workers.usd != null || sp.coordinator.usd != null) ? (sp.workers.usd ?? 0) + (sp.coordinator.usd ?? 0) : "none");
    }).catch(() => { if (live) setSpend("none"); });
    return () => { live = false; };
  }, [pid]);

  const mine = useMemo(() => Object.values(prs).filter((p) => p.project_id === pid), [prs, pid]);
  const merged = mine.filter((p) => p.state === "merged" && sameDay(p.merged_at)).length;
  const open = mine.filter((p) => p.state === "open" || p.state === "draft").length;
  const active = counts ? ACTIVE.reduce((n, k) => n + counts[k], 0) : null;
  const breakdown = counts ? ACTIVE.filter((k) => counts[k]).map((k) => `${counts[k]} ${PULSE[k].label.toLowerCase()}`).join(", ") : "";

  return (
    <div className="ov-cards">
      <Card testid="overview-card-workers" label="Workers" value={active ?? "…"} sub={active ? breakdown : "none running"} />
      <Card testid="overview-card-issues" label="Issues ready" value={issues === null ? "…" : issues === "none" ? "—" : issues.ready}
        sub={issues === "none" ? "Beads is not set up" : issues && issues.blocked ? `${issues.blocked} blocked` : ""} />
      <Card testid="overview-card-merged" label="Merged today" value={merged} sub={`${open} open PR${open === 1 ? "" : "s"}`} />
      <Card testid="overview-card-spend" label="Spend today" value={spend === null ? "…" : spend === "none" ? "—" : usd(spend)}
        sub={spend === "none" ? "not reported" : "workers and coordinator"} />
    </div>
  );
}

function Card({ testid, label, value, sub }: { testid: string; label: string; value: React.ReactNode; sub: string }) {
  return (
    <div className="ov-card" data-testid={testid}>
      <span className="ov-card-label">{label}</span>
      <span className="ov-card-value">{value}</span>
      <span className="ov-card-sub">{sub || "\u00a0"}</span>
    </div>
  );
}

function TaskName({ taskId, title, engineTask }: { taskId?: string | null; title?: string | null; engineTask?: string | null }) {
  const name = title ?? engineTask ?? "Project";
  const cls = "ov-task" + (title ? "" : " mono");
  return taskId ? <a className={cls} href={href({ name: "task", id: taskId })}>{name}</a> : <span className={cls}>{name}</span>;
}

function Pulse({ t }: { t: TaskPulse }) {
  const p = PULSE[t.state];
  return (
    <li className="ov-row" data-testid="overview-task" data-state={t.state}>
      <span className="ov-dot" style={{ background: p.color }} title={p.label.replace(" today", "")} />
      <div className="ov-main">
        <div className="ov-line">
          <TaskName taskId={t.task_id} title={t.title} engineTask={t.engine_task} />
          {t.open_decisions.length > 0 && <span className="pill ov-decision" title="Open decisions">{t.open_decisions.join(", ")}</span>}
          {t.pull_request && <a className="ov-link mono" href={t.pull_request} target="_blank" rel="noreferrer">{prLabel(t.pull_request)}</a>}
        </div>
        <div className="ov-note dim" title={t.note}><span className="mono faint">{t.verb}</span> {t.note}</div>
      </div>
      {t.harness && <span className="ov-agent faint" title={t.model ? `${t.harness} · ${t.model}` : t.harness}><HarnessLogo harness={t.harness} size={14} />{t.model && <span className="mono">{t.model}</span>}</span>}
      <span className="ov-when faint">{ago(t.at)}</span>
    </li>
  );
}

function Highlight({ h }: { h: DigestItem }) {
  const k = KIND[h.kind];
  return (
    <li className="ov-row" data-testid="overview-highlight" data-kind={h.kind}>
      <span className="ov-kind" style={{ color: k.color }}>{k.label}</span>
      <div className="ov-main">
        <div className="ov-line">
          <TaskName taskId={h.task_id} title={h.title} engineTask={h.engine_task} />
          {h.url && <a className="ov-link mono" href={h.url} target="_blank" rel="noreferrer">{prLabel(h.url)}</a>}
        </div>
        {h.text && <div className="ov-note dim" title={h.text}>{h.text}</div>}
      </div>
      <span className="ov-when faint">{ago(h.at)}</span>
    </li>
  );
}

/** `owner/repo#12` for a pull request URL. */
function prLabel(url: string): string {
  const m = /([^/]+\/[^/]+)\/pull\/(\d+)/.exec(url);
  return m ? `${m[1]}#${m[2]}` : "pull request";
}

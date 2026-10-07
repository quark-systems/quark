// Project dashboard, Settings tab: every per-Project switch in one place. Standing approval and
// each source's holdout tests change here; delivery and the agent are shown as chosen at creation;
// dispatch rules, pools and memory are summarized with a link to the screen that edits them.
import React, { useEffect, useState } from "react";
import { api, ApiError, NotAvailable, ProjectSettings, SourceVerification } from "../../api";
import { href } from "../../nav";
import { useStore } from "../../store";
import { errText } from "../../util";
import { Unavailable } from "../../components/Unavailable";
import { StandingApproval } from "../../components/StandingApproval";
import { DashboardTabs } from "./Tabs";
import { PersonaPicker } from "../../components/PersonaPicker";
import "./settings.css";
import "./overview.css";

const DELIVERY = {
  gated: ["Gated", "Every change passes the verification gates before its pull request opens."],
  direct: ["Direct", "Workers open the pull request directly; CI is the only check."],
} as const;

export function Settings({ project: pid }: { project: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);
  const [settings, setSettings] = useState<ProjectSettings | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const load = () => {
    setLoadErr(null);
    return api.projectSettings(pid).then(setSettings)
      .catch((e) => { if (e instanceof NotAvailable) setUnavailable(true); else setLoadErr(errText(e)); });
  };
  useEffect(() => { void load(); }, [pid, connected]);

  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<{ text: string; stale: boolean } | null>(null);
  const setHoldout = async (source: string, enabled: boolean) => {
    if (!settings) return;
    setBusy(source); setErr(null);
    try {
      setSettings(await api.updateProjectSettings(pid, { revision: settings.verification.revision, holdout: [{ source, enabled }] }));
    } catch (e) {
      setErr({ text: errText(e), stale: e instanceof ApiError && e.code === "settings_changed" });
    } finally { setBusy(null); }
  };

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }
  const s = settings;
  const agent = s?.agent_config ?? project.agent_config;
  return (
    <>
      <div className="header">
        <h1>Settings</h1>
        <a className="crumb" href={href({ name: "project", id: pid })}>{project.name}</a>
        <span className="spacer" />
        <DashboardTabs project={pid} current="settings" />
      </div>
      <div className="screen settings">
        {unavailable && <Unavailable what="Project settings" endpoint={`GET /v1/projects/${pid}/settings`} />}
        {loadErr && <div className="set-error" role="alert">{loadErr} <button className="btn" onClick={() => void load()}>Retry</button></div>}
        <div className="set-body">
          <Section title="Merging" testId="settings-merging">
            <StandingApproval project={project} />
            <p className="faint">When on, the coordinator merges this Project's pull requests as soon as their checks pass.</p>
          </Section>

          <Section title="Delivery" testId="settings-delivery">
            {s && <>
              <div className="set-row"><span className="pill accent" data-testid="settings-delivery-mode">{DELIVERY[s.delivery][0]}</span>
                <span className="dim">{DELIVERY[s.delivery][1]}</span></div>
              <p className="faint">Chosen when the Project was created.</p>
            </>}
          </Section>

          <Section title="Agent" testId="settings-agent">
            {agent ? (
              <dl className="set-kv">
                <dt>Harness</dt><dd className="mono">{agent.harness}</dd>
                <dt>Model</dt><dd className="mono">{agent.model ?? "harness default"}</dd>
                <dt>Effort</dt><dd className="mono">{agent.effort ?? "harness default"}</dd>
                <dt>Pool</dt><dd className="mono" data-testid="settings-pool">{agent.pool ?? "default account"}</dd>
              </dl>
            ) : <p className="faint">No agent configured.</p>}
            <a className="btn" href={href({ name: "accounts" })}>Accounts and pools</a>
          </Section>

          <Section title="Verification gates" testId="settings-gates">
            {s?.verification.error && <div className="set-error" role="alert">{s.verification.error}</div>}
            {s && !s.verification.revision && <p className="faint">This Project has no project.yaml to declare gates in.</p>}
            {s?.verification.sources.map((v) => (
              <Source key={v.source} v={v} busy={busy === v.source} onHoldout={(on) => void setHoldout(v.source, on)} />
            ))}
            {err && <div className="set-error" role="alert" data-testid="settings-error">{err.text}
              {err.stale && <button className="btn" onClick={() => { setErr(null); void load(); }}>Load again</button>}</div>}
            <p className="faint">Checks and journeys are declared per source under <span className="mono">verification</span> in the Project repo's project.yaml.</p>
          </Section>

          <Section title="Dispatch" testId="settings-dispatch">
            {s && (s.dispatch.error
              ? <div className="set-error" role="alert">{s.dispatch.error}</div>
              : <p data-testid="settings-dispatch-summary">
                {plural(s.dispatch.rules, "rule")} and {plural(s.dispatch.default_candidates, "default candidate")}.{" "}
                {s.dispatch.classifier && s.dispatch.classifier !== "none" ? `Classifier: ${s.dispatch.classifier}.` : "No classifier; the coordinator picks."}
              </p>)}
            <a className="btn" href={href({ name: "dispatch", project: pid })} data-testid="settings-open-dispatch">Edit dispatch rules</a>
          </Section>

          <Section title="Automation" testId="settings-automation">
            <p className="faint">The coordinator's inbox, trigger rules, and what reaches you while you are away or quiet.</p>
            <a className="btn" href={href({ name: "automation", project: pid })} data-testid="settings-open-automation">Open automation</a>
          </Section>

          <Section title="Memory" testId="settings-memory">
            {s && <p data-testid="settings-memory-summary">
              {plural(s.memory.entries, "entry", "entries")}, {plural(s.memory.proposals_to_review, "proposal")} to review.
            </p>}
            <a className="btn" href={href({ name: "memory", project: pid })} data-testid="settings-open-memory">Open memory</a>
          </Section>

          <Section title="Persona" testId="settings-persona">
            <PersonaPicker projectId={pid} />
            <p className="faint">Role names, how agents address you and some labels. It changes how the Project reads, not what it does.</p>
          </Section>
        </div>
      </div>
    </>
  );
}

function Section({ title, testId, children }: { title: string; testId: string; children: React.ReactNode }) {
  return (
    <section className="set-section" data-testid={testId}>
      <h2>{title}</h2>
      {children}
    </section>
  );
}

function Source({ v, busy, onHoldout }: { v: SourceVerification; busy: boolean; onHoldout: (on: boolean) => void }) {
  const h = v.holdout;
  const runs = h.enabled && h.categories.length > 0;
  return (
    <div className="set-source" data-testid="settings-source">
      <div className="set-source-name mono">{v.source}</div>
      <div className="set-gate">
        <span className="set-gate-kind">Checks</span>
        {v.checks.length ? (
          <ul>{v.checks.map((c) => <li key={c.name}><span className="mono">{c.name}</span> <span className="faint mono">{c.run}</span></li>)}</ul>
        ) : <span className="faint">none</span>}
      </div>
      <div className="set-gate">
        <span className="set-gate-kind">Journeys</span>
        {v.journeys ? <span className="mono faint">{v.journeys.start} → {v.journeys.url}</span> : <span className="faint">none</span>}
      </div>
      <div className="set-gate">
        <span className="set-gate-kind">Holdout</span>
        <label className={"standing" + (h.enabled ? " on" : "")} data-testid="settings-holdout">
          <input type="checkbox" checked={h.enabled} disabled={busy} onChange={() => onHoldout(!h.enabled)}
            aria-label={`Holdout tests for ${v.source}`} />
          <span className="switch" aria-hidden />
          <span data-testid="settings-holdout-state">{
            runs ? `Runs ${plural(h.categories.length, "category", "categories")}: ${h.categories.join(", ")}`
              : h.enabled ? "On, but there are no tests under holdout/" + v.source
                : "Off" + (h.categories.length ? ` (${plural(h.categories.length, "category", "categories")} not run)` : "")
          }</span>
        </label>
      </div>
    </div>
  );
}

function plural(n: number, one: string, many = one + "s") { return `${n} ${n === 1 ? one : many}`; }

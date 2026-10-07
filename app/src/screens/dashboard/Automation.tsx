// Project dashboard, Automation tab: the coordinator's inbox, trigger rules (condition to action)
// and the away policy (what wakes the coordinator and how the user hears, per posture).
import React, { useEffect, useState } from "react";
import {
  api, AwayOccasion, AwayPosture, AwayReach, AwayRoute, NotAvailable, ProjectAutomation,
  TriggerAction, TriggerCondition, TriggerRule,
} from "../../api";
import { href } from "../../nav";
import { useStore } from "../../store";
import { errText } from "../../util";
import { Unavailable } from "../../components/Unavailable";
import "./settings.css";
import "./automation.css";

const POSTURES: AwayPosture[] = ["present", "away", "quiet"];
const OCCASIONS: [AwayOccasion, string][] = [
  ["progress", "Progress"], ["paused", "Paused"], ["stale", "Stale worker"], ["done", "Done"],
  ["decision", "Decision"], ["blocked", "Blocked"], ["failed", "Failed"], ["inbound", "Inbound message"],
  ["trigger_fired", "Trigger fired"], ["trigger_failed", "Trigger failed"],
];
const REACH: AwayReach[] = ["silent", "digest", "notify", "hold"];
const cap = (s: string) => s[0].toUpperCase() + s.slice(1);

export function Automation({ project: pid }: { project: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);
  const [a, setA] = useState<ProjectAutomation | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const load = () => {
    setLoadErr(null);
    return api.projectAutomation(pid).then(setA)
      .catch((e) => { if (e instanceof NotAvailable) setUnavailable(true); else setLoadErr(errText(e)); });
  };
  useEffect(() => { void load(); }, [pid, connected]);

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }
  return (
    <>
      <div className="header">
        <h1>Automation</h1>
        <a className="crumb" href={href({ name: "project", id: pid })}>{project.name}</a>
        <span className="spacer" />
      </div>
      <div className="screen settings">
        {unavailable && <Unavailable what="Automation" endpoint={`GET /v1/projects/${pid}/automation`} />}
        {loadErr && <div className="set-error" role="alert">{loadErr} <button className="btn" onClick={() => void load()}>Retry</button></div>}
        {a && <div className="set-body">
          {!a.acting && <p className="faint" data-testid="automation-shadow">
            Rules and the away policy are recorded but not acted on yet: the current engine still wakes the coordinator, and
            each rule's count is how often it would have fired.
          </p>}
          <InboxSection pid={pid} a={a} reload={load} />
          <RulesSection pid={pid} rules={a.rules} reload={load} />
          <AwaySection pid={pid} a={a} onSaved={(away) => setA({ ...a, away })} />
        </div>}
      </div>
    </>
  );
}

function InboxSection({ pid, a, reload }: { pid: string; a: ProjectAutomation; reload: () => Promise<void> }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [sent, setSent] = useState(false);
  const send = async () => {
    setBusy(true); setErr(null); setSent(false);
    try { await api.postInboxNote(pid, text.trim()); setText(""); setSent(true); await reload(); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  return (
    <section className="set-section" data-testid="automation-inbox">
      <h2>Inbox</h2>
      {a.inbox.length ? (
        <ul className="auto-list">{a.inbox.map((m) => (
          <li key={m.channel + m.id} data-testid="automation-inbox-item">
            <span className="auto-body">{m.body}</span>
            <span className="faint mono">{m.channel} · {m.from} · {new Date(m.at).toLocaleString()}</span>
          </li>))}
        </ul>
      ) : <p className="faint">Nothing waiting for the coordinator.</p>}
      <div className="auto-note">
        <textarea aria-label="Note for the coordinator" placeholder="A note the coordinator reads when it next looks" value={text}
          onChange={(e) => { setText(e.target.value); setSent(false); }} rows={2} />
        <button className="btn" disabled={busy || !text.trim()} onClick={() => void send()}>Leave note</button>
      </div>
      {sent && <p className="faint" data-testid="automation-note-sent">Left for the coordinator.</p>}
      {err && <div className="set-error" role="alert">{err}</div>}
    </section>
  );
}

function RulesSection({ pid, rules, reload }: { pid: string; rules: TriggerRule[]; reload: () => Promise<void> }) {
  const [editing, setEditing] = useState<TriggerRule | "new" | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const run = async (f: () => Promise<unknown>) => {
    setErr(null);
    try { await f(); await reload(); } catch (e) { setErr(errText(e)); }
  };
  return (
    <section className="set-section" data-testid="automation-rules">
      <h2>Trigger rules</h2>
      {rules.length ? (
        <ul className="auto-list">{rules.map((r) => (
          <li key={r.id} data-testid="automation-rule">
            <div className="set-row">
              <span className="mono auto-id">{r.id}</span>
              <span className="dim">{describeWhen(r.when)} → {describeThen(r.then)}</span>
            </div>
            {r.description && <span className="faint">{r.description}</span>}
            <div className="set-row">
              <label className={"standing" + (r.enabled ? " on" : "")}>
                <input type="checkbox" checked={r.enabled} aria-label={`Rule ${r.id} enabled`}
                  onChange={() => void run(() => api.putTriggerRule(pid, r.id, { ...r, enabled: !r.enabled }))} />
                <span className="switch" aria-hidden /><span>{r.enabled ? "On" : "Off"}</span>
              </label>
              <span className="faint" data-testid="automation-rule-fires">{r.fires === 1 ? "1 fire" : `${r.fires} fires`}{r.once ? ", once" : ""}</span>
              <button className="btn" onClick={() => setEditing(r)}>Edit</button>
              <button className="btn" onClick={() => void run(() => api.deleteTriggerRule(pid, r.id))}>Delete</button>
            </div>
          </li>))}
        </ul>
      ) : <p className="faint">No rules yet.</p>}
      {editing ? (
        <RuleForm key={editing === "new" ? "" : editing.id} rule={editing === "new" ? null : editing}
          onCancel={() => setEditing(null)}
          onSave={async (id, put) => { await api.putTriggerRule(pid, id, put); setEditing(null); await reload(); }} />
      ) : <button className="btn" data-testid="automation-add-rule" onClick={() => setEditing("new")}>Add rule</button>}
      {err && <div className="set-error" role="alert">{err}</div>}
      <p className="faint">Actions are deterministic; anything that needs judgment wakes the coordinator. Commands run without a shell.</p>
    </section>
  );
}

const lines = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean);
const str = (v: unknown) => (typeof v === "string" ? v : v == null ? "" : String(v));

function RuleForm({ rule, onCancel, onSave }: {
  rule: TriggerRule | null; onCancel: () => void; onSave: (id: string, put: Parameters<typeof api.putTriggerRule>[2]) => Promise<void>;
}) {
  const [id, setId] = useState(rule?.id ?? "");
  const [description, setDescription] = useState(rule?.description ?? "");
  const [once, setOnce] = useState(rule?.once ?? false);
  const [when, setWhen] = useState<TriggerCondition>(rule?.when ?? { source: "event", kind: "" });
  const [then, setThen] = useState<TriggerAction>(rule?.then ?? { do: "wake", note: "" });
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const w = (k: string, v: unknown) => setWhen({ ...when, [k]: v });
  const t = (k: string, v: unknown) => setThen({ ...then, [k]: v });
  const save = async () => {
    setBusy(true); setErr(null);
    try { await onSave(id.trim(), { description, once, enabled: rule?.enabled ?? true, when, then }); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  const expect = when.expect as { op: string; value: string } | undefined;
  return (
    <div className="auto-form" data-testid="automation-rule-form">
      <label>Id<input aria-label="Rule id" className="mono" value={id} disabled={!!rule} onChange={(e) => setId(e.target.value)} placeholder="nightly-check" /></label>
      <label>Description<input aria-label="Rule description" value={description} onChange={(e) => setDescription(e.target.value)} /></label>
      <label>When
        <select aria-label="Condition" value={when.source} onChange={(e) => setWhen(fresh(e.target.value as TriggerCondition["source"]))}>
          <option value="event">An event is recorded</option>
          <option value="every">Every interval</option>
          <option value="at">At a time</option>
          <option value="command">A command succeeds</option>
        </select>
      </label>
      {when.source === "event" && <>
        <label>Event kind<input aria-label="Event kind" className="mono" value={str(when.kind)} placeholder="firstmate.status or pr.*" onChange={(e) => w("kind", e.target.value)} /></label>
        <label>Task<input aria-label="Event task" className="mono" value={str(when.task)} placeholder="any" onChange={(e) => w("task", e.target.value || undefined)} /></label>
      </>}
      {when.source === "every" && <label>Seconds<input aria-label="Interval seconds" type="number" min={1} value={str(when.secs)} onChange={(e) => w("secs", Number(e.target.value))} /></label>}
      {when.source === "at" && <label>Time (RFC 3339)<input aria-label="At time" className="mono" value={str(when.at)} placeholder="2026-10-08T09:00:00Z" onChange={(e) => w("at", e.target.value)} /></label>}
      {when.source === "command" && <>
        <label>Command, one argument per line<textarea aria-label="Condition command" className="mono" rows={3}
          value={((when.argv as string[] | undefined) ?? []).join("\n")} onChange={(e) => w("argv", lines(e.target.value))} /></label>
        <label>Poll every (seconds)<input aria-label="Poll seconds" type="number" min={1} value={str(when.interval_secs ?? 60)} onChange={(e) => w("interval_secs", Number(e.target.value))} /></label>
        <label>Output
          <select aria-label="Expected output" value={expect?.op ?? ""} onChange={(e) => w("expect", e.target.value ? { op: e.target.value, value: expect?.value ?? "" } : undefined)}>
            <option value="">Any (exit 0 is enough)</option>
            <option value="equals">Equals</option>
            <option value="differs">Differs from</option>
          </select>
        </label>
        {expect && <label>Text<input aria-label="Expected text" className="mono" value={expect.value} onChange={(e) => w("expect", { ...expect, value: e.target.value })} /></label>}
      </>}
      <label>Then
        <select aria-label="Action" value={then.do} onChange={(e) => setThen(freshAction(e.target.value as TriggerAction["do"]))}>
          <option value="wake">Wake the coordinator</option>
          <option value="inbox">Leave an inbox note</option>
          <option value="steer">Steer a task</option>
          <option value="command">Run a command</option>
        </select>
      </label>
      {then.do === "wake" && <label>Note<input aria-label="Wake note" value={str(then.note)} onChange={(e) => t("note", e.target.value)} /></label>}
      {then.do === "inbox" && <label>Note<input aria-label="Inbox note" value={str(then.body)} onChange={(e) => t("body", e.target.value)} /></label>}
      {then.do === "steer" && <>
        <label>Task<input aria-label="Steer task" className="mono" value={str(then.task)} onChange={(e) => t("task", e.target.value)} /></label>
        <label>Message<input aria-label="Steer text" value={str(then.text)} onChange={(e) => t("text", e.target.value)} /></label>
      </>}
      {then.do === "command" && <label>Command, one argument per line<textarea aria-label="Action command" className="mono" rows={3}
        value={((then.argv as string[] | undefined) ?? []).join("\n")} onChange={(e) => t("argv", lines(e.target.value))} /></label>}
      <label className="auto-check"><input type="checkbox" checked={once} onChange={(e) => setOnce(e.target.checked)} /> Fire once</label>
      <div className="set-row">
        <button className="btn primary" disabled={busy || !id.trim()} onClick={() => void save()}>Save rule</button>
        <button className="btn" onClick={onCancel}>Cancel</button>
      </div>
      {err && <div className="set-error" role="alert" data-testid="automation-rule-error">{err}</div>}
    </div>
  );
}

function fresh(source: TriggerCondition["source"]): TriggerCondition {
  switch (source) {
    case "event": return { source, kind: "" };
    case "every": return { source, secs: 3600 };
    case "at": return { source, at: "" };
    case "command": return { source, argv: [] };
  }
}
function freshAction(d: TriggerAction["do"]): TriggerAction {
  switch (d) {
    case "wake": return { do: d, note: "" };
    case "inbox": return { do: d, body: "" };
    case "steer": return { do: d, task: "", text: "" };
    case "command": return { do: d, argv: [] };
  }
}
function describeWhen(c: TriggerCondition): string {
  switch (c.source) {
    case "event": return `on ${str(c.kind)}${c.task ? ` for ${str(c.task)}` : ""}`;
    case "every": return `every ${duration(Number(c.secs))}`;
    case "at": return `at ${str(c.at)}`;
    case "command": return `when \`${((c.argv as string[]) ?? []).join(" ")}\` succeeds`;
  }
}
function describeThen(a: TriggerAction): string {
  switch (a.do) {
    case "wake": return "wake the coordinator";
    case "inbox": return "leave a note";
    case "steer": return `steer ${str(a.task)}`;
    case "command": return `run \`${((a.argv as string[]) ?? []).join(" ")}\``;
  }
}
function duration(s: number): string {
  if (s % 86400 === 0) return s === 86400 ? "day" : `${s / 86400} days`;
  if (s % 3600 === 0) return s === 3600 ? "hour" : `${s / 3600} hours`;
  if (s % 60 === 0) return s === 60 ? "minute" : `${s / 60} minutes`;
  return `${s} seconds`;
}

function AwaySection({ pid, a, onSaved }: { pid: string; a: ProjectAutomation; onSaved: (away: ProjectAutomation["away"]) => void }) {
  const [routes, setRoutes] = useState<AwayRoute[]>(a.away.routes);
  const [minutes, setMinutes] = useState(Math.round(a.away.digest_secs / 60));
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => { setRoutes(a.away.routes); setMinutes(Math.round(a.away.digest_secs / 60)); }, [a.away]);
  const cell = (p: AwayPosture, o: AwayOccasion) => routes.find((r) => r.posture === p && r.occasion === o);
  const setCell = (p: AwayPosture, o: AwayOccasion, change: Partial<AwayRoute>) =>
    setRoutes(routes.map((r) => (r.posture === p && r.occasion === o ? { ...r, ...change } : r)));
  const dirty = minutes * 60 !== a.away.digest_secs || JSON.stringify(routes) !== JSON.stringify(a.away.routes);
  const save = async () => {
    setBusy(true); setErr(null);
    try {
      // Every cell goes; the daemon keeps only the ones that differ from the default.
      const all = routes.map(({ overridden: _, ...r }) => r);
      onSaved(await api.putAwayPolicy(pid, { digest_secs: minutes * 60, routes: all }));
    } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  return (
    <section className="set-section" data-testid="automation-away">
      <h2>Away policy</h2>
      <div className="set-row">
        <span className="pill accent" data-testid="automation-posture">{cap(a.away.posture)}</span>
        <span className="dim">{a.away.waiting} waiting for the next digest, {a.away.held} held for your return.</span>
      </div>
      <table className="auto-table">
        <thead><tr><th />{POSTURES.map((p) => <th key={p} colSpan={2}>{cap(p)}</th>)}</tr>
          <tr><th>When</th>{POSTURES.map((p) => <React.Fragment key={p}><th>Wake</th><th>You hear</th></React.Fragment>)}</tr></thead>
        <tbody>{OCCASIONS.map(([o, label]) => (
          <tr key={o} data-testid={`automation-route-${o}`}>
            <td>{label}</td>
            {POSTURES.map((p) => {
              const c = cell(p, o);
              if (!c) return <React.Fragment key={p}><td /><td /></React.Fragment>;
              return (
                <React.Fragment key={p}>
                  <td><input type="checkbox" aria-label={`${label} ${p}: wake the coordinator`} checked={c.wake} onChange={(e) => setCell(p, o, { wake: e.target.checked })} /></td>
                  <td><select aria-label={`${label} ${p}: you hear`} className={c.overridden ? "auto-overridden" : ""} value={c.user}
                    onChange={(e) => setCell(p, o, { user: e.target.value as AwayReach })}>
                    {REACH.map((r) => <option key={r} value={r}>{r}</option>)}
                  </select></td>
                </React.Fragment>
              );
            })}
          </tr>))}
        </tbody>
      </table>
      <div className="set-row">
        <label>Digest every <input aria-label="Digest minutes" type="number" min={1} value={minutes} onChange={(e) => setMinutes(Number(e.target.value))} /> minutes</label>
        <button className="btn primary" disabled={busy || !dirty || minutes < 1} onClick={() => void save()}>Save policy</button>
      </div>
      {err && <div className="set-error" role="alert" data-testid="automation-away-error">{err}</div>}
      <p className="faint">Away holds your decisions for your return; quiet keeps you present but only interrupts for what matters. A policy where a decision or failure reaches no one is refused.</p>
    </section>
  );
}

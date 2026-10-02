// J2: name the Project, state the goal, add repos, pick a default agent config and a
// dispatch preset. The agent picker lists the harnesses the daemon reports as able to
// coordinate, and the config is validated before the Project is created.
import React, { useEffect, useMemo, useState } from "react";
import { api, DeliveryPolicy, DispatchPreset, HarnessInfo, NotAvailable, ValidationIssue } from "../api";
import { href } from "../nav";
import { addProject } from "../store";
import { errText } from "../util";

/** Used until `GET /v1/harnesses` is served. */
export const FALLBACK_HARNESSES: HarnessInfo[] = [
  { id: "claude-code", name: "Claude Code" },
  { id: "codex", name: "Codex" },
  { id: "pi", name: "Pi" },
].map((h) => ({
  ...h, roles: ["coordinator", "worker"], install: { installed: true, install_hint: "" },
  models: { selection: "free_form" }, efforts: [], auth: { state: "unknown" }, transcript: true,
}));

export const PRESETS: { id: DispatchPreset; label: string; description: string }[] = [
  { id: "single", label: "Single", description: "Every task uses the default agent." },
  { id: "light_trivial", label: "Light for trivial work", description: "Trivial mechanical edits run at low effort; everything else uses the default agent." },
];

const REPO_RE = /^(https?:\/\/\S+|ssh:\/\/\S+|git@\S+:\S+|\/\S+|[\w.-]+\/[\w.-]+)$/;
export function validRepo(s: string) { return REPO_RE.test(s.trim()); }

/** `owner/name` is GitHub shorthand; anything else is passed through as a clone URL or path. */
export function repoUrl(s: string) {
  const t = s.trim();
  return /^[\w.-]+\/[\w.-]+$/.test(t) ? `https://github.com/${t}.git` : t;
}

export function NewProject({ onCreated }: { onCreated: (id: string) => void }) {
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [repos, setRepos] = useState<string[]>([""]);
  const [harnesses, setHarnesses] = useState<HarnessInfo[] | null>(null);
  const [harnessesLive, setHarnessesLive] = useState(true);
  const [harness, setHarness] = useState("");
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("");
  const [preset, setPreset] = useState<DispatchPreset>("single");
  const [delivery, setDelivery] = useState<DeliveryPolicy>("gated");
  const [workspace, setWorkspace] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [issues, setIssues] = useState<ValidationIssue[]>([]);

  useEffect(() => {
    api.harnesses()
      .then((h) => setHarnesses(h))
      .catch((e) => {
        if (!(e instanceof NotAvailable)) console.warn("harnesses", e);
        setHarnesses(FALLBACK_HARNESSES);
        setHarnessesLive(false);
      });
  }, []);

  // The Project's agent config runs its coordinator, so only coordinator-capable harnesses fit.
  const options = useMemo(() => (harnesses ?? []).filter((h) => h.roles.includes("coordinator")), [harnesses]);
  useEffect(() => {
    if (!harness) setHarness((options.find((h) => h.install.installed) ?? options[0])?.id ?? "");
  }, [options, harness]);
  const current = options.find((h) => h.id === harness);
  useEffect(() => { setModel(""); setEffort(""); setIssues([]); }, [harness]);

  const cleanRepos = repos.map((r) => r.trim()).filter(Boolean);
  const badRepo = cleanRepos.find((r) => !validRepo(r));
  const canSubmit = name.trim() !== "" && !badRepo && !!harness && !busy;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!canSubmit) return;
    setBusy(true); setError(null); setIssues([]);
    const agent_config = {
      harness,
      model: current?.models.selection === "automatic" ? null : model.trim() || null,
      effort: effort || null,
    };
    try {
      try {
        const v = await api.validateAgent(agent_config, "coordinator");
        if (!v.valid) { setIssues(v.errors); return; }
      } catch (err) {
        if (!(err instanceof NotAvailable)) throw err; // older daemon: the create call validates
      }
      const p = await api.createProject({
        name: name.trim(),
        goal: goal.trim() || null,
        workspace_path: workspace.trim() || null,
        repos: cleanRepos.map((r) => ({ url: repoUrl(r) })),
        agent_config,
        dispatch_preset: preset,
        delivery,
      });
      addProject(p);
      onCreated(p.id);
    } catch (err) {
      setError(errText(err));
    } finally {
      setBusy(false);
    }
  };

  const modelHint = current?.models.selection === "provider_qualified" ? "provider/model" : "Model (optional)";

  return (
    <>
      <div className="header">
        <h1>New project</h1>
        <span className="spacer" />
        <a className="btn" href={href({ name: "projects" })}>Cancel</a>
      </div>
      <div className="screen scroll">
        <form className="form" onSubmit={submit} data-testid="new-project-form">
          <label className="field">
            <span>Name</span>
            <input autoFocus required value={name} onChange={(e) => setName(e.target.value)} placeholder="Parser rewrite" name="name" />
          </label>
          <label className="field">
            <span>Goal</span>
            <textarea rows={3} value={goal} onChange={(e) => setGoal(e.target.value)} name="goal"
              placeholder="What this Project should achieve, in a sentence or two. The coordinator plans against it." />
          </label>

          <fieldset className="field">
            <span>Repositories</span>
            {repos.map((r, i) => (
              <div className="repo-row" key={i}>
                <input value={r} placeholder="owner/name, a clone URL or a local path" aria-label={`Repository ${i + 1}`}
                  className={r.trim() && !validRepo(r) ? "invalid" : ""}
                  onChange={(e) => setRepos(repos.map((x, j) => (j === i ? e.target.value : x)))} />
                {repos.length > 1 && (
                  <button type="button" className="btn" aria-label="Remove repository"
                    onClick={() => setRepos(repos.filter((_, j) => j !== i))}>×</button>
                )}
              </div>
            ))}
            <button type="button" className="btn add" onClick={() => setRepos([...repos, ""])}>Add repository</button>
            {badRepo && <div className="field-error">“{badRepo}” is not owner/name, a clone URL or a local path.</div>}
          </fieldset>

          <fieldset className="field">
            <span>Default agent</span>
            <div className="agent-row">
              <select value={harness} onChange={(e) => setHarness(e.target.value)} aria-label="Harness" disabled={!harnesses}>
                {!harnesses && <option>Loading…</option>}
                {options.map((h) => (
                  <option key={h.id} value={h.id} disabled={!h.install.installed}>
                    {h.name}{h.install.version ? ` ${h.install.version}` : ""}{h.install.installed ? "" : " (not installed)"}
                  </option>
                ))}
              </select>
              {current?.models.selection !== "automatic" && (
                <input value={model} onChange={(e) => setModel(e.target.value)} placeholder={modelHint} aria-label="Model" />
              )}
              {current && current.efforts.length > 0 && (
                <select value={effort} onChange={(e) => setEffort(e.target.value)} aria-label="Effort">
                  <option value="">Default effort</option>
                  {current.efforts.map((x) => <option key={x} value={x}>{x}</option>)}
                </select>
              )}
            </div>
            {current?.models.discovery && <div className="hint-line">Models: {current.models.discovery}</div>}
            {current?.models.selection === "automatic" && <div className="hint-line">{current.name} picks its model itself.</div>}
            {current?.auth.state === "not_configured" && (
              <div className="field-error">{current.name} is not signed in{current.auth.detail ? `: ${current.auth.detail}` : "."}</div>
            )}
            {!harnessesLive && <div className="hint-line">The daemon does not report installed harnesses yet, so this is a fixed list.</div>}
            {options.filter((h) => !h.install.installed && h.install.install_hint).map((h) => (
              <div className="hint-line" key={h.id}>{h.name} is not installed: <span className="mono">{h.install.install_hint}</span></div>
            ))}
            {issues.map((i) => <div className="field-error" key={i.field + i.code}>{i.message}</div>)}
          </fieldset>

          <fieldset className="field">
            <span>Dispatch preset</span>
            <div className="presets">
              {PRESETS.map((p) => (
                <label key={p.id} className={"preset" + (preset === p.id ? " on" : "")}>
                  <input type="radio" name="preset" value={p.id} checked={preset === p.id} onChange={() => setPreset(p.id)} />
                  <b>{p.label}</b>
                  <span className="faint">{p.description}</span>
                </label>
              ))}
            </div>
          </fieldset>

          <details className="field">
            <summary>Advanced</summary>
            <label className="field">
              <span>Delivery</span>
              <select value={delivery} onChange={(e) => setDelivery(e.target.value as DeliveryPolicy)} aria-label="Delivery">
                <option value="gated">Gated: changes pass the verification gates before a PR</option>
                <option value="direct">Direct: workers open PRs directly; CI is the only check</option>
              </select>
            </label>
            <label className="field">
              <span>Workspace path</span>
              <input value={workspace} onChange={(e) => setWorkspace(e.target.value)} placeholder="Attach an existing workspace instead of creating one" />
            </label>
          </details>

          {error && <div className="form-error" role="alert">{error}</div>}
          <div className="form-actions">
            <button className="btn on" type="submit" disabled={!canSubmit}>{busy ? "Creating…" : "Create project"}</button>
          </div>
        </form>
      </div>
    </>
  );
}

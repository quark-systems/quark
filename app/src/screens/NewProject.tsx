// J2: name the Project, state the goal, add repos, pick a default agent config and a
// dispatch preset. Harnesses and presets come from the daemon when it serves them.
import React, { useEffect, useMemo, useState } from "react";
import { api, DispatchPreset, Harness, NotAvailable } from "../api";
import { href } from "../nav";
import { addProject } from "../store";
import { errText } from "../util";

/** Used until `GET /v1/harnesses` is served (workstream 6). */
export const FALLBACK_HARNESSES: Harness[] = [
  { id: "claude", name: "Claude Code", installed: true, models: [], efforts: [] },
  { id: "codex", name: "Codex", installed: true, models: [], efforts: [] },
  { id: "pi", name: "Pi", installed: true, models: [], efforts: [] },
  { id: "opencode", name: "OpenCode", installed: true, models: [], efforts: [] },
];
/** Used until `GET /v1/dispatch/presets` is served. */
export const FALLBACK_PRESETS: DispatchPreset[] = [
  { id: "balanced", label: "Balanced", description: "Default harness for most work; strongest model for design and investigation." },
  { id: "fast", label: "Fast", description: "Cheaper, quicker models for well-understood work." },
  { id: "thorough", label: "Thorough", description: "Strongest models and highest effort everywhere." },
];

const REPO_RE = /^(https?:\/\/\S+|git@\S+:\S+|[\w.-]+\/[\w.-]+)$/;

export function validRepo(s: string) { return REPO_RE.test(s.trim()); }

export function NewProject({ onCreated }: { onCreated: (id: string) => void }) {
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [repos, setRepos] = useState<string[]>([""]);
  const [harnesses, setHarnesses] = useState<Harness[] | null>(null);
  const [harnessesLive, setHarnessesLive] = useState(true);
  const [presets, setPresets] = useState<DispatchPreset[]>(FALLBACK_PRESETS);
  const [harness, setHarness] = useState("");
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("");
  const [preset, setPreset] = useState("balanced");
  const [workspace, setWorkspace] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.harnesses()
      .then((h) => setHarnesses(h))
      .catch((e) => {
        if (!(e instanceof NotAvailable)) console.warn("harnesses", e);
        setHarnesses(FALLBACK_HARNESSES);
        setHarnessesLive(false);
      });
    api.dispatchPresets()
      .then((p) => { if (p.length) { setPresets(p); setPreset(p[0].id); } })
      .catch(() => {});
  }, []);

  const installed = useMemo(() => (harnesses ?? []).filter((h) => h.installed), [harnesses]);
  useEffect(() => { if (!harness && installed[0]) setHarness(installed[0].id); }, [installed, harness]);
  const current = harnesses?.find((h) => h.id === harness);
  useEffect(() => { setModel(""); setEffort(""); }, [harness]);

  const cleanRepos = repos.map((r) => r.trim()).filter(Boolean);
  const badRepo = cleanRepos.find((r) => !validRepo(r));
  const canSubmit = name.trim() !== "" && !badRepo && !!harness && !busy;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!canSubmit) return;
    setBusy(true); setError(null);
    try {
      const p = await api.createProject({
        name: name.trim(),
        goal: goal.trim() || null,
        workspace_path: workspace.trim() || null,
        repos: cleanRepos,
        agent_config: { harness, model: model || null, effort: effort || null },
        dispatch_preset: preset,
      });
      addProject(p);
      onCreated(p.id);
    } catch (err) {
      setError(errText(err));
    } finally {
      setBusy(false);
    }
  };

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
                <input value={r} placeholder="owner/name or a clone URL" aria-label={`Repository ${i + 1}`}
                  className={r.trim() && !validRepo(r) ? "invalid" : ""}
                  onChange={(e) => setRepos(repos.map((x, j) => (j === i ? e.target.value : x)))} />
                {repos.length > 1 && (
                  <button type="button" className="btn" aria-label="Remove repository"
                    onClick={() => setRepos(repos.filter((_, j) => j !== i))}>×</button>
                )}
              </div>
            ))}
            <button type="button" className="btn add" onClick={() => setRepos([...repos, ""])}>Add repository</button>
            {badRepo && <div className="field-error">“{badRepo}” is not owner/name or a clone URL.</div>}
          </fieldset>

          <fieldset className="field">
            <span>Default agent</span>
            <div className="agent-row">
              <select value={harness} onChange={(e) => setHarness(e.target.value)} aria-label="Harness" disabled={!harnesses}>
                {!harnesses && <option>Loading…</option>}
                {(harnesses ?? []).map((h) => (
                  <option key={h.id} value={h.id} disabled={!h.installed}>
                    {h.name}{h.version ? ` ${h.version}` : ""}{h.installed ? "" : " (not installed)"}
                  </option>
                ))}
              </select>
              {current && current.models.length > 0 ? (
                <select value={model} onChange={(e) => setModel(e.target.value)} aria-label="Model">
                  <option value="">Harness default model</option>
                  {current.models.map((m) => <option key={m} value={m}>{m}</option>)}
                </select>
              ) : (
                <input value={model} onChange={(e) => setModel(e.target.value)} placeholder="Model (optional)" aria-label="Model" />
              )}
              {current && current.efforts.length > 0 ? (
                <select value={effort} onChange={(e) => setEffort(e.target.value)} aria-label="Effort">
                  <option value="">Default effort</option>
                  {current.efforts.map((x) => <option key={x} value={x}>{x}</option>)}
                </select>
              ) : (
                <input value={effort} onChange={(e) => setEffort(e.target.value)} placeholder="Effort (optional)" aria-label="Effort" />
              )}
            </div>
            {!harnessesLive && <div className="hint-line">The daemon does not report installed harnesses yet, so this is a fixed list.</div>}
            {current && !current.installed && current.install_hint && <div className="hint-line">{current.install_hint}</div>}
          </fieldset>

          <fieldset className="field">
            <span>Dispatch preset</span>
            <div className="presets">
              {presets.map((p) => (
                <label key={p.id} className={"preset" + (preset === p.id ? " on" : "")}>
                  <input type="radio" name="preset" value={p.id} checked={preset === p.id} onChange={() => setPreset(p.id)} />
                  <b>{p.label}</b>
                  {p.description && <span className="faint">{p.description}</span>}
                </label>
              ))}
            </div>
          </fieldset>

          <details className="field">
            <summary>Advanced</summary>
            <label className="field">
              <span>Workspace path</span>
              <input value={workspace} onChange={(e) => setWorkspace(e.target.value)} placeholder="Default: under the Quark home" />
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

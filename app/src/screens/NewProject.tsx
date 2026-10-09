// J2: name the Project, state the goal, add repos, pick a default agent config and a
// dispatch preset. The agent picker lists the harnesses the daemon reports as able to
// coordinate, and the config is validated before the Project is created.
import React, { useEffect, useMemo, useState } from "react";
import { Account, api, DeliveryPolicy, DispatchPreset, HarnessInfo, NotAvailable, ValidationIssue } from "../api";
import { poolsFor } from "../accounts";
import { href } from "../nav";
import { addProject } from "../store";
import { Button, ButtonLink, ControlRow, Disclosure, Field, FieldError, FieldHint, Form, FormActions, FormError, OptionCards, Select, TextArea, TextInput } from "../ui";
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
  const [pool, setPool] = useState("");
  const [accounts, setAccounts] = useState<Account[]>([]);
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

  useEffect(() => {
    // Pools are optional: an older daemon without accounts simply offers none.
    api.accounts().then(setAccounts).catch(() => setAccounts([]));
  }, []);

  // The Project's agent config runs its coordinator, so only coordinator-capable harnesses fit.
  const options = useMemo(() => (harnesses ?? []).filter((h) => h.roles.includes("coordinator")), [harnesses]);
  useEffect(() => {
    if (!harness) setHarness((options.find((h) => h.install.installed) ?? options[0])?.id ?? "");
  }, [options, harness]);
  const current = options.find((h) => h.id === harness);
  useEffect(() => { setModel(""); setEffort(""); setPool(""); setIssues([]); }, [harness]);
  const pools = useMemo(() => poolsFor(accounts, harness), [accounts, harness]);

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
      ...(pool ? { pool } : {}),
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

  const agentHints = [
    pool && "Each task runs under one of the pool's accounts, the least busy when it starts.",
    current?.models.discovery && `Models: ${current.models.discovery}`,
    current?.models.selection === "automatic" && `${current.name} picks its model itself.`,
    !harnessesLive && "The daemon does not report installed harnesses yet, so this is a fixed list.",
    ...options.filter((h) => !h.install.installed && h.install.install_hint).map((h) => (
      <React.Fragment key={h.id}>{h.name} is not installed: <span className="mono">{h.install.install_hint}</span></React.Fragment>
    )),
  ];
  const agentErrors = [
    current?.auth.state === "not_configured" && `${current.name} is not signed in${current.auth.detail ? `: ${current.auth.detail}` : "."}`,
    ...issues.map((i) => i.message),
  ];

  return (
    <>
      <div className="header">
        <h1>New project</h1>
        <span className="spacer" />
        <ButtonLink href={href({ name: "projects" })}>Cancel</ButtonLink>
      </div>
      <div className="screen scroll">
        <Form className="new-project" onSubmit={submit} data-testid="new-project-form">
          <Field label="Name">
            <TextInput autoFocus required value={name} onChange={(e) => setName(e.target.value)} placeholder="Parser rewrite" name="name" />
          </Field>
          <Field label="Goal" hint="The coordinator plans against it.">
            <TextArea rows={3} value={goal} onChange={(e) => setGoal(e.target.value)} name="goal"
              placeholder="What this Project should achieve, in a sentence or two." />
          </Field>

          <Field group label="Repositories">
            {repos.map((r, i) => (
              <div className="np-repo" key={i}>
                <TextInput mono value={r} placeholder="quark-systems/quark" aria-label={`Repository ${i + 1}`}
                  invalid={!!r.trim() && !validRepo(r)}
                  onChange={(e) => setRepos(repos.map((x, j) => (j === i ? e.target.value : x)))} />
                {repos.length > 1 && (
                  <Button kind="quiet" aria-label="Remove repository" onClick={() => setRepos(repos.filter((_, j) => j !== i))}>Remove</Button>
                )}
              </div>
            ))}
            {badRepo
              ? <FieldError>“{badRepo}” is not owner/name, a clone URL or a local path.</FieldError>
              : <FieldHint>owner/name for GitHub, a clone URL, or a local path.</FieldHint>}
            <div><Button onClick={() => setRepos([...repos, ""])}>Add repository</Button></div>
          </Field>

          <Field group label="Default agent" hint={agentHints.filter(Boolean)} error={agentErrors.filter(Boolean)}>
            <ControlRow>
              <Select value={harness} onChange={(e) => setHarness(e.target.value)} aria-label="Harness" disabled={!harnesses}>
                {!harnesses && <option>Loading…</option>}
                {options.map((h) => (
                  <option key={h.id} value={h.id} disabled={!h.install.installed}>
                    {h.name}{h.install.version ? ` ${h.install.version}` : ""}{h.install.installed ? "" : " (not installed)"}
                  </option>
                ))}
              </Select>
              {current?.models.selection !== "automatic" && (
                <TextInput value={model} onChange={(e) => setModel(e.target.value)} placeholder={modelHint} aria-label="Model" />
              )}
              {current && current.efforts.length > 0 && (
                <Select value={effort} onChange={(e) => setEffort(e.target.value)} aria-label="Effort">
                  <option value="">Default effort</option>
                  {current.efforts.map((x) => <option key={x} value={x}>{x}</option>)}
                </Select>
              )}
              {pools.length > 0 && (
                <Select value={pool} onChange={(e) => setPool(e.target.value)} aria-label="Account pool">
                  <option value="">Default account</option>
                  {pools.map((p) => <option key={p} value={p}>Pool: {p}</option>)}
                </Select>
              )}
            </ControlRow>
          </Field>

          <Field group label="Dispatch preset">
            <OptionCards name="preset" value={preset} onChange={setPreset}
              options={PRESETS.map((p) => ({ value: p.id, label: p.label, description: p.description }))} />
          </Field>

          <Disclosure summary="Advanced">
            <Field label="Delivery">
              <Select value={delivery} onChange={(e) => setDelivery(e.target.value as DeliveryPolicy)} aria-label="Delivery">
                <option value="gated">Gated: changes pass the verification gates before a PR</option>
                <option value="direct">Direct: workers open PRs directly; CI is the only check</option>
              </Select>
            </Field>
            <Field label="Workspace path" hint="Leave empty to create a new workspace.">
              <TextInput mono value={workspace} onChange={(e) => setWorkspace(e.target.value)} placeholder="~/work/parser" />
            </Field>
          </Disclosure>

          {error && <FormError>{error}</FormError>}
          <FormActions>
            <Button kind="primary" type="submit" disabled={!canSubmit}>{busy ? "Creating…" : "Create project"}</Button>
          </FormActions>
        </Form>
      </div>
    </>
  );
}

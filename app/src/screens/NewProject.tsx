// J2: name the Project, state the goal, add repos, pick a default agent config and a
// dispatch preset. The agent picker lists the harnesses the daemon reports as able to
// coordinate, and the config is validated before the Project is created.
import React, { useEffect, useMemo, useState } from "react";
import { Account, api, DeliveryPolicy, DispatchPreset, HarnessInfo, NotAvailable, ValidationIssue } from "../api";
import { poolsFor } from "../accounts";
import { href } from "../nav";
import { addProject } from "../store";
import { PickedRepo, RepoPicker } from "../components/RepoPicker";
import { Button, ButtonLink, ControlRow, Disclosure, Field, FolderPicker, Form, FormActions, FormError, OptionCards, Select, TextArea, TextInput } from "../ui";
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
  { id: "single", label: "Same for every worker", description: "Every worker runs the agent above, at the effort above." },
  { id: "light_trivial", label: "Low effort for small edits", description: "Renames, typo fixes and one-line changes run at low effort, which is faster and cheaper. Everything else runs as above." },
];

export function NewProject({ onCreated }: { onCreated: (id: string) => void }) {
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [repos, setRepos] = useState<PickedRepo[]>([]);
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

  const canSubmit = name.trim() !== "" && !!harness && !busy;

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
        repos: repos.map((r) => ({ url: r.url })),
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
            <RepoPicker value={repos} onChange={setRepos} />
          </Field>

          <Field group label="Agent" hint={["Runs the coordinator, and every worker unless a dispatch rule says otherwise.", ...agentHints].filter(Boolean)} error={agentErrors.filter(Boolean)}>
            <ControlRow>
              <Select value={harness} onValueChange={setHarness} aria-label="Harness" disabled={!harnesses}>
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
                <Select value={effort} onValueChange={setEffort} aria-label="Effort">
                  <option value="">Default effort</option>
                  {current.efforts.map((x) => <option key={x} value={x}>{x}</option>)}
                </Select>
              )}
              {pools.length > 0 && (
                <Select value={pool} onValueChange={setPool} aria-label="Account pool">
                  <option value="">Default account</option>
                  {pools.map((p) => <option key={p} value={p}>Pool: {p}</option>)}
                </Select>
              )}
            </ControlRow>
          </Field>

          <Field group label="Workers" hint="You can add your own rules later in Settings, under Dispatch.">
            <OptionCards name="preset" value={preset} onChange={setPreset}
              options={PRESETS.map((p) => ({ value: p.id, label: p.label, description: p.description }))} />
          </Field>

          <Disclosure summary="Advanced">
            <Field label="Delivery">
              <Select value={delivery} onValueChange={(v) => setDelivery(v as DeliveryPolicy)} aria-label="Delivery">
                <option value="gated">Gated: changes pass the verification gates before a PR</option>
                <option value="direct">Direct: workers open PRs directly; CI is the only check</option>
              </Select>
            </Field>
            <Field group label="Workspace folder" hint="Leave empty and Quark creates a new workspace for this project.">
              <FolderPicker value={workspace} onChange={setWorkspace} title="Use an existing workspace folder" label="Workspace path" />
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

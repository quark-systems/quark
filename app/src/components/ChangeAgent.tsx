// Settings › Agent: change the coordinator's harness, model, effort or pool, and follow the
// switch to its end (`PUT /v1/coordinators/{id}/agent`, then its context's `switch`).
// The daemon's protocol is in docs/engine/coordinator-switch.md.
import React, { useEffect, useMemo, useState } from "react";
import { Account, AgentConfig, api, CoordinatorSwitch, HarnessInfo, NotAvailable } from "../api";
import { poolsFor } from "../accounts";
import { switching, switchLine } from "../coordinatorContext";
import { Button, ControlRow, Field, Form, FormActions, FormError, Select, TextInput } from "../ui";
import { errText } from "../util";

/** Harnesses the native coordinator runs on so far; the daemon refuses the others. */
const NATIVE_COORDINATOR = ["claude-code"];
/** How often a switch under way is read. */
const POLL_MS = 2_000;

export function ChangeAgent({ pid, agent, onChanged }: { pid: string; agent?: AgentConfig | null; onChanged: () => void }) {
  const [sw, setSw] = useState<CoordinatorSwitch | null>(null);
  const [open, setOpen] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const read = () => api.coordinatorContext(pid).then((c) => setSw(c.switch ?? null))
    .catch((e) => { if (e instanceof NotAvailable) setUnavailable(true); });
  useEffect(() => { void read(); }, [pid]);
  const under = switching(sw);
  useEffect(() => {
    if (!under) return;
    const t = setInterval(() => void read(), POLL_MS);
    return () => clearInterval(t);
  }, [under, pid]);
  // The new agent shows once the switch commits it.
  const state = sw?.state;
  useEffect(() => { if (state && state !== "requested" && state !== "wrapping_up") onChanged(); }, [state]);

  if (unavailable) return null;
  return (
    <>
      {sw && <p className={sw.state === "failed" ? "set-error" : "dim"} role="status" data-testid="agent-switch">{switchLine(sw)}</p>}
      {open
        ? <AgentForm agent={agent} onCancel={() => setOpen(false)}
            onSubmit={async (to) => { await api.switchCoordinatorAgent(pid, to); setOpen(false); await read(); }} />
        : <Button disabled={under} onClick={() => setOpen(true)} data-testid="change-agent">Change agent</Button>}
    </>
  );
}

function AgentForm({ agent, onSubmit, onCancel }: {
  agent?: AgentConfig | null; onSubmit: (to: AgentConfig) => Promise<void>; onCancel: () => void;
}) {
  const [harnesses, setHarnesses] = useState<HarnessInfo[] | null>(null);
  const [accounts, setAccounts] = useState<Account[]>([]);
  // Starts from the current agent when its harness can still coordinate.
  const keep = !!agent && NATIVE_COORDINATOR.includes(agent.harness);
  const [harness, setHarness] = useState(keep ? agent.harness : NATIVE_COORDINATOR[0]);
  const [model, setModel] = useState((keep && agent.model) || "");
  const [effort, setEffort] = useState((keep && agent.effort) || "");
  const [pool, setPool] = useState((keep && agent.pool) || "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.harnesses().then(setHarnesses).catch(() => setHarnesses([]));
    api.accounts().then(setAccounts).catch(() => setAccounts([]));
  }, []);
  const options = useMemo(() => (harnesses ?? []).filter((h) => h.roles.includes("coordinator")), [harnesses]);
  const current = options.find((h) => h.id === harness);
  const pools = useMemo(() => poolsFor(accounts, harness), [accounts, harness]);
  const changeHarness = (h: string) => { setHarness(h); setModel(""); setEffort(""); setPool(""); };
  const sameHarness = !agent || agent.harness === harness;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true); setError(null);
    try {
      await onSubmit({
        harness,
        model: current?.models.selection === "automatic" ? null : model.trim() || null,
        effort: effort || null,
        pool: pool || null,
      });
    } catch (err) { setError(errText(err)); } finally { setBusy(false); }
  };
  return (
    <Form onSubmit={submit} data-testid="change-agent-form">
      <Field group label="New agent" hint={sameHarness
        ? "The coordinator keeps its conversation and carries on with the new model and effort."
        : "The coordinator saves its work and writes a handoff, then the new one starts fresh with it."}>
        <ControlRow>
          <Select value={harness} onValueChange={changeHarness} aria-label="Harness" disabled={!harnesses}>
            {!harnesses && <option value={harness}>Loading…</option>}
            {options.map((h) => {
              const ok = NATIVE_COORDINATOR.includes(h.id) && h.install.installed;
              const why = !h.install.installed ? " (not installed)" : NATIVE_COORDINATOR.includes(h.id) ? "" : " (not a coordinator yet)";
              return <option key={h.id} value={h.id} disabled={!ok}>{h.name}{why}</option>;
            })}
          </Select>
          {current?.models.selection !== "automatic" && (
            <TextInput value={model} onChange={(e) => setModel(e.target.value)} placeholder="Model (optional)" aria-label="Model" />
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
      {error && <FormError>{error}</FormError>}
      <FormActions>
        <Button kind="primary" type="submit" disabled={busy || !harness} data-testid="change-agent-save">{busy ? "Changing…" : "Change agent"}</Button>
        <Button type="button" onClick={onCancel}>Cancel</Button>
      </FormActions>
    </Form>
  );
}

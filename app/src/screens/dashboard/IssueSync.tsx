// Project dashboard, Settings › Issue sync: the Project's one Beads database and the rules that
// sync it with issue trackers. A Project has no rules until someone adds one, so nothing leaves
// Beads by default; each rule names one repository, which way it syncs, and which new issues it pushes.
import React, { useEffect, useState } from "react";
import { api, BeadsStatus, Project, SyncDirection, SyncRule, WriteSyncRule } from "../../api";
import { loadBeads, setBeads, useStore } from "../../store";
import { defaultLabel, githubRepo } from "../../issues";
import { ago, errText } from "../../util";
import { Button, Field, FieldHint, FormActions, FormError, Select, TextInput } from "../../ui";

const DIRECTIONS: { value: SyncDirection; label: string }[] = [
  { value: "both", label: "Both ways" },
  { value: "pull", label: "Pull issues in only" },
  { value: "push", label: "Push issues out only" },
];

/** A rule being edited; `key` tells new rows apart before they have an id. */
interface Draft extends WriteSyncRule { key: string; last_sync?: SyncRule["last_sync"] }

let nextKey = 0;
const fromRule = (r: SyncRule): Draft => ({ key: r.id, id: r.id, repository: r.repository, direction: r.direction, label: r.label, enabled: r.enabled, last_sync: r.last_sync });
const blank = (repository = ""): Draft => ({ key: `new-${nextKey++}`, repository, direction: "both", label: null, enabled: true });
const comparable = (rules: Draft[]) => JSON.stringify(rules.map(({ id, repository, direction, label, enabled }) => [id ?? null, repository.trim(), direction, label?.trim() || null, enabled]));

export function IssueSync({ project }: { project: Project }) {
  const pid = project.id;
  const beads = useStore((s) => s.beads[pid]);
  useEffect(() => { void loadBeads(pid).catch(() => undefined); }, [pid]);
  if (!beads) return <p className="faint">Loading…</p>;
  if (beads.state !== "ready") return <NotReady b={beads} />;
  return <Rules key={pid} project={project} beads={beads} />;
}

function NotReady({ b }: { b: BeadsStatus }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const setup = async () => {
    setBusy(true); setErr(null);
    try { setBeads(await api.setupBeads(b.project_id)); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  const what = {
    missing: "This project has no Beads database yet.",
    setting_up: `Setting up this project's Beads database: ${b.detail ?? "starting"}.`,
    failed: `Setting up this project's Beads database failed: ${b.detail ?? "unknown error"}.`,
    unavailable: b.detail ?? "Beads is not installed on this machine.",
    ready: "",
  }[b.state];
  return (
    <div className="set-row" data-testid="issue-sync-not-ready">
      <span className="dim">{what}</span>
      {(b.state === "missing" || b.state === "failed") && <Button onClick={() => void setup()} disabled={busy}>{busy ? "Setting up…" : "Set up Beads"}</Button>}
      {err && <span className="set-error">{err}</span>}
    </div>
  );
}

function Rules({ project, beads }: { project: Project; beads: BeadsStatus }) {
  const saved = (beads.sync_rules ?? []).map(fromRule);
  const [rules, setRules] = useState<Draft[]>(saved);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // A save elsewhere (or a sync run updating last outcomes) replaces an unedited list.
  const dirty = comparable(rules) !== comparable(saved);
  useEffect(() => { if (!dirty) setRules(saved); }, [JSON.stringify(beads.sync_rules)]);

  const edit = (key: string, patch: Partial<Draft>) => setRules((rs) => rs.map((r) => (r.key === key ? { ...r, ...patch } : r)));
  const remove = (key: string) => setRules((rs) => rs.filter((r) => r.key !== key));
  const suggestions = (project.repos ?? []).map((r) => githubRepo(r.url)).filter((r): r is string => !!r)
    .filter((r) => !rules.some((x) => x.repository.trim().toLowerCase() === r.toLowerCase()));

  const save = async () => {
    setBusy(true); setErr(null);
    try {
      const body = rules.map(({ id, repository, direction, label, enabled }) => ({ id, repository: repository.trim(), direction, label: label?.trim() || null, enabled }));
      const next = await api.setSyncRules(project.id, body);
      setBeads(next);
      setRules((next.sync_rules ?? []).map(fromRule));
    } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };

  return (
    <div className="sync-rules" data-testid="issue-sync">
      <p className="faint">
        Issues and memory live in this project's own Beads database, outside its repos, and every agent works in it.{" "}
        {rules.length === 0 && "It is not synced with any issue tracker; nothing leaves Beads unless a rule below says so."}
      </p>
      {rules.map((r) => (
        <div key={r.key} className="sync-rule" data-testid="sync-rule">
          <div className="sync-rule-fields">
            <Field label="GitHub repository">
              <TextInput mono value={r.repository} placeholder="owner/repo" onChange={(e) => edit(r.key, { repository: e.target.value })} />
            </Field>
            <Field label="Direction">
              <Select value={r.direction} aria-label="Direction" onValueChange={(v) => edit(r.key, { direction: v as SyncDirection })}>
                {DIRECTIONS.map((d) => <option key={d.value} value={d.value}>{d.label}</option>)}
              </Select>
            </Field>
            <Field label="Pushes new issues labelled">
              <TextInput mono value={r.label ?? ""} placeholder={r.repository.includes("/") ? defaultLabel(r.repository.trim()) : "repo:name"}
                disabled={r.direction === "pull"} onChange={(e) => edit(r.key, { label: e.target.value })} />
            </Field>
          </div>
          <div className="set-row">
            <label className="sync-on">
              <input type="checkbox" checked={r.enabled ?? true} onChange={(e) => edit(r.key, { enabled: e.target.checked })} /> On
            </label>
            <Button kind="quiet" onClick={() => remove(r.key)}>Remove</Button>
            {r.last_sync && (
              <FieldHint>
                <span className={r.last_sync.ok ? "" : "bad"} data-testid="sync-rule-last">Last sync {ago(r.last_sync.at)}: {r.last_sync.message}</span>
              </FieldHint>
            )}
          </div>
        </div>
      ))}
      <div className="set-row">
        {suggestions.map((repo) => (
          <Button key={repo} onClick={() => setRules((rs) => [...rs, blank(repo)])} data-testid="sync-add-suggested">Sync with {repo}</Button>
        ))}
        <Button kind="quiet" onClick={() => setRules((rs) => [...rs, blank()])} data-testid="sync-add">Add a rule</Button>
      </div>
      <FieldHint>
        A pull brings in that repository's issues. A push sends the issues that came from it, plus new issues with its label.
        Decisions never sync.
      </FieldHint>
      {err && <FormError>{err}</FormError>}
      {dirty && (
        <FormActions>
          <Button kind="quiet" onClick={() => { setRules(saved); setErr(null); }} disabled={busy}>Cancel</Button>
          <Button kind="primary" onClick={() => void save()} disabled={busy} data-testid="sync-save">{busy ? "Saving…" : "Save sync rules"}</Button>
        </FormActions>
      )}
    </div>
  );
}

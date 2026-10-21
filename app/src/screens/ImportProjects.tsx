// Import projects from another tool. firstmate is the first source: pick its folder, preview what
// would come over (nothing is written, and firstmate's folder is only read), then import.
import React, { useState } from "react";
import { api, FirstmateImportReport, ImportOutcome } from "../api";
import { href } from "../nav";
import { leftBehind, pendingProjects, projectSummary, totals, unroutedGroups } from "../imports";
import { refreshSnapshots } from "../store";
import { Button, ButtonLink, ControlRow, Disclosure, Field, FolderPicker, Form, FormActions, FormError, OptionCards, Select } from "../ui";
import { errText } from "../util";

/** Where projects can be imported from. A new source adds an entry here and its own form below. */
export const SOURCES = [
  { id: "firstmate", label: "firstmate", description: "Projects, dispatch rules, work, decisions and memory from a firstmate folder. The folder is only read." },
] as const;
type SourceId = (typeof SOURCES)[number]["id"];

export function ImportProjects({ source: initial }: { source?: string }) {
  const [source, setSource] = useState<SourceId>(SOURCES.find((s) => s.id === initial)?.id ?? SOURCES[0].id);
  return (
    <>
      <div className="header">
        <h1>Import projects</h1>
        <span className="spacer" />
        <ButtonLink href={href({ name: "projects" })}>Cancel</ButtonLink>
      </div>
      <div className="screen scroll">
        <div className="import" data-testid="import-projects">
          <Field group label="Import from">
            <OptionCards name="source" value={source} onChange={setSource}
              options={SOURCES.map((s) => ({ value: s.id, label: s.label, description: s.description }))} />
          </Field>
          {source === "firstmate" && <FirstmateImport />}
        </div>
      </div>
    </>
  );
}

const OUTCOME: Record<ImportOutcome, string> = {
  imported: "Imported", present: "Already there", pending: "Waiting", not_imported: "Not imported",
};

function FirstmateImport() {
  const [home, setHome] = useState("");
  const [archive, setArchive] = useState(false);
  const [routes, setRoutes] = useState<Record<string, string>>({});
  const [report, setReport] = useState<FirstmateImportReport | null>(null);
  const [busy, setBusy] = useState<"preview" | "import" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async (dry_run: boolean, nextRoutes = routes) => {
    setBusy(dry_run ? "preview" : "import"); setError(null);
    try {
      const r = await api.importFirstmate({
        home: home.trim(), dry_run, include_archive: archive, routes: nextRoutes, wait_secs: dry_run ? undefined : 120,
      });
      setReport(r);
      if (!dry_run) await refreshSnapshots();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  };
  const assign = (prefix: string, project: string) => {
    const next = { ...routes };
    if (project) next[prefix] = project; else delete next[prefix];
    setRoutes(next);
    void run(true, next);
  };
  const reset = () => { setReport(null); setError(null); };

  if (report && !report.dry_run) return <Done report={report} busy={busy === "import"} again={() => run(false)} reset={reset} />;

  return (
    <Form onSubmit={(e) => { e.preventDefault(); if (home.trim() && !busy) void run(true); }} data-testid="import-firstmate-form">
      <Field group label="firstmate folder" hint="The folder holding firstmate's data, config and projects.">
        <FolderPicker value={home} onChange={(v) => { setHome(v); setReport(null); }} title="Choose the firstmate folder" label="firstmate folder" />
      </Field>
      <Disclosure summary="Options">
        <label className="import-check">
          <input type="checkbox" checked={archive} onChange={(e) => { setArchive(e.target.checked); setReport(null); }} />
          Also bring over finished work from firstmate's archive (can be thousands of items)
        </label>
      </Disclosure>
      {error && <FormError>{error}</FormError>}
      {!report && (
        <FormActions>
          <Button kind="primary" type="submit" disabled={!home.trim() || !!busy}>{busy === "preview" ? "Reading…" : "Preview"}</Button>
        </FormActions>
      )}
      {report && (
        <Preview report={report} routes={routes} assign={assign} busy={busy}
          start={() => run(false)} reset={reset} />
      )}
    </Form>
  );
}

function Preview({ report, routes, assign, busy, start, reset }: {
  report: FirstmateImportReport; routes: Record<string, string>;
  assign: (prefix: string, project: string) => void; busy: "preview" | "import" | null; start: () => void; reset: () => void;
}) {
  const t = totals(report);
  const groups = unroutedGroups(report.unrouted);
  return (
    <div className="import-preview" data-testid="import-preview">
      <p className="import-lead">
        {t.projects === 0 ? "This folder has no projects to import."
          : <>Quark will set up <b>{t.created}</b> new {t.created === 1 ? "project" : "projects"}{t.projects > t.created ? ` and update ${t.projects - t.created} already here` : ""}, with {t.issues} {t.issues === 1 ? "issue" : "issues"} and {t.decisions} open {t.decisions === 1 ? "decision" : "decisions"}. Nothing has been written yet.</>}
      </p>
      <table className="import-table">
        <thead><tr><th>Project</th><th>Repository</th><th>Issues</th><th>Decisions</th><th></th></tr></thead>
        <tbody>
          {report.projects.map((p) => (
            <tr key={p.name} data-testid={`import-row-${p.name}`}>
              <td>{p.name}</td>
              <td className="mono">{p.repo_url ?? "none"}</td>
              <td>{p.tasks.imported}{p.tasks.present > 0 && <span className="faint"> (+{p.tasks.present} already there)</span>}</td>
              <td>{p.holds.imported}{p.holds.present > 0 && <span className="faint"> (+{p.holds.present} already there)</span>}</td>
              <td className="faint">{projectSummary(p, true)}</td>
            </tr>
          ))}
        </tbody>
      </table>

      {groups.length > 0 && (
        <section className="import-unrouted" data-testid="import-unrouted">
          <h2>Work without a project</h2>
          <p className="faint">These items do not say which project they belong to. Choose one, or they are left out.</p>
          {groups.map((g) => (
            <ControlRow key={g.prefix}>
              <span className="mono">{g.prefix}</span>
              <span className="faint">{g.ids.length} {g.ids.length === 1 ? "item" : "items"}</span>
              <Select value={routes[g.prefix] ?? ""} onValueChange={(v) => assign(g.prefix, v)} aria-label={`Project for ${g.prefix}`}>
                <option value="">Leave out</option>
                {report.projects.map((p) => <option key={p.name} value={p.name}>{p.name}</option>)}
              </Select>
            </ControlRow>
          ))}
        </section>
      )}

      {report.notes.length > 0 && <ul className="import-notes">{report.notes.map((n, i) => <li key={i}>{n}</li>)}</ul>}
      <LeftBehind report={report} />
      <FormActions>
        <Button type="button" onClick={reset} disabled={!!busy}>Start over</Button>
        <Button kind="primary" type="button" onClick={start} disabled={t.projects === 0 || !!busy}>
          {busy === "import" ? "Importing…" : `Import ${t.projects} ${t.projects === 1 ? "project" : "projects"}`}
        </Button>
      </FormActions>
    </div>
  );
}

function LeftBehind({ report }: { report: FirstmateImportReport }) {
  const items = leftBehind(report).filter((i) => i.count > 0 || i.detail);
  if (!items.length) return null;
  return (
    <Disclosure summary={`Not imported (${items.length})`}>
      <ul className="import-notes" data-testid="import-left-behind">
        {items.map((i, k) => <li key={k}><span className="mono">{i.source}</span>: {i.detail}</li>)}
      </ul>
    </Disclosure>
  );
}

function Done({ report, busy, again, reset }: { report: FirstmateImportReport; busy: boolean; again: () => void; reset: () => void }) {
  const pending = pendingProjects(report);
  return (
    <div className="import-done" data-testid="import-done">
      <p className="import-lead">
        {pending.length === 0 ? "Imported. Your projects are in the left list." : `${report.projects.length - pending.length} of ${report.projects.length} projects imported. The rest are still being set up.`}
      </p>
      <table className="import-table">
        <thead><tr><th>Project</th><th>Result</th><th>Issues</th><th>Decisions</th></tr></thead>
        <tbody>
          {report.projects.map((p) => (
            <tr key={p.name} data-testid={`import-result-${p.name}`}>
              <td>{p.project_id && p.outcome !== "not_imported" ? <a href={href({ name: "project", id: p.project_id })}>{p.name}</a> : p.name}</td>
              <td>{OUTCOME[p.outcome]} · {projectSummary(p, false)}{p.detail && <div className="faint">{p.detail}</div>}</td>
              <td>{p.tasks.imported}</td>
              <td>{p.holds.imported}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {report.notes.length > 0 && <ul className="import-notes">{report.notes.map((n, i) => <li key={i}>{n}</li>)}</ul>}
      <FormActions>
        <Button type="button" onClick={reset}>Import another</Button>
        {pending.length > 0 && <Button kind="primary" type="button" onClick={again} disabled={busy}>{busy ? "Checking…" : "Check again"}</Button>}
        <ButtonLink href={href({ name: "projects" })}>Done</ButtonLink>
      </FormActions>
    </div>
  );
}

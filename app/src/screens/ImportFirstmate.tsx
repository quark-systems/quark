// Create a Project from a firstmate install: pick its folder, preview what comes over and
// what stays behind, then import. The daemon only reads the firstmate folder; the Project
// is created at once and the rest of the import lands once it is provisioned.
import React, { useState } from "react";
import { api, FirstmateImport, ImportItem } from "../api";
import { href } from "../nav";
import { addProject } from "../store";
import { Button, ButtonLink, Field, FolderPicker, Form, FormActions, FormError, SectionLabel, TextInput } from "../ui";
import { errText } from "../util";

function Items({ items }: { items: ImportItem[] }) {
  return (
    <ul className="import-items">
      {items.map((i) => (
        <li key={i.what}>
          <span className="import-what">{i.what}{i.count > 0 && <span className="pill">{i.count}</span>}</span>
          <span className="dim">{i.note}</span>
        </li>
      ))}
    </ul>
  );
}

export function ImportFirstmate({ onCreated }: { onCreated: (id: string) => void }) {
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [preview, setPreview] = useState<FirstmateImport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const input = (dry_run: boolean) => ({ path: path.trim(), name: name.trim() || null, dry_run });

  const look = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!path.trim() || busy) return;
    setBusy(true); setError(null); setPreview(null);
    try {
      setPreview(await api.importFirstmate(input(true)));
    } catch (err) {
      setError(errText(err));
    } finally {
      setBusy(false);
    }
  };

  const run = async () => {
    setBusy(true); setError(null);
    try {
      const done = await api.importFirstmate(input(false));
      if (!done.project) throw new Error("The daemon did not create the project.");
      addProject(done.project);
      onCreated(done.project.id);
    } catch (err) {
      setError(errText(err));
    } finally {
      setBusy(false);
    }
  };

  const plan = preview?.plan;
  const imported = plan?.items.filter((i) => i.outcome === "imported") ?? [];
  const skipped = plan?.items.filter((i) => i.outcome === "skipped") ?? [];

  return (
    <>
      <div className="header">
        <h1>Import from firstmate</h1>
        <span className="spacer" />
        <ButtonLink href={href({ name: "new" })}>Cancel</ButtonLink>
      </div>
      <div className="screen scroll">
        <Form className="new-project" onSubmit={look} data-testid="import-firstmate-form">
          <Field group label="Firstmate folder" hint="The folder you run firstmate from, with its data, config and projects folders. Quark only reads it.">
            <FolderPicker value={path} onChange={(p) => { setPath(p); setPreview(null); }} title="Choose your firstmate folder" label="Firstmate folder" />
          </Field>
          <Field label="Name" hint="Leave empty to name it after firstmate's projects.">
            <TextInput value={name} onChange={(e) => { setName(e.target.value); setPreview(null); }} name="name" />
          </Field>
          {!plan && (
            <FormActions>
              <Button kind="primary" type="submit" disabled={!path.trim() || busy}>{busy ? "Reading…" : "Preview import"}</Button>
            </FormActions>
          )}

          {plan && (
            <div className="import-preview" data-testid="import-preview">
              <SectionLabel>The new project</SectionLabel>
              <p><strong>{plan.project.name}</strong>, with {plan.project.repos?.map((r) => r.name ?? r.url).join(", ")}. The coordinator runs on {plan.project.agent_config?.harness}{plan.project.agent_config?.model ? ` with ${plan.project.agent_config.model}` : ""}.</p>
              <SectionLabel>Comes over</SectionLabel>
              <Items items={imported} />
              {skipped.length > 0 && <>
                <SectionLabel>Stays behind</SectionLabel>
                <Items items={skipped} />
              </>}
              {plan.warnings.length > 0 && <>
                <SectionLabel>Before you import</SectionLabel>
                <ul className="import-notes">{plan.warnings.map((w) => <li key={w}>{w}</li>)}</ul>
              </>}
              <p className="faint">The project is created now and sets itself up; its issues, memory and dispatch rules appear once it is ready.</p>
            </div>
          )}

          {error && <FormError>{error}</FormError>}
          {plan && (
            <FormActions>
              <Button kind="primary" type="button" onClick={() => void run()} disabled={busy}>{busy ? "Importing…" : "Import and create project"}</Button>
            </FormActions>
          )}
        </Form>
      </div>
    </>
  );
}

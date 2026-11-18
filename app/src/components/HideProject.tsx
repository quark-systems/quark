// Per-Project hiding: a hidden project stays out of the left list, All projects, the decisions
// inbox, the PR center, next attention and the palette until the app menu's
// "Show hidden projects" reveals it. Its coordinator and workers keep running.
import React, { useState } from "react";
import { api, NotAvailable, Project } from "../api";
import { addProject } from "../store";
import { errText } from "../util";
import { Toggle } from "../ui";

export function HideProject({ project }: { project: Project }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const on = !!project.hidden;

  const toggle = async (next: boolean) => {
    setBusy(true); setErr(null);
    try {
      const p = await api.setProjectHidden(project.id, next);
      addProject(p && typeof p === "object" ? { ...project, ...p } : { ...project, hidden: next });
    } catch (e) {
      setErr(e instanceof NotAvailable ? "Hiding a project is not available from this daemon yet" : errText(e));
    } finally { setBusy(false); }
  };

  return (
    <div className="set-row" data-testid="hide-project">
      <Toggle checked={on} disabled={busy} onCheckedChange={(next) => void toggle(next)}
        label={`Hide ${project.name}`} testid="hide-project-toggle" />
      <span>Hide this project</span>
      {err && <span className="bad small-text" role="alert">{err}</span>}
    </div>
  );
}

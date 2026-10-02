// Per-Project standing approval: when on, the daemon merges that Project's green PRs itself.
import React, { useState } from "react";
import { api, NotAvailable, Project } from "../api";
import { addProject } from "../store";
import { errText } from "../util";

export function StandingApproval({ project, compact = false }: { project: Project; compact?: boolean }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const on = !!project.standing_approval;

  const toggle = async () => {
    setBusy(true); setErr(null);
    try {
      const p = await api.setStandingApproval(project.id, !on);
      addProject(p && typeof p === "object" ? { ...project, ...p } : { ...project, standing_approval: !on });
    } catch (e) {
      setErr(e instanceof NotAvailable ? "Standing approval is not available from this daemon yet" : errText(e));
    } finally { setBusy(false); }
  };

  return (
    <label className={"standing" + (on ? " on" : "")} data-testid="standing-approval"
      title="Merge this Project's pull requests as soon as their checks pass">
      <input type="checkbox" checked={on} disabled={busy} onChange={() => void toggle()}
        aria-label={`Standing approval for ${project.name}`} />
      <span className="switch" aria-hidden />
      <span className="ellipsis">{compact ? project.name : "Merge green PRs automatically"}</span>
      {err && <span className="bad small-text" role="alert">{err}</span>}
    </label>
  );
}

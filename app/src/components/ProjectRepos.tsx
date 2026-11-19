// A project's repositories after it was created: add a GitHub repository, a clone URL or a local
// folder, or take one out. The daemon clones an added one into the project's workspace before it
// answers; a removed one's clone is set aside, never deleted, and removing is refused while work
// in it is on no remote.
import React, { useState } from "react";
import { api, ApiError, NotAvailable, Project, RepoSource } from "../api";
import { addProject } from "../store";
import { errText } from "../util";
import { PickedRepo, RepoPicker } from "./RepoPicker";

const GITHUB_RE = /^(?:git@github\.com:|ssh:\/\/git@github\.com\/|https?:\/\/github\.com\/)([\w.-]+\/[\w.-]+?)(?:\.git)?\/?$/;

/** How a project's repo shows in the picker. */
export function pickedOf(r: RepoSource): PickedRepo {
  const gh = GITHUB_RE.exec(r.url);
  if (gh) return { url: r.url, label: gh[1], kind: "github" };
  if (/^(\/|~\/)/.test(r.url)) return { url: r.url, label: r.url, kind: "folder" };
  return { url: r.url, label: r.url, kind: "url" };
}

export function ProjectRepos({ project }: { project: Project }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const repos = project.repos ?? [];
  const value = repos.map(pickedOf);
  const settingUp = project.status === "provisioning";
  const archived = !!project.archived_at;

  const onChange = async (next: PickedRepo[]) => {
    const added = next.find((n) => !value.some((v) => v.url === n.url));
    const removed = repos.find((r) => !next.some((n) => n.url === r.url));
    setErr(null);
    try {
      let p: Project;
      if (added) {
        setBusy(`Adding ${added.label}…`);
        p = await api.addProjectRepo(project.id, { url: added.url });
      } else if (removed?.name) {
        setBusy(`Removing ${removed.name}…`);
        p = await api.removeProjectRepo(project.id, removed.name);
      } else return;
      addProject(p && typeof p === "object" ? { ...project, ...p } : project);
    } catch (e) {
      setErr(e instanceof NotAvailable ? "Changing a project's repositories is not available from this daemon yet"
        : e instanceof ApiError && e.code === "unlanded_work" ? `Kept: ${e.message}`
          : errText(e));
    } finally { setBusy(null); }
  };

  return (
    <div className="flex flex-col gap-2" data-testid="project-repos">
      <fieldset disabled={!!busy || settingUp || archived} className="m-0 min-w-0 border-0 p-0">
        <RepoPicker value={value} onChange={(v) => void onChange(v)} />
      </fieldset>
      {repos.length === 0 && !busy && <p className="faint">No repositories yet. Adding the first sets up the project's workspace and starts its coordinator.</p>}
      {busy && <p className="dim" role="status" data-testid="project-repos-busy">{busy}</p>}
      {settingUp && <p className="faint">The project is being set up. Its repositories can change once it is ready.</p>}
      {archived && <p className="faint">Unarchive the project to change its repositories.</p>}
      {err && <div className="set-error" role="alert" data-testid="project-repos-error">{err}</div>}
    </div>
  );
}

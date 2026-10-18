// Archiving and deleting a project. Archive stops its coordinator and workers and hides it from the
// left list and home, keeping everything; Unarchive brings it back and its coordinator picks up
// where it left off. Delete asks for the project's name, then removes what Quark keeps for it.
import React, { useState } from "react";
import { api, ApiError, Project } from "../api";
import { go } from "../nav";
import { addProject, removeProject } from "../store";
import { errText } from "../util";
import { Button, Field, TextInput } from "../ui";

function useAction() {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const run = async (f: () => Promise<void>) => {
    setBusy(true); setErr(null);
    try { await f(); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  return { busy, err, run };
}

/** Brings an archived project back. Safe inside a link: it does not navigate. */
export function UnarchiveButton({ project }: { project: Project }) {
  const { busy, err, run } = useAction();
  return (
    <>
      <Button kind="quiet" disabled={busy} data-testid="unarchive-project"
        onClick={(e) => {
          e.preventDefault(); e.stopPropagation();
          void run(async () => addProject(await api.unarchiveProject(project.id)));
        }}>Unarchive</Button>
      {err && <span className="bad small-text" role="alert">{err}</span>}
    </>
  );
}

/** The Settings section: archive or unarchive, and delete with the project's name typed. */
export function ProjectLifecycle({ project }: { project: Project }) {
  const archive = useAction();
  const del = useAction();
  const [confirming, setConfirming] = useState(false);
  const [typed, setTyped] = useState("");
  const archived = !!project.archived_at;
  const matches = typed.trim() === project.name;

  const toggleArchive = () => archive.run(async () => {
    try {
      addProject(await (archived ? api.unarchiveProject(project.id) : api.archiveProject(project.id)));
    } catch (e) {
      // Archived, but an agent did not stop: show it archived and say what is still running.
      if (!archived && e instanceof ApiError && e.code === "engine_failed") addProject({ ...project, archived_at: new Date().toISOString() });
      throw e;
    }
  });
  const remove = () => del.run(async () => {
    try {
      await api.deleteProject(project.id, typed.trim());
    } catch (e) {
      // A refused delete still archived the project first.
      if (e instanceof ApiError && (e.code === "unlanded_work" || e.code === "engine_failed")) {
        addProject({ ...project, archived_at: project.archived_at ?? new Date().toISOString() });
      }
      throw e;
    }
    go({ name: "projects" });
    removeProject(project.id);
  });

  return (
    <div className="flex flex-col gap-3" data-testid="project-lifecycle">
      <div className="set-row">
        <Button disabled={archive.busy} onClick={() => void toggleArchive()} data-testid={archived ? "unarchive-project" : "archive-project"}>
          {archived ? "Unarchive" : "Archive"}
        </Button>
        <span className="dim">{archived
          ? "Archived. Its coordinator and workers are stopped and it is hidden; nothing was removed."
          : "Stops its coordinator and workers and hides it from the left list and All projects. Everything is kept."}</span>
      </div>
      {archive.err && <div className="set-error" role="alert">{archive.err}</div>}

      {!confirming ? (
        <div className="set-row">
          <Button kind="danger" onClick={() => setConfirming(true)} data-testid="delete-project">Delete…</Button>
          <span className="dim">Removes the project, its history, workspace and issues from Quark. Your repositories are not touched.</span>
        </div>
      ) : (
        <form className="flex flex-col gap-3" onSubmit={(e) => { e.preventDefault(); if (matches) void remove(); }}>
          <Field label={<>Type <span className="mono">{project.name}</span> to delete it</>}
            hint="Its workers are stopped first. A worker whose changes are not pushed keeps the project from being deleted, so no work is lost."
            error={del.err}>
            <TextInput autoFocus value={typed} onChange={(e) => setTyped(e.target.value)} aria-label="Project name"
              data-testid="delete-confirm-name" />
          </Field>
          <div className="set-row">
            <Button kind="danger" type="submit" disabled={!matches || del.busy} data-testid="delete-confirm">Delete project</Button>
            <Button kind="quiet" onClick={() => { setConfirming(false); setTyped(""); }}>Cancel</Button>
          </div>
        </form>
      )}
    </div>
  );
}

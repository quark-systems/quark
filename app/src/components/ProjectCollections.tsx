// Which collections a project is in, on its Settings page. A project may be in several, or
// none; a collection only decides how the left list groups it.
import React, { useState } from "react";
import { api, Collection, NotAvailable, Project } from "../api";
import { setCollections, useStore } from "../store";
import { errText } from "../util";
import { Button, TextInput, Toggle } from "../ui";

export function ProjectCollections({ project }: { project: Project }) {
  const collections = useStore((s) => s.collections);
  const available = useStore((s) => s.collectionsAvailable);
  const [busy, setBusy] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [err, setErr] = useState<string | null>(null);

  const run = async (key: string, work: () => Promise<unknown>) => {
    setBusy(key); setErr(null);
    try {
      await work();
      setCollections(await api.collections());
    } catch (e) {
      setErr(e instanceof NotAvailable ? "Collections are not available from this daemon yet" : errText(e));
    } finally { setBusy(null); }
  };

  const toggle = (c: Collection, on: boolean) => run(c.id, () => api.updateCollection(c.id, {
    projects: on ? [...c.projects, project.id] : c.projects.filter((id) => id !== project.id),
  }));

  const create = () => {
    const trimmed = name.trim();
    if (!trimmed) return;
    return run("new", async () => {
      const made = await api.createCollection(trimmed);
      await api.updateCollection(made.id, { projects: [project.id] });
      setName("");
    });
  };

  if (available === false) return <p className="faint">This daemon does not serve collections yet.</p>;
  return (
    <div data-testid="project-collections">
      {collections.map((c) => (
        <div className="set-row" key={c.id}>
          <Toggle checked={c.projects.includes(project.id)} disabled={busy !== null}
            onCheckedChange={(on) => void toggle(c, on)}
            label={`Put ${project.name} in ${c.name}`} testid={`collection-${c.id}`} />
          <span className="ellipsis">{c.name}</span>
        </div>
      ))}
      <div className="set-row">
        <TextInput value={name} disabled={busy !== null} placeholder="New collection" style={{ minWidth: 200 }}
          aria-label="Name of a new collection" data-testid="new-collection"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") void create(); }} />
        {name.trim() !== "" && <Button disabled={busy !== null} onClick={() => void create()} data-testid="new-collection-add">Add</Button>}
      </div>
      {err && <div className="set-error" role="alert">{err}</div>}
    </div>
  );
}

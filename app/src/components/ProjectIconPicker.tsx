// Choosing the glyph and colour a project is shown with. Saved as you pick, so the left list
// changes under you and there is nothing to confirm.
import React, { useMemo, useState } from "react";
import { api, NotAvailable, Project } from "../api";
import { addProject } from "../store";
import { errText } from "../util";
import { Button, TextInput } from "../ui";
import { DEFAULT_COLOR, DEFAULT_ICON, ICON_COLORS, PROJECT_ICONS, ProjectIconMark, colorNamed } from "../ui/projectIcons";

const STALE = "Project icons need a newer daemon than the one running — rebuild and restart quarkd";

/** Whether the saved project came back with what was asked for. An empty string asks for the default. */
function kept(saved: Project, patch: { icon?: string; icon_color?: string }): boolean {
  const asked = (want: string | undefined, got: string | null | undefined) =>
    want === undefined || (want === "" ? !got : got === want);
  return asked(patch.icon, saved.icon) && asked(patch.icon_color, saved.icon_color);
}

export function ProjectIconPicker({ project }: { project: Project }) {
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const icon = project.icon ?? DEFAULT_ICON;
  const color = project.icon_color ?? DEFAULT_COLOR;

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return PROJECT_ICONS;
    return PROJECT_ICONS.filter((i) => i.name.includes(q) || i.words.includes(q));
  }, [query]);

  const save = async (patch: { icon?: string; icon_color?: string }) => {
    setBusy(true); setErr(null);
    try {
      const saved = await api.setProjectIcon(project.id, patch);
      // A daemon from before icons drops the field rather than refusing it, so the pick would
      // quietly do nothing. Read it back instead of trusting the call to have landed.
      if (!kept(saved, patch)) { setErr(STALE); return; }
      addProject(saved);
    } catch (e) {
      setErr(e instanceof NotAvailable ? STALE : errText(e));
    } finally { setBusy(false); }
  };

  return (
    <div className="set-stack" data-testid="project-icon">
      <div className="set-row">
        <span className="icon-preview"><ProjectIconMark icon={icon} color={color} size={22} /></span>
        <span className="ellipsis">{project.name}</span>
        {(project.icon || project.icon_color) && (
          <Button kind="quiet" disabled={busy} onClick={() => void save({ icon: "", icon_color: "" })}
            data-testid="project-icon-reset">Reset</Button>
        )}
      </div>

      <div className="icon-colors" role="radiogroup" aria-label="Icon colour">
        {ICON_COLORS.map((c) => (
          <button key={c.name} type="button" role="radio" aria-checked={c.name === color} aria-label={c.name}
            title={c.name} disabled={busy} data-testid={`icon-color-${c.name}`}
            className={"icon-color" + (c.name === color ? " on" : "")}
            style={{ background: colorNamed(c.name) }}
            onClick={() => void save({ icon_color: c.name })} />
        ))}
      </div>

      <TextInput value={query} placeholder="Search icons…" aria-label="Search icons"
        data-testid="icon-search" onChange={(e) => setQuery(e.target.value)} />

      {shown.length === 0 ? <p className="faint icon-empty">No icon matches “{query.trim()}”.</p> : (
      <div className="icon-grid" role="radiogroup" aria-label="Icon">
        {shown.map((i) => (
          <button key={i.name} type="button" role="radio" aria-checked={i.name === icon} aria-label={i.name}
            title={i.name} disabled={busy} data-testid={`icon-${i.name}`}
            className={"icon-choice" + (i.name === icon ? " on" : "")}
            onClick={() => void save({ icon: i.name })}>
            <ProjectIconMark icon={i.name} color={color} size={18} />
          </button>
        ))}
      </div>
      )}
      {err && <div className="set-error" role="alert">{err}</div>}
    </div>
  );
}

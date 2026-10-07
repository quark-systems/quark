// Pick the persona a Project reads with: its own pack, or the global default.
import { useEffect, useState } from "react";
import { api, NotAvailable, PersonaList } from "../api";
import { errText } from "../util";
import "./PersonaPicker.css";
import { personaChanged, useProjectPersona } from "../persona";

const DEFAULT = "";

export function PersonaPicker({ projectId }: { projectId: string }) {
  const current = useProjectPersona(projectId);
  const [list, setList] = useState<PersonaList | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    api.personas().then(setList).catch((e) => { if (!(e instanceof NotAvailable)) setErr(errText(e)); });
  }, []);

  // Older daemons have no personas: show nothing.
  if (!list || !current) return err ? <span className="bad small-text">{err}</span> : null;

  const defaultName = list.packs.find((p) => p.id === list.default)?.name ?? list.default;
  const pick = async (value: string) => {
    setBusy(true); setErr(null);
    try { personaChanged(await api.setProjectPersona(projectId, value === DEFAULT ? null : value)); }
    catch (e) { setErr(errText(e)); }
    finally { setBusy(false); }
  };

  return (
    <label className="persona-picker small-text" title={current.fallback ?? "Role names and labels this Project uses"}>
      <span className="faint">Persona</span>{" "}
      <select value={current.project_override ?? DEFAULT} disabled={busy} data-testid="persona-picker"
        onChange={(e) => void pick(e.target.value)} aria-label="Persona">
        <option value={DEFAULT}>Default ({defaultName})</option>
        {list.packs.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
      </select>
      {err && <span className="bad"> {err}</span>}
    </label>
  );
}

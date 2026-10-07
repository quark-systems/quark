// Persona label lookup: what a Project calls its roles and a few app labels.
// Every caller passes the neutral text, so with no pack (or an older daemon)
// the app reads exactly as before.
import { useEffect, useSyncExternalStore } from "react";
import { api, NotAvailable, Persona, PersonaRole, ProjectPersona } from "./api";

export interface Labels {
  /** A role's label, lowercase as written in the pack: "first mate". */
  role(r: PersonaRole): string;
  /** A role's label with a capital first letter, for headings and buttons. */
  Role(r: PersonaRole): string;
  /** An app label by neutral id, else `fallback`. */
  ui(id: string, fallback: string): string;
}

const NEUTRAL: Record<PersonaRole, string> = {
  user: "user", coordinator: "coordinator", worker: "worker",
  sub_coordinator: "sub-coordinator", investigation: "investigation", decision: "decision",
};

export const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

export function labels(p: Persona | null | undefined): Labels {
  const role = (r: PersonaRole) => p?.roles[r] || NEUTRAL[r];
  return {
    role,
    Role: (r) => capitalize(role(r)),
    ui: (id, fallback) => p?.ui_labels[id] || fallback,
  };
}

// One cached answer per Project, shared by every view of it.
const cache = new Map<string, ProjectPersona | null>();
const loading = new Set<string>();
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());

function load(projectId: string) {
  if (cache.has(projectId) || loading.has(projectId)) return;
  loading.add(projectId);
  api.projectPersona(projectId)
    .then((p) => cache.set(projectId, p))
    // A daemon without personas, or a failed read, means neutral labels.
    .catch((e) => { if (!(e instanceof NotAvailable)) console.warn("persona", e); cache.set(projectId, null); })
    .finally(() => { loading.delete(projectId); notify(); });
}

/** Records a Project's new pack, as returned by the daemon, for every view. */
export function personaChanged(p: ProjectPersona) {
  cache.set(p.project_id, p);
  notify();
}

/** The Project's persona, or null until loaded and when the daemon has none. */
export function useProjectPersona(projectId: string | null | undefined): ProjectPersona | null {
  useEffect(() => { if (projectId) load(projectId); }, [projectId]);
  return useSyncExternalStore(
    (l) => { listeners.add(l); return () => listeners.delete(l); },
    () => (projectId ? cache.get(projectId) ?? null : null),
  );
}

/** Label lookup for a Project's views. */
export function useLabels(projectId: string | null | undefined): Labels {
  return labels(useProjectPersona(projectId)?.persona);
}

// Persona packs: role names, form of address and app labels per Project.

import { enc, req } from "./client";

/** Neutral roles. The API and events only ever use these. */
export type PersonaRole = "user" | "coordinator" | "worker" | "sub_coordinator" | "investigation" | "decision";

export interface Persona {
  id: string; name: string; builtin: boolean;
  address?: string | null; voice: string; vocabulary: string[];
  /** Label per neutral role; a missing role keeps its neutral name. */
  roles: Partial<Record<PersonaRole, string>>;
  /** App label per neutral label id; a missing id keeps the app's text. */
  ui_labels: Record<string, string>;
}
export interface PersonaList { default: string; packs: Persona[]; errors: string[] }
export interface ProjectPersona {
  project_id: string; persona: Persona;
  project_override?: string | null; default: string; fallback?: string | null;
}

export const personasApi = {
  personas: () => req<PersonaList>("GET", "/v1/personas"),
  setDefaultPersona: (persona: string) => req<PersonaList>("PUT", "/v1/personas/default", { persona }),
  projectPersona: (projectId: string) => req<ProjectPersona>("GET", `/v1/projects/${enc(projectId)}/persona`),
  /** `null` clears the Project's own choice so it follows the default. */
  setProjectPersona: (projectId: string, persona: string | null) =>
    req<ProjectPersona>("PUT", `/v1/projects/${enc(projectId)}/persona`, { persona }),
};

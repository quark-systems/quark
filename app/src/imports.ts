// Importing projects: what a preview or an import report means for the screen.
import type { FirstmateImportReport, ImportInventoryItem, ImportedProject } from "./api";

/** Backlog items no project could be chosen for, grouped by the id prefix a route would assign. */
export interface UnroutedGroup { prefix: string; ids: string[]; reasons: string[] }

export function unroutedGroups(unrouted: string[]): UnroutedGroup[] {
  const groups = new Map<string, UnroutedGroup>();
  for (const line of unrouted) {
    const at = line.indexOf(": ");
    const id = at < 0 ? line : line.slice(0, at);
    const reason = at < 0 ? "" : line.slice(at + 2);
    const word = id.split("-")[0];
    const prefix = id.includes("-") ? `${word}-` : id;
    const g = groups.get(prefix) ?? { prefix, ids: [], reasons: [] };
    g.ids.push(id);
    if (reason && !g.reasons.includes(reason)) g.reasons.push(reason);
    groups.set(prefix, g);
  }
  return [...groups.values()].sort((a, b) => b.ids.length - a.ids.length || a.prefix.localeCompare(b.prefix));
}

/** What an import did to one project, in words for the screen. */
export function projectSummary(p: ImportedProject, dryRun: boolean): string {
  if (p.outcome === "pending") return "Waiting";
  if (p.outcome === "not_imported") return "Not imported";
  if (dryRun) return p.project === "created" ? "New project" : "Already in Quark";
  return p.project === "created" ? "Created" : "Updated";
}

/** Projects an import left unfinished; importing again completes them. */
export const pendingProjects = (r: FirstmateImportReport) => r.projects.filter((p) => p.outcome === "pending");

/** Inventory rows for what was left behind, with the reason. */
export const leftBehind = (r: FirstmateImportReport): ImportInventoryItem[] =>
  r.inventory.filter((i) => i.outcome === "not_imported");

/** Totals across projects for the preview's headline. */
export function totals(r: FirstmateImportReport) {
  const sum = (f: (p: ImportedProject) => number) => r.projects.reduce((n, p) => n + f(p), 0);
  return {
    projects: r.projects.length,
    created: r.projects.filter((p) => p.project === "created").length,
    issues: sum((p) => p.tasks.imported),
    decisions: sum((p) => p.holds.imported),
  };
}

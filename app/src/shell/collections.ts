// How the left list groups projects: under the collections they are in, or flat.
//
// A project may be in several collections, so it appears under each of them. Projects in no
// collection fall into a trailing "Ungrouped" section. Whether the list is grouped at all is a
// preference the daemon keeps (see ./grouping).
import type { Collection } from "../api";
import type { ProjectGroup } from "./groups";

/** A heading in the left list with the projects under it. `id` is null for the trailing
 *  "Ungrouped" section, which has no collection behind it. */
export interface CollectionSection {
  id: string | null;
  name: string;
  /** The projects shown under the heading. */
  groups: ProjectGroup[];
  /** Whether it is folded shut, showing its heading alone. */
  collapsed: boolean;
  /** The collection's whole membership, including projects the list is not showing. Reordering
   *  is applied to this, so an archived or hidden member is never dropped from the collection. */
  members: string[];
}

/** The name of the section holding projects that are in no collection. */
export const UNGROUPED = "Ungrouped";

/** The left list's sections: every collection in its order, each holding its projects in the
 *  order the collection lists them, then Ungrouped for whatever is in no collection. A project
 *  in several collections appears under each of them. An empty collection keeps its heading, so
 *  a new one is visible before anything is put in it. */
export function collectionSections(collections: Collection[], groups: ProjectGroup[]): CollectionSection[] {
  const byId = new Map(groups.map((g) => [g.project.id, g]));
  const placed = new Set<string>();
  const sections: CollectionSection[] = collections.map((c) => {
    const inside: ProjectGroup[] = [];
    for (const id of c.projects) {
      const g = byId.get(id);
      // A project the left list is not showing (archived, or hidden and not revealed) is
      // skipped here rather than leaving a gap under the heading.
      if (!g) continue;
      inside.push(g);
      placed.add(id);
    }
    return { id: c.id, name: c.name, groups: inside, members: c.projects, collapsed: c.collapsed };
  });
  const rest = groups.filter((g) => !placed.has(g.project.id));
  if (rest.length > 0) sections.push({ id: null, name: UNGROUPED, groups: rest, members: [], collapsed: false });
  return sections;
}

/** `ids` with `id` moved `delta` places, for Move up and Move down. Out-of-range moves and an
 *  id that is not there leave the order alone. */
export function moved(ids: string[], id: string, delta: number): string[] {
  const from = ids.indexOf(id);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= ids.length) return ids;
  const next = ids.slice();
  next.splice(to, 0, ...next.splice(from, 1));
  return next;
}

/** `ids` with `id` taken out and put back before `before` (or at the end with null), for a drop. */
export function dropped(ids: string[], id: string, before: string | null): string[] {
  const without = ids.filter((x) => x !== id);
  const at = before === null ? without.length : without.indexOf(before);
  if (at < 0) return ids;
  without.splice(at, 0, id);
  return without;
}

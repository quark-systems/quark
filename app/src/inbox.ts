// Ordering and selection for the decisions inbox, kept pure so it is easy to test.
// The Memory screen moves through its lists with the same `step` and `nextAfterAnswer`.
import type { Decision, DecisionState } from "./api";

/** Open decisions oldest first, since they hold up work longest; answered ones newest first. */
export function inboxList(all: Decision[], filter: DecisionState): Decision[] {
  // A decision someone has acted on is still an answered one here.
  const list = all.filter((d) => (d.state === "open") === (filter === "open"));
  return filter === "open"
    ? list.sort((a, b) => a.opened_at.localeCompare(b.opened_at) || a.id.localeCompare(b.id))
    : list.sort((a, b) => (b.answered_at ?? b.opened_at).localeCompare(a.answered_at ?? a.opened_at) || a.id.localeCompare(b.id));
}

/** The id `delta` rows from `id`, clamped to the list; the first row when `id` is not in it. */
export function step(list: { id: string }[], id: string | undefined, delta: number): string | undefined {
  if (!list.length) return undefined;
  const i = list.findIndex((d) => d.id === id);
  if (i < 0) return list[0].id;
  return list[Math.min(list.length - 1, Math.max(0, i + delta))].id;
}

/** After answering `id`, the open decision to show next: the one after it, else the one before. */
export function nextAfterAnswer(openBefore: { id: string }[], id: string): string | undefined {
  const i = openBefore.findIndex((d) => d.id === id);
  const rest = openBefore.filter((d) => d.id !== id);
  if (!rest.length) return undefined;
  return rest[Math.min(Math.max(i, 0), rest.length - 1)].id;
}

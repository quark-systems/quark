// Each Project's issue titles by id, so any screen can name an issue it only has the id of.
// Loaded once per Project when something asks, and again after `beads.changed` settles.
import { useEffect, useSyncExternalStore } from "react";
import { api } from "./api";
import type { BeadNames } from "./beadRefs";
import { loadBeads, useStore } from "./store";

const names = new Map<string, BeadNames>();
/** The `beads.changed` count each Project's titles were last fetched at. */
const fetchedAt = new Map<string, number>();
/** Projects whose Beads status was asked for here, so it is asked once. */
const asked = new Set<string>();
const listeners = new Set<() => void>();
const EMPTY: BeadNames = {};

function subscribe(l: () => void) { listeners.add(l); return () => { listeners.delete(l); }; }

function fetchNames(pid: string, activity: number) {
  if (fetchedAt.get(pid) === activity) return;
  fetchedAt.set(pid, activity);
  api.issues(pid, "all").then((list) => {
    names.set(pid, Object.fromEntries(list.map((i) => [i.id, { id: i.id, title: i.title, status: i.status, issue_type: i.issue_type }])));
    for (const l of listeners) l();
  }).catch(() => { fetchedAt.delete(pid); });
}

/** Records titles a screen already fetched (the Issues tab), so others need not fetch them again. */
export function rememberBeadNames(pid: string, list: { id: string; title: string; status?: string; issue_type?: string }[]) {
  names.set(pid, Object.fromEntries(list.map((i) => [i.id, { id: i.id, title: i.title, status: i.status, issue_type: i.issue_type }])));
  for (const l of listeners) l();
}

/** The Project's issues by id; empty until loaded, or when the Project has no Beads. */
export function useBeadNames(pid: string | null | undefined): BeadNames {
  const connected = useStore((s) => s.connected);
  const known = useStore((s) => !!pid && !!s.beads[pid]);
  const ready = useStore((s) => !!pid && s.beads[pid]?.state === "ready");
  const activity = useStore((s) => (pid ? s.beadsActivity[pid] ?? 0 : 0));
  useEffect(() => {
    if (!pid || !connected || known || asked.has(pid)) return;
    asked.add(pid);
    loadBeads(pid).catch(() => { asked.delete(pid); });
  }, [pid, connected, known]);
  useEffect(() => {
    if (!pid || !ready || !connected) return;
    // `beads.changed` comes in bursts (a sync touches many issues), so fetch once it settles.
    const t = setTimeout(() => fetchNames(pid, activity), fetchedAt.has(pid) ? 250 : 0);
    return () => clearTimeout(t);
  }, [pid, ready, connected, activity]);
  return useSyncExternalStore(subscribe, () => (pid ? names.get(pid) : undefined) ?? EMPTY);
}

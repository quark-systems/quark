// The Agents tab's project panel, per project: which fixed tab was last open (Work, Needs you,
// PRs, Activity), which workers are open as tabs beside them, and whether an open worker is
// expanded to the full width. Remembered in this browser only; a worker in the URL is always open.
import { useSyncExternalStore } from "react";

export type PanelTab = "work" | "needs" | "prs" | "activity";
export const PANEL_TABS: PanelTab[] = ["work", "needs", "prs", "activity"];

export type PanelState = { tab: PanelTab; workers: string[]; expanded: boolean };

const KEY = "quark.projectPanel";
const MAX_OPEN = 8;
const EMPTY: PanelState = { tab: "work", workers: [], expanded: false };

type All = Record<string, PanelState>;

function read(): All {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return v && typeof v === "object" ? v : {};
  } catch { return {}; }
}

let all: All = typeof localStorage === "undefined" ? {} : read();
const ls = new Set<() => void>();

function write(pid: string, next: PanelState) {
  all = { ...all, [pid]: next };
  try { localStorage.setItem(KEY, JSON.stringify(all)); } catch { /* not remembered */ }
  ls.forEach((l) => l());
}

export function panelState(pid: string): PanelState {
  const s = all[pid];
  if (!s) return EMPTY;
  return { tab: PANEL_TABS.includes(s.tab) ? s.tab : "work", workers: Array.isArray(s.workers) ? s.workers : [], expanded: !!s.expanded };
}

export function usePanel(pid: string): PanelState {
  return useSyncExternalStore((l) => { ls.add(l); return () => ls.delete(l); }, () => all[pid] ?? EMPTY);
}

/** Open `id` as a tab: newest last, the oldest dropped past the limit. */
export function withWorker(workers: string[], id: string): string[] {
  if (workers.includes(id)) return workers;
  const next = [...workers, id];
  return next.length > MAX_OPEN ? next.slice(next.length - MAX_OPEN) : next;
}

/** The tab to show after closing `id` while it was showing: its right neighbour, else its left, else none. */
export function afterClose(workers: string[], id: string): string | null {
  const i = workers.indexOf(id);
  if (i < 0) return null;
  return workers[i + 1] ?? workers[i - 1] ?? null;
}

export function openWorker(pid: string, id: string) {
  const s = panelState(pid);
  const workers = withWorker(s.workers, id);
  if (workers !== s.workers) write(pid, { ...s, workers });
}

export function closeWorker(pid: string, id: string) {
  const s = panelState(pid);
  write(pid, { ...s, workers: s.workers.filter((w) => w !== id) });
}

export function setPanelTab(pid: string, tab: PanelTab) {
  write(pid, { ...panelState(pid), tab });
}

export function setExpanded(pid: string, expanded: boolean) {
  write(pid, { ...panelState(pid), expanded });
}

// How much a worker has changed so far (+N −M over its files), for the board's worker cards.
// Read from the task's changes endpoint, cached per task and refetched only when the task updates.
import { useEffect, useState } from "react";
import { api, Task, TaskChanges, TaskState } from "../../api";

export interface DiffStat { files: number; adds: number; dels: number }

/** States in which a worker has a working copy worth counting. */
const WITH_WORKTREE = new Set<TaskState>(["running", "needs_decision", "blocked", "paused", "in_review"]);

export function statOf(c: TaskChanges): DiffStat {
  let adds = 0, dels = 0;
  for (const f of c.files) { adds += f.additions ?? 0; dels += f.deletions ?? 0; }
  return { files: c.files.length, adds, dels };
}

const cache = new Map<string, { key: string; stat: Promise<DiffStat | null> }>();

function load(task: Task): Promise<DiffStat | null> {
  const key = task.state + ":" + task.updated_at;
  const hit = cache.get(task.id);
  if (hit && hit.key === key) return hit.stat;
  // Any failure (no working copy yet, an older daemon) just leaves the card without counts.
  const stat = api.changes(task.id).then(statOf, () => null);
  cache.set(task.id, { key, stat });
  return stat;
}

export function useDiffStat(task: Task): DiffStat | null {
  const wanted = WITH_WORKTREE.has(task.state);
  const [stat, setStat] = useState<DiffStat | null>(null);
  useEffect(() => {
    if (!wanted) { setStat(null); return; }
    let live = true;
    load(task).then((s) => { if (live) setStat(s); });
    return () => { live = false; };
  }, [task.id, task.state, task.updated_at, wanted]);
  return stat;
}

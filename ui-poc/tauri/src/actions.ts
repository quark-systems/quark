import { api } from "./api";
import { getState } from "./store";
import { getNav, setNav } from "./nav";

export const STRESS_PROMPT = "Stress test: summarize every worker's progress in detail, with code samples.";

/** Turn the stub's stress mode on/off for every pane, and kick off a streaming coordinator reply. */
export async function setStress(on: boolean, workerIds?: string[]) {
  setNav({ stress: on });
  const s = getState();
  const ids = workerIds ?? s.workers.map((w) => w.id);
  await Promise.all(ids.map((id) => api.stress(id, on).catch(() => {})));
  if (on) {
    const pid = getNav().project ?? s.projects[0]?.id;
    if (pid) api.send(pid, STRESS_PROMPT).catch(() => {});
  }
}

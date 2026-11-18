// Whether the left list groups projects under their collections. The daemon keeps this, so
// every client it serves — this app, a browser, another machine once quarkd is reached over a
// network — opens the same way.
import { api } from "../api";
import { setPreferences, useStore } from "../store";

/** Whether the left list is grouped right now. */
export function useGroupByCollection(): boolean {
  return useStore((s) => s.preferences.group_by_collection);
}

/** Groups the left list by collection, or shows every project flat, and saves it. */
export async function setGroupByCollection(on: boolean) {
  // Shown at once, then replaced by what the daemon answers.
  setPreferences({ group_by_collection: on });
  try {
    setPreferences(await api.updatePreferences({ group_by_collection: on }));
  } catch {
    // An older daemon keeps no preferences; the choice holds for this run.
  }
}

// Whether hidden projects are shown right now. A project is hidden when its Settings turn
// "Hide this project" on; the app menu's "Show hidden projects" reveals them for this run
// only, so Quark reopens with them hidden again.
import { useSyncExternalStore } from "react";

const KEY = "quark.show-hidden";
const EVENT = "quark:show-hidden";

/** Whether hidden projects are revealed right now. */
export function showHidden(): boolean {
  try { return sessionStorage.getItem(KEY) === "1"; } catch { return false; }
}

/** Reveals hidden projects, or hides them again, until Quark is reopened. */
export function setShowHidden(on: boolean) {
  try {
    if (on) sessionStorage.setItem(KEY, "1");
    else sessionStorage.removeItem(KEY);
  } catch { /* private mode */ }
  window.dispatchEvent(new CustomEvent<boolean>(EVENT, { detail: on }));
}

function onChange(cb: () => void) {
  window.addEventListener(EVENT, cb);
  return () => window.removeEventListener(EVENT, cb);
}

/** True while hidden projects are revealed, re-rendering when that changes. */
export function useShowHidden(): boolean {
  return useSyncExternalStore(onChange, showHidden, () => false);
}

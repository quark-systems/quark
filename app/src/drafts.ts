// Text typed into a message box, kept in local storage so it survives leaving the view and quitting the app.
import { useCallback, useState } from "react";

const PREFIX = "quark.draft.";

export function readDraft(key: string): string {
  try { return localStorage.getItem(PREFIX + key) ?? ""; } catch { return ""; }
}

/** An empty draft is removed rather than stored. */
export function writeDraft(key: string, text: string) {
  try {
    if (text) localStorage.setItem(PREFIX + key, text);
    else localStorage.removeItem(PREFIX + key);
  } catch { /* storage unavailable */ }
}

/** Like useState(""), but the text is the saved draft for `key` (one per box: project, worker, decision…). */
export function useDraft(key: string): [string, (text: string) => void] {
  const [state, setState] = useState(() => ({ key, text: readDraft(key) }));
  // The same box can be shown for another key (another project's chat) without remounting.
  let current = state;
  if (state.key !== key) {
    current = { key, text: readDraft(key) };
    setState(current);
  }
  const set = useCallback((text: string) => {
    writeDraft(key, text);
    // A send that finishes after switching boxes clears its own draft, not the one now shown.
    setState((s) => (s.key === key ? { key, text } : s));
  }, [key]);
  return [current.text, set];
}

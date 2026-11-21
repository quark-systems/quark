// Files a person marked viewed in a pull request, as on GitHub: kept in local storage per pull
// request with a fingerprint of each file's change, so a file a later commit changes again shows
// as unviewed and changed since it was viewed.
import { useCallback, useMemo, useState } from "react";
import type { DiffFile } from "./model";
import { changeSignature } from "./fileTree";

const PREFIX = "quark.viewed.";
type Marks = Record<string, string>; // path -> change signature when marked

function read(key: string): Marks {
  try {
    const v = JSON.parse(localStorage.getItem(PREFIX + key) ?? "{}");
    return v && typeof v === "object" && !Array.isArray(v) ? v : {};
  } catch { return {}; }
}
function write(key: string, marks: Marks) {
  try {
    if (Object.keys(marks).length) localStorage.setItem(PREFIX + key, JSON.stringify(marks));
    else localStorage.removeItem(PREFIX + key);
  } catch { /* storage unavailable */ }
}

export interface ViewedFiles {
  /** Marked viewed, and unchanged since. */
  isViewed: (path: string) => boolean;
  /** Marked viewed once, then changed by a later commit. */
  changedSince: (path: string) => boolean;
  set: (f: DiffFile, viewed: boolean) => void;
  /** How many of the current files are viewed. */
  count: number;
}

export function useViewedFiles(key: string, files: DiffFile[] | null): ViewedFiles {
  const [state, setState] = useState(() => ({ key, marks: read(key) }));
  let marks = state.marks;
  if (state.key !== key) { marks = read(key); setState({ key, marks }); }
  const sigs = useMemo(() => new Map((files ?? []).map((f) => [f.path, changeSignature(f)])), [files]);
  const set = useCallback((f: DiffFile, viewed: boolean) => {
    setState((s) => {
      const next = { ...s.marks };
      if (viewed) next[f.path] = changeSignature(f); else delete next[f.path];
      write(s.key, next);
      return { key: s.key, marks: next };
    });
  }, []);
  return useMemo(() => {
    const isViewed = (p: string) => marks[p] != null && marks[p] === sigs.get(p);
    return {
      isViewed,
      changedSince: (p: string) => marks[p] != null && sigs.has(p) && marks[p] !== sigs.get(p),
      set,
      count: [...sigs.keys()].filter(isViewed).length,
    };
  }, [marks, sigs, set]);
}

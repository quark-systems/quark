import { useSyncExternalStore } from "react";

export type Screen = "board" | "terminals" | "chat" | "diff" | "inbox";
export const SCREENS: { id: Screen; label: string; glyph: string }[] = [
  { id: "board", label: "Task board", glyph: "▦" },
  { id: "terminals", label: "Terminals", glyph: "▚" },
  { id: "chat", label: "Coordinator", glyph: "◆" },
  { id: "diff", label: "Diff review", glyph: "±" },
  { id: "inbox", label: "Decisions & PRs", glyph: "◉" },
];

export interface Nav {
  screen: Screen;
  project: string | null; // null = all projects
  pr: string | null;
  palette: boolean;
  perf: boolean;
  stress: boolean;
}
const qp = new URLSearchParams(location.search);
let nav: Nav = {
  screen: (qp.get("screen") as Screen) || "board",
  project: qp.get("project"),
  pr: qp.get("pr"),
  palette: false,
  perf: qp.has("perf") || qp.has("bench"),
  stress: false,
};
const ls = new Set<() => void>();
export function setNav(p: Partial<Nav>) { nav = { ...nav, ...p }; ls.forEach((l) => l()); }
export function getNav() { return nav; }
export function useNav(): Nav {
  return useSyncExternalStore((l) => { ls.add(l); return () => ls.delete(l); }, () => nav);
}

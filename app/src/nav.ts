// Hash routes (`#/p/<project id>`), so the desktop app needs no server-side routing.
import { useSyncExternalStore } from "react";

// The route table: one line per screen, `name: pattern`. A `:param` segment
// captures a value; a trailing `?` makes it optional. A new screen adds a line
// here and one in `routes.tsx`.
export const ROUTES = {
  projects: "", // the Projects list
  new: "new", // create a Project
  project: "p/:id", // board and coordinator chat
  memory: "p/:project/memory/:id?", // the Project's memory, optionally with one proposal or entry selected
  dispatch: "p/:project/dispatch", // the Project's dispatch rules: edit, save, test
  overview: "p/:project/overview", // the Project dashboard's Overview tab: live status and what changed since you last looked
  settings: "p/:project/settings", // the Project dashboard's Settings tab: every per-Project switch
  metrics: "p/:project/metrics", // the Project dashboard's Metrics tab: how the work has gone
  automation: "p/:project/automation", // the Project's inbox, trigger rules and away policy
  task: "t/:id", // worker view
  inbox: "inbox/:id?", // decisions inbox, optionally with one decision selected
  prs: "prs", // PR center
  pr: "pr/:id", // one pull request: checks, reviews, diff
  accounts: "accounts", // harness accounts, pools and quota
  hosts: "hosts", // every host: health, telemetry, what runs there, worktree pools
} as const;

type Param<S> = S extends `:${infer P}?` ? { [K in P]?: string } : S extends `:${infer P}` ? { [K in P]: string } : unknown;
type Params<S> = S extends `${infer A}/${infer B}` ? Param<A> & Params<B> : Param<S>;
type Flat<T> = { [K in keyof T]: T[K] };
type RouteName = keyof typeof ROUTES;

export type Route = { [N in RouteName]: Flat<{ name: N } & Params<(typeof ROUTES)[N]>> }[RouteName];

const segments = (pattern: string) => pattern.split("/").filter(Boolean);

/** The route `parts` matches, binding its params, or null. Extra trailing parts are ignored. */
function match(name: RouteName, parts: string[]): Route | null {
  const r: Record<string, string> = { name };
  const segs = segments(ROUTES[name]);
  for (let i = 0; i < segs.length; i++) {
    const seg = segs[i], part = parts[i];
    if (seg.startsWith(":")) {
      const optional = seg.endsWith("?");
      if (part === undefined) { if (optional) continue; return null; }
      r[seg.slice(1, optional ? -1 : undefined)] = part;
    } else if (seg !== part) {
      return null;
    }
  }
  return r as Route;
}

export function parseRoute(hash: string): Route {
  const parts = hash.replace(/^#\/?/, "").split("/").filter(Boolean).map(decodeURIComponent);
  // The most specific pattern wins, so `p/x/memory` is memory, not project.
  let best: Route = { name: "projects" }, bestLen = 0;
  for (const name of Object.keys(ROUTES) as RouteName[]) {
    const len = segments(ROUTES[name]).length;
    if (len <= bestLen) continue;
    const r = match(name, parts);
    if (r) { best = r; bestLen = len; }
  }
  return best;
}

export function href(r: Route): string {
  const params = r as Record<string, string | undefined>;
  const parts: string[] = [];
  for (const seg of segments(ROUTES[r.name])) {
    if (!seg.startsWith(":")) { parts.push(seg); continue; }
    const v = params[seg.slice(1).replace(/\?$/, "")];
    if (v === undefined) break;
    parts.push(encodeURIComponent(v));
  }
  return "#/" + parts.join("/");
}

export function go(r: Route) { location.hash = href(r); }

const hasWindow = typeof window !== "undefined";
let current = parseRoute(hasWindow ? location.hash : "");
const ls = new Set<() => void>();
if (hasWindow) window.addEventListener("hashchange", () => { current = parseRoute(location.hash); ls.forEach((l) => l()); });

export function useRoute(): Route {
  return useSyncExternalStore((l) => { ls.add(l); return () => ls.delete(l); }, () => current);
}

// Hash routes, so the desktop app needs no server-side routing:
//   #/                 Projects
//   #/new              create a Project
//   #/p/<project id>   board and coordinator chat
//   #/t/<task id>      worker view
//   #/inbox[/<id>]     decisions inbox, optionally with one decision selected
import { useSyncExternalStore } from "react";

export type Route =
  | { name: "projects" }
  | { name: "new" }
  | { name: "project"; id: string }
  | { name: "task"; id: string }
  | { name: "inbox"; id?: string };

export function parseRoute(hash: string): Route {
  const parts = hash.replace(/^#\/?/, "").split("/").filter(Boolean).map(decodeURIComponent);
  if (parts[0] === "new") return { name: "new" };
  if (parts[0] === "p" && parts[1]) return { name: "project", id: parts[1] };
  if (parts[0] === "t" && parts[1]) return { name: "task", id: parts[1] };
  if (parts[0] === "inbox") return parts[1] ? { name: "inbox", id: parts[1] } : { name: "inbox" };
  return { name: "projects" };
}

export function href(r: Route): string {
  switch (r.name) {
    case "projects": return "#/";
    case "new": return "#/new";
    case "project": return `#/p/${encodeURIComponent(r.id)}`;
    case "task": return `#/t/${encodeURIComponent(r.id)}`;
    case "inbox": return r.id ? `#/inbox/${encodeURIComponent(r.id)}` : "#/inbox";
  }
}

export function go(r: Route) { location.hash = href(r); }

const hasWindow = typeof window !== "undefined";
let current = parseRoute(hasWindow ? location.hash : "");
const ls = new Set<() => void>();
if (hasWindow) window.addEventListener("hashchange", () => { current = parseRoute(location.hash); ls.forEach((l) => l()); });

export function useRoute(): Route {
  return useSyncExternalStore((l) => { ls.add(l); return () => ls.delete(l); }, () => current);
}

// Hash routes, so the desktop app needs no server-side routing:
//   #/                 Projects
//   #/new              create a Project
//   #/p/<project id>   board and coordinator chat
//   #/p/<project id>/memory[/<id>]  the Project's memory, optionally with one proposal or entry selected
//   #/p/<project id>/dispatch  the Project's dispatch rules: edit, save, test
//   #/t/<task id>      worker view
//   #/inbox[/<id>]     decisions inbox, optionally with one decision selected
//   #/prs              PR center
//   #/pr/<pr id>       one pull request: checks, reviews, diff
//   #/accounts         harness accounts, pools and quota
import { useSyncExternalStore } from "react";

export type Route =
  | { name: "projects" }
  | { name: "new" }
  | { name: "project"; id: string }
  | { name: "memory"; project: string; id?: string }
  | { name: "dispatch"; project: string }
  | { name: "task"; id: string }
  | { name: "inbox"; id?: string }
  | { name: "prs" }
  | { name: "pr"; id: string }
  | { name: "accounts" };

export function parseRoute(hash: string): Route {
  const parts = hash.replace(/^#\/?/, "").split("/").filter(Boolean).map(decodeURIComponent);
  if (parts[0] === "new") return { name: "new" };
  if (parts[0] === "p" && parts[1] && parts[2] === "memory") {
    return parts[3] ? { name: "memory", project: parts[1], id: parts[3] } : { name: "memory", project: parts[1] };
  }
  if (parts[0] === "p" && parts[1] && parts[2] === "dispatch") return { name: "dispatch", project: parts[1] };
  if (parts[0] === "p" && parts[1]) return { name: "project", id: parts[1] };
  if (parts[0] === "t" && parts[1]) return { name: "task", id: parts[1] };
  if (parts[0] === "inbox") return parts[1] ? { name: "inbox", id: parts[1] } : { name: "inbox" };
  if (parts[0] === "prs") return { name: "prs" };
  if (parts[0] === "pr" && parts[1]) return { name: "pr", id: parts[1] };
  if (parts[0] === "accounts") return { name: "accounts" };
  return { name: "projects" };
}

export function href(r: Route): string {
  switch (r.name) {
    case "projects": return "#/";
    case "new": return "#/new";
    case "project": return `#/p/${encodeURIComponent(r.id)}`;
    case "memory": return `#/p/${encodeURIComponent(r.project)}/memory` + (r.id ? `/${encodeURIComponent(r.id)}` : "");
    case "dispatch": return `#/p/${encodeURIComponent(r.project)}/dispatch`;
    case "task": return `#/t/${encodeURIComponent(r.id)}`;
    case "inbox": return r.id ? `#/inbox/${encodeURIComponent(r.id)}` : "#/inbox";
    case "prs": return "#/prs";
    case "pr": return `#/pr/${encodeURIComponent(r.id)}`;
    case "accounts": return "#/accounts";
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

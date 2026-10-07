// What each route renders: one line per screen, keyed by its name in `ROUTES`
// (nav.ts). TypeScript requires an entry for every route.
import React from "react";
import { go, useRoute, type Route } from "./nav";
import { Projects } from "./screens/Projects";
import { NewProject } from "./screens/NewProject";
import { ProjectBoard } from "./screens/ProjectBoard";
import { Memory } from "./screens/Memory";
import { Dispatch } from "./screens/Dispatch";
import { WorkerView } from "./screens/WorkerView";
import { Inbox } from "./screens/Inbox";
import { PullRequests } from "./screens/PullRequests";
import { PullRequestView } from "./screens/PullRequestView";
import { Accounts } from "./screens/Accounts";
import { Hosts } from "./screens/Hosts";
import { Settings } from "./screens/dashboard/Settings";
import { Metrics } from "./screens/dashboard/Metrics";
import { Automation } from "./screens/dashboard/Automation";
import { Overview } from "./screens/dashboard/Overview";

type Screens = { [N in Route["name"]]: (r: Extract<Route, { name: N }>) => React.ReactNode };

export const SCREENS: Screens = {
  projects: () => <Projects />,
  new: () => <NewProject onCreated={(id) => go({ name: "project", id })} />,
  project: (r) => <ProjectBoard key={r.id} id={r.id} />,
  memory: (r) => <Memory key={r.project} project={r.project} id={r.id} />,
  dispatch: (r) => <Dispatch key={r.project} project={r.project} />,
  overview: (r) => <Overview key={r.project} project={r.project} />,
  settings: (r) => <Settings key={r.project} project={r.project} />,
  metrics: (r) => <Metrics key={r.project} project={r.project} />,
  automation: (r) => <Automation key={r.project} project={r.project} />,
  task: (r) => <WorkerView key={r.id} id={r.id} />,
  inbox: (r) => <Inbox id={r.id} />,
  prs: () => <PullRequests />,
  pr: (r) => <PullRequestView key={r.id} id={r.id} />,
  accounts: () => <Accounts />,
  hosts: () => <Hosts />,
};

/** Renders the current route's screen. */
export function RouteView() {
  const route = useRoute();
  const render = SCREENS[route.name] as (r: Route) => React.ReactNode;
  return <>{render(route)}</>;
}

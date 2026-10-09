// What each route renders: one line per screen, keyed by its name in `ROUTES`
// (nav.ts). TypeScript requires an entry for every route. Project screens sit under the
// project header (its tabs); Dispatch and Automation are sections of Settings.
import React from "react";
import { go, useRoute, type Route } from "./nav";
import { Projects } from "./screens/Projects";
import { NewProject } from "./screens/NewProject";
import { Conversation, ProjectBoard } from "./screens/ProjectBoard";
import { Memory } from "./screens/Memory";
import { Issues } from "./screens/Issues";
import { Decisions } from "./screens/Decisions";
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
import { Catalogue } from "./screens/Catalogue";
import { ProjectFrame } from "./shell/ProjectHeader";

type Screens = { [N in Route["name"]]: (r: Extract<Route, { name: N }>) => React.ReactNode };

const framed = (r: Route, project: string, node: React.ReactNode) => <ProjectFrame key={project} route={r} project={project}>{node}</ProjectFrame>;

export const SCREENS: Screens = {
  projects: () => <Projects />,
  new: () => <NewProject onCreated={(id) => go({ name: "project", id })} />,
  project: (r) => framed(r, r.id, <Conversation key={r.id} id={r.id} />),
  work: (r) => framed(r, r.project, <ProjectBoard key={r.project} id={r.project} />),
  issues: (r) => framed(r, r.project, <Issues key={r.project} project={r.project} id={r.id} />),
  decisions: (r) => framed(r, r.project, <Decisions key={r.project} project={r.project} id={r.id} />),
  memory: (r) => framed(r, r.project, <Memory key={r.project} project={r.project} id={r.id} />),
  dispatch: (r) => framed(r, r.project, <Dispatch key={r.project} project={r.project} />),
  overview: (r) => framed(r, r.project, <Overview key={r.project} project={r.project} />),
  settings: (r) => framed(r, r.project, <Settings key={r.project} project={r.project} />),
  metrics: (r) => framed(r, r.project, <Metrics key={r.project} project={r.project} />),
  automation: (r) => framed(r, r.project, <Automation key={r.project} project={r.project} />),
  task: (r) => <WorkerView key={r.id} id={r.id} />,
  inbox: (r) => <Inbox id={r.id} />,
  prs: () => <PullRequests />,
  pr: (r) => <PullRequestView key={r.id} id={r.id} />,
  accounts: () => <Accounts />,
  hosts: () => <Hosts />,
  catalogue: () => <Catalogue />,
};

/** Renders the current route's screen. */
export function RouteView() {
  const route = useRoute();
  const render = SCREENS[route.name] as (r: Route) => React.ReactNode;
  return <>{render(route)}</>;
}

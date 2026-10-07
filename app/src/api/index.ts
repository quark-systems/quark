// Typed client for the quarkd v1 API. Shapes follow api/openapi.json and the Phase 1 PRs
// listed in app/CONTRACT.md.
//
// One module per domain; a new domain adds a file, an export line and its
// spread into `api` here.

export * from "./client";
export * from "./projects";
export * from "./tasks";
export * from "./decisions";
export * from "./memory";
export * from "./harnesses";
export * from "./accounts";
export * from "./transcripts";
export * from "./terminals";
export * from "./dispatch";
export * from "./pullRequests";
export * from "./settings";
export * from "./metrics";
export * from "./automation";
export * from "./overview";
export * from "./personas";

import { projectsApi } from "./projects";
import { tasksApi } from "./tasks";
import { decisionsApi } from "./decisions";
import { memoryApi } from "./memory";
import { harnessesApi } from "./harnesses";
import { accountsApi } from "./accounts";
import { transcriptsApi } from "./transcripts";
import { terminalsApi } from "./terminals";
import { dispatchApi } from "./dispatch";
import { pullRequestsApi } from "./pullRequests";
import { settingsApi } from "./settings";
import { metricsApi } from "./metrics";
import { automationApi } from "./automation";
import { overviewApi } from "./overview";
import { personasApi } from "./personas";

export const api = {
  ...projectsApi,
  ...tasksApi,
  ...decisionsApi,
  ...memoryApi,
  ...harnessesApi,
  ...accountsApi,
  ...transcriptsApi,
  ...terminalsApi,
  ...dispatchApi,
  ...pullRequestsApi,
  ...settingsApi,
  ...metricsApi,
  ...automationApi,
  ...overviewApi,
  ...personasApi,
};

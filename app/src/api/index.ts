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
export * from "./hosts";
export * from "./beads";
export * from "./imports";
export * from "./setup";
export * from "./update";

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
import { hostsApi } from "./hosts";
import { beadsApi } from "./beads";
import { importsApi } from "./imports";
import { setupApi } from "./setup";
import { updateApi } from "./update";

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
  ...hostsApi,
  ...beadsApi,
  ...importsApi,
  ...setupApi,
  ...updateApi,
};

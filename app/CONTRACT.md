# What the desktop app uses from quarkd

This is the slice of the daemon API v1 the desktop app calls, and where each part is defined.
`api/openapi.json` is the authority for every shape; when a PR below lands, its endpoints join it.
`src/api.ts` holds the matching TypeScript types, and `mock/daemon.mjs` serves all of it for development and tests.

The app reads `404` with no error body (an unknown route), `405` and `501` as "not available yet" and shows that in the panel, so each screen lights up as its endpoint lands.
A `404` with an `ErrorBody` is a real "not found".

| Area | Endpoints | Defined in |
| --- | --- | --- |
| Projects, board, decisions, events | `GET /v1/health`, `GET/POST /v1/projects`, `GET /v1/projects/{id}`, `GET /v1/projects/{id}/tasks`, `GET /v1/tasks/{id}`, `GET /v1/decisions`, `GET /v1/events?cursor=` | main |
| Steering and control | `POST /v1/tasks/{id}/messages`, `:cancel`, `:relaunch` | quark#5 |
| Harnesses | `GET /v1/harnesses`, `POST /v1/harnesses:validate` | quark#6 |
| Transcripts and coordinator chat | `GET /v1/tasks/{id}/transcript`, `GET/POST /v1/coordinators/{id}/messages` | quark#7 |
| Changes and diff | `GET /v1/tasks/{id}/changes`, `GET /v1/tasks/{id}/diff[?path=]`, `task.event` events | quark#8 |
| Terminals | `GET /v1/terminals/{id}`, `POST /v1/terminals/{id}/snapshot`, `/input`, `/resize` | quark#9 |
| Answering decisions | `POST /v1/decisions/{id}:answer` with `answer` and optional `answered_by`; `Decision.answered_by`, `answered_at` | Phase 2 workstream 1 (decisions answer path) |
| Project creation | `POST /v1/projects` with `repos`, `agent_config`, `dispatch_preset`, `delivery`; `Project.status`; `POST /v1/projects/{id}:provision` | quark#10 |
| Project memory | `GET /v1/projects/{id}/memory/proposals[?state=]`, `POST .../memory/proposals/{proposal_id}:accept` with optional `text` and `decided_by`, `:reject` with optional `decided_by`, `GET /v1/projects/{id}/memory`; `memory.proposed`, `memory.accepted`, `memory.rejected` events | quark#29 |
| Accounts and pools | `GET/POST /v1/accounts`, `GET/PATCH/DELETE /v1/accounts/{id}`, `account.quota_changed` events; `AgentConfig.pool`, `Task.account_id`, `RelaunchTask.pool`, `HarnessInfo.account_env` | quark#23 |
| Why this agent | `GET /v1/tasks/{id}/dispatch`, `dispatch.recorded` events | quark#27 |
| Failover on rate limits | `DispatchRecord.failover`, `Task.failovers` (`AccountFailover`: `from_account_id`, `to_account_id`, `pool`, `outcome`, `signal`, `detail`, `at`), a `failover` entry in `GET /v1/tasks/{id}/events`; a decision the daemon opens itself, answered through `POST /v1/decisions/{id}:answer` | quark#26 |
| Memory review and promotion | `GET /v1/projects/{id}/memory/commits/{commit}`, `POST /v1/projects/{id}/memory/{entry_id}:promote` with optional `promoted_by`, `GET /v1/memory`; `MemoryEntry.commit` on listed entries | quark#30 |
| Testing dispatch rules | `POST /v1/projects/{id}/dispatch:test` with `description` | quark#24 |
| PR center | `GET /v1/pull-requests`, `GET /v1/pull-requests/{id}`, `/diff`, `POST .../{id}/comments`, `POST .../{id}:merge`, `PATCH /v1/projects/{id}` `{standing_approval}`, `pr.updated`, `check.updated`, `review.updated` events; `GET .../{id}/evidence/artifacts/{artifact_id}` | quark#16, quark#20 |

## How the app uses them

- **Create flow.** The harness picker lists harnesses whose `roles` include `coordinator`, since the Project's agent config runs its coordinator.
  The model field is free text, hinted by `models.discovery`, and hidden when `models.selection` is `automatic`.
  Before creating, the app calls `POST /v1/harnesses:validate` with role `coordinator` and shows any errors.
  `owner/name` repos are sent as `https://github.com/owner/name.git`.
  The board shows `status_detail` while the Project is provisioning, and a Retry button (`:provision`) when it failed.
- **Coordinator chat.** The chat shows `user` and `assistant` entries and collapses runs of `thinking`, `tool_call` and `tool_result` into one line.
  A sent message is not echoed, so it shows as pending until the coordinator's session records it as a `user` entry; `confirmed: false` is shown on it.
- **Transcripts.** Entries are keyed by `id`, the `seq` of the event that carried them; live `worker.transcript` and `coordinator.message` events use the event's `seq`.
- **Terminals.** The terminal id of a worker is its task id.
  To open one, the app, already subscribed to the event stream, calls `POST /v1/terminals/{id}/snapshot`, applies the returned event, then applies that terminal's `worker.output` events with a greater `seq`.
  A `snapshot` chunk resets the emulator to its `cols` x `rows` and redraws.
  Input is serialized per terminal, one request in flight, without the optional `seq`.
  A `503` with code `unavailable` means the daemon has no tmux, and the panel says so.
- **Changes.** The file list refetches when the task changes state and on each `task.event` for it.
  `409 no_worktree` shows "no working copy yet"; a `truncated` diff is flagged.
- **Why this agent (ADR-11).** The worker view's "Why this agent" tab loads `GET /v1/tasks/{id}/dispatch` (one record per spawn, oldest first) and appends `dispatch.recorded` events for that task.
  The newest record is shown in full: its summary, the chosen harness, model, effort and account (an `Account.id`, shown by its label when the accounts list is loaded; `null` when unknown), who decided (`classifier`, `coordinator` or `relaunch`), the matched rule, the classifier's provider, model and confidence (`provider: "none"` when the coordinator picked), the resolution's status and reason, and every candidate with its pass or fail reason and quota evidence; earlier records (the first spawn before a relaunch) are collapsed below it.
  An empty list means the worker has not started. Records outlive the task, so a finished task still shows its own.
- **Testing dispatch rules (ADR-11).** `api.testDispatch` posts a task description to `POST /v1/projects/{id}/dispatch:test` and gets a `DispatchTest`, in the words of a dispatch record: the matched `rule` and the `chosen` profile when the classifier decides, or `decided_by: "coordinator"` with `classifier.provider: "none"` when there is none.
  `candidates` are the matched rule's profiles when the resolution weighed them, else every profile of every rule and the default; each carries `passed`, `reason`, `evidence` and four `checks` (`harness_installed`, `model_accepted`, `account_health`, `quota_headroom`) with a `detail` each.
  Nothing is dispatched or recorded. `409 dispatch_invalid` means `dispatch.yaml` does not compile.
  No screen calls it yet; the rule editor (quark#25) will.
- **Decisions inbox.** The app loads every decision (`GET /v1/decisions`, no state filter) so the inbox can list answered ones with who answered, and keeps them current from `decision.opened` and `decision.answered`.
  An answer is sent with `answered_by` from the "Answering as" field (remembered per viewer), or `null` to let the daemon use its own user; the `200` body is the answered decision.
  `409 already_answered` is shown as an error on the decision.
  A decision answered outside Quark arrives as `decision.answered` with `answer` and `answered_by` null, and a question asked again after its answer is a new decision with a new id.
- **Cancel and relaunch** answer `204` after the engine confirms, which can take tens of seconds, so the app sets no client timeout.
- **PR center.** The list loads every PR once and keeps it current from `pr.updated` (a whole PR); `check.updated` and `review.updated` are merged into a loaded PR, which is then refetched for its rolled-up `checks_state` and `review_decision`.
  A check for another `head_sha` than the PR's is ignored.
  Line comments send `path`, `line` and `side` (`new` unless the line was deleted) and go to the owning worker, not the forge; the app keeps what it sent on screen, since the daemon does not list them.
  "Approve and merge" is the one approval action: it is disabled for what the engine's guarded merge refuses (draft, closed, conflicting, checks failing or running), asks for a second click when changes were requested, and shows a `409 merge_refused` message as returned.
- **Verification evidence (ADR-15).** `PullRequest.evidence` carries the gates (repo checks, Playwright journeys, holdout tests), each with its cases and their artifacts; it changes through `pr.updated`.
  The side panel shows one line per gate; the Evidence tab shows every case, failures first and expanded, with screenshots inline (click to enlarge), videos playable, and traces opened in trace.playwright.dev or downloaded.
  Artifact bytes come from `GET /v1/pull-requests/{id}/evidence/artifacts/{artifact_id}`; an artifact `url` is relative to the daemon.
  `stale` evidence (for another `head_sha`) is flagged; `pending` and `running` gates show as in progress.
- **Project memory (J8).** The Memory screen (`#/p/<project id>/memory`) reviews a Project's proposals and browses its entries.
  A finished task's learnings arrive as `memory.proposed` events, each a `MemoryProposal` with `text`, `evidence` (task, PR, files), `source` (`worker` or `coordinator`) and `proposed_at`.
  The app loads every Project's proposals with its snapshots and keeps them current from `memory.proposed`, `memory.accepted` and `memory.rejected`; the board shows how many await review.
  Accepting, with `text` only when it was edited, commits one file under the Project repo's `memory/` and returns the proposal `accepted` with its `entry` (path and commit); `memory.accepted` carries the same.
  `:reject` returns it `rejected` and emits `memory.rejected`. A second decision is `409 already_decided`; accepting in a Project without a Project repo is `409 no_project_repo`.
  `decided_by` and `promoted_by` come from the "Reviewing as" field, the same remembered name decisions are answered as, or `null` for the daemon's own user.
  `GET /v1/projects/{id}/memory` lists every entry on the Project repo's `main`, including hand-written files, which carry only `id`, `path`, `text` and `commit`; the app reads it again whenever a proposal is accepted.
  An entry's `commit` is the commit that added its file. The screen opens it in place from `GET .../memory/commits/{commit}`, which answers its subject, author, date and a unified diff limited to `memory/`, and `404` for anything that is not a commit on `main`.
  `:promote` copies an entry into user-level memory (`~/.quark/memory/`, or the directory quarkd was started with: `--user-memory`, `QUARK_USER_MEMORY`) and returns the `UserMemoryEntry`, whose `project_id` and `entry_id` say where it came from; the Project keeps its entry.
  Promoting an entry again returns the same copy. `GET /v1/memory` lists user-level memory, which is how the screen knows which entries are already promoted; there is no event for a promotion.
- **Accounts (ADR-11).** The Accounts screen lists `GET /v1/accounts` grouped by harness, the harness's default account first, each with its credential health (`health`, from the harness adapter) and latest quota (`quota`); `account.quota_changed` replaces one account's `quota` live.
  "Refresh quota" calls `GET /v1/accounts?refresh=true`, which reads every account's quota (one `quota-axi --provider claude|codex --profile-only` call each) before answering.
  Adding an account sends `harness`, an absolute `config_dir`, an optional `label` and `pools`; only harnesses with an `account_env` take one, and the form shows the login line (`CLAUDE_CONFIG_DIR=<dir> claude`).
  The new account's quota is `pending` until its `account.quota_changed` arrives.
  `409 conflict` means the directory is already an account (or the harness's default); removing answers `409` for a default account or one a running task uses.
  Pools are replaced with `PATCH /v1/accounts/{id}` `{pools}`; a default account can join pools but keeps its label.
  `launchable: false` marks an account the engine cannot start its harness under yet (today every non-default Pi and Grok account); its quota is still read.
- **Pools.** The New project form offers the pools of the chosen harness's accounts as `agent_config.pool`. The daemon starts the coordinator under the pool's least busy ready account, its workers inherit it, and each task records the account it started under in `Task.account_id`.
- **Failover (ADR-11).** When a worker's session log ends at a rate limit, the daemon has the engine relaunch it from its branch, in the same worktree, under the next healthy account of its pool: the Project agent config's pool when the worker runs that harness, else any pool the worker's account is in.
  `task.state_changed` then carries the new `account_id` and one more `failovers` entry (`outcome: relaunched`), and the task's activity log gains a `failover` entry; `signal` names the harness log line that reported the limit.
  The relaunch's dispatch record (`GET /v1/tasks/{id}/dispatch`, `dispatch.recorded`) carries the same entry as `failover`, names the new account in `chosen.account`, and says so in its `summary`, so the "Why this agent" panel shows it.
  A task never returns on its own to an account it left. With no healthy account left (`no_healthy_account`), or when the engine refuses the relaunch (`relaunch_failed`), the daemon opens a decision for the task (`decision.opened`) and does nothing more until it is answered.
  Answering it relaunches the worker with the answer as its note, under another account if one is healthy by then and under its own otherwise; `502` means the engine refused and the decision stays open.

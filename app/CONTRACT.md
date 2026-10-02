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
| PR center | `GET /v1/pull-requests`, `GET /v1/pull-requests/{id}`, `/diff`, `POST .../{id}/comments`, `POST .../{id}:merge`, `PATCH /v1/projects/{id}` `{standing_approval}`, `pr.updated`, `check.updated`, `review.updated` events | quark#16 |

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
- **Decisions inbox.** The app loads every decision (`GET /v1/decisions`, no state filter) so the inbox can list answered ones with who answered, and keeps them current from `decision.opened` and `decision.answered`.
  An answer is sent with `answered_by` from the "Answering as" field (remembered per viewer), or `null` to let the daemon use its own user; the `200` body is the answered decision.
  `409 already_answered` is shown as an error on the decision.
- **Cancel and relaunch** answer `204` after the engine confirms, which can take tens of seconds, so the app sets no client timeout.
- **PR center.** The list loads every PR once and keeps it current from `pr.updated` (a whole PR); `check.updated` and `review.updated` are merged into a loaded PR, which is then refetched for its rolled-up `checks_state` and `review_decision`.
  A check for another `head_sha` than the PR's is ignored.
  Line comments send `path`, `line` and `side` (`new` unless the line was deleted) and go to the owning worker, not the forge; the app keeps what it sent on screen, since the daemon does not list them.
  "Approve and merge" is the one approval action: it is disabled for what the engine's guarded merge refuses (draft, closed, conflicting, checks failing or running), asks for a second click when changes were requested, and shows a `409 merge_refused` message as returned.
  The evidence slot shows `evidence` when present (ADR-15); it is always absent for now.

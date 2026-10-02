# What the desktop app expects from quarkd

This is the slice of the daemon API v1 that the desktop app uses.
The authority for anything already served is `api/openapi.json`; this file adds the endpoints the app needs that `quarkd` does not serve yet, with the shape the app codes against.
Each proposed endpoint names the Phase 1 workstream expected to build it.
When a workstream settles on a different shape, update this file and `src/api.ts` together.

The app treats `404`, `405` and `501` from a proposed endpoint as "not available yet" and shows that in place, so each screen lights up as its endpoint lands.

## Served today (`api/openapi.json`)

| Method and path | Used for |
| --- | --- |
| `GET /v1/health` | connection indicator, engine name |
| `GET /v1/projects`, `POST /v1/projects`, `GET /v1/projects/{id}` | Projects list and create flow |
| `GET /v1/projects/{id}/tasks`, `GET /v1/tasks/{id}` | board, worker header |
| `GET /v1/decisions` | open-decision counts on the board |
| `GET /v1/events?cursor=<seq>` (WebSocket) | everything live |

The app sends the J2 create fields as optional extras on `POST /v1/projects` (see below), which today's daemon ignores.

## Proposed

### Project creation (workstream 2, J2)

`POST /v1/projects` adds optional fields to `CreateProject`:

```
{ name, goal?, workspace_path?,
  repos?: string[],                       // forge URLs or owner/name
  agent_config?: { harness, model?, effort? },
  dispatch_preset?: string }
```

`Project` may carry `repos?: string[]`, `agent_config?` and `dispatch_preset?` back, plus `coordinator_state?: "starting" | "running" | "stopped" | "failed"`; the app shows the coordinator state when present.
Dispatch presets come from `GET /v1/dispatch/presets` -> `[{ id, label, description? }]` when served; until then the app offers `balanced` (default), `fast` and `thorough`.

### Harnesses (workstream 6)

`GET /v1/harnesses` -> `[Harness]`

```
Harness { id, name, installed: bool, version?, models: string[], efforts: string[], install_hint? }
```

The create flow's agent picker uses it; until it is served the picker offers a fixed list (claude, codex, pi, opencode).

### Steering and control (workstream 1)

| Method and path | Body | Response |
| --- | --- | --- |
| `POST /v1/tasks/{id}/messages` | `{ "text" }` | `202` (empty) |
| `POST /v1/tasks/{id}:cancel` | none | `202` |
| `POST /v1/tasks/{id}:relaunch` | none | `202` |

The resulting state changes arrive as `task.state_changed` events.
A `200` or `204` is also accepted.
Errors use the existing `ErrorBody`, and the app shows `error.message`.

### Terminal (workstream 3)

| Method and path | Body | Response |
| --- | --- | --- |
| `GET /v1/tasks/{id}/terminal` | | `TerminalInfo` |
| `POST /v1/tasks/{id}/terminal/input` | `{ "data_b64" }` | `204` |
| `POST /v1/tasks/{id}/terminal/resize` | `{ "cols", "rows" }` | `204` or `TerminalInfo` |

```
TerminalInfo { task_id, cols, rows, attached: bool, seq: int, snapshot_b64? }
```

`snapshot_b64`, when present, is bytes that redraw the pane's current screen (for example `capture-pane -e -p` output with cursor positioning), and `seq` is the last event `seq` already reflected in it.
The app writes the snapshot, then applies only `worker.output` events with a greater `seq`, so opening a worker never replays the whole log.

Event `worker.output`: `{ task_id, data_b64 }` with raw pane bytes (not aligned to lines or UTF-8).
The POC's `worker_id` key is accepted as a fallback.
The app keeps one input request in flight per task and coalesces keys typed meanwhile, because concurrent requests can be reordered (POC finding).

### Transcript (workstream 4)

`GET /v1/tasks/{id}/transcript` -> `[TranscriptEntry]`

```
TranscriptEntry { id, ts, role: "user" | "assistant" | "tool" | "system", text, tool?: string }
```

Event `worker.transcript`: `{ task_id, entry: TranscriptEntry }`.
`text` is markdown for `user` and `assistant`, and plain text (a command and its output) for `tool`.

### Coordinator chat (workstream 4, J3)

The coordinator id of a Project coordinator is the Project id.

| Method and path | Body | Response |
| --- | --- | --- |
| `GET /v1/coordinators/{id}/messages` | | `[ChatMessage]` |
| `POST /v1/coordinators/{id}/messages` | `{ "text" }` | `202` |

```
ChatMessage { id, ts, role: "user" | "coordinator", text }
```

Event `coordinator.message`: a `ChatMessage`, with the event's `project_id` naming the coordinator (or `coordinator_id` in the payload).

### Changes and diff (workstream 5, J4)

| Method and path | Response |
| --- | --- |
| `GET /v1/tasks/{id}/changes` | `Changes` |
| `GET /v1/tasks/{id}/diff` | `text/plain` unified diff of the task worktree against its base |
| `GET /v1/tasks/{id}/diff?path=<path>` | the same, for one file |

```
Changes { base?, head?, files: [{ path, old_path?, status: "added" | "modified" | "deleted" | "renamed", additions, deletions }] }
```

The app refetches changes when the task's `task.state_changed` arrives and when the user presses refresh.

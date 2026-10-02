# UI POC stub daemon contract

Both prototypes (`native/`, `tauri/`) talk only to this API, served by `stub-daemon/` on `http://127.0.0.1:7420`.
It is a subset of the spec's daemon API v1, plus a few POC-only endpoints marked (POC) that the real `quarkd` will need in some form.
All bodies are JSON. Ids are strings.

## REST

| Method and path | Body / response |
| --- | --- |
| `GET /v1/projects` | `[Project]` |
| `GET /v1/projects/{id}/tasks` | `[Task]` |
| `GET /v1/decisions` | `[Decision]` (open and answered) |
| `POST /v1/decisions/{id}:answer` | body `{"option": <index>}` -> `Decision` |
| `GET /v1/pull-requests` | `[PullRequest]` |
| `GET /v1/pull-requests/{id}/diff` (POC) | `text/plain` unified diff |
| `GET /v1/pull-requests/{id}/comments` (POC) | `[Comment]` |
| `POST /v1/pull-requests/{id}/comments` | body `{"path","line","body"}` -> `Comment` |
| `GET /v1/coordinators/{id}/messages` (POC) | `[ChatMessage]` history |
| `POST /v1/coordinators/{id}/messages` | body `{"text"}` -> `202`; the reply streams as events |
| `GET /v1/workers` (POC) | `[Worker]` (live tmux panes) |
| `POST /v1/workers/{id}/input` (POC) | body `{"data_b64"}` raw bytes typed into the pane |
| `POST /v1/workers/{id}/resize` (POC) | body `{"cols","rows"}` |
| `POST /v1/workers/{id}/stress` (POC) | body `{"on": bool}` floods the pane with colored output for perf tests |

Coordinator id is the project id.

```
Project     { id, name, repo, active_tasks }
Task        { id, project_id, title, state, harness, branch, updated_at }
            state: "queued" | "running" | "needs_decision" | "review" | "done" | "failed"
Decision    { id, project_id, task_id, question, context, options: [{label, consequence}], recommended, state, answer }
            state: "open" | "answered"; answer: option index or null
PullRequest { id, project_id, task_id, number, title, url, state, checks, additions, deletions, risk }
            state: "draft" | "open" | "merged"; checks: "pending" | "passing" | "failing"
Comment     { id, path, line, body, author, ts }
ChatMessage { id, role, text, ts }      role: "user" | "coordinator"
Worker      { id, task_id, title, cols, rows }
```

## Event stream

WebSocket `ws://127.0.0.1:7420/v1/events?cursor=<seq>`.
Each frame is one JSON event `{ seq, project_id, type, ts, payload }`, with `seq` strictly increasing.
On connect the server replays every retained event with `seq > cursor` (retention is bounded; `worker.output` is retained for the last 2 MB per worker), then streams live.

| type | payload |
| --- | --- |
| `task.created` | `Task` |
| `task.state_changed` | `Task` |
| `coordinator.message` | `ChatMessage` (complete user or coordinator message) |
| `coordinator.delta` | `{ message_id, text }` text appended to a streaming coordinator message; a final `coordinator.message` with the full text closes it |
| `worker.output` | `{ worker_id, data_b64 }` raw terminal bytes from tmux control mode |
| `decision.opened` / `decision.answered` | `Decision` |
| `pr.updated` | `PullRequest` |

The stub generates background activity: tasks change state every few seconds, a decision opens occasionally, and four tmux panes run scripted worker output.

## Notes from stub

- Timestamps (`ts`, `updated_at`) are RFC 3339 UTC strings with milliseconds, e.g. `2026-10-01T23:11:19.926Z`.
- `POST .../messages` returns `202` with an empty body. `POST .../input` and `POST .../stress` return `204`. `POST .../resize` returns the updated `Worker`.
- Errors are JSON `{"error": "..."}` with `404` (unknown id), `409` (decision already answered) or `422` (option out of range).
- `coordinator.delta.message_id` is the `id` of the closing coordinator `coordinator.message`; the deltas concatenate exactly to its `text`.
- Omitting `cursor` means `cursor=0` (full retained replay). A `cursor` beyond the current head (e.g. saved before a daemon restart) is treated as "live only".
- Replay after eviction can have `seq` gaps; `seq` is still strictly increasing.
- `worker.output` chunks are not aligned to lines or UTF-8 boundaries; feed the bytes straight to a terminal emulator.
- Worker ids are `w-1`..`w-4`; `w-4` is an interactive bash shell (use it for input-latency tests).
- `POST .../input` requests are applied in arrival order, and concurrent HTTP requests can be reordered, so clients should keep one input request in flight per worker and coalesce keys typed meanwhile. Panes run with `LANG=C.UTF-8`.

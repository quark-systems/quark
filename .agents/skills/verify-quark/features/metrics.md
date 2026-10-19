# Project metrics

A user opens a Project's Metrics tab and sees how its work has gone over the last 7, 30 or 90 days: tasks finished (done and failed) per day, lead time from first spawn to done, pass rate and first-time green, interventions (decisions and blockers), relaunches, failovers, and the quota of the accounts its tasks ran under. The daemon computes them from the event log: the supervisor's worker launches and each task's transitions.

## Sub-features

- `met-read` shows the tiles, the per-day chart and the log coverage line.
- `met-window` switches the window between 7, 30 and 90 days.
- `met-unavailable` lists what is not measured yet (spend, coordinator token efficiency).

## How to get to it (user POV)

- From a Project board, the `Metrics` button in the header (route `#/p/<project id>/metrics`).

## Driving it with quark-verify

Preconditions:

- Mock: `$Q launch --daemon mock`; `Quark MVP` has finished tasks.
- Real: `$Q launch --engine native --harness fake`, a ready Project from a local bare repo, and a worker started through the coordinator's `start_task` tool as `bin/journey-native` does. A message `QV-STATUS <status line>` sent to the worker (`POST /v1/tasks/<id>/messages`; `needs-decision [key=k]: ...`, `blocked: ...`, `done: PR <url>`, `failed: ...`) makes it report, and the supervisor records the task's transitions in the event log.

- **Open it.** Run `$Q browser open "#/p/<id>"` and `$Q browser click --testid nav-metrics`; wait with `$Q browser wait --testid metrics-finished --contains done`. Save `metrics/01-metrics.png`.
- **Window.** Run `$Q browser click --testid metrics-days-30`; `$Q browser wait --testid metrics-coverage --contains "Last 30 days"`. Save `metrics/02-30-days.png`.
- **Side effects.** Run `$Q capture metrics/api -- $Q api GET "/v1/projects/<id>/metrics?days=30"`: `throughput.per_day` has 30 entries and the counts match what the workers reported.

## Gotchas

- Logs from before firstmate was retired hold firstmate's events, which Metrics no longer reads; the coverage line says when the log began.
- First-time green counts a task that asked a decision; only a failure, a block or a relaunch disqualifies it.

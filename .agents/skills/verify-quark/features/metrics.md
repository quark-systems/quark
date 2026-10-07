# Project metrics

A user opens a Project's Metrics tab and sees how its work has gone over the last 7, 30 or 90 days: tasks finished (done and failed) per day, lead time from first spawn to done, pass rate and first-time green, interventions (decisions and blockers), relaunches, failovers, and the quota of the accounts its tasks ran under. The daemon computes them from the event log, which mirrors firstmate's status lines and spawns.

## Sub-features

- `met-read` shows the tiles, the per-day chart and the log coverage line.
- `met-window` switches the window between 7, 30 and 90 days.
- `met-unavailable` lists what is not measured yet (spend, coordinator token efficiency).

## How to get to it (user POV)

- From a Project board, the `Metrics` button in the header (route `#/p/<project id>/metrics`).

## Driving it with quark-verify

Preconditions:

- Mock: `$Q launch --daemon mock`; `Quark MVP` has finished tasks.
- Real: `$Q launch` and a ready Project with a local bare repo (an `agent_config` is required to provision). With the stub engine, write firstmate state files into `QV_HOME/workspaces/<id>/state/`: `<task>.meta` with `spawn_gen=s<unix seconds>.1.1` and `harness=claude`, and `<task>.status` with lines such as `working: started`, `needs-decision [key=ttl]: pick`, `blocked: ...`, `done: ...`, `failed: ...`. The daemon ingests them into the event log within one refresh.

- **Open it.** Run `$Q browser open "#/p/<id>"` and `$Q browser click --testid nav-metrics`; wait with `$Q browser wait --testid metrics-finished --contains done`. Save `metrics/01-metrics.png`.
- **Window.** Run `$Q browser click --testid metrics-days-30`; `$Q browser wait --testid metrics-coverage --contains "Last 30 days"`. Save `metrics/02-30-days.png`.
- **Side effects.** Run `$Q capture metrics/api -- $Q api GET "/v1/projects/<id>/metrics?days=30"`: `throughput.per_day` has 30 entries and the counts match the status files written.

## Gotchas

- A status line's time is when the daemon read it, so lines written before the log started all land at its first time; the coverage line says how far back the numbers reach.
- First-time green counts a task that asked a decision; only `failed`, `blocked` or a relaunch disqualifies it.

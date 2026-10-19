# Project overview

A user opens a Project's Overview tab and sees its goal, four numbers (workers by state, issues ready, PRs merged today, spend today), what changed since they last looked beside where the project runs, and every worker's latest status line, open ones first; the live parts are read from the daemon's event log. "Since you last looked" summarizes in one sentence the decisions raised and resolved, tasks done (with pull requests), failures and workers started since the previous visit, and lists them newest first. The first visit digests the whole log; "Mark as read" starts over from now.

## Sub-features

- `ov-cards` (`overview-card-workers`, `-issues`, `-merged`, `-spend`) under the goal (`overview-goal`); issues need Beads and spend needs priced sessions, else they show a dash.
- `ov-now` ("Workers now") shows the live task list, with open decision keys, the pull request a task named, and its harness and model.
- `ov-digest` shows what changed after the previous visit, anchored there for the whole visit.
- `ov-mark-read` resets the digest to the current point in the log.
- `ov-links` leads from a task the daemon knows to its worker view, and between the dashboard's tabs.

## How to get to it (user POV)

- The `Overview` tab in the project's header (route `#/p/<project id>/overview`).

## Driving it with quark-verify

Preconditions:

- Mock: `$Q launch --daemon mock`; the mock folds its own tasks and task events, so cancel or relaunch a task to make a digest.
- Real: `$Q launch --engine native --harness fake`, a ready Project from a local bare repo, and a worker started through the coordinator's `start_task` tool as `bin/journey-native` does. A message `QV-STATUS <status line>` sent to the worker (`POST /v1/tasks/<id>/messages`; `needs-decision [key=k]: ...`, `blocked: ...`, `done: PR <url>`, `failed: ...`) makes it report, and the supervisor records the task's transitions in the event log.

- **First visit.** Run `$Q browser open "#/p/<id>"` and `$Q browser click --testid nav-overview`; wait with `$Q browser wait --testid overview-summary`. Save `dashboard-overview/01-first-visit.png`.
- **Away.** Run `$Q browser click --testid nav-settings`, have the worker report (or cancel a task on the mock), and wait a few seconds.
- **Back.** Run `$Q browser click --testid nav-overview`; `$Q browser wait --testid overview-summary --contains failed`. Save `dashboard-overview/02-since-last-looked.png`.
- **Side effects.** Run `$Q capture dashboard-overview/api -- $Q api GET "/v1/projects/<id>/overview?since=<head of the first visit>"`: the digest's counts and highlights match what the worker reported.

## Gotchas

- Logs from before firstmate was retired hold firstmate's events, which the Overview no longer reads; those tasks do not show.
- The last visit is remembered in the browser's local storage per Project, so a fresh browser profile starts with "Since the log began".
- Tasks in the log the daemon has no task for show their engine id and no link.

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
- Real (stub engine): `$Q launch`, then create a Project whose `workspace_path` is a directory under `QV_HOME` with a `state/` folder (`$Q api POST /v1/projects '{"name":"...","workspace_path":"<dir>"}'`). Appending firstmate status lines to `<dir>/state/<task>.status` (`working: ...`, `needs-decision: [key=k] ...`, `resolved: [key=k] ...`, `done: PR <url> ...`, `failed: ...`) stands in for the engine; the daemon ingests them on its refresh interval.

- **First visit.** Run `$Q browser open "#/p/<id>"` and `$Q browser click --testid nav-overview`; wait with `$Q browser wait --testid overview-summary`. Save `dashboard-overview/01-first-visit.png`.
- **Away.** Run `$Q browser click --testid nav-settings`, append status lines (or cancel a task on the mock), and wait a few seconds.
- **Back.** Run `$Q browser click --testid nav-overview`; `$Q browser wait --testid overview-summary --contains failed`. Save `dashboard-overview/02-since-last-looked.png`.
- **Side effects.** Run `$Q capture dashboard-overview/api -- $Q api GET "/v1/projects/<id>/overview?since=<head of the first visit>"`: the digest's counts and highlights match the lines appended.

## Gotchas

- An event's time is when the daemon logged it, so history ingested on the daemon's first start all shows that start's time.
- The last visit is remembered in the browser's local storage per Project, so a fresh browser profile starts with "Since the log began".
- Tasks from a status file the daemon has no task for show their engine id and no link.

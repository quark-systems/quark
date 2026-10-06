# Daemon API and event stream

A client (the app, a script, another host) talks to quarkd over the `/v1` REST API and follows every change on the `/v1/events` WebSocket, resuming from its last `seq` after a reconnect.

## Sub-features

- `api-health` answers `/v1/health` with status, version, engine and `last_seq`.
- `api-projects` creates, lists and reads Projects, refusing invalid input with 400 and unknown ids with 404.
- `api-lists` serves decisions and pull requests as lists.
- `api-events` streams events with monotonic `seq` and replays from `?cursor=<seq>`.
- `api-openapi` prints the committed contract (`quarkd openapi`, `api/openapi.json`).

## How to get to it (user POV)

- The app's status bar shows the daemon address and engine it is connected to.
- `curl` against the daemon's listen address.
- `quarkd openapi` from a build.

## Driving it with quark-verify

Preconditions:

- `$Q launch` (stub engine) and `$Q doctor` passes.

- **Health.** Run `$Q capture daemon-api/health -- $Q api GET /v1/health`. `status` is `ok` and `engine` is the launched engine.
- **Refuse bad input.** Run `$Q api POST /v1/projects '{"name":"  "}'`. It exits non-zero with HTTP 400, and `$Q api GET /v1/projects/prj_missing` with 404.
- **Create and read back.** Run `$Q api POST /v1/projects '{"name":"API check","goal":"g"}'`, then `$Q api GET /v1/projects/<id>`: the name and goal read back, and `$Q api GET /v1/projects` lists it.
- **Events and replay.** Run `$Q capture daemon-api/events -- $Q events --since 0 --for 1`: frames carry increasing `seq` and include `project.updated` for the new Project. Run `$Q events --since <a seq from the middle> --for 1`: only later frames arrive.

## Gotchas

- The daemon listens on loopback only; reach it from elsewhere through a forwarded port.
- `api` prints the body even on an HTTP error, then exits non-zero; check the exit code, not just the output.

# API overview

`quarkd` serves a JSON HTTP API under `/v1` and a WebSocket event stream on `127.0.0.1:7380`.
The desktop app is one client of it; anything else on the machine can be another.

## The contract

[`api/openapi.json`](../../api/openapi.json) is the committed API contract; a test fails when it drifts from the code.
Print or regenerate it with:

```sh
cargo run -p quarkd -- openapi                       # prints the document
cargo run -p quarkd -- openapi > api/openapi.json    # regenerates the committed copy
```

The request and event types live in `crates/quark-systems`, shared by the daemon and its clients; the routes are in `crates/quarkd/src/api/`.

## Conventions

- Actions on a resource are `POST` to `/{resource}/{id}:{verb}`, such as `POST /v1/decisions/{id}:answer`.
- Long-running work (provisioning a Project, a merge) answers right away and reports progress as events.
- quarkd allows CORS from the app's origins (`tauri://localhost`, `http://tauri.localhost`, and the dev server on port 1420).

## The event stream

The event stream is a WebSocket at `/v1/events?cursor=<seq>`.
Each frame is one JSON event with a monotonic `seq`, and a client that reconnects with its last `seq` replays everything it missed.
Each API page lists the events its resources emit.

## Pages

- [Projects](projects.md)
- [Decisions](decisions.md)
- [Pull requests](pull-requests.md)
- [Terminals](terminals.md)
- [Memory](memory.md)
- [Dispatch](dispatch.md)
- [Imports](imports.md)

[`app/CONTRACT.md`](../../app/CONTRACT.md) lists every endpoint the desktop app calls and the shape it expects.

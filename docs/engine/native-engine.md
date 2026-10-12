# The native engine

`quarkd --engine native` runs Quark with no firstmate checkout.
The engine adapter is `crates/quarkd/src/engine/native.rs` (`NativeEngine`).
It sits on the native supervisor (`quark-supervisor`, slice 4) and the event log (slice 1).

## What it serves

| Adapter operation | Served by |
|---|---|
| `snapshot`, `spawn` | the supervisor's task ledger and fleet |
| `status_tail`, `holds` | `worker.message` events (`WorkerLines`); a tail's offset is a line index |
| `send_message`, `answer`, `control` | `Supervisor::steer`, `answer`, `cancel`, `relaunch` |

The native engine refuses the operations below, each with an error that names the missing part, until it is wired:

- merge and standing approval (slice 2, `quark-verify`)
- verification gates and dispatch resolution (slices 2 and 5)
- adding a repo, creating a Project, and starting its coordinator (native Project setup and slice 6)
- the coordinator's `start_task`, which needs dispatch (slice 5)
- the inbox note (slice 7 channels)

## Getting off firstmate

These steps remain, in order:

1. Native merge, gates and standing approval (slice 2).
2. Native dispatch: `start_task`, `resolve_dispatch` and `set_crew_dispatch` (slice 5).
3. Native Project setup and coordinator start, with no firstmate home (slice 6).
4. The inbox note through the channels API (slice 7).
5. The worker terminal for PTY-backed workers (today `EngineTask::terminal` names only tmux windows).
6. `verify-quark` journeys run with `--engine native`.
7. Self-hosting starts quarkd with `--engine native`.
8. Retire the firstmate adapter, the ingest bridge, and the shadows (slice 10).

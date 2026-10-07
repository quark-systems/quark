# Event log (slice 1)

`crates/quark-eventlog` implements the `EventLog` and `TaskMachine` contracts from `quark-core` (see [contracts.md](contracts.md)).
It holds the native engine's source of truth: every state change is an event first, and every view is rebuilt from the log by replay.

## The file

quarkd keeps the log in `<quark home>/events.db`, a SQLite file of its own, separate from the projection store `quark.db`.

- `events(seq, id, ts, host, project, task, kind, payload)`: `seq` starts at 1 with no gaps; `id` is the event's UUIDv7 and unique; `payload` is JSON.
- `checkpoints(name, value)`: read positions of ingesters, written in the same transaction as the events they cover.

Every append is one `BEGIN IMMEDIATE` transaction in WAL mode with `synchronous=FULL`, so an event is on disk before `append` returns.
`seq` is assigned inside that transaction, and appending a known `id` returns its original `seq`.
`SqliteEventLog::verify` runs SQLite's integrity check and checks that `seq` has no gaps.
Subscriptions wake on appends from the same process and poll the file every 250 ms to follow other writers.

## Tasks

`TaskStates` is the task read model: it folds `task.transition` events through the reference machine (`quark_core::task::ReferenceMachine`).
It skips events it has already applied, so recovery is `quark_core::replay` from wherever it stopped.
A transition already in the log that the machine refuses leaves the state alone and is counted in `TaskRecord::rejected`; the log is never rewritten.

`TaskLedger` is how the native engine changes a task.
It catches up with the log, checks the transition against the machine, appends it, and only then applies it.
An illegal transition is refused with `IllegalTransition` and appends nothing.

## The firstmate bridge

While slice 1 is on bash, firstmate still owns its files.
quarkd runs `FirstmateBridge` over every Project's firstmate home on the projector's interval, so the log carries today's events from day one:

| Source | Event kind | Payload |
|---|---|---|
| each new line of `state/<task>.status` | `firstmate.status` | `StatusPayload`: verb, fold key, corr, note, the raw line and its byte offset |
| each new worker generation in `state/<task>.meta` | `firstmate.spawn` | `quark_engine::meta::SpawnMeta` |

Each file's read position is a checkpoint (`firstmate/<project>/<task>/status` holds device, inode and offset; `.../spawn` holds the last generation), committed with the events it covers, so a line lands exactly once however quarkd dies.
A status file that is replaced or truncated is read again from the start, the same trade the engine makes.
A partial last line waits for its newline.

Status lines are wake-event history, not current task state, so the bridge stores them as written.
Turning them into `task.transition` events and comparing them with firstmate's snapshot is the native read path's job when slice 1 goes to shadow.

## Crash tests

`tests/crash.rs` runs in CI with the rest of `cargo test`.
It starts the `eventlog-crash` binary, lets it append (single events and three-event batches with a checkpoint) or run the bridge while the test writes status lines, SIGKILLs it at a random moment, and checks the file each round:

- the integrity check passes and `seq` has no gaps;
- every append the child acknowledged is present at the `seq` it was given;
- batches and their checkpoint are all or nothing;
- re-appending a stored event returns its `seq`;
- after the last kill, one more bridge pass leaves exactly the file's lines in the log, in order, with no duplicates.

`QUARK_CRASH_SEED` replays a run (the seed is in every failure message) and `QUARK_CRASH_ROUNDS` raises the round count from 20.

## Readers

The Project dashboard's Overview (`GET /v1/projects/{id}/overview`, `crates/quarkd/src/overview.rs`) folds the log into each task's latest status line, open decisions and pull request, and keeps a compact history so it can digest everything after a `seq` the app saved on its previous visit.
It catches up from where it stopped on every read, so a request only reads what was appended since the last one.

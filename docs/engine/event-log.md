# Event log (slice 1)

`crates/quark-eventlog` implements the `EventLog` and `TaskMachine` contracts from `quark-core` (see [contracts.md](contracts.md)).
It holds the native engine's source of truth: every state change is an event first, and every view is rebuilt from the log by replay.

## The file

quarkd keeps the log in `<quark home>/events.db`, a SQLite file of its own, separate from the projection store `quark.db`.

- `events(seq, id, ts, host, project, task, kind, payload)`: `seq` starts at 1 and only counts up; until retention first deletes something it has no gaps. `id` is the event's UUIDv7 and unique; `payload` is JSON.
- `checkpoints(name, value)`: read positions of readers that turn files into events (such as quarkd's `usage.turn` reader), written in the same transaction as the events they cover.

Every append is one `BEGIN IMMEDIATE` transaction in WAL mode with `synchronous=FULL`, so an event is on disk before `append` returns.
`seq` is assigned inside that transaction, and appending a known `id` returns its original `seq`.
`SqliteEventLog::verify` runs SQLite's integrity check and, on a log retention has never pruned, checks that `seq` has no gaps.
Subscriptions wake on appends from the same process and poll the file every 250 ms to follow other writers.

## Tasks

`TaskStates` is the task read model: it folds `task.transition` events through the reference machine (`quark_core::task::ReferenceMachine`).
It skips events it has already applied, so recovery is `quark_core::replay` from wherever it stopped.
A transition already in the log that the machine refuses leaves the state alone and is counted in `TaskRecord::rejected`; the log is never rewritten.

`TaskLedger` is how the native engine changes a task.
It catches up with the log, checks the transition against the machine, appends it, and only then applies it.
An illegal transition is refused with `IllegalTransition` and appends nothing.

## Retention

quarkd's hourly housekeeping (`crates/quarkd/src/housekeeping.rs`) deletes what no read model folds into state, through `SqliteEventLog::prune`:

- `telemetry.sample` and `host.worktrees` older than 3 days, except each host's newest, which dispatch and the Hosts view take as its current reading (the Hosts view shows 48 hours);
- every `firstmate.*` and `shadow.*` event (below).

Every kind a read model replays (task transitions, supervisor, dispatch, coordinator, trigger, verify, usage and worker events) is kept.
`prune` never deletes the head, so `seq` is never reused; it sets the `eventlog.pruned` checkpoint, after which `verify` no longer expects `seq` without gaps.
Readers that follow `seq` (`read`, subscriptions, replay) skip gaps.
When most of the file is free afterwards, it is compacted (`VACUUM`).

The same pass bounds `quark.db`: change notices (every store event but terminal output and transcripts) go after 30 days, a finished task's terminal output 30 days after it ended (trimmed to its last screens after a day), and a worker terminal whose task is gone once it has been quiet a day.
`quarkd diagnose` lists the event types and kinds holding the most payload in each file.

## Old logs

Before firstmate was retired, a bridge copied each Project's firstmate status lines and worker spawns into the log as `firstmate.status`, `firstmate.spawn` and `firstmate.tasks` events, and shadows recorded `shadow.divergence`.
Nothing reads them now, so the Overview and Metrics show no history from before the switch, and retention deletes them.

## Crash tests

`tests/crash.rs` runs in CI with the rest of `cargo test`.
It starts the `eventlog-crash` binary, lets it append single events and three-event batches with a checkpoint, SIGKILLs it at a random moment, and checks the file each round:

- the integrity check passes and `seq` has no gaps;
- every append the child acknowledged is present at the `seq` it was given;
- batches and their checkpoint are all or nothing;
- re-appending a stored event returns its `seq`.

`QUARK_CRASH_SEED` replays a run (the seed is in every failure message) and `QUARK_CRASH_ROUNDS` raises the round count from 20.

## Readers

The Project dashboard's Overview (`GET /v1/projects/{id}/overview`, `crates/quarkd/src/overview.rs`) folds `task.transition`, `supervisor.launched` and `worker.message` events into each task's state, harness and model, latest note, open decisions and pull request, and keeps a compact history so it can digest everything after a `seq` the app saved on its previous visit.
It catches up from where it stopped on every read, so a request only reads what was appended since the last one.

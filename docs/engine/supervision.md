# Spawn, steer and supervise (slice 4)

`crates/quark-supervisor` is the native replacement for firstmate's `fm-spawn`, `fm-send`, `fm-control`, `fm-teardown` and the watcher's liveness checks.
It composes the other native crates through their `quark-core` contracts and keeps everything it must remember in the event log.

| Step | Contract | Crate |
|---|---|---|
| isolated worktree per task | `WorktreeProvider` | `quark-worktree` (treehouse 3.1.2) |
| launch command, keys, hooks | `HarnessManifest` | `quark-harness` |
| native or sandboxed process | `Isolation` | `quark-isolation` |
| terminal session | `SessionBackend` | `quark-sessions` (`quark-ptyd`, tmux) |
| worker messages | `WorkerProtocol` | `quark-worker` |
| task state | `TaskLedger` | `quark-eventlog` |

## Lifecycle

- **Spawn** records `Queued` and a `supervisor.assigned` event (title, brief, repo, branch, harness, model, effort, isolation), takes a worktree on a new branch, asserts it is a linked worktree outside the primary checkout, and launches the first worker generation.
- **Launch** mints an unguessable generation id, writes the brief and an empty status file under `<state>/<project>/<task>/<generation>/` (outside the worktree), builds the argv from the manifest, wraps it for the task's isolation mode (the worktree and the generation directory are writable), creates the session and records `supervisor.launched`, then `Started`.
  Harnesses that take the prompt on the command line get it there; `paste` and `stdin` harnesses get it as a bracketed paste once their screen shows an idle pattern or settles.
  Every worker gets `QUARK_PROJECT`, `QUARK_TASK`, `QUARK_GENERATION`, `QUARK_STATUS_FILE` and, when quarkd serves the worker routes, `QUARK_MCP_URL` and `QUARK_HOOK_URL`.
- **Steer** records `supervisor.steer` first, then pastes it into the live session and records `supervisor.delivered`.
  A message the worker could not receive (no session, or the session just died) goes into the next generation's brief, so steering is never lost; delivery is at least once.
- **Answer** records `DecisionAnswered` and steers the answer in.
- **Interrupt** sends the manifest's interrupt keys. **Cancel** records `Cancelled` first (so the task is not recovered), types the manifest's exit command, and kills the session after a grace period. **Relaunch** stops the current generation and launches a new one in the same worktree, optionally on another harness, model, effort or account, with a note for the new worker.
- **Complete** records that the work landed and stops the worker. **Teardown** is refused until the task is done or failed, then returns the worktree; a worktree with uncommitted or unpushed work is reported `Kept` and never discarded.

## Supervision pass

`Supervisor::tick` runs on a timer (quarkd: every 2s):

1. reads each worker's status file into the log (the file protocol; ids are derived from file offsets, so re-reading after a restart records nothing twice);
2. turns new `worker.message` events into transitions: `blocked`/`paused`/`failed` reports, `needs-decision` asks, `done` (`InReview` with a pull request, `Completed` without), and a `working` report resuming a blocked task; how far it got is recorded as `supervisor.handled`, so a restart never re-applies an old message;
3. finishes a spawn a crash interrupted (assigned but never launched, or launched before `Started` was recorded);
4. notices ended sessions (`supervisor.exited` with the exit code) and relaunches an active task's worker in the same worktree, telling it what happened, up to `max_recoveries` times in a row before failing the task;
5. delivers steering still waiting;
6. records `supervisor.stale` once per episode when a running worker's screen and messages have not changed for `stale_after` (15 minutes by default).

## Durability

The supervisor holds no state the log does not have; its in-memory watch data (status-file cursors, screen hashes) is rebuilt from scratch.

- **Daemon crash:** `quark-ptyd` keeps the sessions. A new supervisor replays the log, `recover` adopts the sessions of current generations and kills any session no generation owns, and supervision carries on with the same workers.
- **Session server loss, reboot:** the sessions are gone, so the next pass records each as exited and relaunches the worker in its worktree with a note and any steering it missed.
- **Replaced workers:** the worker protocol checks the generation against the fleet, so a message from a replaced worker is recorded and refused.

The tests in `crates/quark-supervisor/tests/lifecycle.rs` drive each of these against real pseudo-terminals, real git worktrees and the SQLite log, with a shell script as the agent.

## In quarkd

Slice 4 cannot switch on before slices 1 to 3, so firstmate still spawns every worker and the native supervisor is opt-in: `QUARK_NATIVE_SUPERVISOR=1` makes quarkd start `quark-ptyd` (next to `quarkd`, or `QUARK_PTYD`) on `<home>/run/ptyd.sock`, build the supervisor over `events.db`, treehouse, `<home>/harnesses` and the host sandbox, recover, supervise, and serve the worker protocol under `/v1/worker`.

## Not yet

- Installing harness hooks and MCP config per task (`hooks.install`); workers use the status file until then.
- A native `EngineAdapter` for the slice 4 operations. The shadow comparison exists: `quark_supervisor::shadow` replays firstmate's spawns and status lines through the supervisor's own rules (`quark_supervisor::rules`), and quarkd compares the resulting states with firstmate's (`docs/shadow-readiness.md`). Session liveness is not compared yet.
- Relaunching from an open decision keeps the task in `NeedsDecision`: the reference machine has no `Started` from there, so the ledger keeps the older generation id while the fleet has the current one. A small `quark-core` amendment could allow it.

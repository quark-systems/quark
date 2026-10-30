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
  Harnesses that take the prompt on the command line get it there; `paste` and `stdin` harnesses get it as a bracketed paste once an idle pattern has stayed on screen for `ready_settle` (a harness with no idle pattern: once its screen stops changing; a still loading screen never counts for one that has patterns). A harness whose `busy` signal comes from hooks must send it (or any worker message) within `brief_confirm` (10 s) of the paste; otherwise the supervisor presses Enter, waits again, and pastes the brief once more, since a paste sent before the agent's input is live is dropped without a trace.
  Before that, start-up questions the manifest lists under `launch.dialogs` are answered from the visible screen (rendered through a terminal emulator, so scrollback never counts): with all of a dialog's `detect` text on screen the selector is moved one arrow key at a time toward `choose` and Enter is pressed only once it is there, and while any one piece shows the screen is never taken as ready. A fully drawn dialog wins over another entry's partial match, since dialogs can share words such as "to navigate". Kiro's trust-all-tools confirmation, which opens on "No, exit", is answered this way with the session-only "Yes, I accept", and its once-per-folder workspace-trust question with "Trust, continue".
  A harness whose manifest sets `launch.enter_again_ms` gets one more Enter that long after the brief, and after steering, because IBM Bob can leave a pasted message unsent in its composer.
  A harness with a model catalog command (`models.list`, Kiro's `kiro-cli chat --list-models --format json`) has the model checked first; one the catalog does not list fails the launch, since Kiro would silently start its default model, and an unreadable catalog lets it go ahead with a warning.
  Anything the person must set up for the harness to run unattended is logged as a warning (`quark_harness::launch_warnings`): IBM Bob asks before touching files outside its workspace, whatever `--auto-approve` says, unless `approval.permissionOptions` in `~/.bob/settings/settings.json` opts the `read`, `write` and `edit` groups in, and a worker writes its status file outside its worktree. Quark only checks that setting; it never writes it.
  Every worker gets `QUARK_PROJECT`, `QUARK_TASK`, `QUARK_GENERATION`, `QUARK_STATUS_FILE` and, when quarkd serves the worker routes, `QUARK_MCP_URL` and `QUARK_HOOK_URL`.
- **Steer** records `supervisor.steer` first, then pastes it into the live session and records `supervisor.delivered`; with `launch.enter_again_ms` set, Enter is pressed once more that long later without holding up the call.
  A message the worker could not receive (no session, or the session just died) goes into the next generation's brief, so steering is never lost; delivery is at least once.
- **Answer** records `DecisionAnswered` and steers the answer in.
- **Interrupt** sends the manifest's interrupt keys. **Cancel** records `Cancelled` first (so the task is not recovered), types the manifest's exit command, and kills the session after a grace period. **Relaunch** stops the current generation and launches a new one in the same worktree, optionally on another harness, model, effort or account, with a note for the new worker.
- **Complete** records that the work landed and stops the worker. **Teardown** is refused until the task is done or failed, then returns the worktree; a worktree with uncommitted or unpushed work is reported `Kept` and never discarded.

## Supervision pass

`Supervisor::tick` runs on a timer (quarkd: every 2s):

1. reads each worker's status file into the log (the file protocol; ids are derived from file offsets, so re-reading after a restart records nothing twice);
2. turns new `worker.message` events into transitions: `blocked`/`paused`/`failed` reports, `needs-decision` asks, `done` (`InReview` with a pull request, `Completed` without), and a `working` report resuming a blocked task; how far it got is recorded as `supervisor.handled`, so a restart never re-applies an old message;
3. finishes a spawn a crash interrupted (assigned but never launched, or launched before `Started` was recorded);
4. notices ended sessions (`supervisor.exited` with the exit code) and relaunches an active task's worker in the same worktree, telling it what happened, up to `max_recoveries` times in a row before failing the task. When the harness can resume (`launch.resume` in its manifest) and the supervisor's `Conversations` finds a conversation it logged in the worktree (for IBM Bob, a top-level task for the worktree in `~/.bob/db/bob.db` (its `directory`, or `project_id = file:<path>` as Bob 2.x records it), continued with `--resume latest`; for Kiro CLI, a session in `~/.kiro/sessions` whose `session.json` lists the worktree, continued with `--resume`, which picks the newest local session for the directory), the new generation continues that conversation and is sent the note and missed steering without the brief again. Pool slots are reused, so only a conversation logged since the task took the worktree counts (`Worker::worktree_at`, the time of its `supervisor.worktree` event); a previous occupant's is never resumed, and the new generation starts fresh with its brief. The transcript tap and usage ingest skip those earlier logs the same way. Secret references in the worker's environment (`QUARK_SECRET_<VAR>=keychain:<service>`) are read from the Keychain into `<VAR>` only for the session being started, so no key is written to the log. A session runs the agent as its own program, so the agent exiting ends it; a tmux pane that outlives its program (`remain-on-exit`, or a SIGCHLD tmux missed) is listed as ended and closed before the relaunch;
5. delivers steering still waiting;
6. records `supervisor.stale` once per episode when a running worker's screen and messages have not changed for `stale_after` (15 minutes by default).

## Fleet check

The supervision pass only reports a quiet worker. quarkd's fleet check (`crates/quarkd/src/fleet_check.rs`, firstmate's watcher heartbeat in the native engine) acts on it. Every 5 minutes (`QUARK_FLEET_CHECK_SECS`; 0 turns it off) it reads `Supervisor::quiet_workers`, and for a running worker quiet for `stale_after`:

1. nudges it once in 6 hours (`Supervisor::nudge`): a steering message that says how long it has been quiet and asks it to report what it waits on, report done, or carry on. A screen showing a manifest dialog or a numbered menu (`❯ 1. Yes`) is never typed into, since text and Enter could pick an option for the worker;
2. when it is quiet that long again after its nudge, or a dialog stopped the nudge, wakes the coordinator (`coordinator.requested` by "the fleet check", with the last lines of the worker's screen) to steer, answer, relaunch or cancel it, or ask the user. At most once in 2 hours per worker.

A new generation starts over. The check's memory is in-process, so after a restart quiet time counts from zero again.

## Durability

The supervisor holds no state the log does not have; its in-memory watch data (status-file cursors, screen hashes) is rebuilt from scratch.

- **Daemon crash:** `quark-ptyd` keeps the sessions. A new supervisor replays the log, `recover` adopts the sessions of current generations and kills any session no generation owns, and supervision carries on with the same workers.
- **Session server loss, reboot:** the sessions are gone, so the next pass records each as exited and relaunches the worker in its worktree with a note and any steering it missed.
- **Replaced workers:** the worker protocol checks the generation against the fleet, so a message from a replaced worker is recorded and refused.

The tests in `crates/quark-supervisor/tests/lifecycle.rs` drive each of these against real pseudo-terminals, real git worktrees and the SQLite log, with a shell script as the agent.

## In quarkd

With `--engine native`, quarkd starts the PTY supervisor (itself as `quarkd ptyd`, or the `quark-ptyd` binary `QUARK_PTYD` names) on `<home>/run/ptyd.sock`, build the supervisor over `events.db`, treehouse, `<home>/harnesses` and the host sandbox, recover, supervise, and serve the worker protocol under `/v1/worker`.

## Not yet

- Installing harness hooks and MCP config per task (`hooks.install`); workers use the status file until then.
- Relaunching from an open decision keeps the task in `NeedsDecision`: the reference machine has no `Started` from there, so the ledger keeps the older generation id while the fleet has the current one. A small `quark-core` amendment could allow it.

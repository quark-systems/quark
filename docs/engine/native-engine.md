# The native engine

`quarkd --engine native` runs Quark with no firstmate checkout.
The engine adapter is `crates/quarkd/src/engine/native.rs` (`NativeEngine`).
It sits on the native supervisor (`quark-supervisor`, slice 4), native verification (`quark-verify`, slice 2), native dispatch (`quark-dispatch`, slice 5) and the event log (slice 1).

## What it serves

| Adapter operation | Served by |
|---|---|
| `snapshot`, `spawn` | the supervisor's task ledger and fleet, plus tasks still in dispatch (reported `queued`) |
| `status_tail`, `holds` | `worker.message` events (`WorkerLines`); a tail's offset is a line index |
| `send_message`, `answer`, `control` | `Supervisor::steer`, `answer`, `cancel`, `relaunch` |
| `start_task`, `choose_profile` | `NativeDispatch` (`engine/native_dispatch_engine.rs`) over `quark_dispatch::Dispatcher` |
| `resolve_dispatch`, `resolve_description`, `set_crew_dispatch` | the same, on the Project's rules |
| `merge_pull_request` | `quark_verify::GuardedMerge` (below), then `Supervisor::complete` |
| `set_standing_approval` | `verify.standing_approval` events, one per repo |
| `set_gates`, `gate_evidence`, `gate_artifact` | `config/gates.json` (schema `fm.gates.v1`) and `verify/<task>/` under the Project workspace root |

| `add_source`, `seed_workspace`, `start_coordinator`, `coordinator_terminals` | `ProjectSetup` (`engine/native_project.rs`) |
| a task's `terminal` | the current generation's PTY session, as `pty:<session id>`, while its program runs |

## Worker terminals

Native workers run in `quark-ptyd` sessions, not tmux windows, and the app shows them in the same worker terminal: same terminal ids (the task ids), same `worker.output` events, same input, resize and snapshot routes.
The app needs no change.

- `NativeEngine` reports a running worker's endpoint as `pty:<session id>` (`sessions::pty::target`), from the generation's session in the supervisor's fleet.
  An exited worker reports none, so its terminal goes away, as a closed tmux window's does.
- The projector hands every task endpoint to `Sessions::sync`, as it does tmux targets.
  `Sessions::with_pty` (wired by `quarkd` whenever the native supervisor runs, tmux or not) serves the `pty:` ones from the supervisor's session backend (`crates/quarkd/src/sessions/pty.rs`); everything else stays on tmux.
- Each terminal holds one attach stream.
  Its first chunk is the screen repainted in stream order, recorded as the snapshot; the rest is output.
  A snapshot the app asks for, or the one after old output is pruned, opens a fresh stream after stopping the old one, so nothing is lost or sent twice.
  A viewer that falls behind gets a repaint, which starts with a terminal reset and is recorded as output.
- A relaunch starts a new session; the next refresh names it, and the terminal switches to it with a snapshot.
- `quark-ptyd` keeps the sessions across a daemon restart: the first refresh reattaches each terminal and starts it again from a snapshot.

A PTY worker's process ids are not reported to host attribution yet.

## Verification

The slice 2 glue is `crates/quarkd/src/engine/native_verify.rs` (`NativeVerify`).
A merge reads the pull request, rebases it onto its base branch's current tip in a scratch worktree under `verify/<task>/`, and runs the task's repo gates on the rebased head.
A head that did not contain the tip is pushed rebased and refused until the forge's checks run on it, so a stale head never merges.
The pull request's own checks must be green, then `NativeGuard` re-reads main: while main is red only a change that passes every failing check merges.
The forge merges exactly the verified head, the merge is a `verify.merged` event, and the task is marked landed.
A refusal is an engine command error that names the rule; the last run's gate evidence stays in `verify/<task>/evidence.json` with its logs.
A merge a red main refuses files the episode's decision once, and it shows as a hold `red-main:<decision>` with no task.
Answering it `merge anyway` overrides the guardrail until main is green again; any other answer is refused and the hold stays.

## Dispatch

The coordinator's `start_task` tool (title, brief, optional repo and agent) calls `EngineAdapter::start_task`, and `choose_profile` calls `EngineAdapter::choose_profile`.
Both are additive trait methods whose default refuses, so the firstmate adapter is unchanged.

`start_task`:

1. Makes the task id from the title (lowercase letters, digits and dashes, at most 40 characters, `-2`, `-3` and so on when taken), so it passes `quark_engine::validate_task_id`.
2. Finds the repo in the Project workspace: `<root>/repos/<name>` is each source repo's primary checkout.
   `<root>/projects/<name>`, firstmate's layout, is read too, so a workspace firstmate seeded runs natively.
   With no repo named, the workspace's only repo is used; with several, the call names them and refuses.
3. Records a `dispatch.requested` event and resolves the agent from the Project's rules (classifier, quota, selection), unless the caller named one.
   A clear resolution, or a named agent, chooses an account from the profile's pool (`dispatch.chosen`).
   Anything else escalates (`dispatch.escalated`), and the task waits for `choose_profile`.
   With no classifier configured (`provider: none`), every task without a named agent escalates, as the dispatcher does today.
4. Runs admission control at once: a chosen task is placed on a registered host with room and spawned by the supervisor in an isolated worktree on branch `quark/<task id>`, or held until one has room.
   A timer (`native_dispatch_engine::TICK`, 5 seconds) places held tasks later.

The tool's answer says which of the three happened: started (with the agent and account), queued (with the reason it waits), or escalated (with the reason, and that `choose_profile` picks).
A spawn the supervisor refuses is an error naming why.

The rules are `<root>/config/crew-dispatch.json`, the file firstmate reads, written by `set_crew_dispatch` after `quark_dispatch::DispatchConfig::parse` accepts it.
`resolve_dispatch` returns the resolution recorded when the task was dispatched (`dispatch.resolved`, `dispatch.chosen`), so it never asks the classifier twice; a task the agent was named for reports `not_consulted`.

Accounts come from quarkd's accounts and pools (`StoreAccounts`), set once the daemon has built them.
`quarkd --engine native` also registers and samples this host, which admission control needs.

Known gaps:

- `dispatch.yaml` pools do not reach the rules yet: the compiled `crew-dispatch.json` leaves `pool` out because firstmate refuses it.
  A profile with a `pool` in the file, or a coordinator agent with one, is honored.
- No red-main pause on new dispatch and no idle-worktree pruning yet.
- Workers run as plain processes (`IsolationMode::Native`), as firstmate's do.
- Native account choices are not recorded as account leases, so the Accounts view does not count them.

## Project setup and the coordinator

Creating a Project needs no firstmate home and no `AGENTS.md` persona.

- `add_source` clones the repo into the command-center workspace at `<command>/repos/<name>`.
  Adding it again with the same URL is a no-op; another URL under the same name is refused.
  The delivery policy stays on the Project in the store.
- `seed_workspace` claims the Project workspace (`.quark/workspace.json`) and clones each source into `<workspace>/repos/<name>`, from the command center's copy when there is one, with `origin` set to the source URL.
  Seeding again for the same Project checks what is there; a directory claimed by another Project, or any non-empty directory without a claim, is refused.
- `start_coordinator` renders the coordinator's layered prompt (built-in, persona, the Project repo's `instructions.md` and memory, each code repo's instructions, skills; `quark-coordinator` and `quark-persona`) into `.quark/coordinator.md`.
  It installs the coordinator's MCP route and turn-end hook into the workspace (`.mcp.json`, `.claude/settings.local.json`, as the native coordinator's pass does), then starts the harness in the `coordinator-<Project id>` window of quarkd's tmux server, in the workspace, under the account's variables.
  The prompt reaches the agent as its system prompt (`--append-system-prompt`, read from the file by `sh` because a tmux command cannot carry it), so it takes effect at the next start when it changes.
  With `resume`, the agent continues its latest conversation in the workspace (`--continue`); with no logged conversation the start is refused, and recovery starts the coordinator fresh.
  A coordinator whose window is up is not started a second time.
- Each start is recorded in `<command>/coordinators/<Project id>.json`; `coordinator_terminals` reads those records, so the app's coordinator terminal and chat, and coordinator recovery (`recovery.rs`), find the window after a daemon restart.

Only Claude Code coordinators start so far: its tools and turn-end hook are the ones wired.
Another harness is refused with an error that says so.

With `--engine native`, the native coordinator (slice 6) always runs, in Native wiring (`InputBrain` and `EngineHands`), whatever `QUARK_ENGINE_SLICES` and `QUARK_NATIVE_COORDINATOR` say.
Coordinator recovery runs for the native engine as it does for firstmate.

## Channels and the inbox

With `--engine native`, the slice 7 engine (`native_triggers.rs`) always runs in native mode: the user's inbox note and plug-in channel messages land as `channel.received` events, wake the coordinator, and the coordinator's `ack_message` tool marks them handled.

## Still refused

The native engine refuses the operations below, each with an error that names the missing part, until it is wired:

- coordinators on harnesses other than Claude Code

## Getting off firstmate

These steps remain, in order:

1. Coordinators on other harnesses: their tool and turn-end hook wiring.
2. `verify-quark` journeys run with `--engine native`.
3. Self-hosting starts quarkd with `--engine native`.
4. Retire the firstmate adapter, the ingest bridge, and the shadows (slice 10).

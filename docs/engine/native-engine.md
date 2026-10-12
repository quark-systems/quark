# The native engine

`quarkd --engine native` runs Quark with no firstmate checkout.
The engine adapter is `crates/quarkd/src/engine/native.rs` (`NativeEngine`).
It sits on the native supervisor (`quark-supervisor`, slice 4), native dispatch (`quark-dispatch`, slice 5) and the event log (slice 1).

## What it serves

| Adapter operation | Served by |
|---|---|
| `snapshot`, `spawn` | the supervisor's task ledger and fleet, plus tasks still in dispatch (reported `queued`) |
| `status_tail`, `holds` | `worker.message` events (`WorkerLines`); a tail's offset is a line index |
| `send_message`, `answer`, `control` | `Supervisor::steer`, `answer`, `cancel`, `relaunch` |
| `start_task`, `choose_profile` | `NativeDispatch` (`engine/native_dispatch_engine.rs`) over `quark_dispatch::Dispatcher` |
| `resolve_dispatch`, `resolve_description`, `set_crew_dispatch` | the same, on the Project's rules |

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
- No red-main guard and no idle-worktree pruning: both come with native verification (slice 2) and the native worktree pool.
- Workers run as plain processes (`IsolationMode::Native`), as firstmate's do.
- Native account choices are not recorded as account leases, so the Accounts view does not count them.

## Still refused

The native engine refuses the operations below, each with an error that names the missing part, until it is wired:

- merge and standing approval (slice 2, `quark-verify`)
- verification gates (slice 2)
- adding a repo, creating a Project, and starting its coordinator (native Project setup and slice 6)
- the inbox note (slice 7 channels)

## Getting off firstmate

These steps remain, in order:

1. Native merge, gates and standing approval (slice 2).
2. Native Project setup and coordinator start, with no firstmate home (slice 6); setup clones each source into `<root>/repos/<name>`.
3. The inbox note through the channels API (slice 7).
4. The worker terminal for PTY-backed workers (today `EngineTask::terminal` names only tmux windows).
5. `verify-quark` journeys run with `--engine native`.
6. Self-hosting starts quarkd with `--engine native`.
7. Retire the firstmate adapter, the ingest bridge, and the shadows (slice 10).

# Engine contracts

The native engine is built as separate crates under `crates/`, one per subsystem, each switched on slice by slice beside the firstmate (bash) engine.
They meet only at the traits and types in `crates/quark-core`.
This page says what each contract is for and the rules every implementation keeps; the doc comments in `quark-core` are the exact reference.

## Rules

- `quark-core` holds types and traits only. No files, sockets, processes or databases.
- Native crates depend on `quark-core`, never on each other's internals and never on `quarkd`. `quarkd` depends on them.
- Names are neutral: user, coordinator, worker, sub-coordinator, investigation, decision. Persona packs add the flavor.
- A contract change is a small PR to `quark-core` on its own. A new trait method has a default so existing implementations keep compiling. The same holds for quarkd's `EngineAdapter`.
- Every contract has an in-memory fake in `quark_core::fake`, so a subsystem can be built and tested before the one it depends on lands.

## The event log is the source of truth

Every state change is appended to the `EventLog` first; everything else is a read model rebuilt from it.

- `Event { id, seq, ts, host, project, task?, kind, payload }`.
  `id` is a UUIDv7, so logs from several hosts merge without renumbering. `seq` is per log, starting at 1, with no gaps.
- `append` is durable before it returns and idempotent by `id`: appending a known id returns its original `seq`.
- `read(after, limit)` and `subscribe(after)` start after a `seq`; `Seq::ZERO` means from the beginning.
- `kind` is dotted (`task.transition`, `worker.message`, `worktree.change`, `shadow.divergence`, `slice.mode`). Each subsystem owns its prefix; the payload type is documented next to each constant in `quark_core::event::kinds`.
- Events about the engine itself, not one Project, use the project `_engine` (`ProjectId::engine()`).
- `ReadModel` implementations apply events in order and must tolerate a replayed event. `quark_core::replay` brings one up to the log's head.

`quark-eventlog` implements it as its own SQLite file (`events.db`), separate from quarkd's projection store.

## Contracts

| Contract | Module | What it promises | Implemented by |
|---|---|---|---|
| `EventLog`, `Subscription` | `event` | above | `quark-eventlog` |
| `TaskMachine`, `TaskEvent` | `task` | pure, deterministic `apply(state, event)`; states are `quark_systems::TaskState`. `ReferenceMachine` is the reference table; implementations may be stricter, not looser | `quark-eventlog` |
| `ReadModel`, `replay` | `read_model` | rebuilt from the log; replay-safe | quarkd's projector, dashboard metrics |
| `WorktreeProvider` | `worktree` | `get`, `return_worktree`, `lease`, `status`. Never hands out a primary or tangled checkout; never discards uncommitted or unpushed work (returns `Kept`). Every change mirrored as `worktree.change` | `quark-worktree` (treehouse 3.1.2, then native in slice 9) |
| `SessionBackend`, `OutputStream` | `session` | create, attach, input, resize, snapshot, kill, list. Sessions outlive viewers and daemon restarts | `quark-sessions` (tmux control mode, PTY supervisor) |
| `HarnessManifest`, `HarnessRegistry` | `harness` | the TOML schema (serde types; unknown keys rejected): detect, aliases, models, efforts, launch, account, hooks, turn signals, transcript locator, keys, quota source, plugin escape hatch. `validate()` runs after parsing | `quark-harness` |
| `WorkerProtocol`, `WorkerMessage` | `worker` | `report`, `ask`, `learned`, `done` (plus hook signals) over MCP, hooks or files; each lands as a `worker.message` event before `receive` returns | `quark-worker` |
| `VerifyPipeline`, `MergeGuard` | `verify` | gate stages; `rebase_and_reverify`; `main_health`; `may_merge` refuses a stale head and, while main is red, anything but a change that turns it green; `may_dispatch` allows only one fix-main task while main is red | `quark-verify` |
| `Isolation` | `isolation` | wraps a command for native, sandbox (Seatbelt, bubblewrap) or container mode, so it composes with any session backend | `quark-isolation` |
| `Runtime`, `HostRegistry` | `host` | local, SSH, hosted and private-cloud hosts with identity, platform, capacity and health | `quark-runtime` (local, SSH; see `sub-coordinators.md`), `quark-hosts` |
| `Telemetry` | `telemetry` | one sample per call: CPU, memory, pressure, disk, Quark's own disk use, per-Project and per-task usage | `quark-hosts` |
| `PersonaPack`, `PersonaSource` | `persona` | role labels, address, voice, vocabulary, UI labels and prompt fragments; global with per-Project override | `quark-persona` |
| `SliceSwitch` | `slice` | per-slice `bash`, `shadow` or `native`, switched on strictly in order, with history and rollback | quarkd `engine/shadow.rs` |

## Slice switch and shadow mode

Slices switch on in the order of the requirements: 1 event log, 2 verification, 3 worker protocol, 4 supervision, 5 dispatch, 6 coordinator, 7 sub-coordinators, 8 sandbox, 9 worktree pool.
`SliceSwitch` refuses a slice in `shadow` before every earlier slice is at least `shadow`, and in `native` before every earlier slice is `native`.
Rolling a slice back also rolls back any later slice that would otherwise be ahead of it.

quarkd's `ShadowEngine` is an `EngineAdapter` that routes each operation by its slice (the table is in `crates/quarkd/src/engine/shadow.rs`):

- `bash`: firstmate only.
- `shadow`: firstmate answers. Reads also run on the native engine and any disagreement is appended as a `shadow.divergence` event. Writes go to firstmate only, so nothing acts twice; a native slice that wants to compare its decision for a write (a merge verdict, a dispatch choice) does so inside its own crate without acting.
- `native`: the native engine only.

Mode changes are logged as `slice.mode` events. Startup modes come from `QUARK_ENGINE_SLICES`, such as `1=shadow`; quarkd accepts `shadow` for slice 1 (the event log ingest) and slice 2 (the verification shadow, `docs/engine/verify.md`) and refuses any other non-`bash` mode until that slice's native side lands.
A slice moves from `shadow` to `native` only after its divergences are gone and the verify-quark journeys pass.

# Engine internals

`quarkd --engine native` is the engine: separate crates under `crates/`, one per subsystem, meeting at the contracts in `quark-core`.
It was built slice by slice beside firstmate, the bash engine Quark drove before, which is now retired; the slice numbers in these pages date from that migration.

- [The native engine](native-engine.md): how quarkd serves each engine operation, worker terminals, verification and dispatch glue.
- [Contracts](contracts.md): the traits and types the crates share.
- [Event log](event-log.md) (slice 1): the crash-safe log, task state machine and replay.
- [Verification and merge](verify.md) (slice 2): gates, rebase and re-verify, the red-main guardrail.
- [Spawn, steer and supervise](supervision.md) (slice 4): the worker lifecycle.
- [Dispatch, pools and admission](dispatch.md) (slice 5): which agent, account and host run a task.
- [The judgment-only coordinator](coordinator.md) (slice 6): prompt layers, wake queue and native tools.
- [Sub-coordinators and runtimes](sub-coordinators.md) (slice 7): coordinator homes, local and SSH.
- [Triggers, channels and the away policy](triggers.md) (slice 7): the inbox, watches and quiet modes.
- [Worktree pool](worktrees.md) (slice 9): isolated worktrees, treehouse and the native pool.

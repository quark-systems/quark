# Dispatch, pools and admission control (slice 5)

`crates/quark-dispatch` is the native replacement for firstmate's dispatch intake: `fm-dispatch-resolve.sh` (which agent runs a task), quarkd's account leases (which account it runs under), and the admission control firstmate never had (which host it runs on, or whether it waits).
It sits in front of the slice 4 supervisor (`docs/engine/supervision.md`) and keeps every decision in the event log.

## The path of one task

1. **Request.** `Dispatcher::submit` records `dispatch.requested` with the task's title, brief, repo, branch, isolation and environment. A request may name a profile itself; then the rules are skipped.
2. **Resolve.** The Project's rules (`dispatch.yaml`, compiled to `crew-dispatch.json`) are resolved exactly as firstmate resolves them, and the result is recorded as `dispatch.resolved`:
   - the classifier (the System-1 API; `classify::SystemOne`) picks a rule or `default` from the brief, with a confidence;
   - a rule needing approval, or whose floor cannot be verified, escalates; a rule floor below falls through to the default profiles; below the confidence floor the result is `ambiguous`, unless `on_failure: default` sends it (and any classifier failure) to the default profiles;
   - each candidate is judged on one `quota-axi --json` snapshot (plus firstmate's local Bob and Kiro readers): exhausted runway, 0% left without overage, or a profile floor below rule it out; missing or unverifiable evidence leaves it eligible but unranked; `pricing: budget` needs no quota;
   - `ordered` takes the first eligible candidate (escalating if its evidence is unverifiable), `quota-balanced` takes the highest spend priority, a budget candidate only when none ranks, and escalates on a tie.
3. **Choose.** A clear resolution picks an account from the profile's pool and records `dispatch.chosen`. Pool rules are quarkd's: sticky while the account stays launchable and in the pool, otherwise the ready account with the fewest running tasks, then the most quota left. Anything not clear (off, ambiguous, escalated, error, an empty or exhausted pool) is `dispatch.escalated` until the coordinator calls `Dispatcher::choose` with a profile.
4. **Admit.** Each `Dispatcher::tick` (oldest first) asks the red-main guard (`MergeGuard::may_dispatch`) and then admission control for a host. A held task records `dispatch.held` once per distinct reason.
5. **Place and spawn.** A placed task records `dispatch.placed` and goes to that host's `Spawner` (the local supervisor; other hosts arrive with their runtimes), then `dispatch.spawned` or `dispatch.spawn_failed`.

A task can be taken back with `Dispatcher::withdraw` until it is placed.

## Admission control

A host takes a new worker only when all of these hold:

| Check | Default | Where it is set |
|---|---|---|
| health is `healthy` (degraded and unreachable hosts take nothing new) | | host registry |
| the host runs the Project (an empty list runs every Project) | | `Host.projects` |
| fewer workers than `Capacity.max_workers` (0 is no cap) | no cap | `QUARK_MAX_WORKERS` for this host |
| CPU at most `max_cpu` | 90% | `Limits.host` / `Limits.hosts` |
| memory pressure at most `max_memory_pressure` | 75% | same |
| free disk at least `min_disk_free_bytes` | 5 GiB | same |
| the Project's workers per host and in total under its caps | no caps | `Limits.project` / `Limits.projects` |

Resource checks use the host's latest `telemetry.sample` (from `quark-hosts`). A sample older than `max_sample_age` (2 minutes) is not trusted: the host is judged by worker count alone and the placement says so, so a stopped sampler never stops work.
Among hosts that pass, the least loaded (workers over cap) wins, then one with a fresh sample, then the least CPU in use.
Running workers are counted from `task.transition` events (`Started` until `Completed`, `Failed` or `Cancelled`), plus placements the host has not started yet.

Two engine-wide actions come from the same pass, recorded under the engine project:

- **Disk:** a host whose fresh sample is under its free-disk threshold has its idle pool worktrees pruned once per low-disk episode (`dispatch.pruned`, through an `IdlePool`).
- **Health:** a host unhealthy for `unhealthy_after` (10 minutes) raises one decision (`dispatch.host_unhealthy`), closed by `dispatch.host_recovered`.

## Hosts

`quark_hosts::EventHosts` is the `HostRegistry`: registrations and health changes are `host.change` events, so the host list survives restarts.

## Durability

The dispatcher holds no state the log does not have. A new dispatcher on the same log resolves requests a crash left unresolved and, for a placement never confirmed, asks the host whether it has the task before spawning it again, so a task is never started twice.
The supervisor records `Queued` itself when it takes a task, so a task waiting in dispatch shows in the `dispatch.*` events, not yet in the task ledger.

## Shadow mode

Slice 5 cannot switch on before slices 1 to 4, so firstmate still resolves and spawns every worker. With `QUARK_NATIVE_DISPATCH=1`, quarkd:

- registers this host and records a telemetry sample every minute;
- re-runs every dispatch resolution firstmate makes on the native resolver, with the classifier answer firstmate got (the classifier is asked once) and a fresh quota snapshot, and appends a `shadow.divergence` event (slice `dispatch`, operation `resolve_dispatch`) whenever the status, the selected profile or any candidate's eligibility differ.

`crates/quark-dispatch/tests/parity.rs` holds the native resolver to firstmate's decisions on recorded cases; `tests/parity/record/record.py <firstmate checkout>` re-records them by running `fm-dispatch-resolve.sh` with stub `curl` and `quota-axi`.

Admission has no bash counterpart to compare against, so it is exercised by the dispatcher's tests until the slice switches on.

## Not yet

- The native `EngineAdapter` for `resolve_dispatch`, `resolve_description` and `set_crew_dispatch`, and the dispatcher running for real, once slices 1 to 4 are native.
- An `Accounts` source over quarkd's accounts store, and an `IdlePool` over treehouse's prune, for the native path.
- Spawners for SSH and hosted runtimes (slice 7).
- Recording the task as `Queued` in the ledger while it waits in dispatch needs a small `quark-core` amendment so the supervisor accepts a queued task.

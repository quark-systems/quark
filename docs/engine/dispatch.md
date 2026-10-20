# Dispatch, pools and admission control (slice 5)

`crates/quark-dispatch` is the native replacement for firstmate's dispatch intake: `fm-dispatch-resolve.sh` (which agent runs a task), quarkd's account leases (which account it runs under), and the admission control firstmate never had (which host it runs on, or whether it waits).
It sits in front of the slice 4 supervisor (`docs/engine/supervision.md`) and keeps every decision in the event log.

## The path of one task

1. **Request.** `Dispatcher::submit` records `dispatch.requested` with the task's title, brief, repo, branch, isolation and environment. A request may name a profile itself; then the rules are skipped.
2. **Resolve.** The Project's rules (`dispatch.yaml`, compiled to `crew-dispatch.json`) are resolved exactly as firstmate resolves them, and the result is recorded as `dispatch.resolved`:
   - the classifier (the System-1 API; `classify::SystemOne`) picks a rule or `default` from the brief, with a confidence;
   - a rule needing approval, or whose floor cannot be verified, escalates; a rule floor below falls through to the default profiles; below the confidence floor the result is `ambiguous`, unless `on_failure: default` sends it (and any classifier failure) to the default profiles;
   - each candidate is judged on one `quota-axi --json` snapshot, plus IBM Bob's and Kiro CLI's own readings ([Bob and Kiro quota](#bob-and-kiro-quota)): exhausted runway, 0% left without overage, or a profile floor below rule it out; missing or unverifiable evidence leaves it eligible but unranked; `pricing: budget` needs no quota;
   - `ordered` takes the first eligible candidate (escalating if its evidence is unverifiable), `quota-balanced` takes the highest spend priority, a budget candidate only when none ranks, and escalates on a tie.
3. **Choose.** A clear resolution picks an account from the profile's pool and records `dispatch.chosen`. Pool rules are quarkd's: sticky while the account stays launchable and in the pool, otherwise the ready account with the fewest running tasks, then the most quota left. Anything not clear (off, ambiguous, escalated, error, an empty or exhausted pool) is `dispatch.escalated` until the coordinator calls `Dispatcher::choose` with a profile.
4. **Admit.** Each `Dispatcher::tick` (oldest first) asks the red-main guard (`MergeGuard::may_dispatch`) and then admission control for a host. A held task records `dispatch.held` once per distinct reason.
5. **Place and spawn.** A placed task records `dispatch.placed` and goes to that host's `Spawner` (the local supervisor; other hosts arrive with their runtimes), then `dispatch.spawned` or `dispatch.spawn_failed`.

A task can be taken back with `Dispatcher::withdraw` until it is placed.

## Bob and Kiro quota

quota-axi reports neither IBM Bob nor Kiro CLI, so `quark_dispatch::local` reads them itself (ports of firstmate's `fm-bob-quota.sh` and `fm-kiro-quota.sh`), each as a one-provider schema-5 snapshot merged when quota-axi lacks the provider. Both harnesses' manifests name their provider (`[quota]`), so a profile needs no `provider`. A reading that cannot be had leaves the provider unmeasured (eligible, unranked); it never invents a number. Both rows carry quota-axi's own `spendPriority` for the month, so they rank against quota-axi's rows under `quota-balanced`.

`quarkd quota bob` and `quarkd quota kiro` print the reading in one line (`--json` for the snapshot dispatch sees).

**Kiro** runs `kiro-cli chat --v2 --no-interactive /usage` in `<home>/run/kiro-usage-probe` (12 s bound), which prints the plan's credits without a model call. It always passes `--v2`: under `--v3` the same text is a prompt, and spends credits. `<home>/quota/kiro.json` is optional:

```json
{ "overage_allowed": true }
```

Overage is a budget, never a block: when it is allowed (that file, or `/usage` showing use past the plan) the row never reads exhausted, its runway is `through_reset`, and use past the plan drives `spendPriority` negative. Without it, a spent plan is `exhausted_now` and a month's pace that runs out before the reset is `projected_exhaustion`. A profile floor still applies either way.

**Bob** reads, in order:

1. the live Bob admin API when `BOBSHELL_API_KEY` is set in quarkd's environment (the profile, then each team's usage, summed; the key goes to curl on stdin, never on its command line), which is exact;
2. otherwise this machine's ledger, the per-session coins in `~/.bob/db/bob.db` (opened read-only), plus an anchor: one "Monthly Usage" figure from Bob's `/status`, recorded with `quarkd quota bob --anchor <coins> [--budget <coins>]`. The estimate is the anchor plus ledger growth since, while the anchor is from this charge month and younger than `anchor_max_age_hours`. Without a fresh anchor the reading is `partial`: a floor on Bob is unverifiable, so an `ordered` list escalates rather than skipping past Bob, and a ledger that alone reaches the allowance reads `exhausted_now`.

`<home>/quota/bob.json` is optional; an unknown key is refused:

| Field | Default | |
|---|---|---|
| `monthly_coins` | `1500` | the allowance when the API reports none |
| `reset_day` | `1` | 1 to 28 |
| `reset_timezone` | `UTC` | `UTC` or a fixed offset such as `+05:30` |
| `anchor_max_age_hours` | `24` | |

firstmate's opt-in per-session cap (`spawn_max_cost`) has no setting here: put `--max-cost <coins>` on `launch.argv` in a `<home>/harnesses/bob.toml` copy of the built-in manifest instead.

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

## Parity with firstmate

The native resolver was compared with firstmate's on every dispatch while slice 5 ran in shadow; quarkd no longer runs that comparison.
`crates/quark-dispatch/tests/parity.rs` still holds the resolver to firstmate's decisions on recorded cases; `tests/parity/record/record.py <firstmate checkout>` re-records them by running `fm-dispatch-resolve.sh` with stub `curl` and `quota-axi`.

With `--engine native`, quarkd registers this host and records a telemetry sample every minute, and the native engine serves `resolve_dispatch`, `resolve_description` and `set_crew_dispatch` from the dispatcher (`crates/quarkd/src/engine/native_dispatch_engine.rs`), with accounts from quarkd's accounts store.

## Not yet

- An `IdlePool` over treehouse's prune, for the native path.
- Spawners for SSH and hosted runtimes (slice 7).
- Recording the task as `Queued` in the ledger while it waits in dispatch needs a small `quark-core` amendment so the supervisor accepts a queued task.
- Anchoring Bob from a throwaway `/status` session, as firstmate's `fm-bob-quota.sh anchor --from-status` did; type the figure in with `--anchor` for now.
- Bob and Kiro in the Accounts view: it lists harnesses with per-account config directories only.

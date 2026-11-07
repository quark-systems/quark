# Worktree pool (slice 9)

`crates/quark-worktree` gives every task an isolated git worktree through the `WorktreeProvider` contract (`docs/engine/contracts.md`).
`TreehouseProvider` owns the safety rules (the isolation assertion, the tangle check, the landed-work check before a return) and leases worktrees from a `Pool` underneath it.
`NativePool` is that pool: a Rust port of treehouse 3.1.2 on treehouse's own pools. Treehouse itself (`TreehouseCli`) remains as a fallback, and `ShadowPool` can still compare the two.

## Choosing the pool

quarkd makes one pool for the daemon (`crates/quarkd/src/native_worktrees.rs`), chosen by `QUARK_WORKTREE_POOL`:

| Value | Pool |
|---|---|
| unset, `native` | the native pool; treehouse is not needed |
| `treehouse` | treehouse from `PATH` (needs 3.1.2 or a later 3.x) |
| `shadow` | treehouse serves, the native pool is compared (below) |

`QUARK_NATIVE_WORKTREES=1`, the flag from before the switch, still means `shadow` when `QUARK_WORKTREE_POOL` is unset.
Because both work on the same pool state, switching between them needs no migration: restart quarkd with the other value.

## The native pool

`NativePool` is a Rust port of the parts of treehouse Quark drives, working on treehouse's own pools:

- **Same pool.** The pool directory is resolved as treehouse resolves it (`<root>/.treehouse/<repo>-<sha256(origin url)[..3]>`, with `--root`, `TREEHOUSE_ROOT` and `root` from `treehouse.toml` or `~/.config/treehouse/config.toml`), and slots sit at `<pool>/<n>/<repo>`.
- **Same state.** It reads and writes `treehouse-state.json` (format 4) under the same `flock` on `treehouse-state.lock`, signing each entry's seeded-file inventory with the HMAC-SHA256 key in `treehouse-state.key` exactly as treehouse does. Unknown fields are kept. Treehouse and the native pool can therefore take turns on one pool: each reads, releases and reuses what the other leased, which is what lets slice 9 switch on (and roll back) with no migration.
- **Same rules.** Acquisition fetches origin, resolves the base (an explicit `--base` or `base_branch` must exist; otherwise origin's HEAD), and reuses the first slot that is unleased, unreserved, git, of this clone, idle (no process working in it), clean, and whose HEAD is merged into the base (or into the base it was parked on), resetting it under git's `HEAD.lock`; otherwise it creates a slot up to `max_trees`. A requested branch is created and checked out and verified. Release requires the lease id to still match, never discards a dirty worktree (it is left as it is, treehouse's "not returned"), stops processes still working there, and parks the slot on its base. Reads fail safe: an entry whose digest does not authenticate, a slot directory the state does not list, and a state file that does not parse come back quarantined, and recovered slots are freed under the lock only when proven safe.

- **Prune and destroy.** `prune` removes the stale slots of a pool: unleased, unreserved, nothing running in them, clean, backing repository present, and HEAD merged into origin's default branch (refused when the local tracking ref is stale) or into the base the slot was cut from. A single-slot `destroy` removes a disposable slot; a leased, in-use or unlanded one needs its own opt-in (`DestroyOptions`, treehouse's `--include-*` flags). Both use treehouse's two-phase reservation (mark `destroying` under the lock, run `pre_destroy` hooks unlocked, re-check and remove only if the reservation is intact), `git worktree remove`, then delete the numbered slot directory. Busy slots are skipped; the rest of what was kept comes back with a reason.

quarkd uses them in two places. When a host runs low on disk, dispatch's `IdlePool` prunes every Project repo's pool once per episode (`dispatch.pruned`). When a Project is deleted, its clones' slots are destroyed (only disposable ones) before the clones are, so none is left pointing at a deleted repository.

Not ported yet, and refused with an error naming the setting and the fallback (`QUARK_WORKTREE_POOL=treehouse`): jj workspaces, `worktree_path` templates, `unique_leaf`, APFS sharing, `post_create` hooks, and seeding ignored files from `.worktreeinclude`. Also not ported: interactive acquisition (firstmate's `treehouse get` subshell; its owner reservations are still read and respected), `prune --prune-orphans`, `destroy --all` (it crosses clones sharing a pool; quarkd retires one clone's slots by name instead), and backing up untracked files of a recovered slot (such a slot stays quarantined until a person returns it by name).
The native pool records its own owner reservation to the second; treehouse compares them to the millisecond, so treehouse running at the same moment as a native prune would not see that prune's reservation as alive.

## Shadow

`QUARK_WORKTREE_POOL=shadow` makes the daemon's pool a `ShadowPool`: treehouse answers every call, and the native pool is asked what it would have done on the same pool. It was the switch-on check for slice 9 and is kept as a way to look for differences on a real machine.

| Call | Compared |
|---|---|
| acquire | the slot the native pool would reuse or create, or that it would refuse (`NativePool::plan_acquire`, from local refs, before treehouse acts) |
| release | return, leave as is, or refuse (`NativePool::plan_release`, before treehouse acts) |
| list | each slot's status, branch, lease id and holder, read without writing |

Each disagreement is appended to the event log as `shadow.divergence` (slice `worktree_pool`), once until it changes. The native side never acts in shadow.
Prune and destroy are not compared (treehouse prints no plan); treehouse does them. quarkd only compares with treehouse 3.1.2 or a later 3.x; against an older one, or none, it says so and runs the native pool alone.

## Tests

`tests/native_pool.rs` runs the provider over the native pool on real repositories. `tests/native_parity.rs` runs the native pool against the real treehouse 3.1.2 binary on one pool: each reads what the other wrote, takes over what the other leased, plans match what treehouse then does, and a whole provider lifecycle through the shadow records no divergence; prune and destroy remove exactly what treehouse's remove from the same pool, whichever of the two made it. It needs treehouse (`TREEHOUSE_BIN`, else `treehouse` on `PATH`) and passes vacuously without it; CI installs the pinned release.

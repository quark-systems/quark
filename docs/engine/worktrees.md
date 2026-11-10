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

- **Same pool.** The pool directory is resolved as treehouse resolves it (`<root>/.treehouse/<repo>-<sha256(origin url)[..3]>`, with `--root`, `TREEHOUSE_ROOT` and `root` from `treehouse.toml` or `~/.config/treehouse/config.toml`), and new slots are placed as treehouse places them: `<pool>/<n>/<repo>`, `<pool>/<n>/<repo>-<n>` with `unique_leaf`, or a `worktree_path` template (`{pool}`, `{slot}`, `{repo}`, `{repo_parent}`, `$VAR`) under the same rules (absolute, never in the repository, `{pool}/{slot}/<name>` inside the pool, an existing directory is never adopted and the next slot name is tried). A slot keeps its recorded path whatever the settings say later.
- **Same state.** It reads and writes `treehouse-state.json` (format 4) under the same `flock` on `treehouse-state.lock`, signing each entry's seeded-file inventory with the HMAC-SHA256 key in `treehouse-state.key` exactly as treehouse does. Unknown fields are kept. Treehouse and the native pool can therefore take turns on one pool: each reads, releases and reuses what the other leased, which is what lets slice 9 switch on (and roll back) with no migration.
- **Same rules.** Acquisition fetches origin, resolves the base (an explicit `--base` or `base_branch` must exist; otherwise origin's HEAD), and reuses the first slot that is unleased, unreserved, git, of this clone, idle (no process working in it), clean, and whose HEAD is merged into the base (or into the base it was parked on), resetting it under git's `HEAD.lock`; otherwise it creates a slot up to `max_trees`. Before it is handed out, the slot is seeded from `.worktreeinclude` (below), a requested branch is created, checked out and verified, and a fresh slot with `apfs_sharing = "fresh"` shares tracked files with the primary checkout (below). The user's `post_create` hooks then run in the slot with the pool unlocked. Release requires the lease id to still match, never discards a dirty worktree (it is left as it is, treehouse's "not returned"), stops processes still working there, and parks the slot on its base. Reads fail safe: an entry whose digest does not authenticate, a slot directory the state does not list, and a state file that does not parse come back quarantined, and recovered slots are freed under the lock only when proven safe; a recovered slot's untracked files are first moved (never deleted or overwritten) into the owner-only folder `<pool root>/treehouse-recovered-backup-<pool>-<slot>`, which the status listing shows as `recovery_backup`.

- **`.worktreeinclude`.** The `.worktreeinclude` committed at the slot's commit is read as a git exclude file (negation, escaping and `**` included), and every file of the primary checkout that it selects and git ignores is copied into the slot, unless that path is tracked there. An uncommitted `.worktreeinclude` is never read. The copied paths are the slot's seed inventory, signed in the state; exactly those are removed before the slot is reset for reuse or returned (`git clean -fd` leaves every other ignored file alone, as in treehouse). A seeding that fails part way removes what it copied and quarantines the slot.
- **Hooks.** `post_create` and `pre_destroy` come only from `~/.config/treehouse/config.toml` (a repository's own `treehouse.toml` never supplies them) and run with `/bin/sh -c` in the slot; a failing hook is logged and does not stop the rest or the operation.
- **APFS sharing.** `apfs_sharing = "fresh"` (or `TREEHOUSE_APFS_SHARING`) replaces each tracked file of 64 KiB or more in a newly created slot that is byte-identical to the primary checkout's with a copy-on-write clone of it (macOS, APFS, one volume; elsewhere the pass is skipped and logged). Anything unusual is kept as an ordinary copy: hardlinks, other owners, flags or setuid bits, sparse or compressed files, ACLs, extended attributes, differing content, or a git hook, fsmonitor or checkout filter that could be writing. A slot that changed during the pass is quarantined.

- **Prune and destroy.** `prune` removes the stale slots of a pool: unleased, unreserved, nothing running in them, clean, backing repository present, and HEAD merged into origin's default branch (refused when the local tracking ref is stale) or into the base the slot was cut from. A single-slot `destroy` removes a disposable slot; a leased, in-use or unlanded one needs its own opt-in (`DestroyOptions`, treehouse's `--include-*` flags). Both use treehouse's two-phase reservation (mark `destroying` under the lock, run `pre_destroy` hooks unlocked, re-check and remove only if the reservation is intact), `git worktree remove`, then delete the numbered slot directory. Busy slots are skipped; the rest of what was kept comes back with a reason.

quarkd uses them in two places. When a host runs low on disk, dispatch's `IdlePool` prunes every Project repo's pool once per episode (`dispatch.pruned`). When a Project is deleted, its clones' slots are destroyed (only disposable ones) before the clones are, so none is left pointing at a deleted repository.

Not ported, and refused with an error naming the setting and the fallback (`QUARK_WORKTREE_POOL=treehouse`): jj workspaces (experimental in treehouse; Quark's provider works in git terms). Also not ported, because Quark never asked treehouse for them: interactive acquisition (firstmate's `treehouse get` subshell; its owner reservations are still read and respected), `get --include-file` manifests, `prune --prune-orphans` (it deletes slots whose content cannot be verified), and `destroy --all` (it crosses clones sharing a pool; quarkd retires one clone's slots by name instead). Where the APFS pass differs from treehouse's: a file with extended attributes is kept rather than cloned with its attributes copied over, and the report counts logical bytes only.
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

`tests/native_pool.rs` runs the provider over the native pool on real repositories, and `tests/native_hooks.rs` the user's hooks (its own binary, as it sets `HOME`). `tests/native_parity.rs` runs the native pool against the real treehouse 3.1.2 binary on one pool: each reads what the other wrote, takes over what the other leased, plans match what treehouse then does, and a whole provider lifecycle through the shadow records no divergence; prune and destroy remove exactly what treehouse's remove from the same pool, whichever of the two made it; both seed the same `.worktreeinclude` files and clean each other's, place `unique_leaf` and `worktree_path` slots alike, and back a recovered slot's untracked files up into the same folder. It needs treehouse (`TREEHOUSE_BIN`, else `treehouse` on `PATH`) and passes vacuously without it; CI installs the pinned release.

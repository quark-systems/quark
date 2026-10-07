# Worktree pool (slice 9)

`crates/quark-worktree` gives every task an isolated git worktree through the `WorktreeProvider` contract (`docs/engine/contracts.md`).
`TreehouseProvider` owns the safety rules (the isolation assertion, the tangle check, the landed-work check before a return) and leases worktrees from a `Pool` underneath it.
Treehouse 3.1.2 (`TreehouseCli`) is that pool today; `NativePool` is its replacement, and `ShadowPool` compares the two until slice 9 switches on.

## The native pool

`NativePool` is a Rust port of the parts of treehouse Quark drives, working on treehouse's own pools:

- **Same pool.** The pool directory is resolved as treehouse resolves it (`<root>/.treehouse/<repo>-<sha256(origin url)[..3]>`, with `--root`, `TREEHOUSE_ROOT` and `root` from `treehouse.toml` or `~/.config/treehouse/config.toml`), and slots sit at `<pool>/<n>/<repo>`.
- **Same state.** It reads and writes `treehouse-state.json` (format 4) under the same `flock` on `treehouse-state.lock`, signing each entry's seeded-file inventory with the HMAC-SHA256 key in `treehouse-state.key` exactly as treehouse does. Unknown fields are kept. Treehouse and the native pool can therefore take turns on one pool: each reads, releases and reuses what the other leased, which is what lets slice 9 switch on (and roll back) with no migration.
- **Same rules.** Acquisition fetches origin, resolves the base (an explicit `--base` or `base_branch` must exist; otherwise origin's HEAD), and reuses the first slot that is unleased, unreserved, git, of this clone, idle (no process working in it), clean, and whose HEAD is merged into the base (or into the base it was parked on), resetting it under git's `HEAD.lock`; otherwise it creates a slot up to `max_trees`. A requested branch is created and checked out and verified. Release requires the lease id to still match, never discards a dirty worktree (it is left as it is, treehouse's "not returned"), stops processes still working there, and parks the slot on its base. Reads fail safe: an entry whose digest does not authenticate, a slot directory the state does not list, and a state file that does not parse come back quarantined, and recovered slots are freed under the lock only when proven safe.

Not ported yet, and refused rather than done differently: jj workspaces, `worktree_path` templates, `unique_leaf`, APFS sharing, `post_create` hooks, and seeding ignored files from `.worktreeinclude`. Also not ported: interactive acquisition (firstmate's `treehouse get` subshell; its owner reservations are still read and respected), `prune`, `destroy`, and backing up untracked files of a recovered slot (such a slot stays quarantined until a person returns it by name).

## Shadow

Slice 9 switches on last. Until then, `QUARK_NATIVE_WORKTREES=1` makes the pool behind quarkd's worktree providers (the Hosts view's pool reports every minute, and the native supervisor's worktrees when `QUARK_NATIVE_SUPERVISOR=1`) a `ShadowPool`: treehouse answers every call, and the native pool is asked what it would have done on the same pool.

| Call | Compared |
|---|---|
| acquire | the slot the native pool would reuse or create, or that it would refuse (`NativePool::plan_acquire`, from local refs, before treehouse acts) |
| release | return, leave as is, or refuse (`NativePool::plan_release`, before treehouse acts) |
| list | each slot's status, branch, lease id and holder, read without writing |

Each disagreement is appended to the event log as `shadow.divergence` (slice `worktree_pool`), once until it changes. The native side never acts in shadow.

## Tests

`tests/native_pool.rs` runs the provider over the native pool on real repositories. `tests/native_parity.rs` runs the native pool against the real treehouse 3.1.2 binary on one pool: each reads what the other wrote, takes over what the other leased, plans match what treehouse then does, and a whole provider lifecycle through the shadow records no divergence. It needs treehouse (`TREEHOUSE_BIN`, else `treehouse` on `PATH`) and passes vacuously without it; CI installs the pinned release.

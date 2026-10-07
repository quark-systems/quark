# Verification and merge (slice 2)

`crates/quark-verify` is the native side of slice 2: the gates, rebase and re-verify, the red-main guardrail, and merge permission.
It implements `VerifyPipeline` and `MergeGuard` from `quark-core` and runs in shadow mode beside firstmate's guardrail (firstmate's `bin/fm-pr-merge.sh`, `bin/fm-spawn.sh` and `bin/fm-main-health-lib.sh`).

## The rules

`quark_verify::rules` holds them as pure functions, shared by the native guard and the shadow:

1. A merge's head must contain the base branch's current tip. Otherwise rebase, verify the rebased head, and push it with `--force-with-lease`.
2. When the Project declares gates, they must have passed on exactly that head.
3. While main is red, only a change whose own run of every failing check is green may merge, and new dispatch is paused except one fix-main task. Work already running continues.
4. Only a person overrides rule 3, by answering the red-main episode's one decision. The override lasts until main is green again.
5. A merge refuses when main's health can't be read; a dispatch goes ahead.

Main's health is read from its tip: the newest run of each check and every commit status. A failure, timeout, startup failure or required action is red; cancelled, neutral, skipped and stale are not. Otherwise anything running is pending, any checks are green, and no checks at all count as green.
An episode opens on the first red read and closes on the first green one; a pending or unreadable read keeps it open with its recorded failing checks.

## Pieces

| Type | Does |
|---|---|
| `NativePipeline` | Runs a Project's gates (`checks`, `journeys`, `holdout`, from firstmate's `config/gates.json`, schema `fm.gates.v1`) on a head in a scratch worktree. `rebase_and_reverify` rebases onto `origin/<base>` first and reports a conflict instead of a verdict. It pushes the rebased head only when `push_rebased` is set. Holdout summaries name only the category and whether it passed. |
| `NativeGuard` | `main_health`, `may_merge`, `may_dispatch` over live GitHub reads through `gh`. Episodes are `verify.red_main` events and are rebuilt by `restore()`. The caller files the episode's decision for a person and reports it with `decision_filed`; the answer comes back through `answer`. |
| `ShadowVerifier` | Slice 2's shadow, below. |

## Events

| Kind | Payload |
|---|---|
| `verify.red_main` | `EpisodeChange`: an episode opened, updated, closed, got its decision or its fix-main task |
| `verify.override` | `Override`: a person answered a decision |
| `verify.decision` | `GuardDecision`: one native merge or dispatch answer |
| `verify.shadow` | `ShadowCheck`: one firstmate decision checked in shadow mode |
| `shadow.divergence` | `Divergence` for slice `verification` |

## Shadow mode

Start quarkd with `QUARK_ENGINE_SLICES=1=shadow,2=shadow`. firstmate keeps deciding and acting; nothing native acts.

firstmate appends every guardrail decision, with the inputs it was made on, to `state/guard-decisions.jsonl` in the Project's home (schema `fm.guard.v1`). quarkd's `verify_shadow` reads new lines on each refresh and makes two comparisons per line:

- **Rules** (`may_merge`, `may_dispatch`): native's rules on the recorded inputs must give the same permit and reason.
- **Reads** (`head_fresh`, `main_health`, `pr_green`): native re-reads, through its own forge, the facts that are fixed once known: whether the recorded head contains the recorded tip, the checks on the recorded main tip, and which of main's failing checks passed on the pull request's head. A failed read is listed as unverified, not as a divergence.

Each line produces one `verify.shadow` event and one `shadow.divergence` event per disagreement. Event ids derive from the line's position in the file and the read position is checkpointed with them, so every line is compared exactly once.

Slice 2 moves to `native` when the shadow shows no divergences over real merges and the verify-quark journeys pass, after slice 1 is native.

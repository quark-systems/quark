# Verification and merge (slice 2)

`crates/quark-verify` is the native side of slice 2: the gates, rebase and re-verify, the red-main guardrail, and merge permission.
It implements `VerifyPipeline` and `MergeGuard` from `quark-core`, ported from firstmate's guardrail (`bin/fm-pr-merge.sh`, `bin/fm-spawn.sh` and `bin/fm-main-health-lib.sh`), and the native engine merges through it.

## The rules

`quark_verify::rules` holds them as pure functions, shared by the native guard and the guarded merge:

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
| `GuardedMerge` | The native engine's merge: reads the pull request, runs `rebase_and_reverify` with `push_rebased`, requires the pull request's own checks green, asks `NativeGuard`, then merges exactly the verified head through the forge. A rebased head is pushed and refused until its checks run. Also records standing approval. |

## Events

| Kind | Payload |
|---|---|
| `verify.red_main` | `EpisodeChange`: an episode opened, updated, closed, got its decision or its fix-main task |
| `verify.override` | `Override`: a person answered a decision |
| `verify.decision` | `GuardDecision`: one native merge or dispatch answer |
| `verify.merged` | `Merged`: a task's pull request merged; `outside` when it merged on the forge (a person, or its worker) and the PR center told the engine |
| `verify.standing_approval` | `StandingApproval`: green pull requests for a repo merge without asking, or stop doing so |

Logs from while slice 2 ran in shadow beside firstmate may also hold `verify.shadow` and `shadow.divergence` events; nothing reads them now.

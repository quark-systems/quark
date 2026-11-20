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

### Refusals that end on their own

A refused merge says whether the same merge can go through later with no one acting on the pull request (`Outcome::Refused { retry }`, `EngineError::MergeRefused` in quarkd):

- **Rebased and pushed.** The head lacked the base's tip, the gates passed on the rebased head, and that head waits for the forge's checks.
- **Checks still running** on the head.
- **Main moved** while the gates ran (`stale_head`).
- **A forge read failed** (`head_unreadable`, `main_unknown`). The guard reads the forge for main's tip, main's checks and whether the head contains that tip; it never reads the clone for this. It reads up to three times, two seconds apart, before it refuses, and the refusal names the read that failed and the forge's error.

Every other refusal (failed gates or checks, a conflict, a closed pull request, a red main) stays until the worker or a person acts. A read or write that fails before the rules run, such as a `gh` call or a fetch of the base branch into the clone, is an error, not a refusal.

Standing approval (the PR center) tries a head once and logs a lasting refusal as a warning. A refusal that ends on its own, and a failed read or write, is logged as information and the head is tried again: after a minute, doubling up to fifteen. From the third such try of one head each is logged as a warning, so trouble that does not pass is still reported. A rebased head is a new head, tried as soon as the forge shows it mergeable and green.

### Landing on the default branch

A merge counts as landed only when its change reaches the repository's default branch, which the PR center reads from the forge. A pull request stacked on another branch lands when that branch's own pull request merges into the default branch at or after it. Until then its task stays open and nothing tells the coordinator it merged.

- **Stranded.** It merged after its base branch's pull request had already merged, or no pull request of that branch remains open. The PR center records `verify.stranded` once, which wakes the coordinator to land the change on the default branch.
- **Before merge.** The worker is told once to retarget an open pull request whose base branch's pull request already merged. Quark's merge refuses it (`off_default_branch`), and standing approval merges only pull requests based on the default branch.
- **Unknown default branch.** A row whose default branch was never read counts as landed, as before.

## Pieces

| Type | Does |
|---|---|
| `NativePipeline` | Runs a Project's gates (`checks`, `journeys`, `holdout`, from the Project workspace's `config/gates.json`, schema `quark.gates.v1`, or the older `fm.gates.v1`) on a head in a scratch worktree. `rebase_and_reverify` rebases onto `<remote>/<base>` first and reports a conflict instead of a verdict. The guarded merge picks the remote whose URL is the pull request's repo, else `origin`, and refuses a remote that lacks the forge's tip; when the tip is missing because the fetch failed, that is an error naming the fetch. It pushes the rebased head only when `push_rebased` is set. Holdout summaries name only the category and whether it passed. |
| `NativeGuard` | `main_health`, `may_merge` (`decide` for the rule that decided), `may_dispatch` over live GitHub reads through `gh`. A merge reads again after a failed read. Episodes are `verify.red_main` events and are rebuilt by `restore()`. The caller files the episode's decision for a person and reports it with `decision_filed`; the answer comes back through `answer`. |
| `GuardedMerge` | The native engine's merge: reads the pull request, runs `rebase_and_reverify` with `push_rebased`, requires the pull request's own checks green, asks `NativeGuard`, then merges exactly the verified head through the forge. A rebased head is pushed and refused until its checks run, except when the old head had no checks at all: then it merges the rebased head once the forge reports it. Also records standing approval. |

## Events

| Kind | Payload |
|---|---|
| `verify.red_main` | `EpisodeChange`: an episode opened, updated, closed, got its decision or its fix-main task |
| `verify.override` | `Override`: a person answered a decision |
| `verify.decision` | `GuardDecision`: one native merge or dispatch answer |
| `verify.merged` | `Merged`: a task's pull request merged; `outside` when it merged on the forge (a person, or its worker) and the PR center told the engine |
| `verify.stranded` | `Stranded`: a pull request merged into a branch other than the default, and nothing will carry its change there |
| `verify.standing_approval` | `StandingApproval`: green pull requests for a repo merge without asking, or stop doing so |

Logs from while slice 2 ran in shadow beside firstmate may also hold `verify.shadow` and `shadow.divergence` events; nothing reads them now.

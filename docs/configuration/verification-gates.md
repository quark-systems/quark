# Verification gates

Verification gates (ADR-15, `crates/quarkd/src/gates.rs`) are what a task must pass before its pull request opens, and again on the rebased head before it merges.

## Declaring them

Gates are declared per source repo in the Project repo's `project.yaml`, under `verification`: repo checks (commands such as a formatter, linter and test suite) and Playwright journeys.
Each refresh compiles that declaration into the workspace's `config/gates.json`; workers run the gates before opening a PR, and the evidence lands in `state/<task-id>.gates.json` for the [PR center](../api/pull-requests.md).

[`selfhost/project.yaml`](../../selfhost/project.yaml) is a working example: rustfmt, clippy, `cargo test`, the app build and unit tests, and the app's Playwright suite.

## Holdout tests

Holdout tests live in the Project repo under `holdout/<source>/`, laid out as `<repo>/<category>/run`.
Workers never see them, so never keep them in a source repo, where every worker would.
`selfhost/project-init.sh --holdout <dir>` copies them in from a directory outside any worktree.

## At merge time

A merge rebases the pull request onto its base branch, re-runs the gates, and refuses while main is red unless the change fixes every failing check. See [Verification and merge](../engine/verify.md).

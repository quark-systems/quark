# Concepts

## The pieces

- **Project**: the unit of work. A goal, a definition of done, one or more source repos, and a Project repo (its own git repo) holding instructions, configuration and memory. Every Project has its own workspace under `~/.quark/workspaces/`.
- **Coordinator**: one agent per Project that plans, splits work into tasks, briefs workers, answers them and decides what reaches you. It does not edit code itself. The deterministic lifecycle around it (spawn, watch, recover, rebase, merge) is Rust; see [the coordinator](../engine/coordinator.md).
- **Worker**: an agent running one task in its own git worktree, under a harness (Claude Code, Codex, ...), a model and an account chosen by dispatch.
- **Decision**: a question or hold that waits on a person. All of them, from every Project, collect in Decisions. See the [Decisions API](../api/decisions.md).
- **Verification gates**: what a task must pass before its PR opens and again before it merges: repo checks, Playwright journeys and holdout tests. See [Verification gates](../configuration/verification-gates.md).
- **PR center**: every pull request a task opens, with its checks, reviews and gate evidence. See the [Pull requests API](../api/pull-requests.md).
- **Memory**: what finished tasks learned, proposed to you with evidence; accepted entries are commits in the Project repo, and can be promoted to user-level memory. See the [Memory API](../api/memory.md).
- **Dispatch**: rules that pick the harness, model and account for each task. See [Dispatch rules](../configuration/dispatch-rules.md).
- **Issues**: a Project's Beads issues, one database per Project, kept outside its repos and synced with issue trackers only through rules you set.

The app's own words for these follow [`app/GLOSSARY.md`](../../app/GLOSSARY.md).

## Why "Quark"

Quarks never exist alone: gluons bind them into hadrons. Quark borrows that picture.

| Physics | Quark | What it is |
| :--- | :--- | :--- |
| Quark | Worker | one autonomous agent in an isolated worktree or sandbox |
| Gluon | Control plane | the daemon, event log and coordinator that bind workers, state and memory together |
| Hadron | Project | the goal-driven workspace: repos, backlog, holdout tests, memory and merged PRs |
| Flavors | Harnesses | the agent CLIs workers run on |
| Color charge | Isolation | the boundaries between the desktop, sandboxed processes and (later) cloud sandboxes |

## Local today, cloud later

Quark runs in Local mode: everything on your machine, workers as native processes or sandboxed with Seatbelt (macOS) or bubblewrap (Linux), and optionally on SSH hosts through sub-coordinators.
Cloud mode (a microVM sandbox per Project, container isolation) is planned and not built yet.

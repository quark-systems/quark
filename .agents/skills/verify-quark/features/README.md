# Quark verification map

This directory is the maintained source for verifying Quark's user-facing behavior. Read this index before driving, then use the matching feature file as the recipe.

## Baseline preconditions

- A run is launched with `quark-verify launch` in the mode the feature needs, and `quark-verify doctor` passes.
- The run's daemon home is private (`QV_HOME`), so the run starts with no Projects except in `--daemon mock`, which serves the demo Projects `Quark MVP` (`quark`) and `Website refresh` (`website`).
- Never drive an instance this run did not launch, and never the user's own `~/.quark` daemon.

## Driving conventions

- `Q=.agents/skills/verify-quark/bin/quark-verify`; every command below is literal.
- Start every recipe from `$Q browser open "<route>"` so a previous recipe's state does not leak in.
- Prefer ARIA roles and names, then labels, then test ids; read `$Q browser snapshot` when a handle is unclear.
- Read state back through `$Q api` and `$Q events`, never through page scripts; the only page hook used is the read-only `browser terminal`.

## Proof and skip reporting

- UI proof is an ARIA snapshot or screenshot before and after the action, saved under `<feature-id>/`.
- Side-effect proof is the resource read back from `/v1`, the events the action emitted, and files or commits under `QV_HOME` when the feature writes them.
- Every proof names the run id, `QV_COMMIT`, the mode, the feature id and the entry point used.
- A proof against the mock or the stub engine says so.
- An unreachable path is reported with the command tried and the unmet prerequisite.

## Feature entry contract

Each feature file starts with an H1 title and one paragraph describing the user-visible behavior, then exactly four H2 sections in this order: `Sub-features`, `How to get to it (user POV)`, `Driving it with quark-verify`, `Gotchas`.
Keep implementation details out; name user paths, stable handles, required state, commands and observable proof.

## Features and journey coverage

The journeys gate (ADR-15, `verification.journeys` in the Project's `project.yaml`) runs `app/e2e/*.spec.ts` against the mock daemon.
This table links each feature to the journey tests that script it and the modes a live proof can use; a `none` row is a journey still to write, and a new journey names the feature id it covers.

| Feature | Journey | Scripted by (`app/e2e/app.spec.ts`) | Live modes | Prerequisites |
| :--- | :--- | :--- | :--- | :--- |
| [Create a Project](create-project.md) | J2 | "creates a project and lands on its board" | real+stub, real+firstmate, mock | firstmate: a cloneable repo; `no-mistakes` for gated delivery |
| [Project board and coordinator chat](board-and-chat.md) | J3 | "the board updates live when the coordinator queues a task", "command palette jumps to a task" | mock; real+firstmate | firstmate: a signed-in coordinator harness |
| [Worker terminal](worker-terminal.md) | J4 | "worker view: terminal, steering, transcript, changes, cancel and relaunch", "worker view: why this agent ...", "a queued task explains ..." | mock; real+firstmate | firstmate: a running task window |
| [Decisions inbox](decisions.md) | J5 | "decisions inbox: answer from the keyboard, then see who answered" | mock; real+firstmate | firstmate: an open hold or worker decision |
| [Durability](durability.md) | DUR | none (`quark-verify journey durability` scripts it against the real daemon) | real+firstmate+fake harness; real+stub for daemon checks | firstmate: none beyond the checkout (the fake harness needs no account) |
| [Persona packs](personas.md) | N10 | "switching a project's persona relabels its board, chat and workers", "settings: pick the project's persona" | mock, real+stub | none |
| [Daemon API and event stream](daemon-api.md) | API | none (the `daemon-api` holdout covers it from outside) | real+stub, real+firstmate | none |
| PR center (no file yet) | J6 | "PR center: list by state ...", "PR center: verification evidence ..." | mock | real: `gh` signed in and a task PR |
| Accounts (no file yet) | ADR-11 | "accounts: add a second Claude account ..." | mock | |
| Memory (no file yet) | J8 | "memory: review proposals ..." | mock | |
| Dispatch rules (no file yet) | J9 | "dispatch: edit rules, test them ..." | mock | |
| [Project settings](settings.md) | D1 | "settings: every Project switch in one place ..." | mock, real+stub | real: a Project with a Project repo |
| [Project metrics](metrics.md) | D3 | "metrics: the dashboard's Metrics tab ..." | mock, real+stub | real: firstmate state files in the Project workspace |
| [Project overview](overview.md) | D2 | "overview: live status now, and what changed since you last looked" | mock; real+stub (status lines written by hand); real+firstmate | none |
| [App shell](shell.md) | A1-A4 | `e2e/shell.spec.ts`: "catalogue: ...", "left list: ...", "next attention: ...", "dock: ..." | mock, real+stub | none |

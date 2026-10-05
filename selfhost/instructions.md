# Quark

## Goal

Build the Quark MVP: all ten journeys in the spec pass on Matt's machine with his real repos.
Quark builds itself from Phase 3 on.

The spec is the "Quark MVP Spec" Claude Doc: https://claude.ai/code/artifact/f3d0fe72-408c-4def-8be5-a1aa06afa24d

## Repositories

- `quark`: https://github.com/quark-systems/quark (the daemon `quarkd`, its crates, the desktop app under `app/`)
- `firstmate`: https://github.com/quark-systems/firstmate (the engine fork Quark drives; engine changes go here, never upstream)

## Work items

Work items are GitHub issues in `quark-systems/quark`, labeled by phase (`phase-3`, `phase-4`).
Link each task to its issue and say `Closes #<n>` in the PR body.

## Guidance

- Finish the MVP first and revise later. Interrupt Matt only for genuine architecture or design choices; otherwise pick a sensible default, say which, and keep building.
- One task, one PR against `main`. Never stack PRs: GitHub cannot merge stacked PRs through the API.
- Rebase every PR onto `main` before merging. Rebasing your own PR branch and force-pushing it with `--force-with-lease` needs no approval.
- A PR is ready when CI is green and its verification gates passed. The Project's standing approval decides who merges: on, green PRs merge without asking; off, Matt merges from the PR center.
- `quark`: `api/openapi.json` is the committed API contract. Regenerate it with `cargo run -p quarkd -- openapi > api/openapi.json` whenever the API changes, and keep `app/src/api.ts`, `app/mock/daemon.mjs` and `app/CONTRACT.md` in step.
- `firstmate`: follow its `AGENTS.md` code and commit rules, but not its persona. Never add an agent co-author line to a commit. Bin scripts stay shellcheck-clean with a colocated `tests/<script>.test.sh`; list new scripts in `docs/scripts.md`. The fork has no CI, so the verification gates are its only checks.
- quarkd runs one shared tmux server for every Project (`~/.quark/run/tmux/quark`); engine calls run with `TMUX` pointing at it.

Learnings accepted from finished tasks land in `memory/`, one file per entry.
A worker reports one by appending `learned: <what to keep>` to its status log before `done:` (`learned [files=a,b]: ...` names the files it is about); the coordinator adds `learned [source=coordinator]: ...` to a finished task's log. Each becomes a memory proposal for review.

---
name: create-verification
description: Generate a project-local verify-<app> skill that launches the repo's real app, drives it the way a user does, captures evidence and cleans up, with a per-feature map that feeds the journeys gate. Use when asked to "create a verification skill", "make a verify skill for this repo", or when a repo has no scripted way to prove UI, CLI or service behavior. Works for any language, framework and agent harness.
---

# Create a verification skill

Adapted from Cursor's `create-verification-skill` in [cursor/plugins](https://github.com/cursor/plugins/tree/main/pstack/skills/create-verification-skill) (MIT, Copyright (c) 2026 Lauren Tan; see [`LICENSE-pstack`](LICENSE-pstack)).
This version is harness-neutral and ties the feature map to Quark's verification gates (ADR-15).

Every serious project needs a scripted way to drive the real app and prove behavior: launch it, exercise a feature the way a user would, and capture evidence.
This skill generates that as a project-local skill, `verify-<app>`, tailored to the repo.
You write it for the next agent, not for a human: it will be read cold, mid-task, by an agent that has never seen the app and may run in any harness (Claude Code, Codex, Cursor, OpenCode, Pi, and others).

## Where the generated skill lives

- Write it to `.agents/skills/verify-<app>/` in the repo, the harness-neutral location.
- Harnesses that read another directory get a relative symlink, not a copy: `.claude/skills -> ../.agents/skills` for Claude Code, `.cursor/skills -> ../.agents/skills` for Cursor. Add only the links for harnesses the repo already uses, and keep an existing real directory instead of replacing it (link the single skill inside it).
- Everything the skill needs ships inside its directory: `SKILL.md`, `features/`, and helper scripts under `bin/`.
- Evidence and run state go outside the repo (a cache directory the skill names) or under a gitignored path, so a proof never dirties the working tree.

## 1. Interview the repo, not the user

Answer these from the codebase, and ask the user only what you cannot observe:

- **Surface:** what does a user actually touch? A web UI, a CLI or TUI, a desktop app, an API, a daemon, a library? A repo can have several; name every one, pick the primary, and say which the skill drives.
- **Run:** how does the app start locally? Prefer the repo's own documented commands (package scripts, Makefile, README, CI workflow). Note ports, env vars, data directories, seed data, auth, and external tools it shells out to.
- **Drive:** how can an agent interact with it programmatically? Existing harnesses first: Playwright or Cypress specs, test hooks the app already exposes, expect scripts, PTY or tmux helpers, curl-able endpoints, a debug port. Only then pick a generic recipe: a browser over CDP for web, Electron and webview apps; a tmux or PTY session for CLI and TUI; plain HTTP or WebSocket for services.
- **Observe:** what evidence can be captured? Screenshots, accessibility snapshots, terminal transcripts, response bodies, event streams, logs, exit codes, database rows, files and git refs.
- **Isolate:** can two instances run side by side (ports, data directories, profiles, sockets, servers they share)? If yes, the skill picks free ports and private directories per run. If not, say so: refusing to double-drive a shared instance beats corrupting the user's session.
- **Gates:** what does the repo already verify? Read CI, the scripted end-to-end specs, and the Project's `verification` declaration (`checks`, `journeys`, holdout) if one exists. The feature map must say which of those already covers each feature.

If the checkout does not build or start as-is, fix that first or report it precisely before generating; a skill written against a broken base teaches wrong steps.
When an irrelevant missing asset blocks startup (a static directory the API never serves, a sample config), the generated skill may create it, clearly marked as verification scaffolding, and remove it in cleanup.

## 2. Generate the skill

Write `.agents/skills/verify-<app>/SKILL.md` with YAML frontmatter (`name: verify-<app>` and a `description` that names the app, its surfaces, and when to reach for it; without frontmatter the skill never registers) and these sections, each grounded in what the interview found, with no placeholders left:

- **Launch:** the exact command that starts the app for verification, and how to tell it is ready (a log line, a port answering, a prompt). Include every surface the skill drives (for example a daemon, the UI served against it, and a browser attached to the UI). For a short-lived CLI or TUI there is no server to keep alive: launch builds the binary once, then each drive starts in its own isolated PTY or tmux session.
- **Doctor:** one read-only check that answers "is this instance worth driving?": process up, right build, port owned by this run, auth valid. An agent runs it first, and again whenever anything looks off.
- **Drive:** the harness recipe with real selectors and commands from this repo, not examples. Prefer stable handles (ARIA roles and names, labels, test ids, prompt strings, route paths) over coordinates and tab order. When the app already exposes a test hook (for example a function on `window` the end-to-end specs read), use it only to read state the user can see, never to set state.
- **Hand-off:** how a human opens the same instance to test by hand while the agent keeps it running (the URL, the terminal attach command), and how the agent picks up again afterwards. A verification run can pause at a checkpoint, show its evidence, and wait for a person to approve, comment or take over.
- **Evidence:** what to capture for a proof and where it goes. State the proof standards: exercise the real user path, not internal setters or test-only endpoints; capture the action and the resulting state, not just the final screen; verify side effects (files written, rows inserted, events emitted, messages sent) alongside what is visible; mocks only where a production boundary already isolates the external system, and a proof against a mock says so. When the safe path is a dry run or a test mode, verify what it actually skips by observing (files, network, git refs) rather than trusting its name.
- **Cleanup:** how to tear down what the run created. Never kill by process name; kill what you started, by recorded pid or private socket. Cleanup removes instances and scratch state, never the evidence: proof artifacts survive the teardown, in a location the skill names.
- **Helpers:** every script the skill ships is executable and its invocation is shown in the skill body. A helper the reader has to reverse-engineer is not a helper. Helpers resolve paths from their own location so they work from any working directory and worktree.

## 3. Seed the feature map

Create `.agents/skills/verify-<app>/features/README.md` plus one file per user-facing feature you can identify (the top 3 to 5 to start, from routes, commands, menus, screens or docs).
Follow the shape in [`references/feature-map-example/`](references/feature-map-example/): a README index and one file per feature.
Each file answers, from the user's point of view, what the feature is, how to reach it, how to drive it with the harness, and what observable end state proves it works.
The four H2s are `Sub-features`, `How to get to it (user POV)`, `Driving it with <harness>`, and `Gotchas`.
The map is the repo's maintained verification source; a proof that drives one convenient entry point is incomplete when the map lists others.

The README index also carries a coverage table that feeds the journeys gate: one row per feature with its id, the scripted end-to-end tests that already cover it (spec file and test title), the modes it can be driven in (for example against a demo backend or the real one), and its prerequisites.
A feature no scripted journey covers is marked `none`; those rows are the backlog for new journeys, and a new journey cites the feature id it covers so the two stay linked.

## 4. Prove the generated skill before handing it over

Run its own instructions end to end once: launch, doctor, drive ONE mapped feature on the real path (one is enough; the map exists so later runs cover the rest), capture evidence, clean up.
After cleanup, confirm the evidence still exists at the named location; a cleanup that eats the proof fails this step.
Fix what fails, and run the generated cleanup after every failed iteration too, so broken attempts do not strand processes and ports.
A feature that cannot be reached is reported as unreachable with the concrete prerequisite and the route attempted, never as verified through a different path.
A product bug the proof uncovers is reported to the user (and fixed separately when that is in scope), never papered over in the map.
A generated skill that was never executed is a draft, not a deliverable.

## 5. Keep it honest

The map rots as the app changes.
Re-run the proof for every feature a change touches, update the feature file in the same change that alters the behavior, and add a journey when a feature row reads `none` and the feature matters to the change.

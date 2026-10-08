# Quark glossary

One name per concept, used the same way in every screen, label, empty state and error.
Words under _Avoid_ mean the same thing and must not appear in app copy.
Code identifiers keep their API names (a `Task` is shown as a worker); this file is about what people read.

Adapted from the `CONTEXT.md` discipline of AgentsInTheCloud (MIT, Copyright (c) 2026 Lucas Meijer).

## Places

- **All projects**: the app's home: what needs you across every project, open PRs, and one card per project.
  _Avoid_: dashboard, home screen, projects list.
- **Left list**: the column on the left: projects, each with its coordinator pinned on top and its workers under it, then Accounts and Hosts in its footer.
  _Avoid_: sidebar, nav, tree.
- **Work pane**: the panel that slides in on the right with views of the selected thing: Terminal, Changes, PR, Why this agent.
  _Avoid_: inspector, drawer, details panel.
- **Dock**: the coordinator box at the bottom of every screen that is not the coordinator's own conversation. It carries what you are looking at as its context.
  _Avoid_: chat bar, omnibox, prompt bar.
- **Component catalogue**: the page listing every shared UI part with when to use it (`#/catalogue`).

## People and agents

- **You**: the person using Quark. In copy, address them as "you"; name them only where an answer is recorded ("Answered by Matt").
  _Avoid_: user, captain, operator.
- **Coordinator**: the one agent per project that plans, delegates, asks you and merges. Personas may rename it (first mate); the concept stays.
  _Avoid_: orchestrator, supervisor, manager, lead.
- **Worker**: an agent doing one piece of work in its own working copy. The API calls it a task.
  _Avoid_: task (in copy), crewmate, job, run, session.
- **Harness**: the agent program a worker or coordinator runs (Claude Code, Codex, ...).
  _Avoid_: runtime, CLI, adapter, backend.

## Work

- **Project**: a goal with its repositories, coordinator, workers and settings.
  _Avoid_: workspace, channel, repo.
- **Issue**: a piece of work not yet started, tracked in Beads.
  _Avoid_: ticket, bead (in copy), backlog item, card.
- **PR**: a pull request a worker opened.
  _Avoid_: pull (alone), merge request, change request.
- **Check**: one CI result on a PR. Checks are green, red or running.
  _Avoid_: build, job, pipeline (in copy).
- **Changes**: what a worker changed in its working copy, file by file.
  _Avoid_: diff (as a tab name), patch.

## Asking and answering

- **Decision**: a question that waits on you, with its options and the coordinator's recommendation. Lifecycle: asked, answered, acted on, in the log.
  _Avoid_: hold, gate, ask, blocker, prompt, needs-decision.
- **Standing rule**: a decision you turned into a rule, so the coordinator decides matching cases itself and logs each one.
  _Avoid_: policy, standing approval, auto-approve, yolo.
- **Memory**: what the project learned and every agent reads.
  _Avoid_: learnings, notes, knowledge base.

## State words

- **Busy**: an agent is working right now. Shown as a blue dot with a halo.
  _Avoid_: active, running (in the left list), in progress.
- **Needs you**: something waits on your answer, review or fix. Shown as a violet dot or count. One shortcut, Next attention (⌘J), walks them oldest first.
  _Avoid_: attention required, action needed, alert, notification.
- **Ready**: a worker finished and its PR is green. Green dot.
  _Avoid_: done (until merged), complete.
- **Failed**: a worker or check stopped with an error and nobody is fixing it. Red dot.
  _Avoid_: errored, broken, crashed.
- **Parked**: a worker that is not running and is waiting on something else. Dimmed row.
  _Avoid_: paused (in the left list), archived, sleeping.

## Copy rules

- Lead with the outcome and its consequence: "Merged. The worker's copy was cleaned up." not "Merge operation succeeded".
- One title per dialog; each element has one job; buttons say what they do ("Switch now", not "OK").
- Never put implementation in copy: no status-file names, event types, endpoint paths or engine terms outside developer tools.
- Times are relative ("4 min ago") with the exact time on hover.
- A panel whose endpoint is missing says what is unavailable, never an error trace.

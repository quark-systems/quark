# The judgment-only coordinator (slice 6)

`crates/quark-coordinator` is the deterministic half of the native coordinator.
Rust owns the lifecycle (spawn, watch, recover, gates, rebase, merge, teardown); the coordinator LLM plans, splits work, writes instructions, answers workers and decides what reaches the user, and it is woken only when something needs that judgment.
It replaces firstmate's coordinator contract (`AGENTS.md`, the watcher's wake queue and the turn-end guards) with a layered prompt, a wake queue and native tools, all recorded in the event log.

| Piece | Module | Events |
|---|---|---|
| Layered prompt | `prompt` | `coordinator.prompt` |
| Which events need judgment | `wake` | |
| Wake queue: items batched into turns | `Coordinator` | `coordinator.woken`, `coordinator.turn_ended`, `coordinator.requested`, `coordinator.cursor` |
| Native tools, over MCP | `tools`, `mcp` | `coordinator.tool`, `coordinator.tool_done` |
| Token efficiency | `efficiency`, `baseline` | `coordinator.baseline`, `coordinator.transcript_read`, `coordinator.would_wake` |

## Layered prompt

`LayeredPrompt::build` assembles five layers, in order:

1. **Built-in**: `crates/quark-coordinator/prompts/coordinator.md`, versioned with quarkd (`BUILTIN_REVISION`). It says what the coordinator is for, that the engine owns the lifecycle, that it never acknowledges status, and which tools it acts through. It uses neutral names only.
2. **Persona**: the pack's coordinator fragment, voice and flavor words (`quark-persona`). Switching a Project's pack changes this layer and nothing else.
3. **Instructions**: the Project repo's `instructions.md`, then each code repo's `AGENTS.md` (or a `CLAUDE.md` that is more than a pointer to it), each cut at 48 KiB.
4. **Memory**: the Project's memory entries, then the user's, front matter stripped, 24 KiB in all.
5. **Skills**: each skill's name and description from `.agents/skills/*/SKILL.md` and `.claude/skills/*/SKILL.md` (a symlinked directory is listed once). The body is loaded on demand with `load_skill`.

The log keeps a `PromptRecord` (each layer's source, size and digest, never the text) whenever a Project's prompt changes, so a persona switch or an edited `AGENTS.md` shows up there.

## When the coordinator is woken

`wake::judgment` turns an event into a wake item or nothing:

- Worker messages, firstmate status lines, inbound channel messages and rule outcomes follow the away policy (`docs/engine/triggers.md`): an occasion wakes the coordinator when the Project's route for it says so. By default that is a question, blocked, failed, done, an inbound message and a failed rule.
- Dispatch decisions left to the coordinator: `dispatch.escalated` (pick an agent), `dispatch.spawn_failed` and `dispatch.host_unhealthy`.
- An explicit `Coordinator::request` (a rule's `wake` action, the API), recorded as `coordinator.requested`.

Nothing else wakes it: progress, hook signals, steering, launches, the engine's own transitions.
A question asked again under the same key while still waiting replaces the waiting one.

## Turns

`Coordinator::tick` runs on a timer:

1. It evaluates each new event once and queues the wake items.
2. With no turn open, it opens one for up to 20 waiting items: `coordinator.woken` is recorded first, then the `Brain` delivers the briefing (one line per item) to the coordinator's session.
3. Items that arrive during a turn wait for the next one. The harness's turn-end hook calls `Coordinator::turn_ended` with the turn's token usage, which closes it.
4. A turn with no reported end after 30 minutes is closed as `timed_out`, and its items wake the coordinator again in a new turn.

A turn end with no turn open is the user talking to the coordinator directly; it is counted, never as an acknowledgement.

The coordinator acts through `Coordinator::call` (served over MCP by `CoordinatorMcp`).
Each call is recorded as `coordinator.tool` before it runs and `coordinator.tool_done` after; `Hands` carries it out.

| Tool | Does |
|---|---|
| `start_task` | start a task with a title and self-contained instructions; dispatch picks the agent unless one is given |
| `steer`, `answer` | message a worker; answer its question by key |
| `choose_profile` | pick the agent when dispatch escalated |
| `relaunch`, `cancel` | replace a worker in the same worktree; stop a task (its worktree and unlanded work are kept) |
| `ask_user`, `tell_user` | raise a decision only the user can make; report an outcome |
| `remember`, `ack_message` | propose a memory entry; mark an inbound message handled |
| `fleet`, `task`, `load_skill` | read the work in progress; load a skill |

Gates, rebase, merge and cleanup are deliberately not tools.

## Durability

The coordinator holds nothing the log does not have.

- **Crash mid-turn:** the turn is still open in the log, so the next process delivers it again (`attempt` 2, and the briefing says so) and the timeout restarts.
- **Crash mid-tool:** a call recorded without an outcome is closed as unknown on the next start, never run again; the coordinator checks its effect before repeating it.
- **Crash before an item reached a turn:** the cursor never passes a waiting item, so it is evaluated again. Items already in a turn are never queued twice.
- A log the coordinator has never evaluated starts at its head, so history wakes no one.

`crates/quark-coordinator/tests/queue.rs` drives each of these against the SQLite log.

## Token efficiency

The requirement is that the coordinator spends near zero turns acknowledging status, measured against today's firstmate.
`efficiency::Efficiency` folds both sides per Project: turns, tokens, turns and tokens per task, and the share of turns that changed nothing.

- **Native:** a turn acknowledged status when it made no acting tool call (anything but `fleet`, `task` and `load_skill`).
- **firstmate:** `baseline::read_turns` cuts the coordinator's Claude Code transcript into turns at each prompt that is not a tool result, sums each model call's usage once, and calls a turn started by a supervision notification an acknowledgement when it edited no file and ran none of firstmate's acting scripts (spawn, send, control, teardown, merge, hold, brief, backlog changes). This is a heuristic; the native side is exact. Codex and Pi coordinators are not read yet.

The Project dashboard's Metrics tab shows both (`ProjectMetrics.coordinator`).
When there is no Claude Code baseline (a Codex or Pi coordinator), the turns and tokens come from `usage.turn` events instead, without the acknowledgement split.

## Token use and spend

quarkd's `usage` module mirrors every coordinator and worker turn's token use into the event log as `usage.turn` events, from Claude Code, Codex and Pi session logs (`quark_transcript::read_turns`).
A log quiet for two minutes has its open turn counted, so a worker's last turn lands before its working copy is cleaned up.
The Metrics tab prices them at read time (`quarkd::spend`, API list prices, or Pi's own cost), so a price fix applies to past turns.

## Shadow mode

Slice 6 cannot switch on before slices 1 to 5, so firstmate's coordinator still runs every Project.
Unless `QUARK_NATIVE_COORDINATOR=0`, quarkd runs the native coordinator in shadow on the projector's interval (`crates/quarkd/src/native_coordinator.rs`): for each Project it records the layered prompt, reads firstmate's coordinator transcript into `coordinator.baseline` turns, and records what would have woken the native coordinator as `coordinator.would_wake`.
Nobody is woken and no tool runs.

Deciding is the LLM's job, so there is no decision to compare one by one; what shadow mode measures is when the coordinator would be woken.
Whether firstmate's coordinator would have been woken for the same status lines is already compared by the slice 7 shadow (`away.route` divergences).
The slice 6 shadow checks the other direction: every wake turn in which firstmate's coordinator acted should have a native `would_wake` within 10 minutes, and each one without is a `shadow.divergence` (operation `acting_wake`; `crates/quarkd/src/coordinator_shadow.rs`, `docs/shadow-readiness.md`).

## Not yet

- quarkd's `Brain` (typing the briefing into the coordinator's session, or a direct model call) and `Hands` (the dispatcher, supervisor, channels and memory), the MCP route for the coordinator, and installing its turn-end hook, once slices 1 to 5 are native.
- A native `EngineAdapter::start_coordinator` that launches the coordinator with the layered prompt instead of firstmate's `AGENTS.md`.
- In native slice 7, the triggers engine's `Effects::wake` should become `Coordinator::request` for rule actions only, since occasions already wake the coordinator from the log.
- Baselines from Codex and Pi coordinator transcripts.

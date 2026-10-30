# The judgment-only coordinator (slice 6)

`crates/quark-coordinator` is the deterministic half of the native coordinator.
Rust owns the lifecycle (spawn, watch, recover, gates, rebase, merge, teardown); the coordinator LLM plans, splits work, writes instructions, answers workers and decides what reaches the user, and it is woken only when something needs that judgment.
It replaces firstmate's coordinator contract (`AGENTS.md`, the watcher's wake queue and the turn-end guards) with a layered prompt, a wake queue and native tools, all recorded in the event log.

| Piece | Module | Events |
|---|---|---|
| Layered prompt | `prompt` | `coordinator.prompt` |
| Which events need judgment | `wake` | |
| Wake queue: items batched into turns | `Coordinator` | `coordinator.woken`, `coordinator.turn_ended`, `coordinator.requested`, `coordinator.answered`, `coordinator.cursor` |
| Native tools, over MCP | `tools`, `mcp` | `coordinator.tool`, `coordinator.tool_done` |
| Token efficiency | `efficiency` | |

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

- Worker messages, inbound channel messages and rule outcomes follow the away policy (`docs/engine/triggers.md`): an occasion wakes the coordinator when the Project's route for it says so. By default that is a question, blocked, failed, done, an inbound message and a failed rule.
- Dispatch decisions left to the coordinator: `dispatch.escalated` (pick an agent), `dispatch.spawn_failed` and `dispatch.host_unhealthy`.
- An explicit `Coordinator::request` (a rule's `wake` action, the API), recorded as `coordinator.requested`.
- The user's answer to a decision the coordinator raised with `ask_user`, recorded as `coordinator.answered` (`events::record_answer`).

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
| `start_task` | start a task with a title and self-contained instructions, and the dispatch rule it fits when known (used only when no classifier is configured; a classifier always decides); dispatch picks the agent (an `agent` is refused, since naming one skipped the rules) |
| `steer`, `answer` | message a worker; answer its question by key |
| `choose_profile` | pick the agent when dispatch escalated |
| `relaunch`, `cancel` | replace a worker in the same worktree; stop a task (its worktree and unlanded work are kept) |
| `ask_user`, `tell_user` | ask the user anything that needs their answer, as a decision (one call per question); report an outcome, which carries no questions |
| `remember`, `ack_message` | propose a memory entry; mark an inbound message handled |
| `fleet`, `task`, `load_skill` | read the work in progress; load a skill |

Gates, rebase, merge and cleanup are deliberately not tools.

## Commands from the user

The user asks a coordinator for three things with one control: a button in the app, a slash command typed as the whole message, or `POST /v1/coordinators/{id}:run` with `{"command": ...}` (`crates/quarkd/src/coordinator_commands.rs`).
Each is typed into the session as an engine note that begins `The user asked`, which the built-in prompt tells the coordinator to treat as the user's request, followed by the facts it needs.

| Command | Slash commands | The note carries |
|---|---|---|
| `catch_up` | `/catchup`, `/catch-up`, `/ahoy` | When the user last wrote to the coordinator (wakes, engine notes and harness output do not count), everything notable the event log holds after it, and the open decisions. The coordinator recaps that interval, then presents the open decisions one at a time, most impactful first. With no earlier message it runs as `status_report`. |
| `status_report` | `/status`, `/status-report`, `/bearings` | Every task's live status and the open decisions, for four sections that are always present: Needs you, Recently landed, Underway, Next (from `bd ready` and `bd blocked`). |
| `wrap_up` | `/wrapup`, `/wrap-up`, `/stow` | Nothing: the coordinator proposes memory entries with `remember` for what only its conversation holds, files undone work with `bd create`, corrects stale issues, and replies with what it saved. |

The firstmate names are kept so the commands people already type keep working; the nautical persona pack also shows them on the buttons.
A message that is a slash command plus more text (`/stow the logs`) is an ordinary message.

### Compaction

A harness compacts its conversation on its own only when the context window is nearly full, which on a 1M model means every turn reads close to a million tokens. So the engine compacts the coordinator earlier (`crates/quarkd/src/compaction.rs`):

1. A turn ends with the context (its last model call's input) at the Project's threshold or more: the engine asks for `wrap_up` (the note says the context will be compacted).
2. The wrap up's turn ends: the engine types `/compact` with what the summary must keep (standing rules, open decisions, tasks under way with their ids, branches and pull requests, promises, next steps). Claude Code, Codex and Pi all have `/compact`.
3. A later turn ending smaller, or the compaction entry in the session log, ends it. A compaction not seen in 15 minutes is given up on, and the next full turn starts again.

Each Project sets it in Settings (`compaction` in `GET`/`PATCH /v1/projects/{id}/settings`): `auto` on or off and `at_percent`, the share of the model's context window to compact at, 10 to 95. The threshold in tokens follows the model the coordinator runs, and a window Quark does not know counts as 200k. A Project that never set it uses `QUARK_COORDINATOR_COMPACT_AT_PERCENT`, else 70; `0` turns it off by default. With it off, the harness's own compaction still applies.
`POST /v1/coordinators/{id}:compact` compacts now, with no wrap up.
`POST /v1/coordinators/{id}:interrupt` presses the harness's interrupt key once and closes the open turn as `stopped`, because an interrupted harness reports no turn end. Its items do not wake the coordinator again. The session keeps running.
Each coordinator harness's turn end (Claude Code's `Stop` hook, Codex's `notify`, the Pi extension's `agent_end`) sets the automatic steps off; the context itself is read from the coordinator's session log.

Every compaction, whoever started it, is one `compaction` entry in the conversation (Claude Code's `compact_boundary` line, Codex's `compacted`, Pi's `compaction`), and the `/compact` command line is left out.
`GET /v1/coordinators/{id}/context` returns the coordinator's agent config, the model its log names, the context its last turn ended on, the model's window when known, from the providers' published windows (1M for current Claude models: Opus and Sonnet from 4.6, Haiku from 5, Fable, and Claude Code's aliases; 200k for older ones unless asked for with `[1m]` or seen past it; 272k of input for GPT-5 models), the model the session log names winning over a configured alias, the compaction settings and the threshold in tokens (`compact_at_tokens`), where a compaction is, the last compaction, and the latest change of agent (`switch`). The coordinator's header shows it as a meter.

### Changing the agent

`PUT /v1/coordinators/{id}/agent` changes the coordinator's harness, model, effort or pool through a durable switch: on the same harness it continues its conversation; on another, the old coordinator writes a handoff brief first. See [coordinator-switch.md](coordinator-switch.md).

## Durability

The coordinator holds nothing the log does not have.

- **Crash mid-turn:** the turn is still open in the log, so the next process delivers it again (`attempt` 2, and the briefing says so) and the timeout restarts.
- **Crash mid-tool:** a call recorded without an outcome is closed as unknown on the next start, never run again; the coordinator checks its effect before repeating it.
- **Crash before an item reached a turn:** the cursor never passes a waiting item, so it is evaluated again. Items already in a turn are never queued twice.
- A log the coordinator has never evaluated starts at its head, so history wakes no one.

`crates/quark-coordinator/tests/queue.rs` drives each of these against the SQLite log.

## Token efficiency

The requirement is that the coordinator spends near zero turns acknowledging status.
`efficiency::Efficiency` folds it per Project from `coordinator.turn_ended` and `coordinator.tool`: turns, tokens, turns and tokens per task, and the share of turns that changed nothing.
A turn acknowledged status when it was woken and made no acting tool call (anything but `fleet`, `task` and `load_skill`).
A task counts once a `task.transition` or `dispatch.requested` event names it in the window.

The Project dashboard's Metrics tab shows it (`ProjectMetrics.coordinator`).
Before firstmate was retired, the tab also read firstmate's coordinator transcript as a baseline; logs from then may still hold `coordinator.baseline` and `coordinator.would_wake` events, which nothing reads now.

## Token use and spend

quarkd's `usage` module mirrors every coordinator and worker turn's token use into the event log as `usage.turn` events, from Claude Code, Codex, Pi and Kiro CLI session logs (`quark_transcript::read_turns`).
A log quiet for two minutes has its open turn counted, so a worker's last turn lands before its working copy is cleaned up.
Two worker harnesses bill in their own credits rather than tokens, so their turns carry `credits` and a `credit_unit`:

- **Kiro CLI** (V3 engine) logs each session in `~/.kiro/sessions/<workspace hash>/sess_<id>/messages.jsonl` (or under `$KIRO_HOME`), found by the working directory its `session.json` lists. A turn runs from one `user` event to the next; its `usage_summary` gives the credits billed. Kiro records no token counts, and the model is the session's `modelId` (`kiro-auto` for Kiro's automatic choice).
- **IBM Bob** keeps no session log. Its database (`~/.bob/db/bob.db`, opened read-only) holds each task's running spend in Bobcoins (`tasks.costs`). Each Bob task in a worker's working copy counts as one turn, its id the Bob task's id, and whenever its spend grows the growth is added as a continuation of that turn. Subagent rows are left out (their spend is already in the parent). Token counts and the model are read from `costs` when it records them; otherwise the model reads `bob`.

The Metrics tab prices turns at read time (`quarkd::spend`), so a price fix applies to past turns: a harness's own cost (Pi) first, then credits at their price (Kiro $0.04 a credit, its published overage price; Bob $0.50 a Bobcoin, its list price), then API list prices.

## In quarkd

With `--engine native`, quarkd runs the coordinator for every Project on a timer, in `crates/quarkd/src/native_coordinator.rs`.

| Piece | Where | Does |
|---|---|---|
| `InputBrain` | `coordinator_hands.rs` | Types each turn's briefing into the coordinator's session, the way the coordinator chat does. A typed but unconfirmed wake is not typed again; the turn timeout redelivers it. |
| `EngineHands` | `coordinator_hands.rs` | `fleet`, `task`, `steer`, `answer`, `relaunch` and `cancel` go to the engine adapter. `ask_user` opens a decision in the store, `tell_user` puts a message in the coordinator chat (`coordinator.message`) and `remember` proposes a memory entry. `start_task` and `choose_profile` go to the engine's dispatch, and `ack_message` to the triggers engine's channels (`with_channels`); without them it is refused. |
| Routes | `coordinator_http.rs` | `POST /v1/coordinator/{project}/{token}/mcp` serves the tools over MCP; `POST /v1/coordinator/{project}/{token}/hooks/turn_end` (or `Stop`) closes the turn. On `Stop`, a reply that asks the user something (`questions::questions`) with no successful `ask_user` since the last turn end is sent back once with `{"decision": "block", "reason": questions::NUDGE}`, so the coordinator files each question as a decision; the stop that follows (`stop_hook_active`) closes the turn. `tell_user` refuses text with a question for the same reason. |
| Harness wiring | `coordinator_http::install`, `codex_overrides` | How each coordinator harness reaches its tools and turn end, below. |
| Claude Code | `coordinator_http::install_claude` | Native only: writes the MCP server into the workspace's `.mcp.json` (approved in `.claude/settings.local.json`) a `Stop` hook that POSTs the turn end and prints the answer as the hook's decision, and a `SessionStart` hook that runs `bd prime --memories-only` so the Project's Beads memories (standing rules among them) are injected when the session starts, resumes or is compacted. Everything else in both files is kept. How a coordinator uses Beads (issues, the decision log, memory) is in the built-in prompt's Beads section; it reads through the `bd` CLI and `BEADS_DIR`. |

Each Project's token is minted once into `<home>/coordinators/<project>.token` (mode 0600); a wrong token is `404`.
### Coordinator harnesses

The coordinator runs on Claude Code, Codex or Pi (and Pi's signed build), the Project's agent in Settings. Each gets the same tools and turn end, wired its own way (`coordinator_http::Wiring`), and the same layered prompt from `.quark/coordinator.md` beside the harness's own system prompt:

| Harness | Prompt | Tools | Turn end | Resume |
| :--- | :--- | :--- | :--- | :--- |
| Claude Code | `--append-system-prompt` | `.mcp.json`, approved in `.claude/settings.local.json` | `Stop` hook (`hooks/Stop`), which can send a reply back | `--continue` |
| Codex | `-c developer_instructions=…`, read from `.quark/coordinator.codex` | `-c mcp_servers.quark-coordinator.url=…` (streamable HTTP) | `-c notify=…`: a program POSTing Codex's `agent-turn-complete` JSON to `hooks/turn_end` | `codex resume --last` |
| Pi | `--append-system-prompt` | `-e .quark/pi/quark-coordinator.ts`: Pi has no MCP client, so this extension lists the MCP tools at start, registers each as a Pi tool and forwards its calls | the same extension POSTs `hooks/turn_end` on `agent_end` | `--continue` |

Codex is wired on its command line only, so nothing is written to `~/.codex` or the workspace, and its hooks are not used: Codex asks before running new hooks, which would park the coordinator. The command line also marks the workspace trusted (`-c projects={…}`), since Codex asks about a new folder even with approvals bypassed. Both resumes continue the newest conversation in the workspace.

When a reply asks the user something in text instead of through `ask_user`, quarkd sends the "Quark decision check" nudge back once per turn on every harness: as Claude Code's `Stop` hook block reply, as a Pi follow-up message from the bridge's `agent_end` report, and for Codex by typing it into the coordinator's terminal after `notify` reports the turn end (a Codex `Stop` hook would need the user to approve it in Codex first). Gap against Claude Code: Beads memories reach Claude Code at session start through `bd prime`; Codex and Pi get them only through the prompt's memory layer.

The turn-end hook takes the turn's `usage` from its body when it sends one, else reads it from the coordinator's newest session log; an `Idempotency-Key` header makes a retried hook count once.

The coordinator's decisions have the engine id `quarkd/coordinator/<key>`, so no engine refresh closes them. Asking again under the same key while one is open updates it in place.
Answering one through `POST /v1/decisions/{id}:answer` (from the app, the phone, chat or Beads) records `coordinator.answered` instead of calling the engine, and that wakes the coordinator with the answer.
`crates/quarkd/tests/coordinator.rs` runs that whole loop: a worker's question wakes the coordinator, it asks the user over MCP, its hook ends the turn, and the user's answer wakes it again.

## Not yet


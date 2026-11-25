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
  An escalation recorded while the coordinator's own `start_task`, `choose_profile` or `relaunch` ran does not wake it: that call's result already asks it to pick. A waiting "pick an agent" item is dropped once `dispatch.chosen` records an agent for its task.
- An explicit `Coordinator::request` (a rule's `wake` action, the API), recorded as `coordinator.requested`.
- The user's answer to a decision the coordinator raised with `ask_user`, recorded as `coordinator.answered` (`events::record_answer`).

Nothing else wakes it: progress, hook signals, steering, launches, the engine's own transitions.
A question asked again under the same key while still waiting replaces the waiting one.

## Turns

`Coordinator::tick` runs on a timer:

1. It evaluates each new event once and queues the wake items.
2. With no turn open, and the coordinator able to take a wake (`Brain::ready`, below), it opens one for up to 20 waiting items: `coordinator.woken` is recorded first, then the `Brain` delivers the briefing (one line per item) to the coordinator's session.
3. Items that arrive during a turn wait for the next one. The harness's turn-end hook calls `Coordinator::turn_ended` with the turn's token usage, which closes it.
4. A turn with no reported end after 30 minutes is closed as `timed_out`, and its items wake the coordinator again in a new turn.

A turn end with no turn open is the user talking to the coordinator directly; it is counted, never as an acknowledgement.

### A wake waits for the coordinator's turn to end

The queue knows the turns it opened. The coordinator is also in a turn when the user writes to it or the engine types it a note (a changed agent, a memory entry, a command, the decision nudge), and the queue has no turn open then.
A wake typed into such a turn is not delivered the way the queue assumes. Each harness does its own thing with input typed while it works: Claude Code queues it and passes it on inside the running turn, logging a `queue-operation` line and a `queued_command` attachment and never a user message; Kiro CLI steers the running turn with it. Whatever the harness, that turn's end would close the wake's turn before the coordinator had finished with it.

So before a turn is opened or delivered again, the queue asks `Brain::ready` with the time of the harness's last reported turn end, and while the answer is no the items stay waiting: nothing is typed, no `coordinator.woken` is recorded, the cursor does not pass them, and the next tick asks again. quarkd's `InputBrain` holds a wake (`coordinator_hands::hold`) while:

- a prompt is on the coordinator's screen (below), which would take the typing;
- a message the user sent is queued for the same turn end (`coordinator_outbox`): it goes in first, and the turn it starts holds the wake;
- quarkd typed something into the coordinator after its last reported turn end (`CoordinatorInput::last_typed`), so that turn is still running. An interrupted or restarted harness reports no end for the turn it was in, so one whose transcript has been still for 30 seconds (the outbox's `QUIET`) is taken to be over, as it is for a queued message. Stopping the coordinator from the app ends it at once.

quarkd only knows what it typed itself. A coordinator the person types into directly in its terminal, or one that was in a turn when quarkd started, can still take a wake mid-turn; on Claude Code that wake is confirmed from the queue lines above.

The coordinator acts through `Coordinator::call` (served over MCP by `CoordinatorMcp`).
Each call is recorded as `coordinator.tool` before it runs and `coordinator.tool_done` after; `Hands` carries it out.

| Tool | Does |
|---|---|
| `start_task` | start a task with a title and self-contained instructions, and the dispatch rule it fits when known (used only when no classifier is configured; a classifier always decides); dispatch picks the agent (an `agent` is refused, since naming one skipped the rules) |
| `steer`, `answer` | message a worker; answer its question by key (a `screen-…` question is a prompt on its screen, answered by choice number) |
| `choose_profile` | pick the agent when dispatch escalated |
| `relaunch`, `cancel` | replace a worker in the same worktree; stop a task (its worktree and unlanded work are kept) |
| `ask_user` | ask the user anything that needs their answer, as a decision (one call per question) with context, at least two options and a recommendation, or it is refused; with `answers` (a worker's task and key) the decision replaces that worker's question. Outcomes go in the coordinator's reply, which the chat already shows; the old `tell_user` tool posted a second copy of it and is refused |
| `withdraw_decision` | close one of the coordinator's own open decisions that no longer needs the user, with a reason |
| `remember`, `ack_message` | propose a memory entry; mark an inbound message handled |
| `fleet`, `task`, `screen`, `load_skill` | read the work in progress and a worker's terminal screen; load a skill |

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
`POST /v1/coordinators/{id}:interrupt` presses the harness's interrupt key once and closes the open turn as `stopped`, because an interrupted harness reports no turn end. Its items do not wake the coordinator again. The session keeps running. A session that no longer takes input at all is restarted in the same conversation with `POST /v1/coordinators/{id}:restart` (see [coordinator-switch.md](coordinator-switch.md)).
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

quarkd runs the coordinator for every Project on a timer, in `crates/quarkd/src/native_coordinator.rs`.

| Piece | Where | Does |
|---|---|---|
| `InputBrain` | `coordinator_hands.rs` | Types each turn's briefing into the coordinator's session, the way the coordinator chat does, once the coordinator is between turns (`ready`, above). One coordinator is typed into by one sender at a time, from the paste to the confirmation (`chat::SessionsInput`), so a wake and a message sent at the same moment never share its input box. A wake is confirmed when the harness's session log records taking it (`quark_transcript::read_inputs`). A typed but unconfirmed wake logs `wake typed but not confirmed` and is not typed again, since that could deliver it twice: its turn stays open, the harness's turn end closes it if the coordinator did get it, and the turn timeout redelivers it if not. With wakes held until the coordinator is idle, that warning means the harness logged the wake in a form this build does not read (as Codex 0.169 did before quarkd read its `item_completed` user messages), or did not take it. |
| `EngineHands` | `coordinator_hands.rs` | `fleet`, `task`, `screen`, `steer`, `answer`, `relaunch` and `cancel` go to the engine adapter. `ask_user` opens a decision in the store and `remember` proposes a memory entry. `start_task` and `choose_profile` go to the engine's dispatch, and `ack_message` to the triggers engine's channels (`with_channels`); without them it is refused. |
| Routes | `coordinator_http.rs` | `POST /v1/coordinator/{project}/{token}/mcp` serves the tools over MCP; `POST /v1/coordinator/{project}/{token}/hooks/turn_end` (or `Stop`) closes the turn. On `Stop`, a reply that asks the user something (`questions::questions`) with no successful `ask_user` since the last turn end is sent back once with `{"decision": "block", "reason": questions::NUDGE}`, so the coordinator files each question as a decision; the stop that follows (`stop_hook_active`) closes the turn. |
| Harness wiring | `coordinator_http::install`, `codex_overrides` | How each coordinator harness reaches its tools and turn end, below. |
| Claude Code | `coordinator_http::install_claude` | Native only: writes the MCP server into the workspace's `.mcp.json` (approved in `.claude/settings.local.json`) a `Stop` hook that POSTs the turn end and prints the answer as the hook's decision, and a `SessionStart` hook that GETs the coordinator's `/memory` route, so the Project's Beads memories are injected when the session starts, resumes or is compacted: standing rules (`quark-rule-…`) in full and every other memory as its key and first line, at most 8 KB (`coordinator_http::session_memory`), since Claude Code keeps only a 2 KB preview of a hook's output over about 10 KB. The coordinator reads the rest with `bd recall`. Everything else in both files is kept. How a coordinator uses Beads (issues, the decision log, memory) is in the built-in prompt's Beads section; it reads through the `bd` CLI and `BEADS_DIR`. |

| Environment | `Accounts::coordinator_env` | The coordinator's window starts with its own account's environment plus the API key Quark keeps for every other harness that signs in with one (`HarnessInfo.key_env`: the default account's kept key, else the first key account's, with its team), read from the Keychain at start since the supervisor does not start this window. So `kiro-cli` or `bob` in the coordinator's shell signs in as workers do. Called by provisioning, coordinator switches and recovery; a key changed in Quark reaches the coordinator at its next start. A key that cannot be read is left out with a warning. |

Each Project's token is minted once into `<home>/coordinators/<project>.token` (mode 0600); a wrong token is `404`.
### Coordinator harnesses

The coordinator runs on Claude Code, Codex, Pi (and Pi's signed build), IBM Bob, Kiro CLI or the Antigravity CLI, the Project's agent in Settings. Each gets the same tools and turn end, wired its own way (`coordinator_http::Wiring`), and the same layered prompt from `.quark/coordinator.md` beside the harness's own system prompt:

| Harness | Prompt | Tools | Turn end | Resume |
| :--- | :--- | :--- | :--- | :--- |
| Claude Code | `--append-system-prompt` | `.mcp.json`, approved in `.claude/settings.local.json` | `Stop` hook (`hooks/Stop`), which can send a reply back | `--continue` |
| Codex | `-c developer_instructions=…`, read from `.quark/coordinator.codex` | `-c mcp_servers.quark-coordinator.url=…` (streamable HTTP) | `-c notify=…`: a program POSTing Codex's `agent-turn-complete` JSON to `hooks/turn_end` | `codex resume --last` |
| Pi | `--append-system-prompt` | `-e .quark/pi/quark-coordinator.ts`: Pi has no MCP client, so this extension lists the MCP tools at start, registers each as a Pi tool and forwards its calls | the same extension POSTs `hooks/turn_end` on `agent_end` | `--continue` |
| IBM Bob | `--mode quark-coordinator`, a custom mode in `.bob/custom_modes.yaml` whose rules file `.bob/rules-quark-coordinator/coordinator.md` is the prompt (Bob has no system-prompt flag) | `.bob/mcp.json` (streamable HTTP, every tool in `alwaysAllow`, `groups` = the mode's slug) | a `Stop` command hook in `.bob/settings.json` POSTs Bob's payload (with `last_assistant_message`) to `hooks/turn_end` | `--resume latest` |
| Kiro CLI | `--agent quark-coordinator`, a workspace agent in `.kiro/agents/quark-coordinator.json` whose `prompt` is `file://../../.quark/coordinator.md` (Kiro has no system-prompt flag) | the agent's own `mcpServers` (by `url`), allowed by a `permissions` rule (`mcp`, `quark-coordinator/*`) | the agent's `stop` hook POSTs Kiro's payload to `hooks/turn_end`, output dropped | `--resume` |
| Antigravity CLI | added to the conversation as system messages by the workspace's `PreInvocation` hook (`.agents/hooks.json`, `hooks/agy_prompt`), in parts of at most 30 KB | `.agents/mcp_config.json` (streamable HTTP, every tool eager) | the same named hook's `Stop` POSTs `hooks/agy_stop`, which can send the turn back (and does once after each start, when agy first lists Quark's tools) | `--continue` |

Codex is wired on its command line only, so nothing is written to `~/.codex` or the workspace, and its hooks are not used: Codex asks before running new hooks, which would park the coordinator. The command line also marks the workspace trusted (`-c projects={…}`), since Codex asks about a new folder even with approvals bypassed. Both resumes continue the newest conversation in the workspace.

Bob is wired through the workspace's own `.bob/` settings, so `~/.bob` is left alone; Bob's hook schema is strict (one key outside it makes Bob ignore every hook in the file), so Quark's entries carry only `type`, `command` and `timeout`. The mode leaves out Bob's `mcp` and `skill` groups: Bob puts every MCP server without its own `groups` (all of the user's global `~/.bob` servers) in `mcp`, so their tools would sit beside Quark's and fill the context (about 84k tokens on a well-stocked Mac before the first turn); Quark's server is in the mode's own group instead. Those servers still start, unused. Its `SessionStart` hook prints the same `/memory` text, which Bob adds to the model's context, as Claude Code's does. The worker launch's `--max-turns` is dropped, since a coordinator runs for weeks. Bob keeps its conversation in `~/.bob/db/bob.db`, not a log file, so the coordinator's transcript is read from there and a typed message is confirmed there; Bob sometimes leaves a pasted message unsent, so Enter is pressed once more if it is not seen within 4 seconds. Gaps: Bob's `Stop` hook cannot send a turn back, so the decision nudge is typed in as for Codex; Bob's turn token counts are not read (`bob.db` keeps only each task's running spend and context), so the coordinator's token metrics do not cover it. Its context, model and window come from its task in `bob.db` instead of a session log: `costs.contextTokens`, and `env.model`'s `id` (Bob resolves every tier to `premium-ide` in production) and `contextWindow` (270k), which Bob's server reports and which win over Quark's own window table. Bob has no model or effort to choose, so the header shows the model Bob used and no effort.

Kiro is wired through one workspace agent, so `~/.kiro` is left alone. Its V3 engine skips a JSON agent that has `allowedTools` and no `permissions`, so Quark's tools are allowed by a permissions rule; the launch keeps the worker's `--trust-all-tools`, so commands run without asking. `includeMcpJson` is off, which keeps the workspace's `.kiro/settings/mcp.json` servers out. Kiro's `Stop` payload is only `{session_id, hook_event_name, cwd}`, so whether the turn asked something in text is read from the last reply in its session log, which also gives the transcript, typed-message confirmation and credits. Kiro runs `Stop` once per prompt, so a block decision would send a turn back that never ends again: the hook's output is dropped and the decision nudge is typed in, as for Codex. Kiro does not run its session-start hook again on resume, so the Project's memories go in the prompt, as for Codex and Pi. A message typed while Kiro works (a wake, or the person's message) steers the running turn, and Kiro logs it wrapped in a `[LIVE STEERING …]` note asking for a `[STEERING <id>: …]` report; the transcript reads the message inside the wrapper, so a wake stays hidden as with other harnesses, and leaves those reports out of replies. The trust-all-tools confirmation and Kiro's folder-trust question are answered from the manifest's `launch.dialogs` (below). Gaps: Kiro's global hooks, steering and MCP servers (`~/.kiro/hooks`, `~/.kiro/steering`, `~/.kiro/settings/mcp.json`) load into the coordinator too (Kiro 3.5's interactive chat loads global servers despite `includeMcpJson: false`), and an agent cannot leave them out; Kiro records only a context percentage, not token counts, so the coordinator's context, header and Quark's own compaction do not cover it (Kiro compacts itself at 80%).

The Antigravity CLI is wired through the workspace's own `.agents/` (`crate::coordinator_agy`); the only file under `~/.gemini` Quark writes is the worker hook every agy launch gets (`quark-worker` in `~/.gemini/config/hooks.json`, a no-op without `QUARK_HOOK_URL`). agy has no system-prompt flag, and every place it reads instructions from cuts them: a rule file at 24 KB, all rules at 20k tokens, a hook's system message at about 48 KB. So the hook's `PreInvocation` handler asks quarkd before each model call, and quarkd adds the prompt file (with the Project's memories, as for Kiro) as system messages of at most 30 KB when the conversation does not hold the current one: a new conversation, a compaction since (agy's `/compact` drops them), or a prompt file that changed. The first part carries the prompt's digest, which is how quarkd reads the transcript (`brain/<conversation>/.system_generated/logs/transcript_full.jsonl`) to tell. Quark's tools are eager in `mcp_config.json`, so agy declares them to the model directly instead of making it read each schema from disk. The `Stop` handler prints quarkd's answer: `{"decision": "continue"}` with the decision check sends the turn back, as Claude Code's block does; agy's `continuationActive` stays false on the stop that follows, so quarkd remembers the turn it sent back. Transcript, typed-message confirmation, token use and context come from the transcript (each `PLANNER_RESPONSE` step's tokens; Gemini models' window is 1M), and Quark's compaction types agy's `/compact`. The launch is the worker's less its brief, with `--dangerously-skip-permissions`; agy's folder-trust question is answered from the manifest's `launch.dialogs`. Gaps: agy's global MCP servers, hooks, rules and plugins (`~/.gemini/config/`) load into the coordinator too, and nothing in the workspace can leave them out (a custom agent can, but cannot reach the workspace's MCP tools in agy 1.5); agy 1.5 fixes a conversation's tools before it connects the workspace's MCP servers, so the first turn after every start (new or resumed, however long after the start) runs without Quark's tools: its first `Stop` is sent back once with a note that they are connected now, which lists them (the launch is told apart by `QUARK_AGY_LAUNCH` in the window's environment, which agy passes to its hooks; quarkd keeps it in memory, so a coordinator still running when quarkd restarts gets the note once more); that first run has a shell and its reply shows in the chat; agy key accounts (`GEMINI_API_KEY`, which agy reads only with `modelProvider: "gemini"`) are not wired, so agy signs in with the person's Google login in the Keychain.

### Prompts on the coordinator's screen

A harness can stop on a permission or approval prompt it draws in its terminal and that no hook or log reports: IBM Bob asks before a command that reaches outside its workspace (the coordinator's workspace is `~/.quark/workspaces/<project>`, so any command in a Project repo does), even when started with `--auto-approve`, since that flag does not cover it and only `~/.bob`'s `approval.permissionOptions` can. `quarkd::coordinator_prompt` reads every coordinator's screen every 2 seconds (`capture-pane`, nothing added to the event stream) and finds a numbered menu with a cursor the way the worker supervisor does (`quark_supervisor::prompt`). While one is up, nothing is typed into the coordinator: queued messages wait, a new message is queued, and any other typing is refused. One that stays 8 seconds opens a decision (`quarkd/coordinator-screen/<key>`) whose options are the prompt's choices; answering it picks that choice on the screen with the arrow keys and Enter. A prompt gone for 4 seconds closes its decision as answered elsewhere.

Start-up questions the harness's manifest says how to answer (`launch.dialogs`: Kiro CLI's trust-all-tools confirmation and folder-trust question) are answered on the same reading instead, with the manifest's choice (`quark_supervisor::dialog_key`), as the worker supervisor answers them for workers; the coordinator is not started by the supervisor.

When a reply asks the user something in text instead of through `ask_user`, quarkd sends the "Quark decision check" nudge back once per turn on every harness: as Claude Code's `Stop` hook block reply, as a Pi follow-up message from the bridge's `agent_end` report, and for Codex by typing it into the coordinator's terminal after `notify` reports the turn end (a Codex `Stop` hook would need the user to approve it in Codex first). Gap against Claude Code: Beads memories reach Claude Code at session start through its `/memory` hook; Codex and Pi get them only through the prompt's memory layer.

When the layered prompt changes, quarkd closes the coordinator's window once its conversation (in the harness's own session log) has been still a minute, and recovery starts it again in its conversation. The change is judged against `.quark/coordinator.launched.md`, the layered prompt alone as it was launched: `.quark/coordinator.md` also carries Codex's and Pi's memories and the last switch's handoff, so comparing it restarted those coordinators every minute.

The turn-end hook takes the turn's `usage` from its body when it sends one, else reads it from the coordinator's newest session log; an `Idempotency-Key` header makes a retried hook count once.

The coordinator's decisions have the engine id `quarkd/coordinator/<key>`, so no engine refresh closes them. Asking again under the same key while one is open updates it in place.
Answering one through `POST /v1/decisions/{id}:answer` (from the app, the phone, chat or Beads) records `coordinator.answered` instead of calling the engine, and that wakes the coordinator with the answer.
A worker's question never reaches the user as the worker wrote it. The refresh records it as a decision in state `with_coordinator`, unnumbered and left out of every list and event the app reads, and the worker's ask wakes the coordinator. The coordinator either answers it (`answer`, which also closes the decision in the log, answered via `coordinator`) or frames it with `ask_user` and `answers`: the same decision then takes the coordinator's question and brief, gets its number and opens, and the user's answer goes to the worker with the question as the user saw it and the picked option's consequence. A later refresh never puts the engine's bare brief back over a framed one. `fleet` lists the questions waiting on the coordinator and the open decisions. Migration 0036 moved the raw worker questions that were already open back to the coordinator.

`crates/quarkd/tests/coordinator.rs` runs that whole loop: a worker's question wakes the coordinator, it asks the user over MCP, its hook ends the turn, and the user's answer wakes it again.

## Not yet


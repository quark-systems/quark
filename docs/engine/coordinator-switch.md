# Changing a coordinator's agent

You can change a Project coordinator's harness, model, effort or account pool while it runs. The coordinator's conversation is the one thing a change can lose: a new harness cannot read the old harness's session. So every change goes through a **switch**: a durable record that moves through fixed steps, can pick up again from any of them after a crash, and always ends with a running coordinator.

Code: `crates/quarkd/src/switch.rs` (the driver, a pass every 2 seconds), `crates/quarkd/src/store/switches.rs` (the record). App: Settings › Agent › Change agent, with the switch's progress shown there and in the context popover. API: `PUT /v1/coordinators/{id}/agent` starts a switch. `GET /v1/coordinators/{id}/context` shows the latest one under `switch`.

## Two kinds

| Change | Kind | What carries over |
|---|---|---|
| Model, effort or pool on the same harness | `resume` | The whole conversation: the harness continues its latest session (`--continue`) with the new model and effort. |
| Another harness | `handoff` | A handoff brief that the old coordinator writes before it stops, plus the engine's facts. The new coordinator starts fresh with both in its system prompt. |

Everything durable is outside the conversation either way: memory, Beads issues, decisions, the event log and the workers. The new coordinator reads `fleet` and Beads as usual.

## Steps

Each step is written to `coordinator_switches` before its effect happens, so a crash repeats at most the last step. Every step is safe to repeat.

1. **requested**. The row records `from`, `to`, the kind, and the event log position at the time. Only one switch per Project can be open. A change to the same agent is refused.
2. **wrapping_up** (handoff only, and only when the coordinator is running). The engine types a note asking the coordinator to save what only its conversation holds (`remember`, `bd create`) and then to reply with a handoff brief: what it was doing, what it promised the user, what is waiting on whom, and what it would do next. Once the note is typed, a turn that ended before it no longer counts and the 5 minutes start. The step ends 10 seconds after the coordinator's turn ends (the turn-end hook marks the row, and the wait lets the transcript ingest record the reply), or after the 5 minutes. The brief is the newest of the coordinator's replies since the recorded log position that says "Handoff brief", else its newest reply, else nothing.
3. **switching**. In one transaction, the Project's agent config becomes `to` and the brief is stored. This is the commit point: from now on recovery, dispatch and the header all see the new agent. The engine writes `.quark/handoff.md` in the workspace with the brief and its own facts (open decisions, workers under way), then closes the old coordinator's window.
4. **starting**. The new coordinator starts under an account leased for the new agent. A `resume` switch continues the conversation, and if it cannot, it writes the handoff file (with no brief) and starts fresh like a `handoff` switch. A start that fails puts the old agent back and marks the switch **failed** with the reason, in one transaction. Coordinator recovery then starts the old agent again, resumed, as it does for any lost coordinator. A Project is never left without a coordinator because of a switch.
5. **briefing**. The engine types one note into the new session saying what changed and, for a handoff, to read its handoff and check `fleet`. A new session may not take input at once, so a failed attempt is tried again on the next pass, for up to 2 minutes. Then the switch is **done**.

`.quark/handoff.md` stays in the coordinator's system prompt until the next handoff replaces it. It is headed with its time and says to trust `fleet`, decisions and Beads over it where they differ, so a stale brief cannot mislead.

## Crash safety

- Coordinator recovery leaves alone any Project with an open switch: the switch driver owns its coordinator until the switch ends. So nothing else starts a coordinator with the wrong agent, and nothing else starts a second one.
- On daemon start, the driver picks up each open switch from its recorded step:
  - **requested**: it takes the step again. **wrapping_up**: it waits for the rest of the 5 minutes.
  - **switching**: it writes the handoff file and closes the window again.
  - **starting**: if the window is up, it moves on. Otherwise it starts the coordinator.
  - **briefing**: it types the note again.
- A crash between typing the wrap-up note and recording it cannot happen, because the step is recorded first. The worst case is a note that is never typed. Then the coordinator's turn never ends for it, and the timeout moves the switch on with no brief.

## Harnesses

Today the native coordinator runs on Claude Code only, because its MCP route and turn-end hook are wired for Claude Code alone. A switch to another harness is refused up front with that reason, rather than failing at the start step. Once Codex and Pi coordinators are wired, a switch to them is a `handoff`, with no other change to this protocol.

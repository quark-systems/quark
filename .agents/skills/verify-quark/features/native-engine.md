# Native engine

A person runs Quark on its own engine (`quarkd --engine native`): they create a Project from a git repo, its coordinator starts a task, the worker shows on the board, they steer it, answer its decision, and cancel and relaunch it, all from the app.

## Sub-features

- `nat-project`: a Project created from a local git repo reaches `ready`, its repo cloned into the workspace, with no engine checkout in the daemon's home.
- `nat-coordinator`: the coordinator runs in its own terminal (role `coordinator`) and answers typed input; its MCP route (`.mcp.json` in the workspace, server `quark-coordinator`) lists `start_task`.
- `nat-dispatch`: `start_task` starts a worker in an isolated worktree on branch `quark/<task id>`, and the board's Running column shows its card.
- `nat-steer`: a message sent from the worker view reaches the worker.
- `nat-decision`: the worker's question opens a decision; answering it from the decisions inbox reaches the worker, and the task runs again.
- `nat-lifecycle`: cancel stops the task and its worker process; relaunch starts a new worker in the same worktree.

## How to get to it (user POV)

- Start quarkd with `--engine native` (self-hosting does this once switched on).
- `New project` with a repo URL or path; the Project's `Work` tab (`#/p/<id>/work`) is its board.
- Click a worker's card to open `#/t/<task>`, with `Message the worker`, `Cancel` and `Relaunch`.
- `Decisions` in the sidebar (`#/inbox`) to answer.

## Driving it with quark-verify

Preconditions:

- tmux, jq, git and treehouse 3.1.2 or later (`selfhost/treehouse.sh`).
- No run needs to be up: the journey launches its own (`--engine native --harness fake`) and cleans it up; `--keep` leaves it up to look at.

- **Run it.** Run `$Q journey native`. Every check prints `PASS`, `FAIL` or `SKIP` with its reason; the command exits non-zero on any `FAIL`. Extra launch args go after `--`.
- **The coordinator's tool.** The fake harness is no LLM, so the journey makes the call a coordinator agent would: `tools/call` `start_task` (title, brief, agent `claude-code` at `low`) on the URL in the workspace's `.mcp.json`.
- **The worker's answers.** The fake worker appends a line it is sent as `QV-STATUS <line>` to its status file (`QUARK_STATUS_FILE`), with `{pid}` replaced by its pid, and acknowledges a relayed answer (`Answer to [<key>]: <text>`) with `working: answered <key>: <text>`. The journey reads them back as the task's `state_note` (`$Q api GET /v1/tasks/<id>`), which proves which process got each message.
- **By hand.** `$Q launch --engine native --harness fake`, create a Project (see [create-project.md](create-project.md), delivery `direct`), call `start_task` with `curl -X POST -H 'content-type: application/json' <url> -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"start_task","arguments":{"title":"...","brief":"..."}}}'`, then drive the worker view and the inbox as [worker-terminal.md](worker-terminal.md) and [decisions.md](decisions.md) describe. The journey sets the Project's persona to `plain` (`PUT /v1/projects/<id>/persona`) so the labels read `worker` and `coordinator`.
- **Proof.** The summary lands in `$QUARK_VERIFY_HOME/journeys/native-<stamp>.txt`; the run's `evidence/native/` keeps `result.txt`, `project.json`, `tools.json`, `start_task.json`, `task-events.json`, `decisions.json`, `events.jsonl`, `coordinator-screen.txt`, `agents/` (the start journal), screenshots `01-board.png` to `06-relaunched.png`, and `daemon.log` after cleanup.

## Gotchas

- The journey does not check the worker terminal (served from `quark-ptyd` since quark#115, `docs/engine/native-engine.md`, "Worker terminals"); it follows the worker through steering.
- A cancelled task reads `failed`: the task state machine has no cancelled state.
- `start_task` with no agent asks the Project's dispatch rules, and with no classifier configured every such task escalates and waits for `choose_profile`; the journey names the agent.
- Worker worktrees come from treehouse, and the run keeps its pool in the instance (`TREEHOUSE_ROOT`), so cleanup removes them with the run.

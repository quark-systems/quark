# Worker terminal

A user opens a task from the board and sees the worker's live terminal (the engine's tmux window, streamed by quarkd into xterm.js), types into it, steers the worker with a message, reads its transcript and changed files, and cancels or relaunches it.

## Sub-features

- `term-snapshot` draws the terminal's current screen when the worker view opens.
- `term-input` sends typed keys to the worker's window and shows the echo.
- `term-steer` delivers a message to the worker's inbox (`Message the worker`, `Send`).
- `term-transcript` and `term-changes` show the transcript and the diff tabs.
- `term-lifecycle` cancels and relaunches the worker.
- `term-coordinator` serves a Project's coordinator window as a terminal too (`GET /v1/projects/<id>/terminals`, role `coordinator`); the app does not show it yet, so drive it through the API.

## How to get to it (user POV)

- Click a task card on the Project board (`#/p/<project>`), which opens `#/t/<task>`.
- Press `Ctrl+K` and type part of the task title.
- From a decision, choose the link to the task that asked (or press `t`).

## Driving it with quark-verify

Preconditions:

- Mock: `$Q launch --daemon mock`; the `Quark MVP` Project has a running task titled `Event stream: resync slow clients from the store` whose fake terminal echoes input.
- Real: `$Q launch --engine firstmate --engine-dir <firstmate checkout>`, a ready Project (see [create-project.md](create-project.md)) and a task whose window is up; `$Q tmux list-windows -a` lists it.

- **Open the task.** Run `$Q browser open "#/p/quark"` (or the real Project id) and `$Q browser click --testid task-card --has-text "Event stream"`. `$Q browser url` ends in `#/t/<task-id>`.
- **See the snapshot.** Run `$Q browser terminal <task-id> --contains "writing the failing test"` (mock), or the text `$Q tmux capture-pane -p -t <session:window>` shows (real). Save `$Q browser screenshot --path worker-terminal/01-open.png`.
- **Type into it.** Run `$Q browser type --testid terminal --value "ls -la"` and `$Q browser press Enter`. `$Q browser terminal <task-id> --contains "you typed: ls -la"` (mock); in a real window the command's output appears, and `$Q tmux capture-pane -p -t <target>` shows the same text, proving the keys reached the engine's window.
- **Steer.** Run `$Q browser fill --label "Message the worker" --value "Please also cover the lagged receiver"` and `$Q browser click --role button --name Send`. `$Q browser wait --role status --contains "Delivered to the worker's inbox"`; the transcript shows the message.
- **Coordinator terminal (real).** With a ready Project, run `$Q api GET /v1/projects/<id>/terminals` (one terminal, role `coordinator`, id = the Project id) and `$Q api POST /v1/terminals/<id>/snapshot`: the returned `worker.output` event of kind `snapshot` carries the window's screen as `data_b64`. Type Enter with `$Q api POST /v1/terminals/<id>/input "{\"data_b64\":\"$(printf '\r' | base64)\"}"`, then `$Q tmux capture-pane -p -t <session:window>` shows the coordinator's harness reacted, and `$Q events` shows `worker.output` chunks of kind `output`.
- **Proof.** Save `worker-terminal/02-typed.png` and `$Q browser snapshot --path worker-terminal/02-typed.aria.txt`.

## Gotchas

- The terminal is drawn by WebGL outside Linux, so its rows are not in the DOM; read it with `browser terminal`, never with `browser text`.
- A queued task has no window yet; its view says so instead of showing a terminal.
- Without a signed-in harness the coordinator window shows the harness's own sign-in or onboarding screen and dispatches no tasks, so only the coordinator terminal is reachable.
- The stub engine never opens windows, so this feature cannot be proved live without `--engine firstmate`.
- Real windows run on the run's private tmux server only; `tmux` without `-S "$QV_TMUX_SOCKET"` looks at the user's own server.

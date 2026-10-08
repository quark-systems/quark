# Project board and coordinator chat

A user opens a Project and sees one column per task state, updated live from the event stream, beside a chat with the Project's coordinator; asking the coordinator for work queues a task on the board.

## Sub-features

- `board-columns` shows a column per task state (`col-queued`, `col-running`, ...) with task cards.
- `board-live` moves cards as tasks change state, without a reload.
- `chat-send` sends a message to the coordinator and shows it pending until the coordinator answers.
- `chat-actions` shows the workers the coordinator started and the questions it asked as cards (`coordinator-action`) that stay visible when its work folds; a question links to the inbox.
- `chat-scroll` stays put while the reader is scrolled up and offers a `New activity` button when entries arrive.
- `board-palette` jumps to a task with `Ctrl+P`.

## How to get to it (user POV)

- Click a Project in the sidebar or on the Projects screen (route `#/p/<project>`).
- Land on it after creating a Project.
- Press `Ctrl+P` and type the Project name.

## Driving it with quark-verify

Preconditions:

- Mock: `$Q launch --daemon mock`; the `Quark MVP` Project (`quark`) has a coordinator that answers.
- Real: `$Q launch --engine firstmate ...`, a ready Project, and its coordinator harness signed in so it can answer.

- **Open the board.** Run `$Q browser open "#/p/quark"` and `$Q browser wait --testid connection --contains connected`. `$Q browser snapshot --path board-and-chat/01-board.aria.txt` lists the columns and cards.
- **Ask for work.** Run `$Q browser fill --label "Message the coordinator" --value "Add tests for the parser"` and `$Q browser press Enter --label "Message the coordinator"`. The message shows as pending in `coordinator-chat`.
- **See the cards.** In the mock, `$Q browser count --testid coordinator-action` is at least 3 (two workers, one question); clicking `Open inbox` on the question goes to `#/inbox`.
- **See it queued.** Run `$Q browser wait --testid coordinator-chat --contains "queued Add tests"` (mock wording) and `$Q browser wait --testid col-queued --contains "Add tests for the parser"`. Save `board-and-chat/02-queued.png`.
- **Side effects.** `$Q api GET /v1/projects/quark/tasks` lists the new task as queued; `$Q events --for 1` shows `coordinator.message` and `task.created` events.

## Gotchas

- A real coordinator answers in its own words and may take minutes; wait on the queued card, not on chat wording.
- With the stub engine the chat accepts nothing useful: there is no coordinator behind it.
- A real daemon reads with the `nautical` persona by default, so the chat box is "Message the first mate"; pick "Plain" in the Persona select (see [personas](personas.md)) to use the labels above.

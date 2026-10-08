# Decisions inbox

A user sees every open decision across Projects, oldest first, answers one from the keyboard, and then finds it in the answered list with who answered and when; the task that asked resumes.

## Sub-features

- `dec-list` lists open decisions across Projects with a count in the sidebar.
- `dec-navigate` moves with `j`/`k` and opens a decision from the palette.
- `dec-answer` answers with `r`, the `Answer` box and `Ctrl+Enter`, recording `Answering as`.
- `dec-answered` switches with `o`/`a` and shows `Answered by <name>` and the answer.
- `dec-task` jumps to the task that asked (`t`, or the link in the detail).

## How to get to it (user POV)

- Choose `Decisions` in the sidebar (route `#/inbox`); its badge is the open count.
- Press `Ctrl+P` and type part of the question.
- From the Project board, a task waiting on a decision.

## Driving it with quark-verify

Preconditions:

- Mock: `$Q launch --daemon mock`; two open decisions, the older one `Keep the full answer history per decision` on the task `Decision records carry who answered`.
- Real: `$Q launch --engine firstmate ...` and a ready Project with an open hold or worker decision; `$Q api GET /v1/decisions` lists it.

- **Open the inbox.** Run `$Q browser open "#/inbox"`. `$Q browser text --testid inbox-count` prints `2` (mock) and `$Q browser count --testid decision-row` matches. Save `decisions/01-open.png`.
- **Answer from the keyboard.** Run `$Q browser click --testid decision-row --has-text "answer history"`, `$Q browser press r`, `$Q browser fill --label "Answering as" --value verify`, `$Q browser fill --label Answer --exact --value "Only the latest answer."` and `$Q browser press Control+Enter --label Answer --exact`. The count drops by one.
- **See who answered.** Run `$Q browser press a`, `$Q browser click --testid decision-row --has-text "answer history"` and `$Q browser wait --testid decision-answer --contains "Answered by verify"`. Save `decisions/02-answered.png`.
- **Side effects.** Run `$Q capture decisions/api -- $Q api GET /v1/decisions`: the decision carries the answer and `answered_by` `verify`; `$Q events --for 1` shows `decision.answered`. With the firstmate engine, the task's status log under `QV_HOME` records the answer.

## Gotchas

- Keys act on the page, not inside a text box; after typing, `Ctrl+Enter` must be pressed on the `Answer` box itself.
- The URL changes before the route re-renders; wait for the detail text before typing.
- The stub engine has no decisions, so a live proof needs `--engine firstmate`.

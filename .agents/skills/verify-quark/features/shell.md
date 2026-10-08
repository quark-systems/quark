# App shell: left list, Next attention, coordinator dock, catalogue

A user finds every project's coordinator and workers in the left list, walks everything that waits on them with one button or `Ctrl+J`, and asks a project's coordinator about whatever is on screen from the dock at the bottom (`Ctrl+K`); the component catalogue shows the shared parts screens are built from.

## Sub-features

- `left-list` shows each project with its coordinator row (`ll-coordinator`: open decisions and worker count) and its unfinished workers (`ll-worker`) with a state dot (Busy, Needs you, Ready, Failed, Parked); a recently failed worker stays, done work leaves.
- `next-attention` (`next-attention`) names the oldest thing waiting (open decision, red PR, failed or blocked worker) and its count; clicking it or `Ctrl+J` opens it, then the next, wrapping to the oldest.
- `dock` (`dock`) sits on every screen except the project's conversation; it carries the worker, PR or decision on screen (`dock-about`) and sends to that project's coordinator, confirming in `dock-note`.
- `catalogue` (`#/catalogue`) lists every shared part (`catalogue-entry`) with when to use it, its contract and a live example; it is searchable.

## How to get to it (user POV)

- The left list and Next attention are on every screen.
- Open any worker, PR, decision or app page to see the dock; press `Ctrl+K` from anywhere to type in it.
- `Ctrl+P` and "Component catalogue" opens the catalogue.

## Driving it with quark-verify

Preconditions: `$Q launch --daemon mock` (demo data has open decisions, a red PR and a failed worker), or a real run with a ready Project.

- **Left list.** `$Q browser open "#/p/quark"`, `$Q browser snapshot --path shell/01-left-list.aria.txt`; the `Quark MVP` region lists `Coordinator` and its workers. `$Q browser click --testid ll-worker --has-text "Event stream"` opens `#/t/...`.
- **Next attention.** `$Q browser click --testid next-attention`, then `$Q browser press Control+j`; each step lands on `#/inbox/<id>`, `#/pr/<id>` or `#/t/<id>`. Save `shell/02-next.png`.
- **Dock.** On a worker, `$Q browser press Control+k`, `$Q browser fill --label "Message the coordinator" --value "..."`, `$Q browser press Enter --label "Message the coordinator"`; `$Q browser wait --testid dock-note --contains "Sent to the coordinator"`. Side effect: `$Q api GET /v1/coordinators/quark/messages` shows the message with an `About "<worker title>" (#/t/<id>)` first line.
- **Catalogue.** `$Q browser open "#/catalogue"`, `$Q browser count --testid catalogue-entry`.

## Gotchas

- A real daemon reads with the `nautical` persona by default, so the dock is labelled "Message the first mate".
- The dock sends to the coordinator of the project on screen; on app pages (Accounts, Hosts, All projects) it uses the last project opened, with a Project picker when there are several.

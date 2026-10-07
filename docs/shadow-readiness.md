# Shadow readiness runbook

Each native slice switches from shadow to native strictly in order, 1 to 9, and only after its shadow agrees with firstmate and the verify-quark durability journeys pass.
This runbook turns every shadow on in a self-hosted Quark and reads how close each slice is.

## Turn every shadow on

Start quarkd with `QUARK_SHADOWS=all`:

```sh
QUARK_SHADOWS=all cargo run -p quarkd -- --engine firstmate
```

That turns on every shadow that only observes:

| Slice | Shadow | Setting `all` stands in for |
|---|---|---|
| 1 event log | firstmate events ingested into `events.db`, and the fleet read back from it and compared with firstmate's | `QUARK_ENGINE_SLICES=1=shadow` |
| 2 verification | firstmate's merge-guard decisions replayed with native rules | `QUARK_ENGINE_SLICES=1=shadow,2=shadow` |
| 5 dispatch | each dispatch resolved natively too | `QUARK_NATIVE_DISPATCH=1` |
| 6 coordinator | coordinator turns recorded for the Metrics tab | `QUARK_NATIVE_COORDINATOR` (already on unless `0`) |
| 7 sub-coordinators | the away classifier's routing | `QUARK_NATIVE_TRIGGERS` (already on unless `0`) |
| 9 worktree pool | the native pool asked what it would do on each treehouse call | `QUARK_NATIVE_WORKTREES=1` |

Firstmate keeps deciding and acting in every shadow; nothing native acts.
A setting you give explicitly still wins, so `QUARK_SHADOWS=all QUARK_NATIVE_DISPATCH=0` leaves dispatch off, and an explicit `QUARK_ENGINE_SLICES` replaces the default above.
`QUARK_SHADOWS=all` leaves the native supervisor (`QUARK_NATIVE_SUPERVISOR`) off, because it is not a shadow: it runs native workers itself.

The startup log names the shadows that came up (`shadows running`), and the daemon records the same list in the event log as a `shadow.started` event.
If the coordinator or triggers shadow fails to start, quarkd logs a warning and leaves it out of that list.

## Read the report

```sh
cargo run -p quarkd -- shadows                 # last 7 days
cargo run -p quarkd -- shadows --days 14       # a longer window
cargo run -p quarkd -- shadows --json          # the API's JSON
curl -s http://127.0.0.1:7380/v1/shadows?days=7
```

`quarkd shadows` reads `~/.quark/events.db` directly (or `--home`), so it works whether or not the daemon is running.
It prints one row per slice, then each slice's note, the setting that turns it on, its divergences per operation, and the newest five divergences with what bash and native each answered.

| Status | Meaning |
|---|---|
| `agreeing` | The shadow ran for the whole window with no divergences. The slice still needs the G2 durability journeys before it switches native. |
| `watching` | On with no divergences so far, but for less than the window. Leave it running. |
| `diverging` | At least one divergence in the window. Read the examples; each is a bug in the native slice or a gap in firstmate's record. |
| `off` | The shadow did not run on the latest start. |
| `no check` | The slice has no shadow that compares with firstmate, so divergences can't judge it. |

"On since" is the first of the unbroken run of daemon starts that ran that shadow; a start without it resets the clock.
Time the daemon was stopped counts toward it, so a laptop that was asleep for days reads as watched for those days.

## What the report can't judge yet

Slices 3, 4, 6 and 8 read `no check`:

- 3 worker protocol, 4 supervision, 8 sandbox: no shadow yet.
- 6 coordinator: it compares turns, not decisions; see the Metrics tab's Coordinator section.

## What slice 1 compares

Slice 1's shadow answers `snapshot`, `status_tail` and `holds` from the event log (`crates/quarkd/src/engine/eventlog.rs`) every time firstmate answers them, and records a divergence only when both engines still disagree on a second read.
The log carries firstmate's status lines and worker records, not its backlog, panes or validation runs, so each read compares what the log can know:

| Operation | Compared | Not compared |
|---|---|---|
| `snapshot` | which tasks exist; the state of each task whose state firstmate read from its status log (`done` and `in review` count as one) | queued work with no worker (backlog); states firstmate read from the pane or the validation run; titles, terminals, worktrees |
| `holds` | each open keyed decision and its question, for tasks whose decisions firstmate kept from the status log | captain holds (backlog); decisions firstmate dropped because the pane or validation run moved on; secondmates |
| `status_tail` | the lines and the next offset | a line appended between the two reads |

A `snapshot` divergence's two sides map task id to `{"state": ...}` (`{}` when the state is not compared, `null` when that engine has no such task).

## Roll back

Stop quarkd and start it without `QUARK_SHADOWS`.
Shadows only record events beside firstmate, so turning them off changes nothing firstmate does.

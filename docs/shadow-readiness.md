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
| 3 worker protocol | every firstmate status line read by the native file protocol too | `QUARK_ENGINE_SLICES=1=shadow,2=shadow,3=shadow` |
| 4 supervision | firstmate's spawns and status lines replayed through the native supervisor's rules | `QUARK_ENGINE_SLICES=1=shadow,2=shadow,3=shadow,4=shadow` |
| 5 dispatch | each dispatch resolved natively too | `QUARK_NATIVE_DISPATCH=1` |
| 6 coordinator | coordinator turns recorded for the Metrics tab, and firstmate's acting wakes checked against native would-wakes | `QUARK_NATIVE_COORDINATOR` (already on unless `0`) |
| 7 sub-coordinators | the away classifier's routing | `QUARK_NATIVE_TRIGGERS` (already on unless `0`) |
| 8 sandbox | the dashboard Overview's live task states compared with firstmate's fleet | `QUARK_SHADOW_DASHBOARD=1` |
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

Every slice now has a shadow that compares with firstmate, so none reads `no check`.
The comparisons below are partial: each says what it can't see.

## What slice 1 compares

Slice 1's shadow answers `snapshot`, `status_tail` and `holds` from the event log (`crates/quarkd/src/engine/eventlog.rs`) every time firstmate answers them, and records a divergence only when both engines still disagree on a second read.
The log carries firstmate's status lines and worker records, not its backlog, panes or validation runs, so each read compares what the log can know:

| Operation | Compared | Not compared |
|---|---|---|
| `snapshot` | which tasks exist; the state of each task whose state firstmate read from its status log (`done` and `in review` count as one) | queued work with no worker (backlog); states firstmate read from the pane or the validation run; titles, terminals, worktrees |
| `holds` | each open keyed decision and its question, for tasks whose decisions firstmate kept from the status log | captain holds (backlog); decisions firstmate dropped because the pane or validation run moved on; secondmates |
| `status_tail` | the lines and the next offset | a line appended between the two reads |

A `snapshot` divergence's two sides map task id to `{"state": ...}` (`{}` when the state is not compared, `null` when that engine has no such task).

## What slices 3, 4, 6 and 8 compare

| Slice | Operation | Compared | Not compared |
|---|---|---|---|
| 3 | `status_line` | for each status line, its kind (ask, done, report), state word, decision key, text and PR link, as firstmate's scripts read it and as `quark_worker::parse_status_line` reads it | `resolved` and `captain-held` lines, which firstmate writes, not workers; lines the native protocol skips (blank, `#`) |
| 4 | `supervised_state` | which tasks exist, and each status-log state, against what the native supervisor's rules (`quark_supervisor::rules`) would have made of the same spawns and lines | the same gaps as slice 1's `snapshot`; session liveness (panes against native sessions) |
| 6 | `acting_wake` | each wake turn in which firstmate's coordinator acted (from its transcript) has a native `would_wake` for the Project within 10 minutes either side | native wakes where firstmate did nothing (they cost tokens; the Metrics tab counts them); turns the user started; turns from before the shadow ran |
| 8 | `overview_live` | which tasks the Overview tab lists, and each status-log state, against firstmate's fleet | finished tasks the Overview keeps for a day after firstmate cleaned them up; the Hosts view, whose telemetry firstmate has no counterpart for |

Slices 3 and 6 judge events already in the log (`crates/quarkd/src/log_shadow.rs`): each event is judged once, and a checkpoint written with the divergences survives restarts.
Slices 4 and 8 run beside every firstmate `snapshot`, like slice 1, and record a divergence only when it holds on a second read.
Slice 6 matches turns by time, not by cause, so read its examples as leads: a miss can be firstmate acting on a heartbeat the native engine handles itself.
A turn that falls while quarkd was stopped also reads as a miss, because the native wake for it comes only when the daemon starts again.

## Roll back

Stop quarkd and start it without `QUARK_SHADOWS`.
Shadows only record events beside firstmate, so turning them off changes nothing firstmate does.

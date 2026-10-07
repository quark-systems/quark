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
| `watching` | On with no divergences so far, but for less than the window. Leave it running; "agreeing from" says when it qualifies. |
| `diverging` | At least one divergence in the window. Read the examples; each is a bug in the native slice or a gap in firstmate's record. |
| `off` | The shadow did not run on the latest start. |
| `no check` | The slice has no shadow that compares with firstmate, so divergences can't judge it. |
| `native` | The latest start ran the slice native. |

"On since" is the first of the unbroken run of daemon starts that ran that shadow; a start without it resets the clock.
Time the daemon was stopped counts toward it, so a laptop that was asleep for days reads as watched for those days.
The bar is time only: a slice reads `agreeing` once "on since" is a whole window (7 days by default) old with no divergence in that window, and a `watching` slice prints the moment it gets there ("agreeing from", `agrees_at` in the JSON).
A divergence keeps a slice `diverging` until it ages out of the window.

## Switch slice 1 to native

Slices switch native strictly in order, so slice 1 goes first, once it reads `agreeing` and the G2 durability journeys pass (`verify-quark`).
Restart quarkd with slice 1 native and the rest still in shadow:

```sh
QUARK_SHADOWS=all QUARK_ENGINE_SLICES=1=native,2=shadow,3=shadow,4=shadow quarkd --engine firstmate
```

The explicit `QUARK_ENGINE_SLICES` replaces the shadow default for slices 1 to 4, which is why slices 2 to 4 are named.
The report then reads `native` for slice 1 and prints `Running native: event_log`.

Native slice 1 answers from the event log exactly what its shadow compared: which tasks exist, the state of each task whose state firstmate read from its status log, those tasks' keyed decisions, and status tails.
Everything a later slice owns still comes from firstmate: queued backlog work, titles, terminals, worktrees, states read from a pane or validation run, and captain holds.
Firstmate is still the writer the log ingests from, so if the log can't be read quarkd serves firstmate's answer and logs a warning.

Roll back by restarting with `QUARK_ENGINE_SLICES=1=shadow,2=shadow,3=shadow,4=shadow` (or without it under `QUARK_SHADOWS=all`).
Only slice 1 can run native so far; quarkd refuses `native` for any other slice at startup.

## Slice 9 needs treehouse 3.1.2

The native worktree pool is a port of treehouse 3.1.2, so slice 9's shadow only runs against 3.1.2 or a later 3.x.
An older treehouse answers differently by design (2.x reports no checked-out branch and exits 0 on a dirty return), so quarkd leaves the shadow off, logs which version it found, and the report reads `off` for slice 9.
Install 3.1.2 with `curl -fsSL https://kunchenguid.github.io/treehouse/install.sh | sh` (or the release archive) and restart quarkd.

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
| 4 | `supervised_state` | which tasks exist, and each status-log state, against what the native supervisor's rules (`quark_supervisor::rules`) would have made of the same spawns and lines | the same gaps as slice 1's `snapshot` |
| 4 | `session_liveness` | for each task whose tmux pane firstmate read (a state from the pane or status log, or "backend target gone"), whether the pane is there, against whether the native tmux session backend lists it alive on the shared server | tasks firstmate judged from a validation run or could not reach; panes whose agent exited but whose shell remains, which a native session (running the agent itself) cannot have; remote and non-tmux workers; window-index targets |
| 6 | `acting_wake` | each wake turn in which firstmate's coordinator acted (from its transcript) has a native `would_wake` for the Project within 10 minutes either side | native wakes where firstmate did nothing (they cost tokens; the Metrics tab counts them); turns the user started; turns from before the shadow ran, and turns within 10 minutes of the daemon being down |
| 8 | `overview_live` | which tasks the Overview tab lists, and each status-log state, against firstmate's fleet | finished tasks the Overview keeps for a day after firstmate cleaned them up; the Hosts view, whose telemetry firstmate has no counterpart for |

Slices 3 and 6 judge events already in the log (`crates/quarkd/src/log_shadow.rs`): each event is judged once, and a checkpoint written with the divergences survives restarts.
Slices 4 and 8 run beside every firstmate `snapshot`, like slice 1, and record a divergence only when it holds on a second read.
Slice 6 matches turns by time, not by cause, so read its examples as leads: a miss can be firstmate acting on a heartbeat the native engine handles itself.
Each daemon start first records a `shadow.daemon_started` event, so slice 6 knows a run ended with the last event before it and skips turns from the downtime. A run that was quiet before it stopped ends early by that reckoning, which only leaves more turns unjudged.

## Roll back

Stop quarkd and start it without `QUARK_SHADOWS`.
Shadows only record events beside firstmate, so turning them off changes nothing firstmate does.

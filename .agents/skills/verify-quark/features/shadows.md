# Shadow readiness

Every native slice runs in shadow beside firstmate before it switches native, and `quarkd shadows` reports how often each one disagreed (docs/shadow-readiness.md).
The shadow journey compresses the days of real use that report waits for into minutes: it runs quarkd with `QUARK_SHADOWS=all` against a scripted workload of real firstmate work and prints each slice's divergences, so a native bug or a gap in firstmate's record shows up before a person has to wait a week to see it.

## Sub-features

- `shadow-workload`: one Project and N rounds of work driven through firstmate's own scripts. Each round spawns a ship task, a scout and a ship that fails; the workers report working, a keyed decision, a pause and a keyed blocker; the person answers both decisions; the workers finish (a PR link, a report, a failure); the scout's report is reviewed and both the scout and the failed task are cleaned up.
- `shadow-disruptions`: each round disrupts the run once, in turn a daemon crash and restart, a lost tmux server (after which the coordinator cleans up the tasks the loss left behind), and the daemon down for 3s. Every third round also cancels and relaunches a worker.
- `shadow-report`: the run's `quarkd shadows` report, its JSON, and how many inputs of each kind the event log holds, so a slice that saw nothing is visible.

## How to get to it (user POV)

- A person would see the same report from `quarkd shadows` after running their own daemon with `QUARK_SHADOWS=all` for days; this journey is the same report from a scripted run.

## Driving it with quark-verify

Preconditions:

- A firstmate checkout (`--engine-dir`, default `../firstmate`), `tmux` 3.2+, `jq`, `python3`, `tasks-axi` and `treehouse` on PATH. Treehouse 3.1.2 or a later 3.x turns on the slice 9 shadow; an older one leaves it off.
- No run needs to be up: the journey launches its own (with `--harness fake` and `--env QUARK_SHADOWS=all`) and cleans it up unless `--keep` is given.

- **Run it.** `$Q journey shadows --rounds 3 --engine-dir <firstmate checkout>` takes about ten minutes. It prints one `ok` or `FAIL` line per workload step, then the report. It exits non-zero when any slice diverged or any workload step failed.
- **Keep the run.** `--keep` leaves the run up for `$Q tmux`, `$Q api` and the app; clean up with `QUARK_VERIFY_RUN=<run> $Q cleanup`.
- **Proof.** The step summary lands in `$QUARK_VERIFY_HOME/journeys/shadows-<stamp>.txt`; the run's `evidence/shadows/` keeps `report.txt`, `report.json`, `inputs.txt` and `steps.txt`, plus `daemon.log` and `agents.jsonl` after cleanup.

How the journey acts:

| Role | How |
| :--- | :--- |
| Coordinator | `fm-brief.sh`, `fm-tasks-axi.sh add`, `fm-spawn.sh`, `fm-captain-hold.sh complete` and `fm-teardown.sh` in the Project's firstmate home, run in a window on the run's tmux server so they see what the coordinator sees |
| Worker | `QV-STATUS <line>` typed into the worker's terminal; the fake agent appends `<line>` to the status file its launch brief names, between the UserPromptSubmit and Stop hooks Claude Code fires |
| Person | `POST /v1/decisions/{id}:answer`, `POST /v1/tasks/{id}:cancel` and `:relaunch` |

## Gotchas

- The report's 7-day bar is time, so every slice reads `watching` after a run; what the journey judges is the divergence count.
- What the scripted workload does not reach yet: slice 2 compares guard decisions, which firstmate records only for a github.com origin with `gh` signed in, and the fixture repo is local; slice 6 compares coordinator turns from the harness transcript, which the fake agent does not write. Their zero is no evidence either way. Slice 5 compares every fresh spawn's dispatch and slice 7 the away classifier's routing, so they are exercised.
- The fake agent runs under a copy of bash named `claude`, shows Claude Code's bare `❯` composer and honors `/exit`, because firstmate tells an agent from a bare shell by its process name and types into it only over an empty composer.
- Firstmate refuses to relaunch a worker whose window is gone ("reconcile the task"), and Treehouse hands a dead worker's slot to the next spawn, so after the tmux loss the journey cleans those tasks up before spawning more, as the coordinator would.

# Durability

Quark keeps a person's work through the failures a laptop sees: the daemon crashing, the tmux server its agents run on going away, a reboot, and sleep. After each, every Project, task and decision is still there, the event stream continues where a client left off, the app reconnects on its own, every agent is running again (the same process, or a relaunch that resumes its conversation), and new work can still start.
These journeys check that from outside (the `/v1` API, the event stream, the terminals and the app), so they never depend on which engine runs. They run against the bash engine today and are the switch-on bar for every native slice: a slice moves from shadow to native only when they pass with it.

## Sub-features

- `dur-sleep`: every process the run started is stopped for 20s, then continued. Nothing restarts, nothing changes state, the app reconnects.
- `dur-daemon-crash`: the daemon is killed with SIGKILL and started again on the same home. State and the event stream continue, agents keep running untouched, the app reconnects, and a new Project provisions.
- `dur-tmux-loss`: the tmux server is killed with SIGKILL while the daemon keeps running. The daemon keeps answering, every agent is back and resumed, state is unchanged, and a new Project provisions.
- `dur-reboot`: the daemon, the tmux server and every agent are killed at once, then only the daemon is started, as a login would. Every agent is back and resumed, state and events continue, the app reconnects, and a new Project provisions.

## How to get to it (user POV)

- Close the laptop lid and open it again (sleep).
- quarkd crashes or is killed, and launchd or the person starts it again (daemon crash).
- `tmux kill-server`, an OOM kill or a tmux upgrade takes the server down (tmux loss).
- Restart the Mac with Projects running (reboot).

## Driving it with quark-verify

Preconditions:

- A firstmate checkout to test with (`--engine-dir`), or the stub engine for the daemon-only checks.
- No run needs to be up: each journey launches its own run and cleans it up, because a disruption leaves its run unusable for the next one.

- **Run all four.** Run `$Q journey durability -- --engine firstmate --engine-dir <firstmate checkout> --harness fake`. Each journey provisions a Project from a local fixture repo, records its agent terminals, the state and `last_seq`, opens the board in the app, disrupts the run, and checks recovery. Every check prints `PASS`, `FAIL` or `SKIP` with its reason; the command exits non-zero on any `FAIL`.
- **One journey.** Add `--only sleep`, `--only daemon-crash`, `--only tmux-loss` or `--only reboot` (repeatable). `--recovery <secs>` changes how long agents and the app get to come back (default 60).
- **Daemon checks only.** Without launch args the journeys use the stub engine: state, events, app and new work are checked, and agent checks are skipped because the stub opens no agents.
- **By hand.** On a run of your own, `$Q sleep <secs>`, `$Q crash daemon|tmux|host` and `$Q restart` are the same disruptions; `$Q agents` prints the fake harness's start journal.
- **Proof.** The summary lands in `$QUARK_VERIFY_HOME/journeys/durability-<stamp>.txt`. Each journey's run keeps `evidence/<journey>/`: `result.txt`, `state-before.jsonl` and `state-after.jsonl` (with `state.diff` when they differ), `events-after.jsonl`, `01-before.png` and `02-after.png` of the board, `disrupt.txt`, and `agents/` (the start journal), plus `daemon.log`.

What each check reads:

| Check | Pass when |
| :--- | :--- |
| daemon | `/v1/health` answers (within 5s of waking, or after `restart`) |
| state | Projects (id, name, status, goal), their tasks (id, state, title) and decisions (id, state) read back identical |
| events | replaying from the `last_seq` seen before returns only later seqs, each once and in order, and `last_seq` never goes back |
| agents | each agent terminal listed before is listed again and echoes a typed line within the recovery bound; with the fake harness, it is the same pid (sleep, daemon crash) or a new pid whose start asked to resume (tmux loss, reboot) |
| app | the board left open across the disruption shows `connected` again without a reload |
| new work | a Project created afterwards reaches `ready` |

## Gotchas

- The fake harness (`--harness fake`) puts `bin/fake-agent` on the daemon's PATH as `claude` and `codex` and gives the tmux server a profile-free shell, so a login profile cannot put the real CLIs back first. It draws its pid on screen, echoes typed lines, and journals each start with whether it was asked to resume (`--resume`, `--continue`). It is not an LLM, so a fake coordinator dispatches no tasks: with it, the journeys follow the coordinator agent only. Worker tasks are followed whenever the Project has them (a signed-in harness), since the checks cover every terminal the Project lists.
- With a real harness the agent checks still require each terminal back and answering, but cannot tell a resumed agent from a fresh one, and say so.
- `crash host` cannot clear `/tmp` or reset the clock, and the app server and Chromium stay up so the app can be read afterwards, as a person reopening it would.
- `sleep` stops processes with SIGSTOP; wall-clock time moves on as it does after a real sleep, but the kernel's monotonic clock does too, which a real suspend does not advance.
- Baseline on the bash engine (quark `4baf291`, firstmate `d27e89b`): sleep and daemon crash pass. After a tmux loss nothing relaunches the coordinator and new Projects fail at "Starting the coordinator" until the daemon restarts; after a reboot the daemon comes back and new Projects work, but nothing relaunches existing coordinators.

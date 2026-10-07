# Sub-coordinators and runtimes (slice 7)

A sub-coordinator is a persistent coordinator with its own home, on this machine or on another host: every Project is one.
Firstmate calls them second mates and builds them from `fm-home-seed`, the seventeen `fm-remote-*` scripts, `fm-spawn --secondmate`, `fm-send`, `fm-backlog-handoff`, `fm-config-push` and the parent channel.
The native engine splits that into two crates.

| Crate | What it does |
|---|---|
| `quark-runtime` | Reaches a host: run a command, move files, host terminal sessions. `LocalRuntime` and `SshRuntime` today; hosted and private-cloud runtimes plug in with Cloud mode. |
| `quark-subcoordinator` | Registers, seeds, launches, steers, hands work to, supervises and retires sub-coordinators on any runtime, with every change in the event log. |

## Runtimes

`Exec` extends the `quark_core::Runtime` contract with one primitive: run an argv with environment, working directory and stdin.
Everything else is built on it, so the same code works on every host:

- `fs` writes files atomically, reads from a byte offset and lists directories, passing paths as arguments, never inside a script.
- `probe` reports the platform, home directory, tmux version and which tools are on `PATH`.
- `TmuxSessions` is a `SessionBackend` that runs one `tmux` command per call on the host, on its own socket (`tmux -L quark`) with `remain-on-exit`, so a session belongs to the host's tmux server and outlives the connection that started it. It needs tmux 3.2 or later. Live output streaming over a runtime is not there yet; viewers poll `snapshot`.

`SshRuntime` runs each command as one OpenSSH invocation in batch mode with agent, X11 and port forwarding off and keepalives on, so a vanished host fails within a bounded time.
Hosts, users, keys and host-key checking come from the user's own OpenSSH configuration; `QUARK_SSH` overrides the client.
The remote login shell gets a quoted command line, so it must be POSIX-compatible, and the usual per-user tool directories (`~/.local/bin`, Homebrew, Nix, Cargo) are put in front of its `PATH`.

An `Err` from `run` means the command did not run or its outcome is unknown: ssh exit 255, a timeout, a missing program.
A command that ran and failed is `Ok` with its exit code.
Callers treat the first as "try again later" and never as proof that anything on the host is gone.

## A sub-coordinator's home

```text
<home>/.quark-subcoordinator.json  identity: which sub-coordinator owns the home
<home>/CHARTER.md                  scope and standing instructions
<home>/BRIEF.md                    what the current agent was started with
<home>/inbox/                      messages from the parent, one file each
<home>/inbox/handled/              messages the agent has acted on
<home>/outbox/parent.status        the parent channel, append-only lines
<home>/inherited/                  configuration pushed by the parent
<home>/projects/<name>/            the projects, cloned on the host
```

## Lifecycle

- **Register** records a `subcoordinator.registered` event: id, charter (scope and instructions), placement (host, runtime, home), harness profile and projects with their origins. The harness must allow the `coordinator` role. A home that overlaps another sub-coordinator's is refused.
- **Seed** runs the readiness check, then creates the home, writes the identity and charter, and clones each project on the host itself from its origin. It never takes over a non-empty directory it did not create and never replaces a clone that has a different origin. It is safe to repeat.
- **Readiness** (`doctor`) is the gate for seeding and every launch: macOS or Linux, `git`, tmux 3.2+ when sessions run in tmux, and the harness's binary. Each gap comes with the step a person takes on that machine.
- **Launch** starts the agent in a session named `sub-<id>-<generation>` with the brief (who it is, where its projects are, the inbox and parent-channel contract), and `QUARK_PROJECT`, `QUARK_GENERATION`, `QUARK_HOME`, `QUARK_INBOX` and `QUARK_PARENT_CHANNEL` in its environment. Local sub-coordinators can run in quarkd's PTY supervisor instead of tmux (`RuntimeHosts::with_local_sessions`). **Relaunch** replaces the agent, optionally on another harness, model or effort, with a note; **stop** ends it without retiring.
- **Send**, **answer** and **handoff** record a `subcoordinator.sent` event first, then write the message as a file in the inbox (`0007-<id>.md`, or `.handoff.json` for work items) and ring the session with one pasted line. The agent acknowledges by moving the file into `inbox/handled/`. A message for an unreachable host waits and is delivered on a later pass; delivery is at least once.
- **Inherit** writes shared configuration (a map of relative paths to contents) into `inherited/`, skips an unchanged set by digest, and tells a running agent to re-read it.
- **Retire** is refused while a handoff has not been taken or while the host cannot be reached to stop the agent, unless forced. It stops the agent and records `subcoordinator.retired`; the home stays on its host for a person to remove.

## Supervision pass

`SubCoordinators::tick` runs on a timer. For each live sub-coordinator it:

1. records the host's health when it changes, and stops there when the host is not healthy: an unreachable agent may well be running, so nothing is relaunched or replaced;
2. records an agent whose session ended (`subcoordinator.exited`) and relaunches it with a note, up to `max_recoveries` times in a row, retrying on later passes if the host is not ready; past the limit it stays stopped until a person relaunches it;
3. ends sessions of the sub-coordinator that no current generation owns, such as one a crash started but never recorded;
4. delivers waiting messages, and records the ones the agent moved to `handled/`;
5. reads complete lines appended to the parent channel since the last recorded one, as `subcoordinator.report` events parsed like worker status lines. A `needs-decision [key=<k>]` line opens a decision that `answer` closes.

## Durability

The engine holds nothing the log does not have.
A new `SubCoordinators` on the same log replays the registry, finds the sessions still running on their hosts, and carries on: messages not yet in an inbox are delivered, and the parent channel is read on from the recorded cursor.
Channel report ids are derived from the sub-coordinator, byte offset and line, so reading a line again records nothing.

The tests in `crates/quark-subcoordinator/tests/lifecycle.rs` drive this against real tmux, real git clones and the SQLite log with a shell script as the agent, on this machine and through a stand-in `ssh`, including an unreachable host and an engine restart.

## Shadow

Until slice 7 switches on, firstmate keeps its second mates in `data/secondmates.md`.
`shadow::parse_registry` reads that file and `shadow::compare` sets it beside the native registry, one `shadow.divergence` per sub-coordinator the two record differently (id, host, home, scope, projects).

## Not yet

- quarkd wiring: an opt-in flag that builds `SubCoordinators` over `events.db`, routes the slice 7 `EngineAdapter` operations (`add_source`, `seed_workspace`, `start_coordinator`) to it in native mode, and records shadow divergences.
- A live output stream for tmux over a runtime, so the app can attach to a remote sub-coordinator's terminal.
- Remote readiness repair (installing tools, wrappers, login items); the doctor reports the steps instead.
- Hosted and private-cloud runtimes (Cloud mode).

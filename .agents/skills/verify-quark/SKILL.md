---
name: verify-quark
description: Launch an isolated Quark (quarkd built from this checkout, the desktop app's web build served against it, a headless Chromium) and drive it the way a user does, with evidence and cleanup. Use to prove a change to the desktop app, quarkd's /v1 API and event stream, or engine terminals works in the running product, not just in tests, and before handing a feature to a person to test by hand.
---

# Verify Quark

Generated with the `create-verification` skill. The feature map in [`features/`](features/README.md) is the maintained verification source; read its index before driving.

Quark has three surfaces:

- **Desktop app** (`app/`): Tauri 2 + React. The same frontend runs in a browser, which is what this skill drives; the Tauri shell only opens a window (`app/README.md`).
- **quarkd** (`crates/quarkd`): the `/v1` REST API, the `/v1/events` WebSocket, and terminal sessions on a private tmux server.
- **Engine terminals**: coordinator and worker windows the firstmate engine opens on quarkd's tmux server, streamed to the app's xterm.js terminal.

Everything goes through one helper, run from any directory or worktree:

```sh
Q=.agents/skills/verify-quark/bin/quark-verify   # relative to the repo root
$Q                                               # usage
```

Requirements: `cargo`, `node` 22 with `npm`, `tmux` 3.2+, `curl`, `git`, and a Chromium (`PW_CHROMIUM`, else `/opt/pw-browsers/chromium`, else Playwright's download from `npx playwright install chromium` in `app/`).

## Launch

```sh
$Q launch                                                    # real quarkd, stub engine (no agents, no network)
$Q launch --engine firstmate --engine-dir ../firstmate       # real quarkd driving a clone of a quark-systems/firstmate checkout
$Q launch --daemon mock                                      # the app against app/mock/daemon.mjs (demo data, quiet)
$Q launch --engine firstmate --engine-dir ../firstmate --harness fake   # agents run bin/fake-agent: no account or network needed
```

Launch builds `app/dist` (`npm ci` first if `app/node_modules` is missing) and `quarkd` (`cargo build -p quarkd`), then starts, each on a free port and in its own process group:

- the daemon, with `--home <run>/instance/home`, so its SQLite store, workspaces and tmux server (`<home>/run/tmux/quark`) are private to the run;
- `bin/serve.mjs`, which serves `app/dist` and proxies `/v1` (HTTP and WebSocket) to the daemon on one origin, so no CORS change is needed;
- headless Chromium with a CDP port and its own profile.

It is ready when it prints `launched run <id>` and the run's variables (`QV_APP`, `QV_DAEMON`, `QV_CDP`, `QV_TMUX_SOCKET`, `QV_EVIDENCE`, `QV_COMMIT`).
Later commands act on that run (`$QUARK_VERIFY_RUN`, else the last launch recorded in `$QUARK_VERIFY_HOME/current`), so set `QUARK_VERIFY_RUN=<id>` when two runs are up.
Runs live under `$QUARK_VERIFY_HOME` (default `~/.cache/quark-verify`); keep it short, because the tmux socket path must fit in about 100 bytes and launch refuses otherwise.

Modes and what they prove:

| Mode | Proves | Does not prove |
| :--- | :--- | :--- |
| `--daemon real --engine stub` (default) | the app against the real daemon: API, store, events, Project provisioning without an engine | anything an engine or agent does: no tasks, decisions or terminals appear |
| `--daemon real --engine firstmate` | the real daemon driving the real engine: provisioning, coordinator and worker windows, decisions, terminals | agent behavior unless a harness is installed and signed in (the coordinator window runs it) |
| `--daemon mock` | app-only behavior against demo data (what `npm run e2e` uses) | the daemon or engine; say "against the mock" in any proof |

## Doctor

```sh
$Q doctor
```

Read-only. Checks the three processes are alive by recorded pid, the daemon answers `/v1/health` with the expected engine, the app server proxies `/v1`, Chromium's CDP answers, and (real daemon) whether the private tmux server is up. It prints the build commit and the evidence directory, and exits non-zero on any failure.
Run it before the first drive, after any surprising result, and before handing the instance to a person.

## Drive

The app (`$Q browser ...`, one action per call against the run's persistent page; full reference in the header of `bin/browser.mjs`):

```sh
$Q browser open "#/new"                                        # routes: #/, #/new, #/p/<project>, #/t/<task>, #/inbox, #/prs, #/accounts
$Q browser snapshot                                            # ARIA tree of the page: find roles and names here first
$Q browser fill --role textbox --name Name --exact --value "Verify demo"
$Q browser select --label Harness --exact --value claude-code
$Q browser click --role button --name "Create project"
$Q browser click --testid task-card --has-text "Event stream"
$Q browser wait --testid provision-bar --hidden --timeout 20000
$Q browser wait --testid coordinator-chat --contains "Workspace ready"
$Q browser type --testid terminal --value "ls -la" && $Q browser press Enter
$Q browser terminal <task-id> --contains "you typed"            # the terminal's text, read from xterm's buffer
$Q browser screenshot --path <feature>/<step>.png
```

Stable handles, in order of preference: ARIA roles and names, labels (`Repository 1`, `Harness`, `Effort`, `Delivery`, `Message the coordinator`, `Message the worker`, `Answer`, `Answering as`, `Terminal input`), and test ids (`connection`, `provision-bar`, `coordinator-chat`, `col-<state>`, `task-card`, `task-state`, `terminal`, `transcript`, `inbox-count`, `decision-row`, `decision-detail`, `decision-answer`).
`browser terminal` reads `window.__quark.terminalText`, the read-only hook the e2e specs use because the WebGL renderer draws no DOM rows; never use page scripts to set state.
Keyboard shortcuts are part of the product (`app/README.md`): `Ctrl+K` palette; in Decisions `j`/`k`, `r`, `Ctrl+Enter`, `o`/`a`, `t`.

quarkd directly:

```sh
$Q api GET /v1/projects                                        # body on stdout; non-zero on HTTP >= 400
$Q api POST /v1/projects '{"name":"x","goal":"g"}'
$Q events --since 0 --for 2                                    # event frames as JSON lines (seq, type, payload)
```

Engine terminals (real daemon): `$Q tmux list-windows -a` and `$Q tmux capture-pane -p -t <session:window>` read the private server the engine's windows run on. Drive them through the app's terminal; use tmux only to observe.

Disruptions (real daemon), for durability work; [features/durability.md](features/durability.md) has the scripted journeys (`$Q journey durability`):

```sh
$Q sleep 20                                                    # stop every process the run started, then continue them
$Q crash daemon                                                # SIGKILL the daemon; start it again with: $Q restart
$Q crash tmux                                                  # SIGKILL the private tmux server; the daemon keeps running
$Q crash host                                                  # a reboot: daemon, tmux server and agents gone at once
$Q agents                                                      # the fake harness's start journal (pid, argv, resumed)
```

## Hand-off to a person

While the run is up, a person can test the same instance by hand:

- open `QV_APP` (from `$Q env`) in a browser; the app's status bar shows the daemon it talks to;
- attach to the engine's windows with `tmux -S "$QV_TMUX_SOCKET" attach` (detach with the tmux prefix and `d`; never kill windows from there);
- on another machine, forward the ports first (`ssh -L <port>:127.0.0.1:<port> ...`).

When a verification reaches a point that needs a human judgement, stop driving, post the evidence so far with `QV_APP`, and wait. Before driving again, run `$Q doctor` and re-read the page with `$Q browser snapshot`, because the person may have changed state.

## Evidence

Artifacts go under `QV_EVIDENCE` (`<run>/evidence`), one directory per feature id: relative `--path` values of `browser snapshot` and `browser screenshot` land there, and `$Q capture <feature>/<step> -- <cmd...>` saves a command's `cmd.txt`, `stdout.txt`, `stderr.txt` and `exit-code.txt`.
Cleanup copies the daemon log to `evidence/daemon.log`.

Proof standards:

- Drive the real user path (the screen, its controls, its keys), not API shortcuts or test hooks; use the API to read back what the user did.
- Capture the action and the resulting state: a screenshot or ARIA snapshot before and after, not only the final screen.
- Verify side effects alongside what is visible: the resource read back from `/v1`, the events emitted (`$Q events`), files and commits under `QV_HOME` (for example the Project repo's `git log`).
- A proof against `--daemon mock` or the stub engine says so; it proves the app, not the daemon or engine.
- Record the run id, `QV_COMMIT`, the mode and the feature id with every proof.
- An entry point that could not be reached is reported with the command tried and the unmet prerequisite, never as verified through another path.

## Cleanup

```sh
$Q cleanup
```

Stops Chromium, the app server and the daemon by recorded process group, kills the run's private tmux server by its socket, and removes `<run>/instance`. It never kills by process name and never touches `evidence/`, which it lists at the end.
Run it after every failed attempt too. Evidence of old runs stays until you delete `$QUARK_VERIFY_HOME/<run-id>`.

## Gotchas

- The stub engine provisions a Project without cloning anything and never opens a coordinator window, so worker and coordinator terminals need `--engine firstmate`.
- `--engine firstmate` clones the given checkout into the daemon's `home/engine`, so commit what you want tested there first. Delivery `gated` (the form's default) needs `no-mistakes` installed; pick `direct` under Advanced when it is not. A Project's coordinator runs its harness for real in the private tmux server, which needs that harness signed in.
- Repositories can be local: the form takes an absolute path to a bare repo with a `main` branch (not a `file://` URL), which works offline.
- The first `launch` builds quarkd and the app and can take minutes; later launches reuse the builds.
- The app server serves the production build. After changing `app/src`, launch a new run (or rebuild with `npm run build` in `app/` and reload with `browser open`).

# Quark desktop app

The Quark desktop app: Tauri 2 + React + TypeScript + Vite, with xterm.js terminals (ADR-3).
It grew out of the Phase 0 POC in [`../ui-poc/tauri`](../ui-poc/tauri), which stays as the benchmark record.

The webview talks to `quarkd` directly over HTTP and the `/v1/events` WebSocket; the Rust side only opens the window.
The same frontend runs in a plain browser.

## Screens

- **Projects**: every Project with its active and needs-decision counts.
- **New project** (J2): name, goal, repositories, default agent (harness, model, effort) and dispatch preset.
- **Project board** (J3): one column per task state, updated live from the event stream, beside the coordinator chat.
- **Worker view** (J4): live terminal with input, a box to message the worker, cancel and relaunch, and tabs for the transcript and the changed files with their diff.
- **Decisions** (J5): every open decision across Projects, oldest first, with an answer box; the answered list shows who answered and when.
  Keys: `j`/`k` move, `r` or `Enter` answers, `Ctrl/Cmd+Enter` sends, `o`/`a` switch between open and answered, `t` opens the task that asked.
- **Memory** (J8): per Project, the learnings finished tasks proposed, with their evidence, accepted (edited or not) or rejected from the keyboard; accepted entries with the commit that added each, and promotion to the user-level memory every Project's coordinator reads.
- **Dispatch** (J9): per Project, the dispatch rules in order (name, when, ordered candidates), the default and `default_select`, each candidate checked with its harness as it is edited; saving commits `dispatch.yaml` to the Project repo, and the test pane shows the rule a task description matches and each candidate's pass or fail reason, for the saved rules or for the edit in progress.
  Keys: `j`/`k` move, `p`/`a` switch between proposed and accepted, `e` or `Enter` edits, `Ctrl/Cmd+Enter` accepts, `x` twice rejects, `u` promotes, `c` shows the commit, `t` opens the task.

`Ctrl/Cmd+K` opens a palette that jumps to any Project, task or open decision.

## Run

```sh
npm install

# against quarkd (http://127.0.0.1:7380 by default)
cargo run -p quarkd                # in the repository root
npm run dev                        # web build on http://127.0.0.1:1420
npm run tauri dev                  # desktop app with hot reload
npx tauri build                    # release bundles for this platform

# against the demo daemon, which serves every endpoint the app uses with demo data
npm run mock                       # http://127.0.0.1:7380; add -- --port N or -- --quiet
```

The daemon URL comes from `?daemon=<url>` in the web build, `QUARK_DAEMON` for the desktop app, or the address in the status bar, which you can click to change.
The terminal renderer is xterm.js's DOM renderer on Linux and WebGL elsewhere, until WebGL is measured on a GPU; `?renderer=dom|webgl` (or `QUARK_QUERY=renderer=webgl`) overrides it.

On Linux the desktop build needs `libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev build-essential`.

## Daemon endpoints

`quarkd` serves Projects, tasks, decisions and the event stream today.
Chat, transcripts, terminals, changes and steering are being added by other Phase 1 workstreams.
[`CONTRACT.md`](CONTRACT.md) lists every endpoint the app uses and the shape it expects; until an endpoint is served, its panel says so instead of failing.
`quarkd` allows CORS from the app's origins (`tauri://localhost`, `http://tauri.localhost`, and the dev server on port 1420).

## Tests

```sh
npm run build                      # typecheck and production bundle
npm test                           # unit tests (vitest)
npm run e2e                        # Playwright against the demo daemon in quiet mode
```

`npm run e2e` starts the demo daemon on port 7392 and the dev server on port 1421.
Set `PW_CHROMIUM=/path/to/chrome` to use an installed Chromium instead of `npx playwright install chromium`.

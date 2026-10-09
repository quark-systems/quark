# Quark desktop app

The Quark desktop app: Tauri 2 + React + TypeScript + Vite, with xterm.js terminals (ADR-3).
It grew out of the Phase 0 POC in [`../ui-poc/tauri`](../ui-poc/tauri), which stays as the benchmark record.

The webview talks to `quarkd` directly over HTTP and the `/v1/events` WebSocket; the Rust side only opens the window.
The same frontend runs in a plain browser.

## Screens

- **All projects** (home): what needs you across projects, oldest first, open PRs, and one card per project.
- **New project** (J2): name, goal, repositories, default agent (harness, model, effort) and dispatch preset.
- **Project** (J3): tabs Conversation (the coordinator, with what changed since you looked), Overview, Work (one column per worker state, updated live from the event stream), Issues, Decisions, Memory and Metrics; Settings is one page with General, Dispatch and Automation sections.
- **Worker view** (J4): the transcript and a box to message the worker in the middle, cancel and relaunch, and a work pane with the live terminal (with input), the changed files with their diff, the worker's PR, and Why this agent with a link to the dispatch rule that picked it.
- **Decisions** (J5): every open decision across Projects, oldest first, with an answer box; the answered list shows who answered and when.
  Keys: `j`/`k` move, `r` or `Enter` answers, `Ctrl/Cmd+Enter` sends, `o`/`a` switch between open and answered, `t` opens the task that asked.
- **Memory** (J8): per Project, the learnings finished tasks proposed, with their evidence, accepted (edited or not) for this Project, all your Projects or anyone in the repo, rejected, or turned into an issue; what this Project keeps (its Beads memories, or `memory/` files with the commit that added each and promotion to user-level memory), your own memory, and the Project's decisions.
- **Issues**: per Project, its Beads issues (one database for the whole Project, outside its repos, synced with issue trackers only through the rules in Settings › Issue sync), by Ready, In progress, Blocked and Closed, each with what it waits for and what waits for it; a worker starts on one once nothing it waits for is open. New issue drafts issues with the coordinator in a side chat, created only when accepted.
  Keys: `j`/`k` move, `n` opens New issue, `Esc` closes it.
- **Dispatch** (J9): per Project, the dispatch rules in order (name, when, ordered candidates), the default and `default_select`, each candidate checked with its harness as it is edited; saving commits `dispatch.yaml` to the Project repo, and the test pane shows the rule a task description matches and each candidate's pass or fail reason, for the saved rules or for the edit in progress.
  Keys: `j`/`k` move, `p`/`a` switch between to review and this project, `e` or `Enter` edits, `Ctrl/Cmd+Enter` accepts, `x` twice rejects (or forgets a Beads memory), `u` promotes, `c` shows the commit, `t` opens the task.

The left list shows every project with its coordinator pinned on top and its workers under it; Next attention at its top walks everything that waits on you (open decisions, red PRs, stuck workers), oldest first.
Every screen but the coordinator's own conversation has a coordinator dock at the bottom that sends what you type with what you are looking at as context.

- `Ctrl/Cmd+K` focuses the dock (the coordinator's message box on its own conversation).
- `Ctrl/Cmd+J` opens the next thing that needs you.
- `Ctrl/Cmd+P` opens a palette that jumps to any project, task, decision or PR, and switches the theme (dark, light, or match the system).

Words follow [`GLOSSARY.md`](GLOSSARY.md); shared parts are listed in the component catalogue (`#/catalogue`).

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
Colors come from design tokens at the top of `src/styles.css`; new CSS uses them, never raw hex. `?theme=light|dark` forces a theme for one load.
On macOS the desktop window is translucent with a native blur behind it and the title bar overlays the sidebar; `QUARK_GLASS=0` keeps it opaque.

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

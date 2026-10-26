# Build from source

Quark has no official release yet. This page builds the daemon and the desktop app from a checkout and starts your first Project.
macOS is the main target. Linux runs the daemon and the app's web build; the desktop build needs the WebKitGTK packages listed in [`app/README.md`](../../app/README.md).

## Prerequisites

| Tool | Why | Notes |
| :--- | :--- | :--- |
| Rust (stable) | builds `quarkd` | [rustup](https://rustup.rs) |
| Node 22 | builds the app | |
| git | repos and worktrees | |
| tmux 3.2 or newer | coordinator terminals | `--tmux` or `QUARKD_TMUX` picks the binary |
| treehouse 3.1.2 | the worktree pool workers run in | `selfhost/treehouse.sh` installs it |
| GitHub CLI (`gh`) | cloning, the PR center, merges | run `gh auth login` first |
| Claude Code | the coordinator, and workers by default | signed in; other harnesses are optional for workers |

Optional:

- `bd` and `dolt` ([Beads](https://github.com/gastownhall/beads)) for the Issues tab and Beads memory. quarkd sets up one Beads database per Project under `~/.quark/beads/`.
- `quota-axi` for account quota in the app (`--quota-axi` or `QUARKD_QUOTA_AXI` picks the binary).
- Any other harness you want workers to use (Codex, Cursor, Gemini, OpenCode, Pi, Kiro and more), installed and signed in. The app detects what is installed.

## Build and run the daemon

```sh
git clone git@github.com:quark-systems/quark.git
cd quark
selfhost/treehouse.sh                     # no-op when treehouse 3.1.2+ is on PATH
cargo run --release -p quarkd -- --engine native
```

quarkd serves `http://127.0.0.1:7380` and starts its PTY supervisor (`quarkd ptyd`), which keeps running after quarkd stops so workers survive a restart.
Always pass `--engine native`: the default is an in-memory stub engine for tests.

Useful flags (`cargo run -p quarkd -- --help` lists them all):

| Flag | Environment | Default |
| :--- | :--- | :--- |
| `--home` | `QUARK_HOME` | `~/.quark` |
| `--listen` | `QUARKD_LISTEN` | `127.0.0.1:7380` |
| `--tmux` | `QUARKD_TMUX` | `tmux` on `PATH` |
| `--user-memory` | `QUARK_USER_MEMORY` | `<home>/memory` |
| `--refresh-secs` | | `5` |
| `--pr-refresh-secs` | | `30` |
| | `QUARKD_LOG` | `quarkd=info,tower_http=info` |

The daemon logs to stderr and, as JSON lines, to `<home>/logs/quarkd.<date>.jsonl`, one file a day, keeping a week. Every hour it also prunes the store: successful engine reads after a day, failed ones after 30 days, and a finished task's terminal output down to its last 256 KB.

`quarkd diagnose [--home <home>] [--hours 24] [--json]` reports what went wrong lately: warnings and errors from that log, failed engine reads, supervisor and dispatch trouble, workers gone quiet, decisions left waiting and an oversized store. It reads the home directly, so it works with the daemon down; `GET /v1/diagnostics?hours=24` returns the same report. Each finding has a fingerprint that stays the same while the trouble does.

## Run the app

```sh
cd app
npm install
npm run tauri dev       # desktop app with hot reload
npm run dev             # or the web build on http://127.0.0.1:1420
npx tauri build         # release bundle for this platform
```

The app finds the daemon at `http://127.0.0.1:7380`. Change it with `?daemon=<url>` in the web build, `QUARK_DAEMON` for the desktop app, or by clicking the address in the status bar.

`npm run mock` serves demo data on port 7380 instead of quarkd, which is handy for UI work with no agents running.

## Your first Project

1. Click **New project**.
2. Give it a name and a goal, add one or more GitHub repositories, and choose the default agent (harness, model, effort).
3. Create it. quarkd clones the repos, seeds the Project's workspace and Project repo, and starts the coordinator; the card shows each step.
4. Open the Project's **Conversation** tab and tell the coordinator what you want. It starts workers, and they appear under the Project in the left list and on the **Work** tab.
5. Answer questions in **Decisions**, review and merge in the PR center. Turn on standing approval in Settings to let green PRs merge on their own.

To describe a Project in files instead (gates, instructions, a kickoff message), see [Self-hosting](../../selfhost/README.md) and `selfhost/project-init.sh`.

## Where things live

Everything is under `~/.quark` (or `QUARK_HOME`):

| Path | What |
| :--- | :--- |
| `workspaces/command/` | the command center's clones of every repo |
| `workspaces/<project-id>/` | a Project's workspace: repo checkouts, worktrees, config and state |
| `projects/<project-id>.git` | the Project repo: instructions, `project.yaml`, `dispatch.yaml`, `memory/`, `holdout/` |
| `memory/` | user-level memory every coordinator reads |
| `beads/<project-id>/` | the Project's Beads database |
| `run/tmux/quark` | the private tmux server's socket |
| `config.yaml` | user-level settings, such as the [classifier](../configuration/dispatch-rules.md#classifier) |

## Upgrading

Pull and rebuild:

```sh
git pull
cargo build --release -p quarkd
```

Restart quarkd; running workers stay up in the PTY supervisor and reattach.
If you ran Quark before firstmate was retired, `~/.quark/engine` is no longer used and can be deleted, and any Project that lists the `firstmate` repo can detach it in its settings.

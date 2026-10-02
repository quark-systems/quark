# Quark

**Quark** is an open-source control plane and orchestration surface that connects local development environments with cloud microVM sandboxes. Quark allows developers to delegate entire software engineering goals across multiple repositories while maintaining local authority, multi-account routing, and complete harness neutrality. Quark treats the **Project** as the fundamental unit of work. It coordinates parallel worker agents across isolated git worktrees or cloud sandboxes, driving task intake all the way to evidence-backed, defensible pull requests.
    
## 🔬 System Taxonomy & Physics Metaphor
    
In physics, elementary quarks never exist in isolation, they are bound together by **gluons** to form composite **hadrons**. Quark adapts this quantum chromodynamics (QCD) architecture directly into its systems model:
    
| QCD Metaphor | Component | System Definition |
| :--- | :--- | :--- |
| **Quark** | **Worker Agent** | The individual, autonomous execution unit running inside an isolated microVM sandbox or git worktree. |
| **Gluon** | **Control Plane & Bus** | The central event relay, credential broker, and orchestration engine that binds project memory, state, and active worker threads together. |
| **Hadron** | **Project Workspace** | The goal-driven workspace with memory containing forge repos, issue backlogs, holdout tests, and merged PR pipelines. |
| **Flavors** | **Harness Profiles** | Like the 6 quark flavors (*Up, Down, Charm, Strange, Top, Bottom*), Quark works across agent harnesses (Claude Code, Codex, Cursor, OpenCode, Pi, IBM Bob, Kiro, ...). |
| **Color Charge** | **Isolation Rings** | Security boundaries separating local desktop runtimes, sandboxed processes, and cloud microVM environments. |
    
## ✨ Key Features
    
* **Goal-Driven Projects**: Define a goal and a definition of done. Quark's coordinator plans work, manages backlog intake, and reviews diffs without directly editing production code itself.
* **Multi-Harness Parity & Complexity Routing**: Automatically route routine tasks to lightweight models and reserve frontier models for complex architectural changes using configurable dispatch rules.
* **Local & Cloud Execution Parity**: Run seamlessly in **Cloud Mode** (microVM sandboxes per project) or **Local Mode** (native processes, Seatbelt/bubblewrap sandboxes, or Docker containers).
* **Defensible Verification Gates**: Every agent run must pass repo-native unit checks, Playwright end-to-end journeys, and independent holdout tests before opening a pull request.
* **Git-Backed Context Store**: Project instructions, architectural decisions, and agent learnings live in their own versioned repository, making project context fully diffable, reviewable, and portable.

## Development

The repository is a Cargo workspace:

| Crate | Purpose |
| :--- | :--- |
| `crates/quarkd` | The local control plane daemon: `/v1` REST API, WebSocket event stream, SQLite projection, the `EngineAdapter` seam (`quarkd::engine`), and tmux terminal sessions (`quarkd::sessions`) |
| `crates/quark-systems` | Neutral API and event types shared by the daemon and its clients |
| `crates/quark-engine` | Typed firstmate reads and allowlisted, argument-validated script writes |
| `crates/quark-transcript` | Parsers for harness session logs (Claude Code, Codex, Pi) that feed `coordinator.message` and `worker.transcript` events |

```sh
cargo test --workspace
cargo run -p quarkd                 # serves http://127.0.0.1:7380, data in ~/.quark (or $QUARK_HOME)
cargo run -p quarkd -- openapi      # prints the OpenAPI document
```

`api/openapi.json` is the committed API contract; a test fails when it drifts from the code.
Regenerate it with `cargo run -p quarkd -- openapi > api/openapi.json`.

The event stream is a WebSocket at `/v1/events?cursor=<seq>`: each frame is one JSON event with a monotonic `seq`, and a client that reconnects with its last `seq` replays everything it missed.

`POST /v1/projects` with `repos` and an `agent_config` provisions a Project in the background (`crates/quarkd/src/provision.rs`).
It clones each repo into the command-center workspace (`~/.quark/workspaces/command`), seeds the Project workspace at `~/.quark/workspaces/<project-id>`, writes the Project repo (`~/.quark/projects/<project-id>.git`, checked out at `<workspace>/project`) and starts the coordinator.
`project.updated` events report each step and the final `ready` or `failed` status; `POST /v1/projects/{id}:provision` retries a failed Project.
The engine checkout under `~/.quark/engine` must be the quark-systems firstmate fork, which provides `fm-project-add.sh`, `fm-project-yolo.sh`, `fm-gates.sh`, `fm-crew-dispatch.sh` and `--answered-by` on answers.

`GET /v1/decisions` lists every Project's captain holds and workers' open keyed decisions; `POST /v1/decisions/{id}:answer` with `answer` and `answered_by` (the daemon's own user when absent) answers one through the engine (`fm-captain-hold.sh answer` for a hold, `fm-send.sh --resolve-key` for a worker's decision) and stores who answered.
`decision.opened` and `decision.answered` events report each change; a question answered outside Quark arrives answered with no `answered_by`, and one asked again opens a new decision.

Verification gates (ADR-15, `crates/quarkd/src/gates.rs`) are declared per source in `project.yaml` under `verification` (repo checks and Playwright journeys), and holdout tests live in the Project repo under `holdout/<source>/`.
Each refresh compiles that declaration into the workspace's `config/gates.json`; workers run the gates before opening a PR, and the evidence lands in `state/<task-id>.gates.json` for the PR center.

Project memory (J8, `crates/quarkd/src/memory.rs`): a worker reports what a task taught it as a `learned: <text>` status line (`learned [files=a,b]: ...` names the files; the coordinator writes `learned [source=coordinator]: ...`), and once the task is in review, done or failed each line becomes a proposal at `GET /v1/projects/{id}/memory/proposals` with its task, PR and files as evidence (`memory.proposed`).
`POST .../memory/proposals/{proposal_id}:accept`, optionally with edited `text`, commits one file under the Project repo's `memory/` in its own commit on `main` and tells the coordinator to read it (`memory.accepted`); `:reject` drops it (`memory.rejected`), and `GET /v1/projects/{id}/memory` lists the entries.

Dispatch rules (ADR-11, `crates/quarkd/src/crew_dispatch.rs`) live in the Project repo's `dispatch.yaml`: `rules` (each a `when` with one profile or a list of candidates in `use`), `default`, `default_select` and the `classifier` block.
Each refresh compiles a changed `dispatch.yaml` on `main` into the workspace's `config/crew-dispatch.json` through `fm-crew-dispatch.sh config-set`, mapping Quark harness ids (`claude-code`) to the engine's (`claude`); a file that does not compile or that the engine refuses is recorded as a failed `dispatch` adapter call and the last good config stays.

Terminal sessions need tmux 3.2 or newer (`--tmux` or `QUARKD_TMUX` picks the binary; without tmux the terminal routes answer 503).
quarkd runs one private tmux server on `~/.quark/run/tmux/quark` and attaches to it in control mode; engine calls point `TMUX` at it, so the command center, every coordinator and every worker run there (firstmate records no tmux socket per task, so the server is shared rather than one per Project).
Each Project's coordinator window (the command center's secondmate window for that Project, re-read on every refresh so it survives restarts) and task windows become its terminals: `GET /v1/projects/{id}/terminals`, input, resize and snapshot under `/v1/terminals/{id}`, and output as `worker.output` events.
A terminal's output starts with a `snapshot` chunk (reset the emulator, then feed the bytes), and the daemon keeps about 2 MiB of output per terminal.
To open a terminal, subscribe to the event stream, call `POST /v1/terminals/{id}/snapshot`, apply the returned event, then the terminal's events with a greater `seq`.

The PR center (`crates/quarkd/src/pr_center.rs`) lists every pull request a task reports at `GET /v1/pull-requests`, with checks and reviews read from GitHub through the `gh` CLI every 30 seconds (`--pr-refresh-secs`), and streams changes as `pr.updated`, `check.updated` and `review.updated`.
Each pull request carries its verification gate results (ADR-15) as `evidence`, read from the gate runner's `state/<task>.gates.json` manifest (`quark.gates.v1`); traces, screenshots and logs are served at `/v1/pull-requests/{id}/evidence/artifacts/{artifact_id}`.
Review comments (`POST /v1/pull-requests/{id}/comments`) go to the owning worker as steering messages, and `:merge` runs the engine's guarded `fm-pr-merge.sh`.
A Project's `standing_approval` merges its green pull requests without asking; turning it on also sets the engine's yolo posture for the Project's repos through `fm-project-yolo.sh` from the firstmate fork.

Quark builds itself: [`selfhost/`](selfhost/README.md) sets up the Quark Project for this repo and the firstmate fork, with its verification gates and the Phase 3 work items.

### Desktop app

`app/` is the desktop app: Tauri 2 with React, TypeScript and xterm.js, talking to `quarkd` over the `/v1` API and event stream.
See [`app/README.md`](app/README.md) for running it, and [`app/CONTRACT.md`](app/CONTRACT.md) for the endpoints it expects.

```sh
cd app && npm install
npm run dev                         # web build on http://127.0.0.1:1420, against quarkd on :7380
npm run tauri dev                   # desktop app
```

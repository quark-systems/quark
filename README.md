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
| `crates/quarkd` | The local control plane daemon: `/v1` REST API, WebSocket event stream, SQLite projection, and the `EngineAdapter` seam (`quarkd::engine`) |
| `crates/quark-systems` | Neutral API and event types shared by the daemon and its clients |
| `crates/quark-engine` | Typed firstmate reads and allowlisted, argument-validated script writes |

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
The engine checkout under `~/.quark/engine` must be the quark-systems firstmate fork, which provides `fm-project-add.sh`.

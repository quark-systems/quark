# Working on Quark

Quark is a Cargo workspace (the daemon and the native engine) plus a Tauri app in `app/`.
Build and run it as in [Build from source](../getting-started/build-from-source.md).

## Repository layout

| Path | What |
| :--- | :--- |
| `crates/` | the Rust crates, below |
| `api/openapi.json` | the committed `/v1` contract ([API overview](../api/README.md)) |
| `app/` | the desktop app: Tauri 2, React, TypeScript, xterm.js ([`app/README.md`](../../app/README.md)) |
| `selfhost/` | Quark's own Project definition and `project-init.sh` ([`selfhost/README.md`](../../selfhost/README.md)) |
| `docs/` | this documentation ([index](../README.md)) |
| `ui-poc/` | the Phase 0 UI proof of concept and benchmarks, kept as a record |
| `.agents/skills/` | agent skills for this repo, harness neutral (`.claude/skills` links to it) |

## Crates

`quarkd` is the daemon: the `/v1` REST API, the WebSocket event stream, the SQLite projection, the engine adapter seam (`quarkd::engine`) and terminal sessions.
The `quark-*` crates are the native engine, one per subsystem, meeting at the contracts in `quark-core` ([Engine internals](../engine/README.md)).

| Crate | Purpose |
| :--- | :--- |
| [`quark-coordinator`](../../crates/quark-coordinator) | The judgment-only coordinator for the native Quark engine (slice 6): layered prompt, wake queue, native tools and turn accounting. |
| [`quark-core`](../../crates/quark-core) | Contracts for the native Quark engine: event, task, worktree, session, harness, worker, verification, isolation, host, telemetry and persona traits, plus in-memory fakes. |
| [`quark-dispatch`](../../crates/quark-dispatch) | Dispatch, account pools and admission control for the native Quark engine (slice 5): which agent runs a task, under which account, and on which host, with every decision in the event log. |
| [`quark-eventlog`](../../crates/quark-eventlog) | The native engine's crash-safe event log (SQLite events.db), task state machine and replay. |
| [`quark-harness`](../../crates/quark-harness) | Declarative harness manifests: the built-in TOML manifests, the loader and registry, and the detection, credential and launch logic every engine shares. |
| [`quark-hosts`](../../crates/quark-hosts) | Host resource telemetry for the native Quark engine: sampled CPU, memory, pressure and disk, attributed per Project and task. |
| [`quark-isolation`](../../crates/quark-isolation) | Isolation for the native Quark engine: native processes and sandboxes (Seatbelt on macOS, bubblewrap on Linux). |
| [`quark-persona`](../../crates/quark-persona) | Persona packs: the built-in plain, nautical and kitchen brigade packs, the TOML and prompt-fragment loader, and the global default with per-Project overrides. |
| [`quark-runtime`](../../crates/quark-runtime) | Runtimes for the native Quark engine: run commands, move files and host terminal sessions on the local machine or an SSH host, behind one interface. |
| [`quark-sessions`](../../crates/quark-sessions) | Terminal sessions for the native Quark engine: tmux control mode and the PTY supervisor, behind quark-core's SessionBackend. |
| [`quark-subcoordinator`](../../crates/quark-subcoordinator) | Sub-coordinators for the native Quark engine (slice 7): persistent coordinator homes on this machine or an SSH host, seeded, launched, supervised, steered and handed work, with every change in the event log. |
| [`quark-supervisor`](../../crates/quark-supervisor) | Spawn, steer and supervise workers for the native Quark engine (slice 4): worktree, harness manifest, isolation and session composed behind one supervisor, with every change in the event log. |
| [`quark-systems`](../../crates/quark-systems) | Neutral /v1 API and event types for the Quark control plane |
| [`quark-transcript`](../../crates/quark-transcript) | Parsers for harness session logs (Claude Code, Codex, Pi, Kiro CLI) into neutral transcript entries. |
| [`quark-triggers`](../../crates/quark-triggers) | Triggers, channels (the inbox first) and the away policy for the native Quark engine (slice 7), recorded in the event log. |
| [`quark-verify`](../../crates/quark-verify) | The native engine's verification workflow and merge guard (slice 2): gates, rebase and re-verify, and the red-main guardrail. |
| [`quark-worker`](../../crates/quark-worker) | The worker protocol: MCP tools, harness hook ingest and a status-file fallback, all recorded in the event log. |
| [`quark-worktree`](../../crates/quark-worktree) | WorktreeProvider for the native Quark engine: treehouse 3.1.2 adapter, the native pool that replaces it (slice 9) and the shadow comparing them, with the isolation assertion, tangle check and landed-work check. |
| [`quarkd`](../../crates/quarkd) | Quark local control plane daemon |

## Tests

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd app
npm run build           # typecheck and production bundle
npm test                # unit tests (vitest)
npm run e2e             # Playwright against the demo daemon
```

The worktree tests need treehouse 3.1.2 (`selfhost/treehouse.sh`, or `TREEHOUSE_BIN`).
CI (`.github/workflows/ci.yml`) runs the same checks on every pull request.

After changing the API, regenerate the contract with `cargo run -p quarkd -- openapi > api/openapi.json`.

## Agent skills

- `verify-quark` launches an isolated Quark (quarkd from this checkout, the app's web build against it, a headless Chromium) and drives it the way a user does, with evidence and cleanup. Its `features/` map lists what to verify and which journey covers each feature.
- `create-verification` is the generic skill that generated it, adapted from Cursor's create-verification-skill (MIT); it generates a `verify-<app>` skill for any repo.
- `beads` covers issue tracking with `bd`.

Agents working in this repo follow [`AGENTS.md`](../../AGENTS.md).

## Writing docs

Put a new page in the `docs/` folder it belongs to and link it from that folder's index and from [the docs index](../README.md).
Engine pages describe one crate each; API pages describe one resource each, with its routes and events.
Keep the README short: it is for someone deciding to try Quark and getting it running.

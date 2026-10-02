# quark-ui-stub-daemon

A stub daemon for the UI proof-of-concept.
It serves the API in [`../CONTRACT.md`](../CONTRACT.md) on `http://127.0.0.1:7420`, using realistic fake data, background activity, a streaming fake coordinator chat, and four live terminal panes backed by tmux.

## Run

Requires Rust (edition 2024) and tmux 3.x.

```sh
cargo run --release              # listens on 127.0.0.1:7420
cargo run --release -- --port 7421
```

Press Ctrl-C (or send SIGTERM) to stop it. Stopping also kills the private tmux server.
The tmux socket is `quark-poc` on the default port and `quark-poc-<port>` on any other port, so several instances can run side by side.
To look at the panes directly, run `tmux -L quark-poc attach`.

If tmux is missing or cannot start, the daemon still serves everything else, and `GET /v1/workers` returns `[]`.

## What it serves

- **Data**: 3 projects (`quark`, `firstmate`, `website`), 15 tasks, 3 open decisions plus 1 answered one, and 4 PRs.
  Each PR has a real multi-file unified diff (Rust, TS/TSX, Markdown) and some existing review comments.
  Each coordinator has a chat history in markdown.
- **Background activity**: a task changes state every 3 s.
  New tasks appear every 15 to 30 s, and sooner when few tasks are active.
  A decision opens every 45 s, and its task moves to `needs_decision`.
  PR checks flip every 6 to 12 s (`pr.updated`).
  Answering a decision moves its task back to `running`.
- **Chat**: `POST /v1/coordinators/{id}/messages` first emits the user message.
  It then streams one of a few canned markdown replies (code blocks, a table, lists) as `coordinator.delta` events, a few words every 30 ms.
  The stream ends with a `coordinator.message` that carries the full text.
  The deltas concatenate exactly to the final text.
- **Workers** (one tmux window each, 120x36 to start):
  - `w-1`: a colourful `cargo build` and `cargo test` log that loops, with a failing test every 4th run.
  - `w-2`: `top -d 1`, a full-screen app that redraws when resized.
  - `w-3`: an agent log with 256-colour and truecolour escapes, a progress bar redrawn with `\r`, box drawing, CJK and emoji.
  - `w-4`: an interactive `bash` shell. Typed input echoes back, which makes it the pane for input-latency tests.

## Implementation notes

- **Event store** (`src/store.rs`): each event is serialised once and stored with a monotonic `seq`.
  The store keeps the last 2 MB of `worker.output` per worker and the last 20,000 other events.
  Live fan-out uses a `tokio::broadcast` channel that holds 4096 events.
  A client that falls behind is resynced from the store, so a slow client never blocks the server.
- **tmux** (`src/tmux.rs`): one control-mode client (`tmux -C attach`) reads `%output %<pane> <octal-escaped>` lines and decodes the `\ooo` escapes.
  Lines from the same pane are joined while more input is already buffered, and flushed right away when idle, so echo latency is not affected.
  - Input uses `send-keys -t %N -H <hex…>`.
  - Resize uses `resize-window`, which also switches that window to `window-size manual`.
  - The pane scripts are compiled into the binary from `scripts/` and written to a temp directory at startup.
  - Each pane runs `loop.sh`, which restarts the program when it exits, so typing `exit` or `q` is harmless.
- **Stress**: `POST /v1/workers/{id}/stress {"on":true}` creates a flag file.
  The daemon then stops the pane's current program, and the loop starts `flood.sh` instead.
  Turning stress off reverses this.
  The bash pane's shell history is lost when stress toggles.
  `respawn-pane` is not used, because tmux 3.4 crashes when a pane that is flooding a control client gets respawned.
- **CORS**: any origin is allowed.

## Verification

`cargo test` covers the octal decoding, worker-output retention bounds, cursor replay, and that chat chunks reassemble exactly.

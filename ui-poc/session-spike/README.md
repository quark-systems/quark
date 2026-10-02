# quark-session-spike

A spike that checks whether quarkd should use [herdr](https://github.com/herdrdev/herdr) instead of tmux control mode as the session layer for worker terminals.
The findings and the recommendation are in [REPORT.md](REPORT.md).
The raw output of the runs quoted there is in [results/](results/).

The spike has two arms:

- **herdr arm** (`src/herdr_arm.rs`): an isolated herdr server, driven only through herdr's public surfaces.
  These are the JSON socket API (`src/herdr.rs`, `Api`) and the `herdr terminal session observe|control` bridge (`TermSession`).
  It creates 4 panes running the stub daemon's scripts (`../stub-daemon/scripts/`) and attaches one controller per pane.
  Then it measures stream volume, input-to-echo latency (idle and during a flood), resize, two viewers at different sizes, events, and a simulated quarkd crash and reattach.
  A separate command restarts the herdr server and reports what survived.
- **tmux arm** (`src/stub_arm.rs`): the UI POC stub daemon (tmux control mode) on its own port, driven over its HTTP and WebSocket contract (`../CONTRACT.md`).
  It runs the same measurements.
  `tmux-reattach.sh` checks what tmux keeps when a control-mode client dies.

## Requirements

- Linux (CPU, RSS and process lookups use `/proc`). Rust 1.85 or newer (edition 2024).
- tmux 3.x, for the tmux arm.
- A herdr binary. The runs used the official v0.9.3 release, not a source build. REPORT.md ("Build and packaging") explains why.

## Get herdr

```sh
mkdir -p .run
curl -fL -o .run/herdr https://github.com/herdrdev/herdr/releases/download/v0.9.3/herdr-linux-x86_64
# Must match distribution/latest.json in the herdr repo at tag v0.9.3:
echo "18a8dc65f1c2fa485884344356dea1cfd911c6f06cf46fa78e193f4087f4dba7  .run/herdr" | sha256sum -c
chmod +x .run/herdr
```

To build from source instead, you need Zig 0.16.0 and the Zig packages listed in REPORT.md.
Then run `cargo build --release` in a herdr checkout and pass `--herdr <checkout>/target/release/herdr`.

## Run

```sh
cargo build --release

# herdr arm. Starts an isolated server under .run/config (XDG_CONFIG_HOME), so
# your own herdr sessions are not touched. The config it writes turns off
# herdr's background calls to herdr.dev.
./target/release/quark-session-spike herdr > results/herdr-fresh.md

# Run it again: the panes from the first run are found and reused (they survive
# the spike process exiting).
./target/release/quark-session-spike herdr > results/herdr-reused.md

# Stop and restart the herdr server and report what survived. Run this last:
# it ends the pane processes.
./target/release/quark-session-spike server-restart > results/herdr-server-restart.md

# tmux arm: start the stub daemon on its own port, then measure it.
(cd ../stub-daemon && cargo run --release -- --port 7450) &
./target/release/quark-session-spike stub --port 7450 > results/tmux-stub.md
./tmux-reattach.sh > results/tmux-reattach.md

# Clean up: close the spike workspace and stop the isolated herdr server.
./target/release/quark-session-spike cleanup
kill %1    # the stub daemon (this also kills its tmux server)
```

Options: `--samples N` sets the number of latency samples (default 200).
`--herdr BIN` and `--run DIR` override the herdr binary and the state directory (default `.run/`).
Keep `--run` short, because herdr's socket path must fit in `sun_path` (about 108 bytes).

## Notes

- Each run takes about 1.5 minutes. The host for the quoted runs was a shared 4-vCPU VM, so p99 values are noisy.
- Latency is measured from the moment the bytes are handed to the transport until the echoed character arrives in the stream the daemon would consume.
  For herdr that stream is the controller's `terminal.frame` records. For tmux it is the stub's WebSocket `worker.output` events, so the tmux numbers include an HTTP request and a WebSocket hop.
- The flood producer rate comes from the 8-digit counter that `flood.sh` prints.
  For herdr it is read from the screen through `pane.read`, because frames carry only screen changes.
  For tmux it is read from the delivered bytes.
- `.run/` (herdr binary, isolated config, scripts) is gitignored.

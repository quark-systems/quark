# Terminals

Each Project's coordinator and each running worker has a terminal.
Coordinators run in a private tmux server (tmux 3.2 or newer) on `~/.quark/run/tmux/quark`, which quarkd attaches to in control mode; workers run in the PTY supervisor (`quark-ptyd`) and report their terminal as `pty:<session id>`.
The API is the same for both; without tmux the coordinator terminal routes answer 503.

## Routes

- `GET /v1/projects/{id}/terminals` lists a Project's terminals.
- Input, resize and snapshot live under `/v1/terminals/{id}`.
- Output arrives as `worker.output` events.

## Opening a terminal

A terminal's output starts with a `snapshot` chunk (reset the emulator, then feed the bytes), and the daemon keeps about 2 MiB of output per terminal.

1. Subscribe to the event stream.
2. Call `POST /v1/terminals/{id}/snapshot`.
3. Apply the returned event, then the terminal's events with a greater `seq`.

[The native engine](../engine/native-engine.md#worker-terminals) covers how PTY sessions reattach after a restart or relaunch.

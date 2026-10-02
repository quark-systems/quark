# Session-log fixtures

Small hand-built logs, one per harness, shaped like what each harness writes:

- `claude.jsonl`: Claude Code `~/.claude/projects/<slug>/<id>.jsonl`. Record shapes checked against a real Claude Code 2.1 log.
- `codex.jsonl`: Codex CLI rollout envelope (`timestamp`, `type`, `payload`), from `codex-rs/protocol` (`ResponseItem`, `EventMsg`).
- `pi.jsonl`: Pi coding agent session v3, from `@mariozechner/pi-coding-agent` `SessionManager` and `@mariozechner/pi-ai` message types.

Each covers bookkeeping lines that must be skipped, a user prompt, reasoning, a tool call and result (one failing), and a final reply.

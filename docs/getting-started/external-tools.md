# External tools

Every program outside this repository that quarkd, the desktop app or the menu bar app runs at runtime. Rust and npm library dependencies are not listed; see `Cargo.lock`, `app/package.json` and [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md).
Keep this page current: a PR that makes Quark shell out to a new program, or stop using one, updates it, and adds a check to `crates/quarkd/src/setup.rs` (`quarkd doctor`, `GET /v1/setup`) when people must install it.

## Third-party helper tools

Small tools Quark inherited from firstmate. Each one is a candidate to port into Quark, as the native worktree pool did for treehouse and the native quota readers did for quota-axi.

| Tool | What Quark uses it for | Where | How it is installed | Required |
| :--- | :--- | :--- | :--- | :--- |
| `treehouse` 3.1.2+ ([kunchenguid/treehouse](https://github.com/kunchenguid/treehouse)) | Only as a fallback: quarkd's own native pool serves worktrees by default, on treehouse's pool format. `QUARK_WORKTREE_POOL=treehouse` uses treehouse instead (e.g. for a jj repository, the one setting the native pool refuses), and `QUARK_WORKTREE_POOL=shadow` runs it with the native pool compared | `crates/quark-worktree/src/treehouse.rs`, `crates/quarkd/src/native_worktrees.rs` | `selfhost/treehouse.sh` (pinned sha256); CI downloads it for the parity tests (`TREEHOUSE_BIN`) | No: only with `QUARK_WORKTREE_POOL=treehouse` or `shadow` |

Account quota for every harness with a quota reader (Claude Code, Codex, IBM Bob, Kiro CLI, Grok, Kimi Code, Cursor, Antigravity, Muse, Gemini CLI, GitHub Copilot, OpenRouter, DeepSeek, Z.ai, MiniMax, OpenCode Go, Command Code, Xiaomi MiMo and Alibaba Coding Plan, and every provider Pi and OpenCode are signed in to) is read by quarkd itself (`crates/quark-dispatch/src/local/`). `quota-axi` and `tasks-axi`, needed in the firstmate era, are no longer used.

## Core tools

| Tool | What Quark uses it for | Where | How it is installed | Required |
| :--- | :--- | :--- | :--- | :--- |
| `git` | Repos, worktrees, branches, rebases and merges, Project memory | `crates/quark-worktree`, `crates/quark-verify`, `crates/quarkd` (`project_repo.rs`, `worktree.rs`, `gates.rs`, `memory.rs`) | System or Homebrew | Yes |
| `tmux` 3.2+ | Coordinator terminals, over one shared server in control mode | `crates/quark-sessions/src/tmux/`, `crates/quark-runtime/src/tmux.rs` | `brew install tmux`; `--tmux` or `QUARKD_TMUX` picks the binary | Yes |
| `gh` (GitHub CLI) | The PR center and merges (`gh api`), cloning, the GitHub token for Beads sync (`gh auth token` when `GITHUB_TOKEN` is unset), and GitHub Copilot's quota when no other Copilot sign-in is found | `crates/quarkd/src/forge.rs`, `crates/quark-verify/src/forge.rs`, `crates/quarkd/src/beads.rs`, `crates/quark-dispatch/src/local/copilot.rs` | `brew install gh`, then `gh auth login` | Yes |
| `bd` ([Beads](https://beads.gascity.com)) | The Issues tab, decisions and Beads memory; `quarkd triage` files issues with it | `crates/quarkd/src/beads.rs`, `crates/quarkd/src/beads/`, `crates/quarkd/src/triage.rs` | Latest bd from beads.gascity.com; `QUARKD_BD` picks the binary | Yes (Setup requires it for 0.1) |
| `dolt` | The SQL server Beads runs each Project's database on (server mode) | `crates/quarkd/src/beads.rs` (checked before Beads starts) | [Dolt install](https://docs.dolthub.com/introduction/installation), e.g. `brew install dolt`; `BEADS_DOLT_BIN` overrides | With `bd` |
| `curl` | The providers' usage APIs (Claude Code, Codex, IBM Bob, Grok, Kimi Code, Cursor, Muse, Gemini CLI, GitHub Copilot and the key-based providers), and the dispatch classifier's HTTP calls (System One or Ollama); the key goes on stdin, never argv | `crates/quark-dispatch/src/local/`, `crates/quark-dispatch/src/classify.rs` | System | For quota and the classifier |
| `bl` (Alibaba Cloud CLI) | Alibaba Coding Plan's quota (`bl usage token-plan --output json`) | `crates/quark-dispatch/src/local/alibaba.rs` | Alibaba Cloud's installer, then `bl login` | For Alibaba quota only |
| `ssh` | Remote runtimes for SSH hosts | `crates/quark-runtime/src/exec.rs`, `crates/quark-runtime/src/ssh.rs` | System OpenSSH; `QUARK_SSH` overrides | For SSH hosts only |
| `bash`, `sh` | Verification gate checks, trigger commands, login-shell PATH and quoting probes | `crates/quark-verify/src/pipeline.rs`, `crates/quark-triggers/src/commands.rs`, `crates/quark-runtime/src/quote.rs`, `app/src-tauri/src/release.rs` | System | Yes |

## Agent harnesses

The coordinator and workers are CLI agents. Each has a manifest in `crates/quark-harness/manifests/`, which names the binary quarkd detects on `PATH` and the install hint the app shows. You need at least one coordinator harness: Claude Code (the default, and the default worker), Codex or Pi.

| Harness | Binary | Install |
| :--- | :--- | :--- |
| Claude Code | `claude` | `npm install -g @anthropic-ai/claude-code` |
| Codex | `codex` | `npm install -g @openai/codex` |
| Cursor Agent | `cursor-agent` or `agent` | `curl https://cursor.com/install -fsS \| bash` |
| Gemini CLI | `gemini` | `npm install -g @google/gemini-cli` |
| OpenCode | `opencode` | `npm install -g opencode-ai` |
| Pi, Pi (signed) | `pi`, `pi-signed` | `npm install -g @mariozechner/pi-coding-agent` |
| IBM Bob | `bob` | Vendor installer |
| Kiro CLI | `kiro-cli` | Vendor installer |
| Antigravity CLI | `agy` | Vendor installer |
| Grok Build | `grok` | Vendor installer |
| Kimi Code | `kimi` | Vendor installer |
| Muse Code | `muse` | Vendor installer |
| Oh My Pi | `omp` | Vendor installer |
| Rovo Dev | `rovo` | Vendor installer |

A manifest's `models.list` command, if any, also runs to list models (`crates/quark-harness/src/catalog.rs`).

Some quota readers also run a harness's own CLI (`crates/quark-dispatch/src/local/`): `agy -p /quota --output-format json` for Antigravity's quota (with stand-in `open` and `xdg-open` first on its `PATH`, so it never opens a browser), `grok models` to let the Grok CLI renew a rejected session, and `claude doctor` to let Claude Code renew an expired one. Gemini CLI is not run; its OAuth client is read from its installed `oauth2.js` to renew an expired token in memory.

## Operating system tools

Shipped with the OS; listed so a port to a new platform knows what to replace.

| Tool | What Quark uses it for | Where | Platform |
| :--- | :--- | :--- | :--- |
| `security` | Keychain: whether a harness login exists, the Claude Code, Cursor CLI, Muse and Copilot CLI logins for their quota reads, API keys for key accounts and default logins (Bob, Kiro), the classifier's `keychain:` credential | `crates/quark-harness/src/host.rs`, `crates/quarkd/src/secrets.rs`, `crates/quark-dispatch/src/classify.rs`, `crates/quark-dispatch/src/local/` (`claude.rs`, `support.rs`) | macOS |
| `sqlite3` | Cursor's sign-in from the editor's `state.vscdb`, for its quota read (opened read-only) | `crates/quark-dispatch/src/local/cursor.rs` | macOS (Linux: `apt install sqlite3`) |
| `sandbox-exec` | Seatbelt isolation for sandboxed workers | `crates/quark-isolation/src/seatbelt.rs` | macOS |
| `bwrap` (bubblewrap) | Sandbox isolation for workers, when installed | `crates/quark-isolation/src/bwrap.rs` | Linux (`apt install bubblewrap`) |
| `sysctl` | Memory pressure for host health | `crates/quark-hosts/src/pressure.rs` | macOS (Linux reads `/proc/pressure`) |
| `ps` | Which process is in a terminal's foreground | `crates/quark-sessions/src/tmux/mod.rs` | macOS, Linux |
| `hostname` | The host name in `quarkd diagnose` reports | `crates/quarkd/src/diagnose.rs` | macOS, Linux |
| `launchctl` | The menu bar app restarts quarkd's launch agent | `mac/Sources/QuarkMenuBar/Daemon.swift` | macOS |

## Services

Reached over the network rather than run as a program.

| Service | What for | Where |
| :--- | :--- | :--- |
| GitHub API | PRs, checks, merges (through `gh`), Beads sync to `refs/dolt/data` | `crates/quarkd/src/forge.rs`, `crates/quarkd/src/beads.rs` |
| Anthropic OAuth usage API (`api.anthropic.com/api/oauth/usage`) | Claude Code's session and weekly windows, with Claude Code's own login | `crates/quark-dispatch/src/local/claude.rs` |
| ChatGPT usage API (`chatgpt.com/backend-api/wham/usage`) | Codex's session and weekly windows, with Codex's (or Pi's) login | `crates/quark-dispatch/src/local/codex.rs` |
| IBM Bob usage API | Bob's live coin usage; needs `BOBSHELL_API_KEY` in quarkd's environment | `crates/quark-dispatch/src/local/bob.rs` |
| System One API or an Ollama server | The dispatch classifier, when configured in `~/.quark/config.yaml` | `crates/quark-dispatch/src/classify.rs` |

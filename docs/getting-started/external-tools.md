# External tools

Every program outside this repository that quarkd, the desktop app or the menu bar app runs at runtime. Rust and npm library dependencies are not listed; see `Cargo.lock`, `app/package.json` and [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md).
Keep this page current: a PR that makes Quark shell out to a new program, or stop using one, updates it.

## Third-party helper tools

Small tools Quark inherited from firstmate. Each one is a candidate to port into Quark, as the native worktree pool is doing for treehouse.

| Tool | What Quark uses it for | Where | How it is installed | Required |
| :--- | :--- | :--- | :--- | :--- |
| `quota-axi` ([kunchenguid/quota-axi](https://github.com/kunchenguid/quota-axi)) | Account quota for Claude and Codex: dispatch judges candidates on one `quota-axi --json` snapshot, and the Accounts view runs `quota-axi --provider claude\|codex --json` per account | `crates/quark-dispatch/src/quota.rs`, `crates/quarkd/src/accounts.rs`, `crates/quarkd/src/native_dispatch.rs` | `npm install -g quota-axi`; `--quota-axi` or `QUARKD_QUOTA_AXI` picks the binary | Optional: without it quota is unmeasured, so candidates stay eligible but unranked |
| `treehouse` 3.1.2+ ([kunchenguid/treehouse](https://github.com/kunchenguid/treehouse)) | The worktree pool workers run in (acquire, return, list) | `crates/quark-worktree/src/treehouse.rs`, `crates/quarkd/src/native_worktrees.rs` | `selfhost/treehouse.sh` (pinned sha256); `packaging/wsl/install.sh` on WSL; CI downloads it. `QUARK_TREEHOUSE_BIN` overrides | Yes, until the native pool (`QUARK_NATIVE_WORKTREES=1`, shadow only today) replaces it |

IBM Bob and Kiro CLI quota is read by quarkd itself (`crates/quark-dispatch/src/local/`), not by quota-axi. `tasks-axi`, needed in the firstmate era, is no longer used.

## Core tools

| Tool | What Quark uses it for | Where | How it is installed | Required |
| :--- | :--- | :--- | :--- | :--- |
| `git` | Repos, worktrees, branches, rebases and merges, Project memory | `crates/quark-worktree`, `crates/quark-verify`, `crates/quarkd` (`project_repo.rs`, `worktree.rs`, `gates.rs`, `memory.rs`) | System or Homebrew | Yes |
| `tmux` 3.2+ | Coordinator terminals, over one shared server in control mode | `crates/quark-sessions/src/tmux/`, `crates/quark-runtime/src/tmux.rs` | `brew install tmux`; `--tmux` or `QUARKD_TMUX` picks the binary | Yes |
| `gh` (GitHub CLI) | The PR center and merges (`gh api`), cloning, and the GitHub token for Beads sync (`gh auth token` when `GITHUB_TOKEN` is unset) | `crates/quarkd/src/forge.rs`, `crates/quark-verify/src/forge.rs`, `crates/quarkd/src/beads.rs` | `brew install gh`, then `gh auth login` | Yes |
| `bd` ([Beads](https://beads.gascity.com)) | The Issues tab, decisions and Beads memory; `quarkd triage` files issues with it | `crates/quarkd/src/beads.rs`, `crates/quarkd/src/beads/`, `crates/quarkd/src/triage.rs` | Latest bd from beads.gascity.com; `QUARKD_BD` picks the binary | Optional: Issues and Beads memory are off without it |
| `dolt` | The SQL server Beads runs each Project's database on (server mode) | `crates/quarkd/src/beads.rs` (checked before Beads starts) | [Dolt install](https://docs.dolthub.com/introduction/installation), e.g. `brew install dolt`; `BEADS_DOLT_BIN` overrides | With `bd` |
| `curl` | IBM Bob's live usage API and the dispatch classifier's HTTP calls (System One or Ollama); the key goes on stdin, never argv | `crates/quark-dispatch/src/local/bob.rs`, `crates/quark-dispatch/src/classify.rs` | System | For Bob live quota and the classifier |
| `ssh` | Remote runtimes for SSH hosts | `crates/quark-runtime/src/exec.rs`, `crates/quark-runtime/src/ssh.rs` | System OpenSSH; `QUARK_SSH` overrides | For SSH hosts only |
| `bash`, `sh` | Verification gate checks, trigger commands, login-shell PATH and quoting probes | `crates/quark-verify/src/pipeline.rs`, `crates/quark-triggers/src/commands.rs`, `crates/quark-runtime/src/quote.rs`, `app/src-tauri/src/release.rs` | System | Yes |

## Agent harnesses

The coordinator and workers are CLI agents. Each has a manifest in `crates/quark-harness/manifests/`, which names the binary quarkd detects on `PATH` and the install hint the app shows. Only Claude Code is required (it is the coordinator, and the default worker).

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

## Operating system tools

Shipped with the OS; listed so a port to a new platform knows what to replace.

| Tool | What Quark uses it for | Where | Platform |
| :--- | :--- | :--- | :--- |
| `security` | Keychain: whether a harness login exists, Bob API keys for added accounts, the classifier's `keychain:` credential | `crates/quark-harness/src/host.rs`, `crates/quarkd/src/secrets.rs`, `crates/quark-dispatch/src/classify.rs` | macOS |
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
| IBM Bob usage API | Bob's live coin usage; needs `BOBSHELL_API_KEY` in quarkd's environment | `crates/quark-dispatch/src/local/bob.rs` |
| System One API or an Ollama server | The dispatch classifier, when configured in `~/.quark/config.yaml` | `crates/quark-dispatch/src/classify.rs` |

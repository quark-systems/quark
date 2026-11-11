# Dispatch, pools and admission control (slice 5)

`crates/quark-dispatch` is the native replacement for firstmate's dispatch intake: `fm-dispatch-resolve.sh` (which agent runs a task), quarkd's account leases (which account it runs under), and the admission control firstmate never had (which host it runs on, or whether it waits).
It sits in front of the slice 4 supervisor (`docs/engine/supervision.md`) and keeps every decision in the event log.

## The path of one task

1. **Request.** `Dispatcher::submit` records `dispatch.requested` with the task's title, brief, repo, branch, isolation and environment. A request may name a profile itself; then the rules are skipped.
2. **Resolve.** The Project's rules (`dispatch.yaml`, compiled to `crew-dispatch.json`) are resolved exactly as firstmate resolves them, and the result is recorded as `dispatch.resolved`:
   - the classifier (the System-1 API, hosted or an Ollama server's; `classify::SystemOne`) picks a rule or `default` from the brief, with a confidence;
   - a rule needing approval, or whose floor cannot be verified, escalates; a rule floor below falls through to the default profiles; below the confidence floor the result is `ambiguous`, unless `on_failure: default` sends it (and any classifier failure) to the default profiles;
   - each candidate is judged on one quota snapshot from quarkd's own readers ([Quota readers](#quota-readers)): exhausted runway, 0% left without overage, or a profile floor below rule it out; missing or unverifiable evidence leaves it eligible but unranked; `pricing: budget` needs no quota;
   - `ordered` takes the first eligible candidate (escalating if its evidence is unverifiable), `quota-balanced` takes the highest spend priority, a budget candidate only when none ranks, and escalates on a tie.
3. **Choose.** A clear resolution picks an account from the profile's pool and records `dispatch.chosen`. Pool rules are quarkd's: sticky while the account stays launchable and in the pool, otherwise the ready account with the fewest running tasks, then the most quota left. Anything not clear (off, ambiguous, escalated, error, an empty or exhausted pool) is `dispatch.escalated` until the coordinator calls `Dispatcher::choose` with a profile.
4. **Admit.** Each `Dispatcher::tick` (oldest first) asks the red-main guard (`MergeGuard::may_dispatch`) and then admission control for a host. A held task records `dispatch.held` once per distinct reason.
5. **Place and spawn.** A placed task records `dispatch.placed` and goes to that host's `Spawner` (the local supervisor; other hosts arrive with their runtimes), then `dispatch.spawned` or `dispatch.spawn_failed`.

A task can be taken back with `Dispatcher::withdraw` until it is placed.

## Quota readers

`quark_dispatch::local` reads every provider itself, each as a one-provider schema-5 snapshot (the shape quota-axi printed, which Quark no longer needs): Claude Code, Codex, IBM Bob and Kiro CLI (ports of firstmate's `fm-bob-quota.sh` and `fm-kiro-quota.sh`), Grok, Kimi Code, Cursor, Antigravity and Muse (ports of quota-axi's), and Gemini CLI and GitHub Copilot. The providers are read side by side. Their harnesses' manifests name their provider (`[quota]`), so a profile needs no `provider`, and Accounts shows each one's quota. A reading that cannot be had leaves the provider unmeasured (eligible, unranked); it never invents a number. Every row carries a `spendPriority` (how far each window's allowance runs ahead of or behind its clock, weighted by cycle length), so providers rank against each other under `quota-balanced`.

`quarkd quota claude|codex|bob|kiro|grok|kimi|cursor|agy|muse|gemini|copilot` prints the reading in one line (`--json` for the snapshot dispatch sees; `--config-dir` reads another login's directory, for Claude, Codex, Grok, Kimi, Muse and Gemini).

**Claude Code** reads Claude Code's own OAuth login: on macOS the Keychain item `Claude Code-credentials` (for another `CLAUDE_CONFIG_DIR`, the item suffixed with the first 8 hex digits of that path's SHA-256), with `security find-generic-password`, the tool Claude Code writes it with, so a quarkd started by launchd reads it without a prompt; else `<config dir>/.credentials.json`. The token goes to `GET https://api.anthropic.com/api/oauth/usage`: the five-hour session and the week bound `all_models`, a model's week adds a bound for its `model:<id>` scope, and extra usage is shown but bounds nothing. An expired session is renewed by Claude Code itself (`claude doctor`, at most every 10 minutes per login); quarkd never spends the refresh token. Should a Keychain item still want a prompt, the background read gives up after 10 s and stops asking for that item, and Accounts offers **Allow Keychain access** (`POST /v1/accounts/{id}/keychain-grant`), which waits for the person to answer it.

**Codex** reads `auth.json` in the account's `CODEX_HOME` (default `~/.codex`) and sends its ChatGPT token and account id to `GET https://chatgpt.com/backend-api/wham/usage` (then `/backend-api/codex/usage`): the five-hour and weekly windows bound `all_models`, a named model limit adds `model:<id>`, code review is its own `code_review` scope. With no Codex login, the default account is read with Pi's (`openai-codex` in `~/.pi/agent/auth.json`). An API-key sign-in has no plan windows.

**Grok** reads the Grok CLI's `auth.json` (`GROK_AUTH_JSON`, `GROK_AUTH_PATH`, else the account's `GROK_HOME`, default `~/.grok`), grok.com sessions first and API keys never (they have no plan credits), and sends the session to grok.com's `GetGrokCreditsConfig` (gRPC-web): the shared credits window for the billing period bounds `all_products`, and each product (`product:grok_build`, `product:chat`, ...) adds its own bound. A rejected session that carries a refresh token is renewed by the CLI (`grok models`, which lists models and spends nothing) and read once more. With no Grok login, the default account is read with Pi's OAuth login to xAI.

**Kimi Code** reads `credentials/kimi-code.json` in `KIMI_CODE_HOME` (default `~/.kimi-code`), or Pi's `kimi-coding` login, and asks `GET https://api.kimi.com/coding/v1/usages`: the five-hour, weekly and monthly-total windows jointly bound `all_models`; where the older principal `usage` figure reports the same window, the more-used figure wins.

**Cursor** reads the editor's token from its `state.vscdb` (with `sqlite3`, read-only), else the Cursor CLI's (Keychain item `cursor-access-token` on macOS, `~/.config/cursor/auth.json` elsewhere), and asks Cursor's dashboard service: included, auto, API and spend-limit usage for the billing cycle jointly bound `all_models`; the Grok Bot weekly allowance is its own `grok_bot` scope. Only the default login is read.

**Antigravity** runs `agy -p /quota --output-format json`: the Gemini five-hour and weekly windows bound the `gemini` scope, the Claude/GPT ones `claude_gpt`.

**Muse** reads the Muse CLI's `auth.json` (`~/.config/muse`, or the account's directory), its Keychain item (`ai.meta.dev.credentials`), or `META_API_KEY`, and asks Meta's `POST /muse-code/key`, at most every five minutes: the five-hour and weekly subscription windows bound `all_models`. API-key accounts are read with their key.

**Gemini CLI** reads `oauth_creds.json` (`~/.gemini`, or the account's directory), finds the Code Assist project (`GOOGLE_CLOUD_PROJECT`, else `loadCodeAssist`), and asks `retrieveUserQuota`: each model's daily window bounds `model:<id>`. An expired token is renewed in memory only, with the installed Gemini CLI's own OAuth client; it is never written back.

**GitHub Copilot** reads the Copilot CLI's login (Keychain item `copilot-cli`), the editors' `github-copilot/apps.json`, or `gh`'s, and asks GitHub's `copilot_internal/user`: each monthly allowance (premium requests, chat, completions) is shown, but none bounds every model, so the reading has no rows.

A Keychain item that would raise a prompt is handled as for Claude Code: the background read gives up on it, and **Allow Keychain access** in Accounts waits for the answer.

Readings are reused for 45 s, so dispatch and Accounts reading together make one request.

**Kiro** runs `kiro-cli chat --v2 --no-interactive /usage` in `<home>/run/kiro-usage-probe` (75 s bound: with an API key `/usage` takes 20-30 s, sometimes more), which prints the plan's credits without a model call. It always passes `--v2`: under `--v3` the same text is a prompt, and spends credits. `<home>/quota/kiro.json` is optional:

```json
{ "overage_allowed": true }
```

Overage is a budget, never a block: when it is allowed (that file, or `/usage` showing use past the plan) the row never reads exhausted, its runway is `through_reset`, and use past the plan drives `spendPriority` negative. Without it, a spent plan is `exhausted_now` and a month's pace that runs out before the reset is `projected_exhaustion`. A profile floor still applies either way.

**Bob** reads, in order:

1. the live Bob admin API when `BOBSHELL_API_KEY` is set in quarkd's environment (the profile, then each team's usage, summed; the key goes to curl on stdin, never on its command line), which is exact;
2. otherwise this machine's ledger, the per-session coins in `~/.bob/db/bob.db` (opened read-only), plus an anchor: one "Monthly Usage" figure from Bob's `/status`, recorded with `quarkd quota bob --anchor <coins> [--budget <coins>]`. The estimate is the anchor plus ledger growth since, while the anchor is from this charge month and younger than `anchor_max_age_hours`. Without a fresh anchor the reading is `partial`: a floor on Bob is unverifiable, so an `ordered` list escalates rather than skipping past Bob, and a ledger that alone reaches the allowance reads `exhausted_now`.

`<home>/quota/bob.json` is optional; an unknown key is refused:

| Field | Default | |
|---|---|---|
| `monthly_coins` | `1500` | the allowance when the API reports none |
| `reset_day` | `1` | 1 to 28 |
| `reset_timezone` | `UTC` | `UTC` or a fixed offset such as `+05:30` |
| `anchor_max_age_hours` | `24` | |

The Accounts view shows the same readings: each installed harness's default login is listed with its quota, Bob's and Kiro's from these readers (`account.quota_changed` as for the others).

## Bob and Kiro accounts

Kiro keeps one login (`kiro-cli login`) and Bob always keeps its login in `~/.bob`, so both take extra accounts as API keys instead of config directories (manifest `account.key_env`, `account.team_env`):

- `POST /v1/accounts` with `api_key` (and `team_id` for a key of type "general") stores the key in the macOS Keychain under `quark/accounts/<account id>` (`quarkd::secrets`, through `security -i` on stdin so the key is never on a command line). The database keeps only that Keychain name; the API never returns the key; removing the account removes the Keychain item.
- A worker starts under it with `QUARK_SECRET_<KEY_ENV>=keychain:<service>` (e.g. `QUARK_SECRET_BOB_API_KEY`) and `BOB_TEAM_ID=<team>` in its environment, which is what dispatch and the event log record. The supervisor reads the key only when it starts the session (`quark_supervisor::secrets`), and the manifest passes the team as `--team-id`.
- Its quota is the live Bob API read with its own key; the ledger and anchor belong to the default login.

Key accounts join pools like any other, so a profile naming a pool balances Bob tasks across keys.

### A default login's kept key

A default account of a key harness can keep a key too: `PUT /v1/accounts/default-kiro/key` with `{"api_key": "…"}` (the app's **Use API key** on the Default row), `DELETE` to forget it. The key goes to the Keychain under `quark/accounts/default-kiro`, the default account then reads `api_key: true` and logged in, its launches carry the `QUARK_SECRET_…` reference, and its quota (and dispatch's Kiro reading) signs in with it. This is how quarkd signs a harness in when launchd starts it (QuarkBar's LaunchAgent runs a login shell, which never reads `~/.zshrc`, so a `KIRO_API_KEY` exported there is not seen). Without a kept key the default login is whatever quarkd's own environment signs in with.

A quota read that does not finish (a reader that timed out or failed to run) keeps the account's last reading for up to 6 hours, with a note saying the latest read failed; a read that answers, such as a lapsed login, always replaces it. Accounts are read side by side, so one slow reader holds up neither the others nor `GET /v1/accounts?refresh=true`.

firstmate's opt-in per-session cap (`spawn_max_cost`) has no setting here: put `--max-cost <coins>` on `launch.argv` in a `<home>/harnesses/bob.toml` copy of the built-in manifest instead.

## Admission control

A host takes a new worker only when all of these hold:

| Check | Default | Where it is set |
|---|---|---|
| health is `healthy` (degraded and unreachable hosts take nothing new) | | host registry |
| the host runs the Project (an empty list runs every Project) | | `Host.projects` |
| fewer workers than `Capacity.max_workers` (0 is no cap) | no cap | `QUARK_MAX_WORKERS` for this host |
| CPU at most `max_cpu` | 90% | `Limits.host` / `Limits.hosts` |
| memory pressure at most `max_memory_pressure` | 75% | same |
| free disk at least `min_disk_free_bytes` | 5 GiB | same |
| the Project's workers per host and in total under its caps | no caps | `Limits.project` / `Limits.projects` |

Resource checks use the host's latest `telemetry.sample` (from `quark-hosts`). A sample older than `max_sample_age` (2 minutes) is not trusted: the host is judged by worker count alone and the placement says so, so a stopped sampler never stops work.
Among hosts that pass, the least loaded (workers over cap) wins, then one with a fresh sample, then the least CPU in use.
Running workers are counted from `task.transition` events (`Started` until `Completed`, `Failed` or `Cancelled`), plus placements the host has not started yet.

Two engine-wide actions come from the same pass, recorded under the engine project:

- **Disk:** a host whose fresh sample is under its free-disk threshold has its idle pool worktrees pruned once per low-disk episode (`dispatch.pruned`, through an `IdlePool`).
- **Health:** a host unhealthy for `unhealthy_after` (10 minutes) raises one decision (`dispatch.host_unhealthy`), closed by `dispatch.host_recovered`.

## Hosts

`quark_hosts::EventHosts` is the `HostRegistry`: registrations and health changes are `host.change` events, so the host list survives restarts.

## Durability

The dispatcher holds no state the log does not have. A new dispatcher on the same log resolves requests a crash left unresolved and, for a placement never confirmed, asks the host whether it has the task before spawning it again, so a task is never started twice.
The supervisor records `Queued` itself when it takes a task, so a task waiting in dispatch shows in the `dispatch.*` events, not yet in the task ledger.

## Parity with firstmate

The native resolver was compared with firstmate's on every dispatch while slice 5 ran in shadow; quarkd no longer runs that comparison.
`crates/quark-dispatch/tests/parity.rs` still holds the resolver to firstmate's decisions on recorded cases; `tests/parity/record/record.py <firstmate checkout>` re-records them by running `fm-dispatch-resolve.sh` with stub `curl` and `quota-axi`.

When a host runs low on disk, dispatch prunes its idle pool worktrees once per episode (`IdlePool`; quarkd's prunes every Project repo's pool with the native pool's `prune`, `crates/quarkd/src/native_worktrees.rs`) and records the outcome as `dispatch.pruned`.

quarkd registers this host and records a telemetry sample every minute, and the native engine serves `resolve_dispatch`, `resolve_description` and `set_crew_dispatch` from the dispatcher (`crates/quarkd/src/engine/native_dispatch_engine.rs`), with accounts from quarkd's accounts store.

## Not yet

- Spawners for SSH and hosted runtimes (slice 7).
- Recording the task as `Queued` in the ledger while it waits in dispatch needs a small `quark-core` amendment so the supervisor accepts a queued task.
- Anchoring Bob from a throwaway `/status` session, as firstmate's `fm-bob-quota.sh anchor --from-status` did; type the figure in with `--anchor` for now.

# Dispatch rules

Dispatch rules (ADR-11, `crates/quarkd/src/crew_dispatch.rs`) decide which harness, model and account run each task.
They live in the Project repo's `dispatch.yaml`, and the app's Settings › Dispatch edits them. The [Dispatch API](../api/dispatch.md) reads, saves and tests them.

## Default rules

Every Project whose `dispatch.yaml` lists no `rules` or `default` of its own follows the default rules, kept in `~/.quark/dispatch.yaml` and edited in the app under Default rules.
A new Project starts that way. Saving rules for a Project writes them to its `dispatch.yaml`, where they apply instead; Reset to defaults drops them again (in a commit, so they stay in the history).
Its `classifier` block is the Project's own either way.

Until default rules are saved, Quark ships recommended tiers: Trivial, Standard, Complex and Scout, and a default for any other task.
The tiers' names and conditions are fixed; their candidates are picked from the harnesses installed here that have an enabled, signed-in account (Bob and Kiro, then Claude Code and Codex, for cheap work; Kiro, Claude Code and Codex for complex work and scouting), with Claude Code standing in when none of those is available.

In the rule editor, Copy from… fills the editor with another Project's rules or the default rules, to save as this Project's own (or as the defaults).

## dispatch.yaml

- `rules`: each a `when` with one profile or an ordered list of candidates in `use`.
- `default` and `default_select`: what runs when no rule matches.
- `classifier`: the optional classifier, below.

Each refresh compiles a changed `dispatch.yaml` on `main` into the workspace's `config/crew-dispatch.json`, mapping Quark harness ids (`claude-code`) to the engine's (`claude`).
A file that does not compile, or that the engine refuses, is recorded as a failed `dispatch` adapter call and the last good config stays.

## Classifier

The optional classifier (`crates/quarkd/src/classifier.rs`) is a System-1-compatible provider you choose; Quark ships none.
It can be the hosted System One API (`system1`, e.g. Jev) or an [Ollama](https://docs.ollama.com/api/systemone) server running a decision model such as `d1` (`ollama`).
Its block takes:

| Field | Default | Meaning |
| :--- | :--- | :--- |
| `provider` | `none` | `system1`, `ollama` or `none` |
| `model` | | the classifier's model |
| `credential` | | a keychain reference, `keychain:<service>`, never a key; `ollama` needs none for local models |
| `url` | `https://api.typesafe.ai` (`system1`), `http://localhost:11434` (`ollama`) | the server; `/v1/systemone` is added |
| `confidence_floor` | `0.6` | below it the classifier's pick is not used |
| `timeout_ms` | `5000` (`system1`), `15000` (`ollama`) | an Ollama model can take seconds to load after it idles |
| `on_failure` | `coordinator` | `coordinator` or `default` |

Any other field is refused, and so is an inline key. Store a key with `security add-generic-password -U -s <service> -a "$USER" -w`.

For a model in a local Ollama:

```yaml
classifier:
  provider: ollama
  model: d1
  url: http://mac.lan:11434   # only when the server is on another machine
```

A user-level default lives under `classifier:` in `~/.quark/config.yaml`; a Project's `dispatch.yaml` overrides it field by field, and with neither the provider is `none`, so the coordinator picks every rule.
The block in effect is compiled into `crew-dispatch.json`, and a change to `config.yaml` is applied on the next refresh.

The engine reads the key from the keychain. Below the floor, on a timeout or on a failure, the task goes to the coordinator or, with `on_failure: default`, to the default rule, which the dispatch record shows as `decided_by: default_rule`.

[Dispatch, pools and admission](../engine/dispatch.md) covers how the engine applies the rules, account pools and host admission.

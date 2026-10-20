# Dispatch rules

Dispatch rules (ADR-11, `crates/quarkd/src/crew_dispatch.rs`) decide which harness, model and account run each task.
They live in the Project repo's `dispatch.yaml`, and the app's Settings › Dispatch edits them. The [Dispatch API](../api/dispatch.md) reads, saves and tests them.

## dispatch.yaml

- `rules`: each a `when` with one profile or an ordered list of candidates in `use`.
- `default` and `default_select`: what runs when no rule matches.
- `classifier`: the optional classifier, below.

Each refresh compiles a changed `dispatch.yaml` on `main` into the workspace's `config/crew-dispatch.json`, mapping Quark harness ids (`claude-code`) to the engine's (`claude`).
A file that does not compile, or that the engine refuses, is recorded as a failed `dispatch` adapter call and the last good config stays.

## Classifier

The optional classifier (`crates/quarkd/src/classifier.rs`) is a System-1-compatible provider you choose; Quark ships none.
Its block takes:

| Field | Default | Meaning |
| :--- | :--- | :--- |
| `provider` | `none` | `system1` or `none` |
| `model` | | the classifier's model |
| `credential` | | a keychain reference, `keychain:<service>`, never a key |
| `confidence_floor` | `0.6` | below it the classifier's pick is not used |
| `timeout_ms` | `5000` | |
| `on_failure` | `coordinator` | `coordinator` or `default` |

Any other field is refused, and so is an inline key. Store a key with `security add-generic-password -U -s <service> -a "$USER" -w`.

A user-level default lives under `classifier:` in `~/.quark/config.yaml`; a Project's `dispatch.yaml` overrides it field by field, and with neither the provider is `none`, so the coordinator picks every rule.
The block in effect is compiled into `crew-dispatch.json`, and a change to `config.yaml` is applied on the next refresh.

The engine reads the key from the keychain. Below the floor, on a timeout or on a failure, the task goes to the coordinator or, with `on_failure: default`, to the default rule, which the dispatch record shows as `decided_by: default_rule`.

[Dispatch, pools and admission](../engine/dispatch.md) covers how the engine applies the rules, account pools and host admission.

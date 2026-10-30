# Triggers, channels and the away policy (slice 7)

`crates/quark-triggers` is the native replacement for firstmate's captain inbox (`fm-inbox.sh`), process-event sources and when-then watches (`fm-procevent*`), upstream contribution follow-ups (`fm-contributions.sh`), and away and quiet modes (`/afk`, `/quiet`, `fm-supervise-daemon.sh`, `fm-afk-*`).
Everything it knows is an event in the log; `Engine` folds its read models back from the log, so recovery is replay.

| Piece | Events | Read model |
|---|---|---|
| Channels: one inbound API, the user's inbox and plug-ins | `channel.received`, `channel.acked` | `Inboxes`: pending messages per Project |
| Triggers: typed sources plus condition-to-action rules | `trigger.defined`, `trigger.removed`, `trigger.fired`, `trigger.outcome`, `trigger.cursor` | `RuleSet`: rules, fires, open claims |
| Away policy: posture and routing, per Project or global | `away.posture`, `away.policy`, `away.routed`, `away.digest_sent` | `AwayState`: posture, policy, digest and held items |

A posture or policy recorded on the engine project (`_engine`) is global; a Project's own setting wins.
Logs from while slice 7 ran in shadow beside firstmate may also hold `trigger.would_fire` and `away.shadowed` events; they are read back but change nothing.

## Channels

`Engine::receive(project, Inbound)` is the one inbound API.
A message carries its channel (`inbox`, or a plug-in's name such as `email`, `voice`, `mention`), sender, body and an id that is stable within the channel, so a retried or re-read message lands once.
It stays pending until `Engine::ack`.
Each arrival wakes the coordinator under the default policy.

Plug-in channels implement `Channel` (`poll` returns what arrived); the inbox needs no plug-in.
The one quarkd loads is a program, `CommandChannel`: one manifest per channel at `<home>/channels/<name>.json`,

```json
{ "project": "<project id>", "argv": ["/usr/local/bin/quark-mail"], "interval_secs": 60, "timeout_secs": 30 }
```

Each poll runs `argv` without a shell and reads one JSON message per output line, `{"id", "body", "from"?, "at"?, "task"?}`.
Exit 0 with no output is "nothing new"; any other exit is an error, logged, and retried on the next interval.
Ids must be stable, so the program may return the same message again; it is received once.
Each new message lands in the engine's own inbox as `channel.received`, which wakes the coordinator; its `ack_message` tool records `channel.acked`.

## Triggers

A `Rule` is a condition, an action and a few switches (`once`, `enabled`).

| Condition | Fires |
|---|---|
| `event`: a kind (exact or `prefix.*`), optional task, JSON-pointer field matches | once per matching event appended after the rule was defined |
| `every`: an interval in seconds | once per interval slot after the one it was defined in; missed slots are not replayed |
| `at`: a time | once, at or after it |
| `command`: argv polled on an interval, optional expected output | once, after `stable` true polls in a row; exit 1 is "not yet", anything else is an error, and `error_budget` errors in a row end the rule with a `condition_error` outcome; past an optional `until` it ends with `never_true` |

Actions are deterministic: `wake` the coordinator with a note, leave an `inbox` note, `steer` a task, or run a bounded `command`.
A `command` condition with a `command` action and an `until` is the native form of firstmate's when-then watch (`fm-procevent-when.sh`).
Commands run without a shell.
Anything that needs judgment stays with the coordinator, which a rule can wake.
Rules can't watch `trigger.*` events, so a rule never triggers itself.

`templates::contribution_followup(repo, issue, seen)` is the upstream follow-up as a rule: it polls the issue hourly with `gh` and wakes the coordinator once its state or comment count differs from what was seen.

**At most once.** A fire is claimed with `trigger.fired` before the action runs and closed with `trigger.outcome`.
Fire ids are derived from the rule's definition and the cause (event, slot), so evaluating the same thing twice claims nothing new.
If the engine stops between claim and outcome, the next start records an `ambiguous` outcome instead of running the action again, and the coordinator hears about it.

**Exactly one evaluation per event.** Each pass evaluates the events after the last `trigger.cursor` and records a new one.
A log the engine has never evaluated starts at its head, so history fires nothing.

## Away policy

The user is `present`, `away` (user-owned decisions wait for their return) or `quiet` (present, but only wants what matters).
Every occasion gets a `Route`: whether the coordinator is woken for judgment, and whether the user hears `silent`ly, in the next `digest`, right away (`notify`), or on return (`hold`).

| Occasion | Wakes the coordinator | Present | Away | Quiet |
|---|---|---|---|---|
| progress, paused, stale | no | silent | silent | silent |
| done | yes | notify | digest | digest |
| decision | yes | notify | hold | notify |
| blocked | yes | notify | hold | digest |
| failed | yes | notify | digest | notify |
| inbound message | yes | silent | silent | silent |
| trigger fired | no | digest | digest | digest |
| trigger failed or ambiguous | yes | notify | digest | notify |

The coordinator column is firstmate's away classifier: only done, decision, blocked and failed escalate, and a stale worker is the engine's to check first: the fleet check nudges it, then wakes the coordinator if it stays stuck (see [supervision](supervision.md#fleet-check)).
The engine does not wake the coordinator for an occasion itself: the coordinator (slice 6, `quark_coordinator::wake`) applies the same policy to the same event and wakes itself, so the engine only reaches the user.
A Project overrides any cell with `away.policy`; a policy where a decision or failure would reach no one is refused.
Digests go out every `digest_secs` (an hour by default) after the first waiting item; held items go out as the return brief when the user is present again.

## Effects

quarkd runs the engine on the projector's interval (`crates/quarkd/src/native_triggers.rs`) and acts through `DaemonEffects`:

| Effect | How |
|---|---|
| a rule's `wake` | `coordinator.requested` in the log; the coordinator wakes for it |
| telling the user now, a digest, the return brief | a `notice.posted` event (`quark_systems::Notice`); the app lists the latest under Updates beside the Conversation |
| a rule's `steer` | the engine adapter's `send_message` to the task's worker |

## API and dashboard

The Project dashboard's Automation tab (`app/src/screens/dashboard/Automation.tsx`) reads and edits all of it through quarkd (`crates/quarkd/src/api/automation.rs`):

| Endpoint | Does |
|---|---|
| `GET /v1/projects/{id}/automation` | inbox, rules with their fire counts, and the away policy with every cell |
| `POST /v1/projects/{id}/inbox` | leaves a note for the coordinator in the engine's inbox |
| `PUT`, `DELETE /v1/projects/{id}/triggers/{rule}` | defines, replaces or removes a rule |
| `PUT /v1/projects/{id}/away/policy` | replaces the Project's policy; cells equal to the default are dropped, and one that loses a decision or failure is refused |

Posture is not set here yet; the engine records it (`Engine::set_posture`) but no endpoint calls that.
With the engine off, these answer 503 `automation_off`.

## Not yet

- Email, voice and public-mention programs for `CommandChannel`, and channel health in the Automation tab (a failing plug-in is only logged).
- Commands run in the daemon's working directory, so a condition that uses relative paths fails.
- An endpoint, and app control, for setting the posture.

# Importing from firstmate

firstmate was the engine Quark drove before the native engine. `POST /v1/imports/firstmate` ([API](../api/imports.md)) seeds Quark from a firstmate home you still have, so its projects, rules, work and memory do not start over.

```sh
curl -s localhost:7380/v1/imports/firstmate -H 'content-type: application/json' \
  -d '{"home": "/Users/you/firstmate", "dry_run": true}' | jq
```

In the app, **Import** on All projects (or *Import projects* in the command palette) does the same: choose the folder, preview, assign work that has no project, then import. A project still being set up shows *Waiting*, and *Check again* finishes it.

Run it with `dry_run` first: the report says what will land where and lists the backlog items it could not route.

## Read only

The import opens files in the home and nothing else. It never runs firstmate's `bin/` scripts or the `state/*.check.sh` polls, never runs `bd` or a git command in a project clone (the origin URL is read from `.git/config` as text, and credentials in it are dropped), and never opens `.env`. A test hashes a home before and after an import.

## Where each category goes

| firstmate | Quark |
| :--- | :--- |
| Projects: `data/projects.md`, each clone's origin URL | One Project per firstmate project, with its repo and delivery (`direct-PR` is `direct`, `no-mistakes` and `local-only` are `gated`). Quark's dispatch, Beads and workspace are per Project, which is why a firstmate project is not folded into one. Coordinators start as `claude-code` at `high` effort; change it in Settings. A project without an origin URL is recorded without a repo. |
| Dispatch: `config/crew-dispatch.json` | Each Project's `dispatch.yaml`, committed to the Project repo on its own. Harness ids are mapped (`claude` to `claude-code`), and `why`, `floor`, `provider`, `pricing`, `select` and `default` are kept. The `classifier` block is not: choose it again and keep its key in the keychain. A `dispatch.yaml` you have edited is kept. `crew-dispatch-*.json` and `.bak-*` files are alternatives and history, not imported. |
| Accounts | firstmate keeps none; harness logins live in each CLI's config directory. Add them under Settings > Accounts. |
| Quota: `config/kiro-quota.json` | `<quark home>/quota/kiro.json`, same field. |
| Quota: `state/bob-quota-anchor.json` | Its `budgetCoins` becomes `quota/bob.json`'s `monthly_coins` when it is not Quark's 1500. The usage anchor is not carried over, since it is bound to a charge month and expires in a day; Bob reads as `partial` until `quarkd quota bob --anchor <coins>`. |
| Tasks: `data/backlog.md` (and `data/done-archive.md` with `include_archive`) | Beads in the item's Project, labelled `firstmate` and `firstmate:<id>`. Done items are closed with their PR or report as the reason, `blocked-by` becomes a dependency, `priority` is kept, `kind` becomes a `kind-<kind>` label. An item's Project is its `(repo: ...)`, then a `routes` entry, then the project in its `state/<id>.meta`, then the project its id starts with, then the one project sharing its id's first word; anything else is `unrouted` and not imported. |
| Holds and decisions: held and `kind: captain` items | `decision` beads, closed with the captain's recorded answer when there is one. They are not Quark decisions (they were never asked in Quark), so they carry no `quark:` ref or gate and closing the bead answers one. |
| Status logs: `state/<id>.status`, `.meta` | A comment on the item's bead with the last lines, and harness, model and effort in its description. No events are written to Quark's event log. An item that was in flight becomes an open bead labelled `was-in-flight`, never a running worker: its pane and worktree cannot be adopted, so dispatch it again. |
| Reports and briefs: `data/<id>/report.md`, `brief.md` | Comments on the item's bead. |
| Memory: `data/captain.md`, `data/learnings.md` | User-level memory (`~/.quark/memory`): one entry per captain bullet and per learnings section. |
| Project clones' own `.beads` | Not read. Quark adopts the database a Project's first repo has when it sets up the Project's Beads. |

## Not imported

`config/backend` and other session tuning (`startup-memory-budget`), `data/secondmates.md`, everything in `state/` that supervises running workers (watcher, lease, inbox, busy-state, turn-end files), `state/*.check.sh`, status logs with no backlog item, scratch directories in `data/`, and `.env`. The report counts each. For `.env`, store a classifier key with `security add-generic-password` and put Bob's `BOBSHELL_API_KEY` in quarkd's environment.

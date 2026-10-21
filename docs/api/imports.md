# Imports

`POST /v1/imports/firstmate` seeds Quark from an existing firstmate home (`crates/quarkd/src/api/firstmate_import.rs`, `crates/quarkd/src/firstmate/`). What each kind of firstmate data becomes is in [Importing from firstmate](../configuration/firstmate-import.md).

| Field | Meaning |
| :--- | :--- |
| `home` | absolute path of the firstmate home (the directory with `data/`, `config/`, `state/`, `projects/`) |
| `dry_run` | report what would happen and change nothing |
| `projects` | only these firstmate projects, by name; all when empty |
| `routes` | id prefix to project name for backlog items nothing else routes, such as `{"magenta-": "magenta-mono"}`; the longest prefix wins |
| `include_archive` | also import `data/done-archive.md`'s done items (often thousands) |
| `wait_secs` | how long to wait for each new Project to provision and get its Beads database (default 120) |

The response lists every category firstmate stores locally with what became of it (`imported`, `present`, `pending` or `not_imported`), each Project, the backlog items that could not be routed (`unrouted`) and notes.
The call answers when it is done rather than reporting progress as events: a Project still provisioning after `wait_secs` is `pending`, and the same request again finishes it.
It is safe to repeat: Projects are found by name, beads by their `firstmate:<id>` label, memory by an `entry` key, and a `dispatch.yaml` that was edited since the Project was created is kept.

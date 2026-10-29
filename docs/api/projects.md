# Projects

`POST /v1/projects` with `repos` and an `agent_config` provisions a Project in the background (`crates/quarkd/src/provision.rs`).
It clones each repo into the command-center workspace (`~/.quark/workspaces/command`), seeds the Project workspace at `~/.quark/workspaces/<project-id>`, writes the Project repo (`~/.quark/projects/<project-id>.git`, checked out at `<workspace>/project`) and starts the coordinator.

`project.updated` events report each step and the final `ready` or `failed` status; `POST /v1/projects/{id}:provision` retries a failed Project.

The native engine ([`docs/engine/native-engine.md`](../engine/native-engine.md)) runs everything in quarkd; it needs no engine checkout.

[`selfhost/project-init.sh`](../../selfhost/README.md#any-other-project) creates a Project from a directory of files through this API.

## Attachments

`POST /v1/projects/{id}/attachments?name=<file name>` saves the raw request body (up to 25 MB) as `~/.quark/attachments/<project-id>/<random>/<name>` and answers `201` with `{ path, name, bytes }`.
The app uses it for images pasted and files picked or dropped into a prompt box (coordinator chat, the dock, a worker's message box, the New issue chat): each file is saved when added, and the message sent ends with an `Attached files:` list of their paths, which the agent reads from disk.
The name is reduced to its last path part with unusual characters replaced. Paths are on the daemon's machine, so a worker on another host cannot read them yet.

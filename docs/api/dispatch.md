# Dispatch

These routes read, save and test a Project's [dispatch rules](../configuration/dispatch-rules.md).

- `GET /v1/projects/{id}/dispatch` answers the rules in effect: `source: project` with the rules as written, or `source: defaults` with the default rules while the Project's `dispatch.yaml` lists none of its own.
- `PUT /v1/projects/{id}/dispatch` saves them: it writes `dispatch.yaml`, compiles it and refuses it with the compile error, then commits it to the Project repo's `main` in its own commit, keeping the `classifier` block and the comment lines that open the file.
- `POST /v1/projects/{id}/dispatch:test` (`crates/quarkd/src/dispatch_test.rs`) resolves a task description with the engine's dispatch rules and returns the matched rule or that the coordinator would pick, and every candidate with its pass or fail reason; it dispatches nothing.
  A `draft` is compiled and tested without being written; the engine resolves only saved rules, so a draft that differs from them lists its candidates without a resolution.
- `POST /v1/projects/{id}/dispatch:reset` drops the Project's own rules in a commit of their own (keeping its `classifier` block), so it follows the default rules again.
- `GET`, `PUT` and `DELETE /v1/dispatch/defaults` (`crates/quarkd/src/dispatch_defaults.rs`) read, save and forget the default rules in `~/.quark/dispatch.yaml`. `shipped` is true while nothing is saved and the recommended tiers apply; `projects` lists the Projects that follow the defaults.

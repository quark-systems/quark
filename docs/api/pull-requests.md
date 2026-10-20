# Pull requests

The PR center (`crates/quarkd/src/pr_center.rs`) lists every pull request a task reports at `GET /v1/pull-requests`, with checks and reviews read from GitHub through the `gh` CLI every 30 seconds (`--pr-refresh-secs`).

## Evidence

Each pull request carries its [verification gate](../configuration/verification-gates.md) results as `evidence`, read from the gate runner's `state/<task>.gates.json` manifest (`quark.gates.v1`).
Traces, screenshots and logs are served at `/v1/pull-requests/{id}/evidence/artifacts/{artifact_id}`.

## Comments and merges

- `POST /v1/pull-requests/{id}/comments` sends a review comment to the owning worker as a steering message.
- `POST /v1/pull-requests/{id}:merge` runs the engine's guarded merge: rebase onto the base branch, re-verify, check main's health, then merge exactly the verified head ([Verification and merge](../engine/verify.md)).
- A Project's `standing_approval` merges its green pull requests without asking.

## Events

`pr.updated`, `check.updated` and `review.updated` stream changes.

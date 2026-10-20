# Decisions

`GET /v1/decisions` lists every Project's holds and workers' open keyed decisions.
`POST /v1/decisions/{id}:answer` with `answer` and `answered_by` (the daemon's own user when absent) answers one through the engine and stores who answered.

## Events

- `decision.opened` and `decision.answered` report each change.
- A question answered outside Quark arrives answered with no `answered_by`, and one asked again opens a new decision.

A merge refused because main is red files a hold `red-main:<decision>`; answering it `merge anyway` overrides the guardrail until main is green again (see [Verification and merge](../engine/verify.md)).

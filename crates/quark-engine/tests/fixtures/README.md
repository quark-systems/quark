Captured from the firstmate engine (quark-systems/firstmate at f10db1f) using the
fixture home in its `tests/fm-bearings-snapshot.test.sh`, plus two captain holds
(one live, one deferred with `hold-until`). Temporary paths were rewritten to
`/fixture`.

- `fleet-snapshot.v1.json`: `fm-fleet-snapshot.sh --json`
- `home-summary.v1.json`: `fm-fleet-snapshot.sh --secondmate-home-summary`
  (the document `fm-home-summary-refresh.sh` publishes as `state/home-summary.json`)

# Releasing Quark

Status: a first pass (the release pipeline spike). The plan it follows is in the project's
release plan; what is not here yet is listed at the end.

## Cut a release

1. `scripts/bump-version.sh 0.1.0-beta.1` sets the version in every file that carries it
   (`crates/quarkd/tests/version.rs` fails if they drift). Add a `## 0.1.0-beta.1` section
   to `CHANGELOG.md`: it becomes the release notes, and the workflow fails without it.
   Commit both to `main`.
2. Tag the commit `v0.1.0-beta.1` and push the tag.
3. `.github/workflows/release.yml` builds these and publishes them as a GitHub Release:
   - `Quark_<version>_aarch64.dmg`: Quark.app for Apple silicon. It carries QuarkBar.app
     (the menubar app and widgets, from `mac/scripts/package.sh`) in
     `Contents/Library/LoginItems`, and QuarkBar carries quarkd. Opening Quark opens
     QuarkBar, which on its first run registers quarkd as a LaunchAgent and itself as a
     login item, so opening Quark once starts all three and they come back after a
     restart (quarkd's log: `~/Library/Logs/Quark/quarkd.log`).
   - `Quark_<version>_x64-setup.exe`: the Windows app (quarkd runs in WSL), only when the
     repository variable `RELEASE_WINDOWS` is `true`. 0.1 is Mac only, so it is off.
   - `quarkd-x86_64-unknown-linux-gnu.tar.gz` and `install-wsl.sh`: quarkd for WSL and Linux.
   - `latest.json` (the updater manifest), `compat.json` and `SHA256SUMS`.
4. Before publishing, the workflow checks the assets: the dmg and Linux quarkd are there
   and, with the updater key set, the signed `Quark.app.tar.gz` and a `latest.json` that
   lists it for darwin-aarch64. Anything missing fails the run and nothing is published.
   Then it rebuilds the update feed. A version with a `-` suffix is marked a pre-release,
   which only the beta channel offers. To withdraw a bad release, add it to `pulled` in
   `packaging/update-policy.json` (see Updates).

Pull requests that change the workflow or `packaging/` run the same builds as a dry run.

## Windows

The app installs natively. In WSL (Ubuntu 24.04+, systemd on), run `install-wsl.sh` from
the release; it installs quarkd, tmux and git, and runs quarkd as a systemd
user service on `127.0.0.1:7380`, which the app reaches through WSL's localhost
forwarding. Running it again upgrades quarkd.

## Updates

The updater key (`TAURI_SIGNING_PRIVATE_KEY` and its password as secrets, the public key
as the variable `TAURI_UPDATER_PUBKEY`) makes the Mac build sign `Quark.app.tar.gz` and
list it in `latest.json`. The public key is built into each release, so a build made
without it can't update itself.

Publishing a release runs `.github/workflows/update-feed.yml`, which rebuilds the channel
manifests on the `update-feed` release (a pre-release, so it never becomes "latest"):

- `stable.json`: the newest published full release.
- `beta.json`: the newest published release, pre-releases included.

Each is that release's `latest.json` plus `compat` (the release's `compat.json`, from
`quarkd compat --json`: API contract, home format, database schemas) and the policy in
`packaging/update-policy.json`:

- `min_from`: the oldest version that may update straight to this one. Raise it when an
  upgrade needs an intermediate release.
- `pulled`: versions withdrawn after release. They are never served, and an updater
  running one is offered the newest release that isn't pulled.

To pull a release or change `min_from`, edit the policy on `main` and run the Update
feed workflow by hand. Quark 0.1.0's desktop app still checks
`releases/latest/download/latest.json` itself, so keep attaching `latest.json` to each
release.

quarkd reads the feed (`crates/quarkd/src/update.rs`): 60 seconds after it starts, then
every six hours, and on `POST /v1/update/check` (Check for Updates…). It offers a
release newer than itself, or any release when it is running a pulled one, and explains
instead when `min_from` is above it. It downloads the platform's bundle into
`~/.quark/updates/<version>/` and verifies it against the built-in public key (the
release build passes `TAURI_UPDATER_PUBKEY` to `cargo build`); `GET /v1/update` reports
the result. Settings (channel, checking, automatic download, install when idle) are in
`~/.quark/update.json`, set with `PUT /v1/update/settings`. To try a feed of your own,
point `QUARK_UPDATE_FEED` at it (`{channel}` is replaced, `file://` works).

`POST /v1/update/install` installs a ready download: quarkd backs up the home, copies
itself to `~/.quark/updates/quark-updater` and starts it in its own process group with
the plan in `~/.quark/updates/intent.json` (`crates/quarkd/src/update_install.rs`). The
updater unpacks the bundle beside Quark.app, quits Quark and QuarkBar, swaps the app
(keeping the old one as `.Quark-previous.app` beside it), restarts quarkd with
`launchctl kickstart -k` and waits up to two minutes for `/v1/health` to report the new
version. If it doesn't, the old app goes back, the backup is restored at the next quarkd
start when the old build can't read the new data, and the old quarkd is restarted. Either
way QuarkBar (and Quark, if it was open) reopen. `POST /v1/update/rollback` does the same
back to the kept app; `last_install` and `can_roll_back` on `GET /v1/update` report both.
Its log is `~/.quark/logs/updater.log`. Workers keep running throughout: their ptyd
outlives quarkd. Quark 0.1.0 still updates itself through Tauri's updater; later builds
leave that to quarkd.

## Compatibility

The desktop app, QuarkBar and quarkd ship together, but can still run different builds:
during an update, after a rollback, or with a quarkd built from source. Each interface
says what it speaks:

- **`/v1` API.** `GET /v1/health` reports `api_version` (`API_CONTRACT`, the newest
  contract served), `api_min` (`API_CONTRACT_MIN`, the oldest still served) and
  `features` (`FEATURES`), all in `crates/quark-systems/src/service.rs`. A client built
  for contract `c` works when `api_min <= c <= api_version`: the app checks with
  `apiMismatch` (`app/src/api/client.ts`), QuarkBar with `Health.apiMismatch`
  (`mac/Sources/QuarkKit/Models.swift`), and each says which side to update. Add fields
  and endpoints freely. A change an older client can't handle bumps `API_CONTRACT` (and
  `API_VERSION` / `Health.apiContract` in the clients); quarkd keeps serving the old shape
  for one minor release, then `API_CONTRACT_MIN` moves up. CI's `packaging/api-compat.sh`
  compares `api/openapi.json` with the newest release tag's and fails on a breaking change
  without a bump.
- **Features.** A client that needs an optional capability checks `features`, not the
  release version. Add a name when one is needed; never remove one that is still served.
- **PTY supervisor.** `quark-ptyd` outlives quarkd, so a new quarkd can meet an old one.
  Its `hello` request reports `PROTOCOL` (`crates/quark-sessions/src/pty/proto.rs`) and its
  version; one from before `hello` (0.1.0) speaks protocol 1.
- **Data.** The home (`format.json`), the store (`quark.db`) and the event log
  (`events.db`) each record a version and a `min_reader`: the oldest version a quarkd
  must know to use them. A change that only adds what older code ignores (a file, a
  table, a column with a default) leaves `min_reader` alone, so after a rollback the
  older quarkd keeps using the newer data as it is. Anything that moves, renames, drops
  or reinterprets data raises it: mark a store migration `additive` in
  `crates/quarkd/src/store/migrations.rs` only when it truly is, and raise
  `home_format::MIN_READER` or `quark_eventlog::MIN_READER` with a breaking change.
  `quarkd compat --json` prints all of them, and the update feed carries it.
- **Backups.** `quarkd backup [--label <version>]` copies the databases and
  `format.json` to `<home>/backups/pre-<label>/` and keeps the newest two; the updater
  takes one before installing. To roll back past a breaking change, stop quarkd and copy
  the files back.
- **Upgrade test.** CI's `packaging/upgrade-test.sh` starts the previous release's Linux
  quarkd on a fresh home, upgrades it with this build, then starts the previous quarkd on
  it again: it must serve the home or refuse it by name, never fail half-way.

## Signing (repository secrets)

| What | Secrets / variables |
|---|---|
| Mac code signing | `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` |
| Mac notarization | `APPLE_API_ISSUER`, `APPLE_API_KEY_ID`, `APPLE_API_KEY_P8` |
| Updates | secrets `TAURI_SIGNING_PRIVATE_KEY` (+ `_PASSWORD`), variable `TAURI_UPDATER_PUBKEY` (`npx tauri signer generate`) |

Without them the builds are unsigned: macOS quarantines the app (right-click, Open, or
`xattr -dr com.apple.quarantine /Applications/Quark.app`), Windows SmartScreen warns, and
the app has no updater.

## Not yet

- WSL setup from the Windows app.
- Windows code signing; Homebrew cask and winget.
- quarkd's updater, which reads the channel manifests (the desktop app's own check reads
  only the latest full release).

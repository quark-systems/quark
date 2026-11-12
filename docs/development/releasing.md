# Releasing Quark

Status: a first pass (the release pipeline spike). The plan it follows is in the project's
release plan; what is not here yet is listed at the end.

## Cut a release

1. `scripts/bump-version.sh 0.1.0-beta.1` sets the version in every file that carries it
   (`crates/quarkd/tests/version.rs` fails if they drift). Add a `## 0.1.0-beta.1` section
   to `CHANGELOG.md`: it becomes the release notes, and the workflow fails without it.
   Commit both to `main`.
2. Tag the commit `v0.1.0-beta.1` and push the tag.
3. `.github/workflows/release.yml` builds and attaches to a **draft** GitHub Release:
   - `Quark_<version>_aarch64.dmg`: Quark.app for Apple silicon. It carries QuarkBar.app
     (the menubar app and widgets, from `mac/scripts/package.sh`) in
     `Contents/Library/LoginItems`, and QuarkBar carries quarkd. Opening Quark opens
     QuarkBar, which on its first run registers quarkd as a LaunchAgent and itself as a
     login item, so opening Quark once starts all three and they come back after a
     restart (quarkd's log: `~/Library/Logs/Quark/quarkd.log`).
   - `Quark_<version>_x64-setup.exe`: the Windows app (quarkd runs in WSL), only when the
     repository variable `RELEASE_WINDOWS` is `true`. 0.1 is Mac only, so it is off.
   - `quarkd-x86_64-unknown-linux-gnu.tar.gz` and `install-wsl.sh`: quarkd for WSL and Linux.
   - `latest.json` (the updater manifest) and `SHA256SUMS`.
4. Publish the draft. A version with a `-` suffix is marked a pre-release, which the
   updater skips (it reads the latest full release).

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
- **Data.** `quarkd compat --json` prints the home format and database schemas a build
  writes; the update feed carries it.

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

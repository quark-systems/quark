# Releasing Quark

Status: a first pass (the release pipeline spike). The plan it follows is in the project's
release plan; what is not here yet is listed at the end.

## Cut a release

1. `scripts/bump-version.sh 0.1.0-beta.1` sets the version in every file that carries it
   (`crates/quarkd/tests/version.rs` fails if they drift). Commit it to `main`.
2. Tag the commit `v0.1.0-beta.1` and push the tag.
3. `.github/workflows/release.yml` builds and attaches to a **draft** GitHub Release:
   - `Quark_<version>_aarch64.dmg`: Quark.app for Apple silicon, with quarkd inside.
     The app starts it (`--engine native`, log in `~/Library/Logs/systems.quark.desktop/`)
     when no daemon is running.
   - `Quark_<version>_x64-setup.exe`: the Windows app. quarkd runs in WSL.
   - `quarkd-x86_64-unknown-linux-gnu.tar.gz` and `install-wsl.sh`: quarkd for WSL and Linux.
   - `latest.json` (the updater manifest) and `SHA256SUMS`.
4. Publish the draft. A version with a `-` suffix is marked a pre-release, which the
   updater skips (it reads the latest full release).

Pull requests that change the workflow or `packaging/` run the same builds as a dry run.

## Windows

The app installs natively. In WSL (Ubuntu 24.04+, systemd on), run `install-wsl.sh` from
the release; it installs quarkd, tmux, git and treehouse, and runs quarkd as a systemd
user service on `127.0.0.1:7380`, which the app reaches through WSL's localhost
forwarding. Running it again upgrades quarkd.

## Compatibility

`GET /v1/health` reports `api_version` (`quark_systems::API_CONTRACT`). The app compares
it with `API_VERSION` in `app/src/api/client.ts` and says which side to update when they
differ. Bump both on a change an older client can't handle.

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

- QuarkBar inside Quark.app (it still ships as its own `mac/scripts/package.sh` bundle).
- First-run setup screen and WSL setup from the Windows app.
- Windows code signing; Homebrew cask and winget.
- A beta update channel.
- A format version for `~/.quark` and an upgrade test.

# QuarkBar

Quark in the macOS menubar: what needs you, every project and its agents,
hosts and quota, with notifications. It also runs quarkd for you.

QuarkBar is a **client of quarkd only**. Everything it shows and does goes
through quarkd's HTTP API (`api/openapi.json`): `/v1/status`, `/v1/accounts`,
`/v1/hosts`, `/v1/harnesses` and the `/v1/events` stream. When it needs
something quarkd doesn't answer yet, add it to quarkd, not here.

## What it does

- **Icon:** shows a violet count while something needs you, and dims when
  quarkd isn't answering.
- **Menu:**
  - **Needs you:** open decisions, red PRs and stuck workers, oldest first.
  - **Projects:** each one with its workers by state.
  - **Hosts** and **Quota**.
  - **quarkd:** start, stop, restart and the log.
  - **Open QuarkBar at Login.**

  Clicking an item opens that place in the Quark desktop app (`quark://open/<route>`).
  A running desktop app gets the link on its socket,
  `~/Library/Application Support/Quark/desktop.sock`, so this works with
  `npm run tauri dev` too, where macOS never delivers `quark://`. When no desktop app
  runs, the link goes to the installed Quark.app; with neither, QuarkBar beeps.
- **Widgets** (desktop and Notification Center): Needs you, Projects, Quota
  and Hosts. Each reads quarkd itself every few minutes, and QuarkBar reloads
  them as soon as what they show changes. Clicking one opens the desktop app at
  that item, through QuarkBar (`quarkbar://open/<route>`).
- **Notifications:** quarkd's `notice.posted` updates. The project's away
  policy decides what's worth telling you.
- **quarkd:** the quarkd bundled in QuarkBar.app runs as the LaunchAgent
  `systems.quark.quarkd`, through SMAppService. launchd keeps it running
  whether or not QuarkBar is open. On first launch QuarkBar starts it, unless
  another quarkd is already answering on port 7380. quarkd runs from a login
  shell, so it finds tmux, gh and the harness CLIs. Its output goes to
  `~/Library/Logs/Quark/quarkd.log`.
- **Address:** QuarkBar talks to `QUARK_DAEMON` when that is set, as the
  desktop app does, and otherwise to `http://127.0.0.1:7380`.

## Build

```sh
cd mac
swift build && swift test         # QuarkKit's models and client
swift run QuarkMenuBar            # menu only: no notifications or quarkd management
scripts/package.sh                # dist/QuarkBar.app with quarkd inside
open dist/QuarkBar.app
```

To change quarkd's arguments, set `QUARKD_ARGS` in your login shell
environment. The default is `--engine native`.

## Layout

- `Sources/QuarkKit`: API models, client, event stream, links, formatting. No
  AppKit; tested in `Tests/QuarkKitTests` against JSON fixtures shaped like
  quarkd's answers.
- `Sources/QuarkUI`: how state words look (tone colors, summaries, meters),
  shared by the menubar and the widgets.
- `Sources/QuarkWidgets`: the WidgetKit extension. `scripts/package.sh` wraps
  it as `Contents/PlugIns/QuarkWidgets.appex`, sandboxed with network access to
  reach quarkd. Widgets show up in the gallery after QuarkBar.app has run once.
- `Sources/QuarkMenuBar`: the app. AppKit status item and menu, with SwiftUI
  rows for meters.
- `Resources`: the Info.plists, the widgets' entitlements, the LaunchAgent plist
  and quarkd's launch script.

#!/usr/bin/env bash
# Builds dist/QuarkBar.app: the menubar app with its widgets and quarkd inside,
# quarkd run as a LaunchAgent. Run on a Mac from anywhere in the repo:
#   mac/scripts/package.sh            # release build, ad-hoc signed
#   SIGN_ID="Developer ID Application: …" mac/scripts/package.sh
# Needs Xcode or the Command Line Tools (Swift 5.9+) and a Rust toolchain.
set -euo pipefail

mac="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$mac/.." && pwd)"
# quarkd takes its version from the workspace.
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
build="$(git -C "$repo" rev-list --count HEAD 2>/dev/null || echo 1)"
app="$mac/dist/QuarkBar.app"

echo "building quarkd $version"
cargo build --release --manifest-path "$repo/Cargo.toml" -p quarkd
echo "building QuarkBar"
# With only the Command Line Tools, SwiftPM's default build system fails to
# start ("Unknown error parsing property list"); the native one works.
swift_flags=(-c release --package-path "$mac")
if [[ "$(xcode-select -p 2>/dev/null)" == */CommandLineTools ]]; then
  swift_flags+=(--build-system native)
fi
swift build "${swift_flags[@]}" --product QuarkMenuBar
swift build "${swift_flags[@]}" --product QuarkWidgets
bin="$(swift build "${swift_flags[@]}" --show-bin-path)"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Library/LaunchAgents"
widgets="$app/Contents/PlugIns/QuarkWidgets.appex"
mkdir -p "$widgets/Contents/MacOS"
cp -f "$bin/QuarkWidgets" "$widgets/Contents/MacOS/QuarkWidgets"
sed -e "s/__VERSION__/$version/" -e "s/__BUILD__/$build/" "$mac/Resources/Widgets-Info.plist" > "$widgets/Contents/Info.plist"
cp -f "$bin/QuarkMenuBar" "$app/Contents/MacOS/QuarkBar"
cp -f "$repo/target/release/quarkd" "$app/Contents/MacOS/quarkd"
cp -f "$mac/Resources/quarkd-launch" "$app/Contents/Resources/quarkd-launch"
cp -f "$mac/Resources/systems.quark.quarkd.plist" "$app/Contents/Library/LaunchAgents/"
sed -e "s/__VERSION__/$version/" -e "s/__BUILD__/$build/" "$mac/Resources/Info.plist" > "$app/Contents/Info.plist"
if [ -f "$repo/app/src-tauri/icons/icon.icns" ]; then
  cp -f "$repo/app/src-tauri/icons/icon.icns" "$app/Contents/Resources/AppIcon.icns"
  /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string AppIcon" "$app/Contents/Info.plist"
fi

# Inside out: the helpers first, then the app.
sign="${SIGN_ID:--}"
codesign --force --options runtime --timestamp=none --sign "$sign" "$app/Contents/MacOS/quarkd"
codesign --force --options runtime --timestamp=none --sign "$sign" \
  --entitlements "$mac/Resources/Widgets.entitlements" "$widgets"
codesign --force --options runtime --timestamp=none --sign "$sign" "$app"
codesign --verify --deep --strict "$app"
echo "built $app"

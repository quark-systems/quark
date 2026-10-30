// Prints the Tauri config a release build adds on top of app/src-tauri/tauri.conf.json:
//   node packaging/release-config.mjs macos|windows > release.conf.json
//   npx tauri build --config release.conf.json
// Kept out of tauri.conf.json so `npm run tauri dev` needs no QuarkBar build and no
// updater key. Reads TAURI_UPDATER_PUBKEY (the updater's public key, a repository
// variable): without it the build has no updater, which is how unsigned dry runs go.
const target = process.argv[2];
if (target !== "macos" && target !== "windows") {
  console.error("usage: node packaging/release-config.mjs macos|windows");
  process.exit(1);
}

const config = { bundle: {} };
if (target === "macos") {
  // QuarkBar.app (mac/scripts/package.sh, quarkd inside it) ships in Quark.app as a login
  // item; the app opens it (src-tauri/src/release.rs). On Windows quarkd runs in WSL
  // instead (packaging/wsl/install.sh).
  config.bundle.macOS = {
    files: { "Library/LoginItems/QuarkBar.app": "../../mac/dist/QuarkBar.app" },
  };
  // Without a Developer ID, sign ad hoc so the bundle's seal covers QuarkBar inside it
  // (Apple silicon won't run an app whose signature doesn't match its contents).
  if (!process.env.APPLE_SIGNING_IDENTITY) config.bundle.macOS.signingIdentity = "-";
}
const pubkey = process.env.TAURI_UPDATER_PUBKEY?.trim();
if (pubkey) {
  config.bundle.createUpdaterArtifacts = true;
  config.plugins = {
    updater: {
      pubkey,
      // The newest published (non-draft, non-prerelease) release's manifest, which the
      // release workflow attaches.
      endpoints: ["https://github.com/quark-systems/quark/releases/latest/download/latest.json"],
    },
  };
}
process.stdout.write(JSON.stringify(config, null, 2) + "\n");

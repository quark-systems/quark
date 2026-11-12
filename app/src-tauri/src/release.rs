// What only an installed (bundled) app does: start QuarkBar and quarkd. `npm run tauri dev`
// doesn't: no QuarkBar sits inside its bundle, so development keeps running a daemon by hand.
// Updates are quarkd's (crates/quarkd/src/update.rs): it replaces this whole bundle, and the
// app only shows what `/v1/update` reports.
//
// On macOS, Quark.app carries QuarkBar.app (the menubar app, built by
// mac/scripts/package.sh) in Contents/Library/LoginItems, and QuarkBar carries quarkd.
// Opening Quark opens QuarkBar, which on its first run registers quarkd as a LaunchAgent
// and itself as a login item, so all three come back after a restart. On Windows the app
// ships without either: quarkd runs in WSL (packaging/wsl/install.sh) on the same port.
use std::path::{Path, PathBuf};

use tauri::AppHandle;

/// QuarkBar.app inside the Quark.app whose binary is `exe` (Contents/MacOS/<exe>).
fn embedded_quarkbar(exe: &Path) -> Option<PathBuf> {
    let contents = exe.parent()?.parent()?;
    let app = contents.join("Library/LoginItems/QuarkBar.app");
    app.is_dir().then_some(app)
}

/// Opens the embedded QuarkBar in the background, unless the app was pointed at another
/// daemon. Opening a running QuarkBar does nothing, so this runs on every launch.
pub fn start_quarkbar(_app: &AppHandle) {
    if !cfg!(target_os = "macos") || std::env::var_os("QUARK_DAEMON").is_some() {
        return;
    }
    let Some(bar) = std::env::current_exe()
        .ok()
        .and_then(|e| embedded_quarkbar(&e))
    else {
        return;
    };
    if let Err(e) = std::process::Command::new("/usr/bin/open")
        .arg("-g")
        .arg(&bar)
        .spawn()
    {
        eprintln!("could not open QuarkBar: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::embedded_quarkbar;

    #[test]
    fn finds_quarkbar_only_inside_the_bundle() {
        let dir = std::env::temp_dir().join(format!("quark-bundle-{}", std::process::id()));
        let exe = dir.join("Contents/MacOS/quark-desktop");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert_eq!(embedded_quarkbar(&exe), None);
        let bar = dir.join("Contents/Library/LoginItems/QuarkBar.app");
        std::fs::create_dir_all(&bar).unwrap();
        assert_eq!(embedded_quarkbar(&exe), Some(bar));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

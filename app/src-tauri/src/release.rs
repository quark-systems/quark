// What only an installed (bundled) app does: start QuarkBar and quarkd, and update itself.
// `npm run tauri dev` does neither: no QuarkBar sits inside its bundle and the updater is
// configured only by the release workflow (.github/workflows/release.yml), so development
// keeps running a daemon by hand.
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

/// Looks for an update once, and offers to install it and restart. Only when the release
/// build configured the updater (`plugins.updater` in tauri.conf.json).
pub fn offer_update(app: &AppHandle) -> tauri::Result<()> {
    if !app.config().plugins.0.contains_key("updater") {
        return Ok(());
    }
    app.plugin(tauri_plugin_updater::Builder::new().build())?;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
        use tauri_plugin_updater::UpdaterExt;
        let update = match app.updater() {
            Ok(updater) => updater.check().await,
            Err(e) => Err(e),
        };
        let update = match update {
            Ok(Some(update)) => update,
            Ok(None) => return,
            Err(e) => {
                eprintln!("update check failed: {e}");
                return;
            }
        };
        let current = app.package_info().version.to_string();
        let dialog = app
            .dialog()
            .message(format!(
                "Quark {} is available (you have {current}). Install it and restart? \
                 Running workers carry on; quarkd restarts on its own.",
                update.version
            ))
            .title("Update Quark")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Install and restart".into(),
                "Later".into(),
            ));
        // The answer waits for the person; keep that wait off the async workers.
        let yes = tauri::async_runtime::spawn_blocking(move || dialog.blocking_show()).await;
        if !matches!(yes, Ok(true)) {
            return;
        }
        match update.download_and_install(|_, _| {}, || {}).await {
            Ok(()) => app.restart(),
            Err(e) => eprintln!("update failed: {e}"),
        }
    });
    Ok(())
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

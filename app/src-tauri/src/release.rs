// What only an installed (bundled) app does: start the quarkd shipped inside it, and
// update itself. `npm run tauri dev` does neither: no quarkd sits beside its binary and the
// updater is configured only by the release workflow (.github/workflows/release.yml), so
// development keeps running a daemon by hand.
//
// On Windows the app ships without quarkd: quarkd runs in WSL (packaging/wsl/install.sh)
// and serves the same loopback port.
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tauri::{AppHandle, Manager};

const DAEMON_ADDR: ([u8; 4], u16) = ([127, 0, 0, 1], 7380);

/// Whether something already listens on quarkd's port.
fn daemon_listening() -> bool {
    let addr = SocketAddr::from(DAEMON_ADDR);
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// The quarkd bundled next to the app's own binary, if this is an installed app.
fn bundled_quarkd(exe: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "quarkd.exe" } else { "quarkd" };
    let path = exe.parent()?.join(name);
    path.is_file().then_some(path)
}

/// The PATH a login shell sees, so quarkd finds tmux, git, gh and the harness CLIs even
/// though an app started from Finder gets launchd's bare PATH (as mac/Resources/quarkd-launch).
#[cfg(target_os = "macos")]
fn login_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let out = std::process::Command::new(shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .output()
        .ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    (!path.is_empty()).then_some(path)
}

#[cfg(not(target_os = "macos"))]
fn login_path() -> Option<String> {
    None
}

/// Starts the bundled quarkd when no daemon is running and the app was not pointed at
/// another one. It keeps running after the app quits, like the LaunchAgent QuarkBar
/// installs; whichever starts first wins the port.
pub fn start_bundled_daemon(app: &AppHandle) {
    if std::env::var_os("QUARK_DAEMON").is_some() || daemon_listening() {
        return;
    }
    let Some(quarkd) = std::env::current_exe().ok().and_then(|e| bundled_quarkd(&e)) else {
        return;
    };
    let log = app
        .path()
        .app_log_dir()
        .ok()
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("quarkd.log"))
                .ok()
        });
    let mut cmd = std::process::Command::new(quarkd);
    cmd.args(["--engine", "native"]).stdin(std::process::Stdio::null());
    if let Some(path) = login_path() {
        cmd.env("PATH", path);
    }
    if let Some(log) = log {
        if let Ok(err) = log.try_clone() {
            cmd.stderr(err);
        }
        cmd.stdout(log);
    }
    if let Err(e) = cmd.spawn() {
        eprintln!("could not start the bundled quarkd: {e}");
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
    use super::bundled_quarkd;

    #[test]
    fn finds_quarkd_only_beside_the_app() {
        let dir = std::env::temp_dir().join(format!("quark-bundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("quark-desktop");
        assert_eq!(bundled_quarkd(&exe), None);
        let name = if cfg!(windows) { "quarkd.exe" } else { "quarkd" };
        std::fs::write(dir.join(name), b"").unwrap();
        assert_eq!(bundled_quarkd(&exe), Some(dir.join(name)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! The user's lifecycle hooks (`post_create`, `pre_destroy` from
//! `~/.config/treehouse/config.toml`; a repo's `treehouse.toml` never
//! supplies them), run as treehouse's `hooks.Run` runs them.

use std::path::Path;
use std::process::{Command, Stdio};

/// Run each hook with `/bin/sh -c` in `dir`, one after another. A failing
/// hook is logged and does not stop the rest or fail the operation. Output
/// goes to the log, never to a caller's stdout.
pub(super) fn run(kind: &str, hooks: &[String], dir: &Path) {
    for hook in hooks {
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(hook)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => {
                tracing::debug!(kind, hook, dir = %dir.display(), "worktree hook ran");
            }
            Ok(o) => tracing::warn!(
                kind,
                hook,
                dir = %dir.display(),
                status = %o.status,
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "worktree hook failed"
            ),
            Err(e) => {
                tracing::warn!(kind, hook, dir = %dir.display(), error = %e, "worktree hook failed")
            }
        }
    }
}

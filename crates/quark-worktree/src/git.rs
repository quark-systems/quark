//! Thin async wrapper over the `git` command line.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

use quark_core::{CoreError, Result};
use tokio::process::Command;

/// Output of one git command.
pub(crate) struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `git -C <dir> <args>` with no prompts and no pager.
pub(crate) async fn run<I, S>(dir: &Path, args: I) -> Result<Out>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| CoreError::Backend(format!("running git: {e}")))?;
    Ok(Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// Trimmed stdout of a git command that must succeed.
pub(crate) async fn read<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<S> = args.into_iter().collect();
    let shown = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let out = run(dir, args).await?;
    if !out.ok {
        return Err(CoreError::Backend(format!(
            "git {shown} in {} failed: {}",
            dir.display(),
            out.stderr
        )));
    }
    Ok(out.stdout.trim().to_string())
}

/// Trimmed stdout of a git command, or `None` when it fails.
pub(crate) async fn try_read<I, S>(dir: &Path, args: I) -> Result<Option<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = run(dir, args).await?;
    Ok(out.ok.then(|| out.stdout.trim().to_string()))
}

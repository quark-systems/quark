//! Running a trigger's condition and action commands.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ran {
    /// The exit code; `None` when it was killed by a signal or timed out.
    pub code: Option<i32>,
    pub timed_out: bool,
    /// A bounded tail of standard output.
    pub stdout: String,
    /// A bounded tail of standard error, or why it could not start.
    pub stderr: String,
}

/// Runs argv directly, with no shell, so nothing is re-split or
/// interpreted.
#[async_trait]
pub trait Commands: Send + Sync {
    async fn run(&self, argv: &[String], timeout: Duration) -> Ran;
}

/// Output kept from each stream.
const TAIL: usize = 4096;

/// Runs commands as child processes of the daemon.
#[derive(Debug, Clone, Default)]
pub struct Processes;

#[async_trait]
impl Commands for Processes {
    async fn run(&self, argv: &[String], timeout: Duration) -> Ran {
        let Some((program, args)) = argv.split_first() else {
            return Ran {
                stderr: "empty command".into(),
                ..Ran::default()
            };
        };
        let child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                return Ran {
                    stderr: format!("{program}: {e}"),
                    ..Ran::default()
                }
            }
        };
        let mut out = child.stdout.take().expect("piped");
        let mut err = child.stderr.take().expect("piped");
        let read_all = async {
            let (mut o, mut e) = (Vec::new(), Vec::new());
            let _ = tokio::join!(out.read_to_end(&mut o), err.read_to_end(&mut e));
            let status = child.wait().await;
            (status, o, e)
        };
        match tokio::time::timeout(timeout, read_all).await {
            Ok((status, o, e)) => Ran {
                code: status.ok().and_then(|s| s.code()),
                timed_out: false,
                stdout: tail(&o),
                stderr: tail(&e),
            },
            // Dropping the future drops the child, which kills it.
            Err(_) => Ran {
                code: None,
                timed_out: true,
                stdout: String::new(),
                stderr: format!("timed out after {}s", timeout.as_secs()),
            },
        }
    }
}

fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(TAIL);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn runs_without_a_shell() {
        let p = Processes;
        let r = p
            .run(
                &argv(&["printf", "%s", "a b; echo no"]),
                Duration::from_secs(5),
            )
            .await;
        assert_eq!(r.code, Some(0));
        assert_eq!(r.stdout, "a b; echo no");
        let r = p.run(&argv(&["false"]), Duration::from_secs(5)).await;
        assert_eq!(r.code, Some(1));
        let r = p
            .run(&argv(&["sleep", "5"]), Duration::from_millis(100))
            .await;
        assert!(r.timed_out);
        let r = p
            .run(&argv(&["/nonexistent/x"]), Duration::from_secs(1))
            .await;
        assert_eq!(r.code, None);
        assert!(!r.stderr.is_empty());
    }
}

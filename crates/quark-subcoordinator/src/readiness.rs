//! Whether a host is ready for a sub-coordinator, and what a person must do
//! when it is not. Seeding and every launch pass this gate first, so a host
//! that drifted out of readiness fails with the steps to fix it rather than
//! leaving a half-made home or session.

use quark_core::harness::HarnessManifest;
use quark_core::Result;
use quark_runtime::probe::{self, tmux_at_least, Probe};
use quark_runtime::Exec;

/// One thing missing on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// What was checked, such as `git` or `tmux`.
    pub check: String,
    /// The step that closes it, for a person at that machine.
    pub action: String,
}

/// The verdict for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub probe: Probe,
    pub gaps: Vec<Gap>,
}

impl Readiness {
    pub fn ready(&self) -> bool {
        self.gaps.is_empty()
    }

    /// The gaps as one message.
    pub fn summary(&self) -> String {
        self.gaps
            .iter()
            .map(|g| format!("{}: {}", g.check, g.action))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Check the host behind `exec` for a sub-coordinator on `manifest`.
/// `tmux` is required when its sessions run in tmux (every SSH host).
pub async fn check(exec: &dyn Exec, manifest: &HarnessManifest, tmux: bool) -> Result<Readiness> {
    let bins: Vec<&str> = manifest.detect.bins.iter().map(String::as_str).collect();
    let mut tools = vec!["git"];
    tools.extend(bins.iter().copied());
    let probe = probe::probe(exec, &tools).await?;
    let mut gaps = Vec::new();
    if !["macos", "linux"].contains(&probe.platform.os.as_str()) {
        gaps.push(Gap {
            check: "platform".into(),
            action: format!(
                "sub-coordinators run on macOS or Linux, not {}",
                probe.platform.os
            ),
        });
    }
    if !probe.has("git") {
        gaps.push(Gap {
            check: "git".into(),
            action: "install git".into(),
        });
    }
    if tmux {
        match &probe.tmux_version {
            Some(v) if tmux_at_least(v, 3, 2) => {}
            Some(v) => gaps.push(Gap {
                check: "tmux".into(),
                action: format!("upgrade tmux to 3.2 or later (found {v})"),
            }),
            None => gaps.push(Gap {
                check: "tmux".into(),
                action: "install tmux 3.2 or later".into(),
            }),
        }
    }
    if !bins.is_empty() && !bins.iter().any(|b| probe.has(b)) {
        let hint = manifest
            .detect
            .install_hint
            .clone()
            .unwrap_or_else(|| format!("install {}", bins.join(" or ")));
        gaps.push(Gap {
            check: manifest.id.clone(),
            action: format!("{hint}, then sign in to it on that host"),
        });
    }
    Ok(Readiness { probe, gaps })
}

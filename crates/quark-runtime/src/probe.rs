//! What a host is and what it has.

use std::collections::BTreeMap;

use quark_core::host::{Health, Platform};
use quark_core::Result;

use crate::exec::{Cmd, Exec};

/// How long a probe may take before the host counts as unreachable.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A host's platform, home directory and the tools found on its `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub platform: Platform,
    pub home: String,
    /// Each tool asked about and where it resolved, if anywhere.
    pub tools: BTreeMap<String, Option<String>>,
    /// `tmux -V`, when tmux is there.
    pub tmux_version: Option<String>,
}

impl Probe {
    pub fn has(&self, tool: &str) -> bool {
        self.tools.get(tool).is_some_and(Option::is_some)
    }
}

/// Probe `exec`'s host for its platform and `tools`.
pub async fn probe(exec: &dyn Exec, tools: &[&str]) -> Result<Probe> {
    let script = r#"uname -s; uname -m; printf '%s\n' "$HOME"
if command -v tmux >/dev/null 2>&1; then tmux -V; else echo; fi
for t in "$@"; do p=$(command -v "$t" 2>/dev/null) || p=; printf '%s\t%s\n' "$t" "$p"; done"#;
    let out = exec
        .run(&Cmd::sh(script, tools.iter().copied()).timeout(PROBE_TIMEOUT))
        .await?
        .check("probing the host")?
        .stdout_str();
    let mut lines = out.lines();
    let mut next = || lines.next().unwrap_or("").trim().to_string();
    let os = match next().as_str() {
        "Darwin" => "macos".to_string(),
        "Linux" => "linux".to_string(),
        other => other.to_lowercase(),
    };
    let arch = next();
    let home = next();
    let tmux_version = Some(next()).filter(|v| !v.is_empty());
    let mut found = BTreeMap::new();
    for line in out.lines().skip(4) {
        if let Some((t, p)) = line.split_once('\t') {
            found.insert(
                t.to_string(),
                Some(p.trim().to_string()).filter(|p| !p.is_empty()),
            );
        }
    }
    Ok(Probe {
        platform: Platform { os, arch },
        home,
        tools: found,
        tmux_version,
    })
}

/// Healthy when the host answers a probe; unreachable when it cannot be
/// asked; degraded when it answers but the probe fails.
pub async fn health(exec: &dyn Exec) -> Health {
    match exec
        .run(&Cmd::new(["uname", "-s"]).timeout(PROBE_TIMEOUT))
        .await
    {
        Ok(out) if out.success() => Health::Healthy,
        Ok(out) => Health::Degraded {
            reason: out
                .check("uname")
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default(),
        },
        Err(e) => Health::Unreachable {
            reason: e.to_string(),
        },
    }
}

/// Whether a `tmux -V` line is at least `major.minor`.
pub fn tmux_at_least(version: &str, major: u32, minor: u32) -> bool {
    let v = version.trim_start_matches("tmux").trim();
    let v = v.trim_start_matches("next-");
    let mut parts = v.split(|c: char| !c.is_ascii_digit());
    let maj: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let min: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (maj, min) >= (major, minor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalRuntime;
    use quark_core::HostId;

    #[tokio::test]
    async fn probes_this_machine() {
        let p = probe(
            &LocalRuntime::new(HostId::from("h")),
            &["sh", "no-such-tool-q"],
        )
        .await
        .unwrap();
        assert!(["linux", "macos"].contains(&p.platform.os.as_str()));
        assert!(!p.platform.arch.is_empty());
        assert!(p.has("sh"));
        assert!(!p.has("no-such-tool-q"));
        assert!(p.tools.contains_key("no-such-tool-q"));
    }

    #[test]
    fn tmux_versions() {
        assert!(tmux_at_least("tmux 3.4", 3, 2));
        assert!(tmux_at_least("tmux 3.2a", 3, 2));
        assert!(!tmux_at_least("tmux 3.1c", 3, 2));
        assert!(tmux_at_least("tmux next-3.5", 3, 2));
        assert!(!tmux_at_least("", 3, 2));
    }
}

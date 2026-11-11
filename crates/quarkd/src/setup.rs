//! First-run setup: what this machine needs before Quark can run a Project,
//! and the fixes Quark can make for it.
//!
//! [`check`] looks at the tools quarkd runs (git, tmux 3.2+, gh signed in, a
//! coordinator harness signed in, Beads and Dolt) and the optional ones
//! (treehouse, only when `QUARK_WORKTREE_POOL` asks for it), plus the home itself (writable,
//! format, free disk). `quarkd doctor` prints it; `GET /v1/setup` serves it
//! to the app's setup screen.
//!
//! A check that fails may carry a [`Fix`]. A fix with `runnable` is one
//! quarkd can do itself without admin rights or a prompt: a Homebrew or npm
//! install, or the pinned treehouse download. Only `POST /v1/setup/<id>`
//! (the person pressing the button) runs it; nothing installs silently.
//! Sign-ins are interactive, so their fix is the command to run in a
//! terminal.

use std::path::{Path, PathBuf};
use std::time::Duration;

use quark_harness::HostEnv;
use quark_systems::{AgentRole, AuthState};
use serde::Serialize;
use utoipa::ToSchema;

use crate::harness::HarnessRegistry;

/// The oldest tmux quarkd works with (control mode features).
pub const TMUX_MIN: (u32, u32) = (3, 2);
/// The oldest treehouse the worktree pool works with.
pub const TREEHOUSE_MIN: (u32, u32, u32) = (3, 1, 2);
/// Below this much free disk in the home, setup warns.
pub const LOW_DISK_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// How long one probe (`tmux -V`, `gh auth status`) may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a fix may take.
const FIX_TIMEOUT: Duration = Duration::from_secs(600);
/// The pinned treehouse installer, the same one self-hosting runs.
const TREEHOUSE_SH: &str = include_str!("../../../selfhost/treehouse.sh");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Present and working.
    Ok,
    /// Works, but something is worth a look (old, low, unknown).
    Warn,
    /// Not there or not working.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Fix {
    /// The button's words, e.g. "Install with Homebrew".
    pub label: String,
    /// What it runs, shown so the person knows; for a sign-in, the command
    /// to run in a terminal.
    pub command: String,
    /// quarkd can run it itself (`POST /v1/setup/<id>`).
    pub runnable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Check {
    /// Stable id, also the fix's path: `tmux`, `gh_auth`, `coordinator`, ...
    pub id: String,
    pub title: String,
    pub status: CheckStatus,
    /// Quark cannot run a Project until a required check is ok or warn.
    pub required: bool,
    /// What was found, e.g. `tmux 3.4 at /opt/homebrew/bin/tmux`.
    pub detail: String,
    /// What it is for, one line.
    pub why: String,
    pub fix: Option<Fix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SetupReport {
    /// Every required check is ok or warn.
    pub ready: bool,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FixOutcome {
    pub id: String,
    pub ok: bool,
    /// The fix's output, stdout then stderr, trimmed to the last 8 KB.
    pub output: String,
    /// The checks again, after the fix.
    pub report: SetupReport,
}

impl SetupReport {
    fn new(checks: Vec<Check>) -> Self {
        let ready = checks
            .iter()
            .all(|c| !c.required || c.status != CheckStatus::Missing);
        Self { ready, checks }
    }

    pub fn get(&self, id: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.id == id)
    }
}

/// Every check, for the home at `home`.
pub async fn check(home: &Path, harnesses: &HarnessRegistry) -> SetupReport {
    let env = harnesses.env();
    let brew = env.which("brew");
    let npm = env.which("npm");
    // The native worktree pool is the default; treehouse is checked only when
    // QUARK_WORKTREE_POOL asks for it (the fallback pool or the shadow).
    let treehouse_needed = crate::native_worktrees::Mode::parse(
        env.var(crate::native_worktrees::ENV),
        env.var(crate::native_worktrees::LEGACY_SHADOW_ENV),
    ) != crate::native_worktrees::Mode::Native;
    let treehouse_probe = async {
        if treehouse_needed {
            version_of(env, "treehouse", &["--version"]).await
        } else {
            None
        }
    };
    let (git, tmux, gh, treehouse, bd, dolt) = tokio::join!(
        version_of(env, "git", &["--version"]),
        version_of(env, "tmux", &["-V"]),
        version_of(env, "gh", &["--version"]),
        treehouse_probe,
        version_of(env, "bd", &["--version"]),
        version_of(env, "dolt", &["version"]),
    );
    let gh_auth = match &gh {
        Some((path, _)) => Some(run(path, &["auth", "status"]).await),
        None => None,
    };
    let infos = harnesses.list(true).await;

    let brew_fix = |formula: &str| Fix {
        label: "Install with Homebrew".into(),
        command: format!("brew install {formula}"),
        runnable: brew.is_some(),
    };
    let mut checks = Vec::new();

    checks.push(tool(
        "git",
        "git",
        true,
        "Repos, worktrees, branches and merges.",
        git.as_ref(),
        None,
        Some(brew_fix("git")),
    ));
    let tmux_old = tmux
        .as_ref()
        .and_then(|(_, v)| parse_version(v))
        .is_some_and(|(a, b, _)| (a, b) < TMUX_MIN);
    let mut tmux_check = tool(
        "tmux",
        "tmux 3.2 or later",
        true,
        "Coordinator terminals run on a private tmux server.",
        tmux.as_ref(),
        None,
        Some(brew_fix("tmux")),
    );
    if tmux_old {
        tmux_check.status = CheckStatus::Missing;
        tmux_check.detail = format!("{}; 3.2 or later is needed", tmux_check.detail);
        tmux_check.fix = Some(Fix {
            label: "Upgrade with Homebrew".into(),
            command: "brew upgrade tmux".into(),
            runnable: brew.is_some(),
        });
    }
    checks.push(tmux_check);
    checks.push(tool(
        "gh",
        "GitHub CLI",
        true,
        "Cloning, the PR center and merges.",
        gh.as_ref(),
        None,
        Some(brew_fix("gh")),
    ));
    checks.push(match gh_auth {
        None => Check {
            id: "gh_auth".into(),
            title: "Signed in to GitHub".into(),
            status: CheckStatus::Missing,
            required: true,
            detail: "needs the GitHub CLI first".into(),
            why: "Quark reads and merges pull requests as you.".into(),
            fix: None,
        },
        Some(r) => Check {
            id: "gh_auth".into(),
            title: "Signed in to GitHub".into(),
            status: if r.ok {
                CheckStatus::Ok
            } else {
                CheckStatus::Missing
            },
            required: true,
            detail: if r.ok {
                first_line_with(&r.output, "Logged in").unwrap_or_else(|| "signed in".into())
            } else {
                "not signed in".into()
            },
            why: "Quark reads and merges pull requests as you.".into(),
            fix: (!r.ok).then(|| Fix {
                label: "Sign in".into(),
                command: "gh auth login".into(),
                runnable: false,
            }),
        },
    });
    checks.push(coordinator_check(&infos, npm.is_some()));
    if treehouse_needed {
        let tree_old = treehouse
            .as_ref()
            .and_then(|(_, v)| parse_version(v))
            .is_some_and(|v| v < TREEHOUSE_MIN || v.0 != TREEHOUSE_MIN.0);
        let treehouse_fix = Fix {
            label: "Install treehouse 3.1.2".into(),
            command:
                "the pinned treehouse 3.1.2 release, into ~/.local/bin (selfhost/treehouse.sh)"
                    .into(),
            runnable: true,
        };
        let mut tree_check = tool(
            "treehouse",
            "treehouse 3.1.2 or a later 3.x",
            true,
            "The worktree pool workers run in (QUARK_WORKTREE_POOL asks for treehouse).",
            treehouse.as_ref(),
            None,
            Some(treehouse_fix.clone()),
        );
        if tree_old {
            tree_check.status = CheckStatus::Missing;
            tree_check.detail = format!("{}; 3.1.2 or a later 3.x is needed", tree_check.detail);
            tree_check.fix = Some(treehouse_fix);
        }
        checks.push(tree_check);
    }
    checks.push(tool(
        "bd",
        "Beads (bd)",
        true,
        "Issues, decisions and memory live in each project's Beads database.",
        bd.as_ref(),
        None,
        Some(Fix {
            label: "How to install".into(),
            command: "see https://beads.gascity.com".into(),
            runnable: false,
        }),
    ));
    checks.push(tool(
        "dolt",
        "Dolt",
        true,
        "The database server Beads runs on.",
        dolt.as_ref(),
        None,
        Some(brew_fix("dolt")),
    ));
    checks.extend(home_checks(home));
    SetupReport::new(checks)
}

/// The checks on the home itself: writable, its format, free disk.
fn home_checks(home: &Path) -> Vec<Check> {
    let mut out = Vec::new();
    let probe = home.join(format!(".setup-probe-{}", std::process::id()));
    let writable = std::fs::create_dir_all(home)
        .and_then(|_| std::fs::write(&probe, b""))
        .and_then(|_| std::fs::remove_file(&probe));
    let format = crate::home_format::read(home);
    let (status, detail) = match (&writable, &format) {
        (Err(e), _) => (CheckStatus::Missing, format!("cannot write: {e}")),
        (Ok(()), Err(e)) => (CheckStatus::Missing, format!("{e:#}")),
        (Ok(()), Ok(Some(f))) if f.format > crate::home_format::CURRENT => (
            CheckStatus::Missing,
            format!(
                "format {} is newer than this quarkd knows ({})",
                f.format,
                crate::home_format::CURRENT
            ),
        ),
        (Ok(()), Ok(Some(f))) => (
            CheckStatus::Ok,
            format!("{} (format {})", home.display(), f.format),
        ),
        (Ok(()), Ok(None)) => (
            CheckStatus::Ok,
            format!("{} (set up on first start)", home.display()),
        ),
    };
    out.push(Check {
        id: "home".into(),
        title: "Quark home".into(),
        status,
        required: true,
        detail,
        why: "Where Quark keeps projects, their working copies and its databases.".into(),
        fix: None,
    });
    if let Some(free) = free_bytes(home) {
        out.push(Check {
            id: "disk".into(),
            title: "Free disk".into(),
            status: if free < LOW_DISK_BYTES {
                CheckStatus::Warn
            } else {
                CheckStatus::Ok
            },
            required: false,
            detail: format!("{:.1} GB free", free as f64 / 1e9),
            why: "Worktrees and agent logs need room; below 5 GB new workers may not start.".into(),
            fix: None,
        });
    }
    out
}

/// At least one harness that can coordinate is installed and signed in.
fn coordinator_check(infos: &[quark_systems::HarnessInfo], npm: bool) -> Check {
    let coordinators: Vec<_> = infos
        .iter()
        .filter(|h| h.roles.contains(&AgentRole::Coordinator))
        .collect();
    let installed: Vec<_> = coordinators
        .iter()
        .filter(|h| h.install.installed)
        .collect();
    let signed_in = installed
        .iter()
        .find(|h| h.auth.state == AuthState::Configured);
    let unknown = installed
        .iter()
        .find(|h| h.auth.state == AuthState::Unknown);
    let why = "The coordinator runs each project's conversation and starts workers.".to_string();
    let base = |status, detail: String, fix| Check {
        id: "coordinator".into(),
        title: "Coordinator harness, signed in".into(),
        status,
        required: true,
        detail,
        why: why.clone(),
        fix,
    };
    if let Some(h) = signed_in {
        return base(
            CheckStatus::Ok,
            format!("{} {}", h.name, h.install.version.as_deref().unwrap_or(""))
                .trim()
                .into(),
            None,
        );
    }
    if let Some(h) = unknown {
        return base(
            CheckStatus::Warn,
            format!(
                "{} is installed; Quark can't tell whether it is signed in",
                h.name
            ),
            None,
        );
    }
    if let Some(h) = installed.first() {
        let cmd = match h.id.as_str() {
            "claude-code" => "claude, then /login",
            "codex" => "codex login",
            "pi" | "pi-signed" => "pi, then /login",
            _ => "the agent's own login",
        };
        return base(
            CheckStatus::Missing,
            format!(
                "{} is installed but not signed in ({})",
                h.name, h.auth.detail
            ),
            Some(Fix {
                label: "Sign in".into(),
                command: cmd.into(),
                runnable: false,
            }),
        );
    }
    let hint = coordinators
        .iter()
        .find(|h| h.id == "claude-code")
        .map(|h| h.install.install_hint.clone())
        .unwrap_or_else(|| "npm install -g @anthropic-ai/claude-code".into());
    base(
        CheckStatus::Missing,
        "no coordinator harness installed (Claude Code, Codex or Pi)".into(),
        Some(Fix {
            label: "Install Claude Code".into(),
            runnable: npm && hint.starts_with("npm install -g "),
            command: hint,
        }),
    )
}

/// A tool check from its probe: `found` is the binary and its version line.
fn tool(
    id: &str,
    title: &str,
    required: bool,
    why: &str,
    found: Option<&(PathBuf, String)>,
    missing_note: Option<&str>,
    fix: Option<Fix>,
) -> Check {
    match found {
        Some((path, version)) => Check {
            id: id.into(),
            title: title.into(),
            status: CheckStatus::Ok,
            required,
            detail: format!("{} at {}", version, path.display()),
            why: why.into(),
            fix: None,
        },
        None => Check {
            id: id.into(),
            title: title.into(),
            status: CheckStatus::Missing,
            required,
            detail: match missing_note {
                Some(n) => format!("not installed; {n}"),
                None => "not installed".into(),
            },
            why: why.into(),
            fix,
        },
    }
}

/// The binary `name` on PATH and the first line of its version output.
async fn version_of(env: &HostEnv, name: &str, args: &[&str]) -> Option<(PathBuf, String)> {
    let path = env.which(name)?;
    let r = run(&path, args).await;
    let line = r
        .output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("version unknown")
        .to_string();
    Some((path, line))
}

/// The first `x.y` or `x.y.z` in `s` (`tmux 3.3a` → 3.3.0).
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // A version starts after a non-alphanumeric, or after a `v` that does (`v3.1.5`).
        let starts = i == 0
            || !bytes[i - 1].is_ascii_alphanumeric()
            || (bytes[i - 1] == b'v' && (i == 1 || !bytes[i - 2].is_ascii_alphanumeric()));
        if bytes[i].is_ascii_digit() && starts {
            let rest = &s[i..];
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(rest.len());
            let nums: Vec<u32> = rest[..end]
                .split('.')
                .take(3)
                .map_while(|p| p.parse().ok())
                .collect();
            if nums.len() >= 2 {
                return Some((nums[0], nums[1], nums.get(2).copied().unwrap_or(0)));
            }
        }
        i += 1;
    }
    None
}

fn first_line_with(text: &str, needle: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| l.contains(needle))
        .map(|l| {
            l.trim_start_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
}

struct Ran {
    ok: bool,
    output: String,
}

async fn run(bin: &Path, args: &[&str]) -> Ran {
    run_with(tokio::process::Command::new(bin).args(args), PROBE_TIMEOUT).await
}

async fn run_with(cmd: &mut tokio::process::Command, timeout: Duration) -> Ran {
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            Ran {
                ok: out.status.success(),
                output: text,
            }
        }
        Ok(Err(e)) => Ran {
            ok: false,
            output: e.to_string(),
        },
        Err(_) => Ran {
            ok: false,
            output: format!("timed out after {}s", timeout.as_secs()),
        },
    }
}

/// Run check `id`'s fix, if quarkd can run it, and check again.
pub async fn fix(home: &Path, harnesses: &HarnessRegistry, id: &str) -> Result<FixOutcome, String> {
    let before = check(home, harnesses).await;
    let Some(c) = before.get(id) else {
        return Err(format!("no setup check {id:?}"));
    };
    let Some(f) = c.fix.as_ref().filter(|f| f.runnable) else {
        return Err(format!("{} has no fix Quark can run", c.title));
    };
    let env = harnesses.env();
    let ran = if id == "treehouse" {
        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(["-c", TREEHOUSE_SH, "treehouse.sh"]);
        run_with(&mut cmd, FIX_TIMEOUT).await
    } else {
        let mut words = f.command.split_whitespace();
        let program = words.next().unwrap_or_default();
        // Only the installers the checks above offer.
        if !matches!(program, "brew" | "npm") {
            return Err(format!("{} has no fix Quark can run", c.title));
        }
        let Some(bin) = env.which(program) else {
            return Err(format!("{program} is not on PATH"));
        };
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(words).env("HOMEBREW_NO_AUTO_UPDATE", "1");
        run_with(&mut cmd, FIX_TIMEOUT).await
    };
    let report = check(home, harnesses).await;
    let tail = ran.output.len().saturating_sub(8 * 1024);
    let tail = (tail..ran.output.len())
        .find(|i| ran.output.is_char_boundary(*i))
        .unwrap_or(ran.output.len());
    Ok(FixOutcome {
        id: id.into(),
        ok: ran.ok,
        output: ran.output[tail..].trim().to_string(),
        report,
    })
}

/// Free bytes on the file system holding `path` (or its nearest existing
/// parent).
fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(p)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// Available bytes from `df -Pk` output (the fourth field of the second line).
fn parse_df(text: &str) -> Option<u64> {
    let kb: u64 = text
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

/// The report as text, for `quarkd doctor`.
pub fn render(report: &SetupReport) -> String {
    let mut s = String::new();
    for c in &report.checks {
        let mark = match c.status {
            CheckStatus::Ok => "ok  ",
            CheckStatus::Warn => "warn",
            CheckStatus::Missing if c.required => "FAIL",
            CheckStatus::Missing => "--  ",
        };
        s.push_str(&format!("{mark}  {:<32} {}\n", c.title, c.detail));
        if c.status != CheckStatus::Ok {
            if let Some(f) = &c.fix {
                s.push_str(&format!("      {:<32} {}\n", "", f.command));
            }
        }
    }
    s.push_str(if report.ready {
        "\nReady: Quark can run a project.\n"
    } else {
        "\nNot ready: fix the FAIL lines above (or open Quark's setup screen).\n"
    });
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_from_tool_output() {
        assert_eq!(parse_version("tmux 3.4"), Some((3, 4, 0)));
        assert_eq!(parse_version("tmux 3.3a"), Some((3, 3, 0)));
        assert_eq!(parse_version("tmux next-3.6"), Some((3, 6, 0)));
        assert_eq!(parse_version("treehouse version 3.1.2"), Some((3, 1, 2)));
        assert_eq!(parse_version("v3.1.5"), Some((3, 1, 5)));
        assert_eq!(parse_version("treehouse v3.1.5"), Some((3, 1, 5)));
        assert_eq!(parse_version("dev3.1"), None);
        assert_eq!(
            parse_version("git version 2.50.1 (Apple Git-155)"),
            Some((2, 50, 1))
        );
        assert_eq!(parse_version("v1x 2"), None);
    }

    #[test]
    fn df_output_parses() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                   /dev/disk3s5 971350180 500000000 400000000 56% /System/Volumes/Data\n";
        assert_eq!(parse_df(out), Some(400_000_000 * 1024));
        assert_eq!(parse_df("header only"), None);
    }

    #[test]
    fn ready_ignores_optional_checks() {
        let c = |id: &str, required, status| Check {
            id: id.into(),
            title: id.into(),
            status,
            required,
            detail: String::new(),
            why: String::new(),
            fix: None,
        };
        let r = SetupReport::new(vec![
            c("git", true, CheckStatus::Ok),
            c("coordinator", true, CheckStatus::Warn),
            c("bd", false, CheckStatus::Missing),
        ]);
        assert!(r.ready);
        let r = SetupReport::new(vec![c("tmux", true, CheckStatus::Missing)]);
        assert!(!r.ready);
    }

    #[tokio::test]
    async fn a_bare_path_reports_what_is_missing() {
        let home = tempfile::tempdir().unwrap();
        let empty = tempfile::tempdir().unwrap();
        let env = HostEnv {
            path: Some(empty.path().as_os_str().to_owned()),
            home: Some(home.path().to_path_buf()),
            ..HostEnv::default()
        };
        let reg = HarnessRegistry::new(crate::harness::builtin(), env);
        let r = check(home.path(), &reg).await;
        assert!(!r.ready);
        for id in ["git", "tmux", "gh", "gh_auth", "coordinator", "bd", "dolt"] {
            assert_eq!(r.get(id).unwrap().status, CheckStatus::Missing, "{id}");
        }
        assert!(r.get("bd").unwrap().required && r.get("dolt").unwrap().required);
        // treehouse is not checked unless the treehouse pool is asked for.
        assert!(r.get("treehouse").is_none());
        let mut env = reg.env().clone();
        env.vars
            .insert(crate::native_worktrees::ENV.into(), "treehouse".into());
        let tree_reg = HarnessRegistry::new(crate::harness::builtin(), env);
        let tree = check(home.path(), &tree_reg).await;
        let t = tree.get("treehouse").unwrap();
        assert!(t.required && t.status == CheckStatus::Missing);
        assert!(t.fix.as_ref().unwrap().runnable);
        assert_eq!(r.get("home").unwrap().status, CheckStatus::Ok);
        let tmux = r.get("tmux").unwrap().fix.clone().unwrap();
        assert_eq!(tmux.command, "brew install tmux");
        assert!(!tmux.runnable, "no brew on this PATH");
        assert!(render(&r).contains("Not ready"));
        // A fix Quark can't run is refused.
        assert!(fix(home.path(), &reg, "gh_auth").await.is_err());
        assert!(fix(home.path(), &reg, "nope").await.is_err());
    }
}

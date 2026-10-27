//! Finding the session log a harness is writing for a working directory.
//!
//! Each harness keys its logs by the agent's working directory, so the daemon
//! finds a worker's log from its worktree and a coordinator's from its
//! workspace root. When several sessions ran in one directory the most
//! recently written one wins, which follows a relaunch onto its new log.
//!
//! Worktree slots are reused, so a directory can hold the logs of a task
//! that ran there before. [`locate_since`] skips logs last written before
//! the current occupant took the directory.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::SessionFormat;

/// How many dated Codex day directories to scan, newest first.
const CODEX_DAYS_SCANNED: usize = 14;

/// Where each harness keeps its state. Several roots per harness allow one
/// config directory per account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRoots {
    /// Claude Code config directories (`~/.claude`, or `$CLAUDE_CONFIG_DIR`).
    pub claude: Vec<PathBuf>,
    /// Codex homes (`~/.codex`, or `$CODEX_HOME`).
    pub codex: Vec<PathBuf>,
    /// Pi agent directories (`~/.pi/agent`, or `$PI_CODING_AGENT_DIR`).
    pub pi: Vec<PathBuf>,
    /// Kiro CLI homes (`~/.kiro`, or `$KIRO_HOME`).
    pub kiro: Vec<PathBuf>,
}

impl SessionRoots {
    /// The default roots for the current user, honouring each harness's own
    /// override variable.
    pub fn from_env() -> SessionRoots {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let pick = |var: &str, default: &[&str]| -> Vec<PathBuf> {
            if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                return vec![PathBuf::from(v)];
            }
            home.iter()
                .map(|h| default.iter().fold(h.clone(), |p, part| p.join(part)))
                .collect()
        };
        SessionRoots {
            claude: pick("CLAUDE_CONFIG_DIR", &[".claude"]),
            codex: pick("CODEX_HOME", &[".codex"]),
            pi: pick("PI_CODING_AGENT_DIR", &[".pi", "agent"]),
            kiro: pick("KIRO_HOME", &[".kiro"]),
        }
    }

    fn for_format(&self, format: SessionFormat) -> &[PathBuf] {
        match format {
            SessionFormat::Claude => &self.claude,
            SessionFormat::Codex => &self.codex,
            SessionFormat::Pi => &self.pi,
            SessionFormat::Kiro => &self.kiro,
        }
    }

    /// The roots of `format`, to add an account's directory to.
    pub fn for_format_mut(&mut self, format: SessionFormat) -> &mut Vec<PathBuf> {
        match format {
            SessionFormat::Claude => &mut self.claude,
            SessionFormat::Codex => &mut self.codex,
            SessionFormat::Pi => &mut self.pi,
            SessionFormat::Kiro => &mut self.kiro,
        }
    }
}

/// The most recently written session log of `format` for an agent working in
/// `cwd`, or `None` when that harness has no log for it.
pub fn locate(format: SessionFormat, cwd: &Path, roots: &SessionRoots) -> Option<PathBuf> {
    locate_since(format, cwd, roots, None)
}

/// [`locate`], counting only logs written at or after `since`: the time the
/// agent's task took `cwd`, so a previous occupant's log is never taken for
/// its own.
pub fn locate_since(
    format: SessionFormat,
    cwd: &Path,
    roots: &SessionRoots,
    since: Option<SystemTime>,
) -> Option<PathBuf> {
    let mut cwds = vec![cwd.to_path_buf()];
    if let Ok(real) = fs::canonicalize(cwd) {
        if real != cwd {
            cwds.push(real);
        }
    }
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for root in roots.for_format(format) {
        for cwd in &cwds {
            let found = match format {
                SessionFormat::Claude => {
                    newest_jsonl(&root.join("projects").join(claude_slug(cwd)))
                }
                SessionFormat::Pi => newest_jsonl(&root.join("sessions").join(pi_slug(cwd))),
                SessionFormat::Codex => codex_rollout(&root.join("sessions"), cwd),
                SessionFormat::Kiro => kiro_session(&root.join("sessions"), cwd),
            };
            if let Some(path) = found {
                let mtime = modified(&path);
                if since.is_some_and(|since| mtime < since) {
                    continue;
                }
                if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                    best = Some((mtime, path));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Claude Code's project directory name: every character outside
/// `[A-Za-z0-9]` becomes `-`.
pub(crate) fn claude_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Pi's session directory name: `--<cwd without its leading separator, with
/// separators and colons as dashes>--`.
pub(crate) fn pi_slug(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let s = s.strip_prefix(['/', '\\']).unwrap_or(&s);
    let s: String = s
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{s}--")
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && p.is_file())
        .max_by_key(|p| modified(p))
}

/// Codex shards rollouts by date, so the newest day directories are scanned
/// first and each rollout's `session_meta` line names its working directory.
fn codex_rollout(sessions: &Path, cwd: &Path) -> Option<PathBuf> {
    let mut days = Vec::new();
    for year in sorted_dirs_desc(sessions) {
        for month in sorted_dirs_desc(&year) {
            for day in sorted_dirs_desc(&month) {
                days.push(day);
                if days.len() == CODEX_DAYS_SCANNED {
                    break;
                }
            }
            if days.len() == CODEX_DAYS_SCANNED {
                break;
            }
        }
        if days.len() == CODEX_DAYS_SCANNED {
            break;
        }
    }
    let want = cwd.to_string_lossy();
    for day in days {
        let Ok(entries) = fs::read_dir(&day) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|x| x == "jsonl")
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
            })
            .collect();
        // Names start with the session's start time, so newest sorts last.
        files.sort();
        for path in files.into_iter().rev() {
            if codex_cwd(&path).as_deref() == Some(want.as_ref()) {
                return Some(path);
            }
        }
    }
    None
}

/// Kiro keeps one directory per session, `<workspace hash>/sess_<id>/`,
/// whose `session.json` lists its `workspacePaths`; the newest log of a
/// session started in `cwd` wins. `cli/` holds the older 2.x store.
fn kiro_session(sessions: &Path, cwd: &Path) -> Option<PathBuf> {
    let want = cwd.to_string_lossy();
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for hash in sorted_dirs_desc(sessions) {
        if hash.file_name().is_some_and(|n| n == "cli") {
            continue;
        }
        for sess in sorted_dirs_desc(&hash) {
            if !sess
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("sess_"))
            {
                continue;
            }
            let log = sess.join("messages.jsonl");
            if !log.is_file() || !kiro_workspaces(&sess).iter().any(|w| *w == want) {
                continue;
            }
            let mtime = modified(&log);
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, log));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn kiro_workspaces(sess: &Path) -> Vec<String> {
    let Ok(raw) = fs::read(sess.join("session.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    v.get("workspacePaths")
        .and_then(|w| w.as_array())
        .into_iter()
        .flatten()
        .filter_map(|w| w.as_str().map(|w| w.trim_end_matches('/').to_string()))
        .collect()
}

fn codex_cwd(path: &Path) -> Option<String> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut line)
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&line).ok()?;
    if v.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    Some(v.get("payload")?.get("cwd")?.as_str()?.to_string())
}

fn sorted_dirs_desc(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.reverse();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_match_each_harness() {
        let cwd = Path::new("/Users/matt/.quark/work/fix-42");
        assert_eq!(claude_slug(cwd), "-Users-matt--quark-work-fix-42");
        assert_eq!(pi_slug(cwd), "--Users-matt-.quark-work-fix-42--");
    }
}

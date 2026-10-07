//! Changed files and diffs of a task's git working tree.
//!
//! A task is compared from where it branched off the default branch to its
//! working tree as it is now, so uncommitted and untracked work shows up while
//! the worker is still writing it. Everything here is read-only: git runs with
//! optional locks off (no index refresh writes), no external diff or textconv
//! drivers, and no network access, so a worktree the engine owns is never
//! changed by looking at it.
//!
//! Local mode reads worktrees on this machine; a cloud runtime will serve the
//! same answers from behind the engine adapter.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use quark_systems::{ChangedFile, FileChangeStatus};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Largest patch returned in one response; longer ones are cut and flagged.
pub const MAX_PATCH_BYTES: usize = 2 * 1024 * 1024;
/// Untracked files larger than this are listed without line counts.
const MAX_COUNTED_FILE_BYTES: u64 = 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("working tree is missing: {0}")]
    Missing(PathBuf),
    #[error("no default branch to compare against (tried origin/HEAD, origin/main, origin/master, main, master)")]
    NoBase,
    #[error("file is not changed in this task: {0}")]
    NotChanged(String),
    #[error("git {args} failed: {detail}")]
    Git { args: String, detail: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, WorktreeError>;

/// What a task changed relative to its base.
#[derive(Debug, Clone, PartialEq)]
pub struct Changes {
    pub base_ref: String,
    pub base: String,
    pub head: String,
    pub files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    pub base: String,
    pub text: String,
    pub truncated: bool,
}

/// Lists every file the task changed, with line counts.
pub async fn changes(worktree: &Path) -> Result<Changes> {
    let git = Git::open(worktree)?;
    let (base_ref, base) = git.base().await?;
    let head = git.rev_parse("HEAD").await?.unwrap_or_default();
    let files = git.changed_files(&base).await?;
    Ok(Changes {
        base_ref,
        base,
        head,
        files,
    })
}

/// A unified diff for the whole task, or for one changed file.
pub async fn diff(worktree: &Path, path: Option<&str>) -> Result<Patch> {
    let git = Git::open(worktree)?;
    let (_, base) = git.base().await?;
    let files = git.changed_files(&base).await?;
    let selected: Vec<&ChangedFile> = match path {
        None => files.iter().collect(),
        Some(p) => {
            let f = files
                .iter()
                .find(|f| f.path == p)
                .ok_or_else(|| WorktreeError::NotChanged(p.to_string()))?;
            vec![f]
        }
    };

    let mut out = Limited::new(MAX_PATCH_BYTES);
    let tracked: Vec<&ChangedFile> = selected
        .iter()
        .copied()
        .filter(|f| f.status != FileChangeStatus::Untracked)
        .collect();
    if !tracked.is_empty() {
        let mut args = vec![
            "diff".to_string(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-color".into(),
            "-M".into(),
            base.clone(),
            "--".into(),
        ];
        if path.is_some() {
            for f in &tracked {
                args.extend(f.old_path.iter().map(|p| literal(p)));
                args.push(literal(&f.path));
            }
        }
        let run = git.run(&args, out.remaining()).await?;
        run.check(&args, &[0])?;
        out.push(&run.stdout, run.truncated);
    }
    for f in selected
        .iter()
        .filter(|f| f.status == FileChangeStatus::Untracked)
    {
        if out.full() {
            break;
        }
        // --no-index exits 1 when the files differ, which they always do here.
        let args: Vec<String> = [
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--",
            "/dev/null",
            &f.path,
        ]
        .map(str::to_string)
        .to_vec();
        let run = git.run(&args, out.remaining()).await?;
        run.check(&args, &[0, 1])?;
        out.push(&run.stdout, run.truncated);
    }
    let (text, truncated) = out.finish();
    Ok(Patch {
        base,
        text,
        truncated,
    })
}

/// A pathspec that matches `p` exactly, with no glob or magic.
fn literal(p: &str) -> String {
    format!(":(literal){p}")
}

struct Git {
    dir: PathBuf,
}

struct Run {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
    truncated: bool,
}

impl Run {
    fn check(&self, args: &[String], ok: &[i32]) -> Result<()> {
        if self.code.is_some_and(|c| ok.contains(&c)) {
            return Ok(());
        }
        Err(WorktreeError::Git {
            args: args.first().cloned().unwrap_or_default(),
            detail: match self.code {
                Some(c) => format!("exit {c}: {}", self.stderr.trim()),
                None => format!("killed: {}", self.stderr.trim()),
            },
        })
    }
}

impl Git {
    fn open(dir: &Path) -> Result<Git> {
        if !dir.is_dir() {
            return Err(WorktreeError::Missing(dir.to_path_buf()));
        }
        Ok(Git {
            dir: dir.to_path_buf(),
        })
    }

    /// Runs git in the worktree, keeping at most `limit` bytes of stdout.
    async fn run(&self, args: &[String], limit: usize) -> Result<Run> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["-c", "core.quotePath=false"])
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let work = async {
            let mut out = Vec::new();
            let mut buf = [0u8; 64 * 1024];
            let mut truncated = false;
            loop {
                let n = stdout.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                let room = limit.saturating_sub(out.len());
                if n > room {
                    out.extend_from_slice(&buf[..room]);
                    truncated = true;
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            if truncated {
                let _ = child.start_kill();
            }
            let mut err = Vec::new();
            let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut err).await;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>(Run {
                // A git killed for truncation still produced a usable prefix.
                code: if truncated { Some(0) } else { status.code() },
                stdout: out,
                stderr: String::from_utf8_lossy(&err).into_owned(),
                truncated,
            })
        };
        match tokio::time::timeout(GIT_TIMEOUT, work).await {
            Ok(res) => Ok(res?),
            Err(_) => Err(WorktreeError::Git {
                args: args.first().cloned().unwrap_or_default(),
                detail: format!("timed out after {}s", GIT_TIMEOUT.as_secs()),
            }),
        }
    }

    async fn output(&self, args: &[&str]) -> Result<Vec<u8>> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let run = self.run(&args, usize::MAX).await?;
        run.check(&args, &[0])?;
        Ok(run.stdout)
    }

    /// The commit `rev` names, or `None` when it names nothing.
    async fn rev_parse(&self, rev: &str) -> Result<Option<String>> {
        let args: Vec<String> = [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ]
        .map(str::to_string)
        .to_vec();
        let run = self.run(&args, 4096).await?;
        Ok(match run.code {
            Some(0) => Some(String::from_utf8_lossy(&run.stdout).trim().to_string()),
            _ => None,
        })
    }

    /// The default branch and the commit HEAD branched from it. Uses refs
    /// already present; never fetches.
    async fn base(&self) -> Result<(String, String)> {
        let mut candidates = Vec::new();
        let args: Vec<String> = [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ]
        .map(str::to_string)
        .to_vec();
        let run = self.run(&args, 4096).await?;
        if run.code == Some(0) {
            candidates.push(String::from_utf8_lossy(&run.stdout).trim().to_string());
        }
        candidates.extend(["origin/main", "origin/master", "main", "master"].map(String::from));
        for base_ref in candidates {
            if self.rev_parse(&base_ref).await?.is_none() {
                continue;
            }
            let out = self.output(&["merge-base", "HEAD", &base_ref]).await;
            if let Ok(out) = out {
                let base = String::from_utf8_lossy(&out).trim().to_string();
                if !base.is_empty() {
                    return Ok((base_ref, base));
                }
            }
        }
        Err(WorktreeError::NoBase)
    }

    async fn changed_files(&self, base: &str) -> Result<Vec<ChangedFile>> {
        let flags = ["--no-ext-diff", "--no-textconv", "-M", "-z"];
        let mut args = vec!["diff", "--name-status"];
        args.extend(flags);
        args.push(base);
        let names = self.output(&args).await?;
        let mut args = vec!["diff", "--numstat"];
        args.extend(flags);
        args.push(base);
        let counts = parse_numstat(&self.output(&args).await?);

        let mut files: BTreeMap<String, ChangedFile> = BTreeMap::new();
        for (status, old_path, path) in parse_name_status(&names) {
            let (additions, deletions) = counts.get(&path).copied().unwrap_or((None, None));
            files.insert(
                path.clone(),
                ChangedFile {
                    path,
                    old_path,
                    status,
                    additions,
                    deletions,
                },
            );
        }

        let untracked = self
            .output(&["ls-files", "--others", "--exclude-standard", "-z"])
            .await?;
        for path in split_nul(&untracked) {
            let additions = count_lines(&self.dir.join(&path));
            files.insert(
                path.clone(),
                ChangedFile {
                    path,
                    old_path: None,
                    status: FileChangeStatus::Untracked,
                    additions,
                    deletions: additions.map(|_| 0),
                },
            );
        }
        Ok(files.into_values().collect())
    }
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// `git diff --name-status -z`: `X\0path\0`, or `Rnn\0old\0new\0` for renames
/// and copies.
fn parse_name_status(bytes: &[u8]) -> Vec<(FileChangeStatus, Option<String>, String)> {
    let mut fields = split_nul(bytes).into_iter();
    let mut out = Vec::new();
    while let Some(code) = fields.next() {
        let status = match code.as_bytes().first() {
            Some(b'A') => FileChangeStatus::Added,
            Some(b'D') => FileChangeStatus::Deleted,
            Some(b'R') => FileChangeStatus::Renamed,
            Some(b'C') => FileChangeStatus::Copied,
            Some(b'T') => FileChangeStatus::TypeChanged,
            _ => FileChangeStatus::Modified,
        };
        let two_paths = matches!(status, FileChangeStatus::Renamed | FileChangeStatus::Copied);
        let first = fields.next();
        let (old, path) = if two_paths {
            (first, fields.next())
        } else {
            (None, first)
        };
        if let Some(path) = path {
            out.push((status, old, path));
        }
    }
    out
}

/// `git diff --numstat -z`: `add\tdel\tpath\0`, or `add\tdel\t\0old\0new\0`
/// for renames. Binary files report `-` for both counts. Keyed by new path.
fn parse_numstat(bytes: &[u8]) -> BTreeMap<String, (Option<u64>, Option<u64>)> {
    let mut fields = bytes.split(|&b| b == 0).map(String::from_utf8_lossy);
    let mut out = BTreeMap::new();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = if path.is_empty() {
            let _old = fields.next();
            match fields.next() {
                Some(new) => new.into_owned(),
                None => break,
            }
        } else {
            path.to_string()
        };
        out.insert(path, (add.parse().ok(), del.parse().ok()));
    }
    out
}

/// Line count of a text file, or `None` for binary, large or unreadable files.
fn count_lines(path: &Path) -> Option<u64> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_COUNTED_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    Some(newlines + u64::from(!bytes.is_empty() && !bytes.ends_with(b"\n")))
}

/// Concatenates patch pieces up to a byte limit.
struct Limited {
    buf: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl Limited {
    fn new(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
            truncated: false,
        }
    }

    fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.buf.len())
    }

    fn full(&self) -> bool {
        self.truncated || self.remaining() == 0
    }

    fn push(&mut self, bytes: &[u8], truncated: bool) {
        let room = self.remaining();
        self.buf.extend_from_slice(&bytes[..bytes.len().min(room)]);
        self.truncated |= truncated || bytes.len() > room;
    }

    fn finish(self) -> (String, bool) {
        (
            String::from_utf8_lossy(&self.buf).into_owned(),
            self.truncated,
        )
    }
}

/// The branch checked out in the working tree at `path`, read from its git
/// files without running git. `None` when it is not a git working tree or
/// its HEAD is detached.
pub fn current_branch(path: &Path) -> Option<String> {
    let dot_git = path.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // A linked worktree: `.git` is a file naming its git directory.
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let dir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if dir.is_absolute() {
            dir
        } else {
            path.join(dir)
        }
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .filter(|b| !b.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_branch_of_a_checkout_and_of_a_linked_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            main.join(".git/worktrees/wt/HEAD"),
            "ref: refs/heads/claude/fix-42\n",
        )
        .unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        assert_eq!(current_branch(&main).as_deref(), Some("main"));
        assert_eq!(current_branch(&wt).as_deref(), Some("claude/fix-42"));
        std::fs::write(main.join(".git/HEAD"), "0123abcd\n").unwrap();
        assert_eq!(current_branch(&main), None);
        assert_eq!(current_branch(dir.path()), None);
    }

    #[test]
    fn name_status_with_renames() {
        let raw = b"M\0src/a.rs\0R087\0old.rs\0new.rs\0A\0b c.txt\0D\0gone\0";
        let got = parse_name_status(raw);
        assert_eq!(
            got,
            [
                (FileChangeStatus::Modified, None, "src/a.rs".into()),
                (
                    FileChangeStatus::Renamed,
                    Some("old.rs".into()),
                    "new.rs".into()
                ),
                (FileChangeStatus::Added, None, "b c.txt".into()),
                (FileChangeStatus::Deleted, None, "gone".into()),
            ]
        );
    }

    #[test]
    fn numstat_with_renames_and_binaries() {
        let raw = b"3\t1\tsrc/a.rs\x002\t0\t\0old.rs\0new.rs\0-\t-\tlogo.png\0";
        let got = parse_numstat(raw);
        assert_eq!(got["src/a.rs"], (Some(3), Some(1)));
        assert_eq!(got["new.rs"], (Some(2), Some(0)));
        assert_eq!(got["logo.png"], (None, None));
        assert!(!got.contains_key("old.rs"));
    }

    #[test]
    fn limited_cuts_and_flags() {
        let mut l = Limited::new(5);
        l.push(b"abc", false);
        assert!(!l.full());
        l.push(b"defg", false);
        assert!(l.full());
        assert_eq!(l.finish(), ("abcde".into(), true));
    }
}

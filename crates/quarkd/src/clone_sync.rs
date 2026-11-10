//! Keeps the repo clones Quark works from on their remote's default branch.
//!
//! Every Project works from its clones at `<workspace>/repos/<name>`, seeded
//! from the command center's `<command>/repos/<name>`. Worktrees are cut
//! from origin after a fetch, so workers start current, but the clones'
//! own default branch stayed where it was cloned: the coordinator, which
//! reads the clones, and anyone looking in them saw main as it was weeks
//! ago. firstmate refreshed its clones after every merge and at session
//! start (`fm-fleet-sync.sh`); this is that, for the native engine.
//!
//! A sync fetches origin and moves the local default branch to
//! `origin/<default>`, only ever as a fast-forward:
//!
//! - On the default branch with a clean tree: `git merge --ff-only`.
//! - On another branch or a detached HEAD: the default branch's ref moves
//!   (compare-and-swap) when no worktree has it checked out. HEAD, the
//!   branch checked out and the files are left alone.
//! - Uncommitted changes on the default branch, or a default branch that
//!   diverged from origin's: left alone and logged as stuck.
//!
//! Nothing is ever checked out, forced, stashed, reset or deleted.
//!
//! It runs for a task's clone as soon as its pull request lands
//! ([`after_merge`]), and over every clone on a timer ([`CloneSync`]), which
//! also catches merges made while quarkd was down.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;

use crate::store::Store;

/// How often every clone is synced. The first sweep waits [`FIRST_RUN`].
pub const INTERVAL: Duration = Duration::from_secs(30 * 60);
const FIRST_RUN: Duration = Duration::from_secs(30);
/// One git command; a fetch that waits on a credential prompt gives up.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// What a sync did to one clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The default branch already matched origin's.
    Current,
    /// The default branch moved `from..to`. `checked_out` is false when
    /// only its ref moved, because HEAD was elsewhere.
    Synced {
        branch: String,
        from: String,
        to: String,
        checked_out: bool,
    },
    /// Left alone because it may hold work, with how far behind it is.
    Stuck { state: String, behind: Option<u64> },
    /// Not synced, harmlessly: not a clone, no origin, fetch failed.
    Skipped(String),
}

impl Outcome {
    fn log(&self, clone: &Path) {
        let clone = clone.display();
        match self {
            Outcome::Current => tracing::debug!(%clone, "clone already current"),
            Outcome::Synced {
                branch,
                from,
                to,
                checked_out,
            } => tracing::info!(%clone, %branch, %from, %to, checked_out, "clone synced"),
            Outcome::Stuck { state, behind } => {
                tracing::warn!(%clone, state = %state, behind = ?behind, "clone not synced: it may hold work")
            }
            Outcome::Skipped(reason) => tracing::info!(%clone, %reason, "clone not synced"),
        }
    }
}

/// Sync `clone` in the background, after a pull request from it landed.
pub fn after_merge(clone: PathBuf) {
    tokio::spawn(async move {
        sync_clone(&clone).await.log(&clone);
    });
}

/// Sync one clone (see the module docs).
pub async fn sync_clone(clone: &Path) -> Outcome {
    match sync(clone).await {
        Ok(o) => o,
        Err(reason) => Outcome::Skipped(reason),
    }
}

async fn sync(clone: &Path) -> Result<Outcome, String> {
    // Git looks upward for a repository, so a stray directory would act on
    // whatever repository holds it.
    let top = git(clone, &["rev-parse", "--show-toplevel"])
        .await
        .map_err(|_| "not a git checkout".to_string())?;
    let here = std::fs::canonicalize(clone).map_err(|e| e.to_string())?;
    if std::fs::canonicalize(&top).ok().as_deref() != Some(here.as_path()) {
        return Err(format!("not a clone root (git would act on {top})"));
    }
    if git(clone, &["remote", "get-url", "origin"]).await.is_err() {
        return Err("no origin remote".into());
    }
    git(clone, &["fetch", "--quiet", "--prune", "origin"])
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    let branch = default_branch(clone)
        .await
        .ok_or("cannot tell the default branch")?;
    let local_ref = format!("refs/heads/{branch}");
    let base_ref = format!("refs/remotes/origin/{branch}");
    let base = commit(clone, &base_ref)
        .await
        .ok_or_else(|| format!("origin/{branch} does not exist"))?;
    let Some(local) = commit(clone, &local_ref).await else {
        return Err(format!("no local {branch}"));
    };
    if local == base {
        return Ok(Outcome::Current);
    }
    if !is_ancestor(clone, &local, &base).await {
        return Ok(stuck(
            clone,
            format!("{branch} diverged from origin/{branch}"),
            &local_ref,
            &base_ref,
        )
        .await);
    }
    let head = git(clone, &["symbolic-ref", "--quiet", "HEAD"]).await.ok();
    let checked_out = head.as_deref() == Some(local_ref.as_str());
    if checked_out {
        // Untracked files need no check: a fast-forward that would
        // overwrite one refuses.
        let status = git(clone, &["status", "--porcelain", "--untracked-files=no"])
            .await
            .map_err(|e| format!("cannot read status: {e}"))?;
        if !status.is_empty() {
            return Ok(stuck(
                clone,
                format!("{branch} with uncommitted changes"),
                &local_ref,
                &base_ref,
            )
            .await);
        }
        git(clone, &["merge", "--quiet", "--ff-only", &base_ref])
            .await
            .map_err(|e| format!("fast-forward failed: {e}"))?;
    } else {
        if checked_out_in_worktree(clone, &local_ref).await {
            return Err(format!("{branch} is checked out in another worktree"));
        }
        // Compare-and-swap: refuses if the branch moved since it was read.
        git(
            clone,
            &[
                "update-ref",
                "-m",
                "quark: fast-forward to origin",
                &local_ref,
                &base,
                &local,
            ],
        )
        .await
        .map_err(|e| format!("fast-forward failed: {e}"))?;
    }
    Ok(Outcome::Synced {
        branch,
        from: short(&local),
        to: short(&base),
        checked_out,
    })
}

/// Left alone, with how far `local_ref` is behind `base_ref`.
async fn stuck(clone: &Path, state: String, local_ref: &str, base_ref: &str) -> Outcome {
    let range = format!("{local_ref}..{base_ref}");
    let behind = git(clone, &["rev-list", "--count", &range])
        .await
        .ok()
        .and_then(|n| n.parse().ok());
    Outcome::Stuck { state, behind }
}

/// origin's HEAD, else `main` or `master` when the clone has one.
async fn default_branch(clone: &Path) -> Option<String> {
    if let Ok(r) = git(
        clone,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .await
    {
        if let Some(b) = r.strip_prefix("refs/remotes/origin/") {
            return Some(b.to_string());
        }
    }
    for b in ["main", "master"] {
        if commit(clone, &format!("refs/heads/{b}")).await.is_some() {
            return Some(b.to_string());
        }
    }
    None
}

async fn commit(clone: &Path, r: &str) -> Option<String> {
    git(
        clone,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{r}^{{commit}}"),
        ],
    )
    .await
    .ok()
}

async fn is_ancestor(clone: &Path, a: &str, b: &str) -> bool {
    git(clone, &["merge-base", "--is-ancestor", a, b])
        .await
        .is_ok()
}

async fn checked_out_in_worktree(clone: &Path, local_ref: &str) -> bool {
    let line = format!("branch {local_ref}");
    git(clone, &["worktree", "list", "--porcelain"])
        .await
        .map(|out| out.lines().any(|l| l == line))
        // Unknown: leave the branch alone.
        .unwrap_or(true)
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// `git -C dir args`, its trimmed stdout, or its first stderr line.
async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => {
            return Err(format!(
                "git {} took over {}s",
                args[0],
                GIT_TIMEOUT.as_secs()
            ))
        }
    };
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().next().unwrap_or("failed").trim().to_string())
    }
}

/// Syncs every Project's clones and the command center's on a timer.
pub struct CloneSync {
    store: Arc<Store>,
    command: PathBuf,
}

impl CloneSync {
    pub fn new(store: Arc<Store>, command: PathBuf) -> Self {
        Self { store, command }
    }

    pub async fn run(self, every: Duration) {
        tokio::time::sleep(FIRST_RUN).await;
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            self.sweep().await;
        }
    }

    /// One pass over every clone, one at a time.
    pub async fn sweep(&self) {
        let store = self.store.clone();
        let command = self.command.clone();
        let clones = tokio::task::spawn_blocking(move || clones(&store, &command))
            .await
            .unwrap_or_default();
        for clone in clones {
            sync_clone(&clone).await.log(&clone);
        }
    }
}

/// The command center's clones and every Project's, each once.
fn clones(store: &Store, command: &Path) -> Vec<PathBuf> {
    let mut out = crate::host_sources::clones(command);
    for p in store.list_projects().unwrap_or_default() {
        if let Some(root) = p.workspace_path {
            out.extend(crate::host_sources::clones(Path::new(&root)));
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit_file(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), name).unwrap();
        run(dir, &["add", name]);
        run(dir, &["commit", "-q", "-m", name]);
    }

    /// An origin, a clone of it, and a second clone to land work from.
    struct Repos {
        _dir: tempfile::TempDir,
        clone: PathBuf,
        other: PathBuf,
    }

    impl Repos {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let origin = dir.path().join("origin.git");
            run(
                dir.path(),
                &["init", "-q", "--bare", "-b", "main", "origin.git"],
            );
            let seed = dir.path().join("seed");
            run(
                dir.path(),
                &["clone", "-q", origin.to_str().unwrap(), "seed"],
            );
            run(&seed, &["checkout", "-q", "-b", "main"]);
            commit_file(&seed, "a");
            run(&seed, &["push", "-q", "origin", "main"]);
            let clone = dir.path().join("repos/app");
            std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
            run(
                dir.path(),
                &[
                    "clone",
                    "-q",
                    origin.to_str().unwrap(),
                    clone.to_str().unwrap(),
                ],
            );
            Self {
                _dir: dir,
                clone,
                other: seed,
            }
        }

        /// A pull request merges on the forge.
        fn land(&self, name: &str) -> String {
            commit_file(&self.other, name);
            run(&self.other, &["push", "-q", "origin", "main"]);
            run(&self.other, &["rev-parse", "HEAD"])
        }

        fn rev(&self, r: &str) -> String {
            run(&self.clone, &["rev-parse", r])
        }
    }

    #[tokio::test]
    async fn fast_forwards_a_clean_default_branch() {
        let r = Repos::new();
        let landed = r.land("b");
        let out = sync_clone(&r.clone).await;
        assert!(
            matches!(
                out,
                Outcome::Synced {
                    checked_out: true,
                    ..
                }
            ),
            "{out:?}"
        );
        assert_eq!(r.rev("main"), landed);
        assert!(r.clone.join("b").exists());
        assert_eq!(sync_clone(&r.clone).await, Outcome::Current);
    }

    #[tokio::test]
    async fn uncommitted_changes_are_left_alone() {
        let r = Repos::new();
        let before = r.rev("main");
        r.land("b");
        std::fs::write(r.clone.join("a"), "edited").unwrap();
        let out = sync_clone(&r.clone).await;
        assert!(
            matches!(
                out,
                Outcome::Stuck {
                    behind: Some(1),
                    ..
                }
            ),
            "{out:?}"
        );
        assert_eq!(r.rev("main"), before);
        assert_eq!(
            std::fs::read_to_string(r.clone.join("a")).unwrap(),
            "edited"
        );
    }

    #[tokio::test]
    async fn untracked_files_do_not_block_a_sync() {
        let r = Repos::new();
        let landed = r.land("b");
        std::fs::write(r.clone.join("notes"), "mine").unwrap();
        sync_clone(&r.clone).await;
        assert_eq!(r.rev("main"), landed);
        assert!(r.clone.join("notes").exists());
    }

    #[tokio::test]
    async fn another_branch_stays_checked_out() {
        let r = Repos::new();
        run(&r.clone, &["checkout", "-q", "-b", "work"]);
        commit_file(&r.clone, "w");
        std::fs::write(r.clone.join("w"), "dirty").unwrap();
        let work = r.rev("work");
        let landed = r.land("b");
        let out = sync_clone(&r.clone).await;
        assert!(
            matches!(
                out,
                Outcome::Synced {
                    checked_out: false,
                    ..
                }
            ),
            "{out:?}"
        );
        assert_eq!(r.rev("main"), landed);
        assert_eq!(run(&r.clone, &["symbolic-ref", "--short", "HEAD"]), "work");
        assert_eq!(r.rev("HEAD"), work);
        assert_eq!(std::fs::read_to_string(r.clone.join("w")).unwrap(), "dirty");
    }

    #[tokio::test]
    async fn a_default_branch_in_another_worktree_is_left_alone() {
        let r = Repos::new();
        run(&r.clone, &["checkout", "-q", "--detach"]);
        let wt = r.clone.parent().unwrap().join("wt");
        run(
            &r.clone,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "main"],
        );
        let before = r.rev("main");
        r.land("b");
        let out = sync_clone(&r.clone).await;
        assert!(matches!(out, Outcome::Skipped(_)), "{out:?}");
        assert_eq!(r.rev("main"), before);
    }

    #[tokio::test]
    async fn a_diverged_default_branch_is_left_alone() {
        let r = Repos::new();
        commit_file(&r.clone, "local");
        let mine = r.rev("main");
        r.land("b");
        let out = sync_clone(&r.clone).await;
        assert!(matches!(out, Outcome::Stuck { .. }), "{out:?}");
        assert_eq!(r.rev("main"), mine);
    }

    #[tokio::test]
    async fn only_clone_roots_and_clones_with_origin() {
        let r = Repos::new();
        let nested = r.clone.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(matches!(sync_clone(&nested).await, Outcome::Skipped(_)));
        run(&r.clone, &["remote", "remove", "origin"]);
        assert_eq!(
            sync_clone(&r.clone).await,
            Outcome::Skipped("no origin remote".into())
        );
        let plain = tempfile::tempdir().unwrap();
        assert!(matches!(
            sync_clone(plain.path()).await,
            Outcome::Skipped(_)
        ));
    }
}

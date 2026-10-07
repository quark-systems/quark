//! The safety checks every handout and return goes through, independent of
//! which pool backs the provider.
//!
//! - [`assert_primary`]: the repo a worktree is cut from is a primary
//!   checkout, not a linked worktree.
//! - [`assert_isolated`]: what the pool handed out is a linked worktree root
//!   of that repo, not the primary checkout, and not tangled (its git dir
//!   points back at itself, not at another worktree).
//! - [`primary_tangle`]: the primary checkout sits on a feature branch,
//!   which is how a worker that branched in the wrong place shows up.
//! - [`landed_work`]: uncommitted changes and commits that are on no remote
//!   and not on the default branch, checked before a worktree is returned.

use std::path::{Path, PathBuf};

use quark_core::worktree::LandedWork;
use quark_core::{CoreError, Result};

use crate::git;

/// Where a checkout's working tree and git directories are, all canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// Root of the working tree.
    pub top: PathBuf,
    /// This checkout's own git dir (`.git` for a primary checkout,
    /// `.git/worktrees/<name>` for a linked worktree).
    pub git_dir: PathBuf,
    /// The repository's shared git dir.
    pub common_dir: PathBuf,
}

impl Checkout {
    /// A primary checkout's git dir is the repository's common dir.
    pub fn is_primary(&self) -> bool {
        self.git_dir == self.common_dir
    }
}

async fn canonical(path: &Path) -> Result<PathBuf> {
    tokio::fs::canonicalize(path).await.map_err(|e| {
        CoreError::Refused(format!(
            "{} is not a readable directory: {e}",
            path.display()
        ))
    })
}

/// Read the checkout `path` is in.
pub async fn checkout(path: &Path) -> Result<Checkout> {
    let dir = canonical(path).await?;
    let out = git::run(
        &dir,
        [
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
    )
    .await?;
    let lines: Vec<&str> = out.stdout.lines().collect();
    if !out.ok || lines.len() != 3 {
        return Err(CoreError::Refused(format!(
            "{} is not inside a git working tree",
            path.display()
        )));
    }
    Ok(Checkout {
        top: canonical(Path::new(lines[0])).await?,
        git_dir: canonical(Path::new(lines[1])).await?,
        common_dir: canonical(Path::new(lines[2])).await?,
    })
}

/// `repo` must be the root of a primary checkout: worktrees are cut from it
/// and it is never handed out itself.
pub async fn assert_primary(repo: &Path) -> Result<Checkout> {
    let dir = canonical(repo).await?;
    let c = checkout(&dir).await?;
    if c.top != dir {
        return Err(CoreError::Refused(format!(
            "{} is a subdirectory of {}, not a repository root",
            repo.display(),
            c.top.display()
        )));
    }
    if !c.is_primary() {
        return Err(CoreError::Refused(format!(
            "{} is a linked worktree, not the repository's primary checkout",
            repo.display()
        )));
    }
    Ok(c)
}

/// `path` must be the root of a linked worktree of `primary`'s repository,
/// distinct from the primary checkout, whose git dir belongs to it alone.
pub async fn assert_isolated(path: &Path, primary: &Checkout) -> Result<Checkout> {
    let dir = canonical(path).await?;
    let c = checkout(&dir).await?;
    if c.top != dir {
        return Err(CoreError::Refused(format!(
            "{} is a subdirectory of worktree {}, not a worktree root",
            path.display(),
            c.top.display()
        )));
    }
    if c.top == primary.top || c.git_dir == primary.common_dir {
        return Err(CoreError::Refused(format!(
            "{} is the repository's primary checkout",
            path.display()
        )));
    }
    if c.common_dir != primary.common_dir {
        return Err(CoreError::Refused(format!(
            "{} belongs to another repository ({}), not {}",
            path.display(),
            c.common_dir.display(),
            primary.top.display()
        )));
    }
    // A linked worktree's git dir is <common>/worktrees/<name>, and its
    // `gitdir` file names this worktree's own `.git`. Anything else means two
    // checkouts share one git dir: work in one would move the other.
    let owner = tokio::fs::read_to_string(c.git_dir.join("gitdir"))
        .await
        .ok()
        .map(|s| PathBuf::from(s.trim()));
    let owner_top = match owner.as_deref().and_then(Path::parent) {
        Some(p) => canonical(p).await.ok(),
        None => None,
    };
    if c.git_dir.parent() != Some(primary.common_dir.join("worktrees").as_path())
        || owner_top.as_deref() != Some(c.top.as_path())
    {
        return Err(CoreError::Refused(format!(
            "{} is tangled: its git dir {} belongs to another worktree",
            path.display(),
            c.git_dir.display()
        )));
    }
    Ok(c)
}

/// The repository's default branch: `origin/HEAD`, else a local `main` or
/// `master`.
pub async fn default_branch(path: &Path) -> Result<Option<String>> {
    if let Some(r) = git::try_read(
        path,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .await?
    {
        if let Some(name) = r.strip_prefix("origin/") {
            return Ok(Some(name.to_string()));
        }
    }
    for name in ["main", "master"] {
        let r = format!("refs/heads/{name}");
        if git::run(path, ["show-ref", "--verify", "--quiet", &r])
            .await?
            .ok
        {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// The feature branch a primary checkout is stranded on, if any.
///
/// `None` for every healthy state: the default branch, a detached HEAD, or
/// no known default. A named non-default branch in the primary checkout is
/// how a worker that branched outside its worktree shows up; the commits are
/// safe on that branch, but the checkout needs restoring.
pub async fn primary_tangle(repo: &Path) -> Result<Option<String>> {
    let c = assert_primary(repo).await?;
    let Some(current) =
        git::try_read(&c.top, ["symbolic-ref", "--quiet", "--short", "HEAD"]).await?
    else {
        return Ok(None);
    };
    let Some(default) = default_branch(&c.top).await? else {
        return Ok(None);
    };
    Ok((current != default).then_some(current))
}

/// What would be lost if the worktree at `path` were cleaned.
///
/// Uncommitted means any tracked change or untracked file (ignored files do
/// not count). Unpushed commits are those reachable from `HEAD` but from no
/// remote-tracking branch and not from the default branch; when the
/// branch's content is already in the default branch (a squash merge), they
/// count as landed.
pub async fn landed_work(path: &Path) -> Result<LandedWork> {
    let status = git::read(
        path,
        [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )
    .await?;
    let uncommitted = !status.is_empty();

    if git::try_read(path, ["rev-parse", "--verify", "--quiet", "HEAD"])
        .await?
        .is_none()
    {
        return Ok(LandedWork {
            uncommitted,
            unpushed_commits: 0,
        });
    }

    let default_ref = match default_branch(path).await? {
        Some(name) => {
            let mut found = None;
            for r in [
                format!("refs/remotes/origin/{name}"),
                format!("refs/heads/{name}"),
            ] {
                if git::run(path, ["show-ref", "--verify", "--quiet", &r])
                    .await?
                    .ok
                {
                    found = Some(r);
                    break;
                }
            }
            found
        }
        None => None,
    };

    let mut args = vec![
        "rev-list".to_string(),
        "--count".into(),
        "HEAD".into(),
        "--not".into(),
        "--remotes".into(),
    ];
    args.extend(default_ref.clone());
    let count: u32 = git::read(path, &args)
        .await?
        .parse()
        .map_err(|e| CoreError::Backend(format!("git rev-list --count: {e}")))?;

    let unpushed_commits = match (&default_ref, count) {
        (Some(r), n) if n > 0 && content_in(path, r).await? => 0,
        (_, n) => n,
    };
    Ok(LandedWork {
        uncommitted,
        unpushed_commits,
    })
}

/// Merging `HEAD` into `default_ref` changes nothing: every change on the
/// branch is already there. Inconclusive (a conflict, an old git) is `false`.
async fn content_in(path: &Path, default_ref: &str) -> Result<bool> {
    let tree = format!("{default_ref}^{{tree}}");
    let Some(default_tree) =
        git::try_read(path, ["rev-parse", "--verify", "--quiet", &tree]).await?
    else {
        return Ok(false);
    };
    let Some(merged) =
        git::try_read(path, ["merge-tree", "--write-tree", default_ref, "HEAD"]).await?
    else {
        return Ok(false);
    };
    Ok(merged.lines().next() == Some(default_tree.as_str()))
}

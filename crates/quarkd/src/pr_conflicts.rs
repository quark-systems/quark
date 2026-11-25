//! Which files of a pull request conflict with its base branch.
//!
//! Forges say only whether a pull request merges cleanly, not which files
//! stop it, so the answer is worked out in the Project's local clone of the
//! repo: fetch the base branch and the pull request's head from origin, then
//! a trial merge with `git merge-tree --write-tree` (git 2.38 or later),
//! which touches no branch, index or working tree. The fetch updates
//! `origin/<base>` as any fetch does; the head is fetched without a ref.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use quark_systems::{Project, PullRequest};
use tokio::process::Command;

use crate::engine::native_dispatch_engine::linked_checkout;
use crate::engine::native_project::REPOS_DIR;
use crate::project_repo::github_repo;
use crate::provision::name_from_url;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// More conflicting paths than this are cut; the tree marks what it can.
const MAX_FILES: usize = 5000;

/// The local clone of the pull request's repo in its Project's workspace.
pub fn clone_for(project: &Project, pr: &PullRequest) -> Option<PathBuf> {
    let root = PathBuf::from(project.workspace_path.as_ref()?);
    project.repos.iter().find_map(|r| {
        if github_repo(&r.url)?.eq_ignore_ascii_case(&pr.repo) {
            let name = r.name.clone().or_else(|| name_from_url(&r.url))?;
            let p = linked_checkout(root.join(REPOS_DIR).join(name));
            p.join(".git").exists().then_some(p)
        } else {
            None
        }
    })
}

/// The paths a merge of `head_sha` into `base` would leave conflicted,
/// fetching both from origin into `clone` first (`pull` is the pull
/// request's number, for GitHub's `refs/pull/<n>/head`).
pub async fn conflicting_files(
    clone: &Path,
    base: &str,
    head_sha: &str,
    pull: u64,
) -> Result<Vec<String>, String> {
    if base.starts_with('-') || base.contains("..") || head_sha.starts_with('-') {
        return Err(format!("refusing an odd ref: {base} / {head_sha}"));
    }
    git(
        clone,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "origin",
            &format!("+refs/heads/{base}:refs/remotes/origin/{base}"),
            &format!("refs/pull/{pull}/head"),
        ],
        &[0],
    )
    .await?;
    let (code, out) = git(
        clone,
        &[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            &format!("refs/remotes/origin/{base}"),
            head_sha,
        ],
        &[0, 1],
    )
    .await?;
    Ok(if code == 0 {
        Vec::new()
    } else {
        parse_merge_tree(&out)
    })
}

/// `git merge-tree --write-tree --name-only --no-messages -z` output: the
/// tree id, then each conflicted path once, NUL-separated.
pub fn parse_merge_tree(out: &[u8]) -> Vec<String> {
    let mut files: Vec<String> = out
        .split(|b| *b == 0)
        .skip(1)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    files.dedup();
    files.truncate(MAX_FILES);
    files
}

async fn git(dir: &Path, args: &[&str], ok: &[i32]) -> Result<(i32, Vec<u8>), String> {
    let run = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(GIT_TIMEOUT, run)
        .await
        .map_err(|_| format!("git {} took over {}s", args[0], GIT_TIMEOUT.as_secs()))?
        .map_err(|e| format!("git {}: {e}", args[0]))?;
    match out.status.code() {
        Some(c) if ok.contains(&c) => Ok((c, out.stdout)),
        code => {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            Err(match code {
                Some(c) => format!("git {} failed (exit {c}): {err}", args[0]),
                None => format!("git {} was killed: {err}", args[0]),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tree_then_paths() {
        assert_eq!(
            parse_merge_tree(b"49b1b46\0sp ace.txt\0x.txt\0"),
            vec!["sp ace.txt".to_string(), "x.txt".into()]
        );
        assert!(parse_merge_tree(b"49b1b46\0").is_empty());
    }

    fn sh(dir: &Path, cmd: &str) {
        let ok = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "{cmd}");
    }

    /// An origin with a pull request ref whose head conflicts with main in
    /// one file, and a clone of it on main.
    #[tokio::test]
    async fn lists_the_files_a_merge_would_conflict_in() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        sh(
            &origin,
            "git init -q -b main && printf 'a\\n' > x.txt && printf 'k\\n' > y.txt && git add . && git commit -qm base \
             && git checkout -qb feature && printf 'b\\n' > x.txt && printf 'n\\n' > y.txt && git commit -qam feature \
             && git update-ref refs/pull/7/head HEAD && git checkout -q main \
             && printf 'c\\n' > x.txt && git commit -qam main",
        );
        sh(dir.path(), "git clone -q origin clone");
        let clone = dir.path().join("clone");
        // main moves on origin after the clone was made; the fetch picks it up.
        sh(
            &origin,
            "printf 'k2\\n' > y.txt && git commit -qam 'main again'",
        );
        let head = String::from_utf8(
            std::process::Command::new("git")
                .args([
                    "-C",
                    origin.to_str().unwrap(),
                    "rev-parse",
                    "refs/pull/7/head",
                ])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let files = conflicting_files(&clone, "main", head.trim(), 7)
            .await
            .unwrap();
        assert_eq!(files, vec!["x.txt".to_string(), "y.txt".into()]);
        // Nothing in the clone moved but origin/main.
        sh(
            &clone,
            "test \"$(git rev-parse --abbrev-ref HEAD)\" = main && git diff --quiet",
        );

        assert!(conflicting_files(&clone, "--upload-pack=x", head.trim(), 7)
            .await
            .is_err());
    }
}

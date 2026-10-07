//! Blocking git calls for the native pool, each a port of the treehouse
//! 3.1.2 `gitvcs` function of the same name (kept close so the two can be
//! compared line by line).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use quark_core::{CoreError, Result};

/// Output of one git run.
pub(crate) struct Out {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

/// `git <args>` in `dir`, never prompting.
pub(crate) fn exec(dir: &Path, args: &[&str]) -> Result<Out> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| CoreError::Backend(format!("running git: {e}")))?;
    Ok(Out {
        code: out.status.code(),
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// Trimmed stdout of a git command that must succeed.
pub(crate) fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = exec(dir, args)?;
    if !out.ok() {
        return Err(failed(args, &out));
    }
    Ok(out.text())
}

fn failed(args: &[&str], out: &Out) -> CoreError {
    CoreError::Backend(format!("git {}: {}", args.join(" "), out.stderr))
}

/// Whether a git command exits 0.
fn succeeds(dir: &Path, args: &[&str]) -> bool {
    exec(dir, args).is_ok_and(|o| o.ok())
}

pub(crate) fn has_remote(repo: &Path, name: &str) -> bool {
    run(repo, &["remote"]).is_ok_and(|out| out.lines().any(|l| l.trim() == name))
}

pub(crate) fn remote_url(repo: &Path) -> Result<String> {
    run(repo, &["remote", "get-url", "origin"])
}

pub(crate) fn fetch(repo: &Path) -> Result<()> {
    if has_remote(repo, "origin") {
        run(repo, &["fetch", "origin"])?;
    }
    Ok(())
}

/// The shared git dir of the repository `dir` is in, absolute.
pub(crate) fn common_git_dir(dir: &Path) -> Result<PathBuf> {
    let out = run(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .or_else(|_| run(dir, &["rev-parse", "--git-common-dir"]))?;
    let p = PathBuf::from(out);
    Ok(if p.is_absolute() { p } else { dir.join(p) })
}

/// The branch worktrees are cut from when none is asked for: origin's HEAD,
/// else the primary checkout's branch, else `init.defaultBranch`.
pub(crate) fn default_branch(repo: &Path) -> Result<String> {
    let main = main_repo_root(repo);
    if has_remote(&main, "origin") {
        if let Ok(out) = run(&main, &["symbolic-ref", "refs/remotes/origin/HEAD"]) {
            if let Some(b) = out.strip_prefix("refs/remotes/origin/") {
                if !b.is_empty() {
                    return Ok(b.to_string());
                }
            }
        }
    }
    if let Ok(out) = run(&main, &["symbolic-ref", "HEAD"]) {
        if let Some(b) = out.strip_prefix("refs/heads/") {
            if !b.is_empty() {
                return Ok(b.to_string());
            }
        }
    }
    if let Ok(out) = run(&main, &["config", "init.defaultBranch"]) {
        if !out.is_empty() {
            return Ok(out);
        }
    }
    Err(CoreError::Backend(
        "cannot determine default branch: try running 'git fetch' or ensure you are on a branch"
            .into(),
    ))
}

/// The default branch of the repository a worktree belongs to.
pub(crate) fn default_branch_for_worktree(wt: &Path) -> Result<String> {
    let top = run(wt, &["rev-parse", "--show-toplevel"])?;
    default_branch(Path::new(&top))
}

fn main_repo_root(repo: &Path) -> PathBuf {
    let common = run(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .or_else(|_| run(repo, &["rev-parse", "--git-common-dir"]));
    if let Ok(dir) = common {
        let dir = PathBuf::from(dir);
        if dir.file_name().is_some_and(|n| n == ".git") {
            if let Some(parent) = dir.parent() {
                return parent.to_path_buf();
            }
        }
    }
    repo.to_path_buf()
}

fn ref_exists(repo: &Path, r: &str) -> bool {
    succeeds(repo, &["rev-parse", "--verify", r])
}

fn exact_ref_exists(repo: &Path, r: &str) -> bool {
    succeeds(repo, &["show-ref", "--verify", "--quiet", r])
}

/// `refs/heads/<b>` or `refs/remotes/origin/<b>` exists, looked up exactly.
pub(crate) fn branch_exists(repo: &Path, branch: &str) -> bool {
    if branch.is_empty() || branch == "HEAD" {
        return false;
    }
    exact_ref_exists(repo, &format!("refs/heads/{branch}"))
        || exact_ref_exists(repo, &format!("refs/remotes/origin/{branch}"))
}

pub(crate) fn verify_base_branch(repo: &Path, branch: &str) -> Result<()> {
    if branch.is_empty() || branch_exists(repo, branch) {
        return Ok(());
    }
    Err(CoreError::Invalid(format!(
        "base branch {branch:?} does not exist: no local branch {branch} and no remote-tracking branch origin/{branch} (fetch first, or fix base_branch/--base)"
    )))
}

fn is_ancestor(repo: &Path, a: &str, b: &str) -> bool {
    succeeds(repo, &["merge-base", "--is-ancestor", a, b])
}

/// Whichever of the local and origin branch is further ahead, preferring
/// origin when they diverged; fully qualified so a tag never wins.
pub(crate) fn branch_ref(repo: &Path, branch: &str) -> String {
    let local = format!("refs/heads/{branch}");
    let remote = format!("refs/remotes/origin/{branch}");
    let has_local = ref_exists(repo, &local);
    let has_remote = ref_exists(repo, &remote);
    match (has_local, has_remote) {
        (true, true) => {
            if is_ancestor(repo, &local, &remote) {
                remote
            } else if is_ancestor(repo, &remote, &local) {
                local
            } else {
                remote
            }
        }
        (true, false) => local,
        _ => remote,
    }
}

pub(crate) fn add_worktree(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    let r = branch_ref(repo, branch);
    let p = path.to_string_lossy();
    run(repo, &["worktree", "add", "--detach", &p, &r]).map(|_| ())
}

pub(crate) fn prune_worktrees(repo: &Path) -> Result<()> {
    run(repo, &["worktree", "prune"]).map(|_| ())
}

pub(crate) fn branch_commit(repo: &Path, branch: &str) -> Result<String> {
    let r = format!("{}^{{commit}}", branch_ref(repo, branch));
    run(repo, &["rev-parse", "--verify", &r])
}

pub(crate) fn head(wt: &Path) -> Result<String> {
    run(wt, &["rev-parse", "--verify", "HEAD^{commit}"])
}

/// The checked-out branch; `None` for a detached HEAD.
pub(crate) fn checked_out_branch(wt: &Path) -> Result<Option<String>> {
    let out = exec(wt, &["symbolic-ref", "-q", "--short", "HEAD"])?;
    match out.code {
        Some(0) => Ok(Some(out.text())),
        Some(1) => Ok(None),
        _ => Err(CoreError::Backend(format!(
            "git symbolic-ref -q --short HEAD: {}",
            out.stderr
        ))),
    }
}

pub(crate) fn worktree_at_commit(wt: &Path, commit: &str) -> Result<bool> {
    let h = head(wt)?;
    Ok(checked_out_branch(wt)?.is_none() && h == commit)
}

pub(crate) fn validate_branch_name(repo: &Path, branch: &str) -> Result<()> {
    let name = run(repo, &["check-ref-format", "--branch", branch])
        .map_err(|e| CoreError::Invalid(format!("invalid branch {branch:?}: {e}")))?;
    if name != branch {
        return Err(CoreError::Invalid(format!(
            "invalid branch {branch:?}: branch name {branch:?} expands to {name:?}"
        )));
    }
    Ok(())
}

pub(crate) fn local_branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    let r = format!("refs/heads/{branch}");
    let out = exec(repo, &["show-ref", "--verify", "--quiet", &r])?;
    match out.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(failed(&["show-ref", "--verify", "--quiet", &r], &out)),
    }
}

/// Why creating a branch failed.
#[derive(Debug)]
pub(crate) struct BranchFailure {
    /// The branch ref was created before the checkout failed; the worktree
    /// may hold hook output and must be inspected, not removed.
    pub created: bool,
    pub error: CoreError,
}

/// Create `branch` at HEAD and check it out, verifying both afterwards.
pub(crate) fn create_branch(wt: &Path, branch: &str) -> Result<(), BranchFailure> {
    let early = |error| BranchFailure {
        created: false,
        error,
    };
    let expected = head(wt).map_err(early)?;
    run(wt, &["branch", "--", branch, &expected]).map_err(early)?;
    let checkout = exec(wt, &["checkout", branch]);
    let checked_out = checked_out_branch(wt);
    let now = head(wt);
    match (checked_out, now) {
        (Ok(Some(b)), Ok(h)) if b == branch && h == expected => Ok(()),
        _ => {
            let detail = match checkout {
                Ok(o) if o.ok() => format!(
                    "checkout of branch {branch:?} did not leave HEAD on that branch at commit {expected}"
                ),
                Ok(o) => format!("git checkout {branch}: {}", o.stderr),
                Err(e) => e.to_string(),
            };
            Err(BranchFailure {
                created: true,
                error: CoreError::Backend(format!(
                    "branch created before checkout failed: {detail} (branch {branch:?} was created and left in place for manual inspection/removal)"
                )),
            })
        }
    }
}

pub(crate) fn detach(wt: &Path) -> Result<()> {
    run(wt, &["checkout", "--detach"]).map(|_| ())
}

/// Tracked or untracked changes, whatever `status.showUntrackedFiles` says.
pub(crate) fn is_dirty(wt: &Path) -> Result<bool> {
    let out = run(wt, &["status", "--porcelain", "--untracked-files=all"])?;
    Ok(!out.is_empty())
}

fn ref_commit(dir: &Path, r: &str) -> Result<String> {
    let r = format!("{r}^{{commit}}");
    run(dir, &["rev-parse", "--verify", &r])
}

fn resolve_reset_ref(wt: &Path, branch: &str) -> Result<String> {
    let top = run(wt, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|_| wt.to_path_buf());
    ref_commit(wt, &branch_ref(&top, branch))
}

/// Whether `wt` can be reset to `branch` without losing committed work,
/// with the commit to reset to and the HEAD the check saw.
pub(crate) fn is_safe_to_reset(wt: &Path, branch: &str) -> Result<(bool, String, String)> {
    let target = resolve_reset_ref(wt, branch)?;
    let h = head(wt)?;
    let safe = is_head_merged_into_ref(wt, &target)?;
    Ok((safe, target, h))
}

/// HEAD is an ancestor of `r`, or (a squash merge) every path HEAD changed
/// since the merge base has the same content in `r`.
pub(crate) fn is_head_merged_into_ref(wt: &Path, r: &str) -> Result<bool> {
    let out = exec(wt, &["merge-base", "--is-ancestor", "HEAD", r])?;
    match out.code {
        Some(0) => Ok(true),
        Some(1) => head_content_merged(wt, r),
        _ => Err(CoreError::Backend(format!(
            "git merge-base --is-ancestor HEAD {r}: {}",
            out.stderr
        ))),
    }
}

fn head_content_merged(wt: &Path, r: &str) -> Result<bool> {
    let out = exec(wt, &["merge-base", "HEAD", r])?;
    let base = out.text();
    if !out.ok() || base.is_empty() {
        return Err(CoreError::Backend(format!(
            "git merge-base HEAD {r} returned no common ancestor"
        )));
    }
    let base_tree = read_tree(wt, &base)?;
    let head_tree = read_tree(wt, "HEAD")?;
    let target = read_tree(wt, r)?;
    let mut delta = false;
    for (path, entry) in &base_tree {
        if head_tree.get(path) == Some(entry) {
            continue;
        }
        delta = true;
        if head_tree.get(path) != target.get(path) {
            return Ok(false);
        }
    }
    for (path, entry) in &head_tree {
        if base_tree.contains_key(path) {
            continue;
        }
        delta = true;
        if Some(entry) != target.get(path) {
            return Ok(false);
        }
    }
    Ok(delta)
}

fn read_tree(dir: &Path, r: &str) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
    let out = exec(dir, &["ls-tree", "-r", "-z", "--full-tree", r])?;
    if !out.ok() {
        return Err(failed(&["ls-tree", "-r", "-z", "--full-tree", r], &out));
    }
    let mut tree = HashMap::new();
    for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record.iter().position(|b| *b == b'\t').ok_or_else(|| {
            CoreError::Backend(format!("git ls-tree {r} returned malformed tree entry"))
        })?;
        let path = record[tab + 1..].to_vec();
        if tree.insert(path, record[..tab].to_vec()).is_some() {
            return Err(CoreError::Backend(format!(
                "git ls-tree {r} returned a duplicate path"
            )));
        }
    }
    Ok(tree)
}

fn git_path(wt: &Path, name: &str) -> Result<PathBuf> {
    if let Ok(out) = run(
        wt,
        &["rev-parse", "--path-format=absolute", "--git-path", name],
    ) {
        return Ok(PathBuf::from(out));
    }
    let dir = run(wt, &["rev-parse", "--absolute-git-dir"])?;
    let rel = PathBuf::from(run(wt, &["rev-parse", "--git-path", name])?);
    Ok(if rel.is_absolute() {
        rel
    } else {
        Path::new(&dir).join(rel)
    })
}

fn is_commit_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reset `wt` to `branch` (resolved once) from whatever HEAD it has now.
pub(crate) fn reset_worktree(wt: &Path, branch: &str) -> Result<()> {
    let target = resolve_reset_ref(wt, branch)?;
    let h = head(wt)?;
    reset_to_ref(wt, &target, &h, false)
}

/// Reset `wt` to the commit `target` while holding git's own `HEAD.lock`,
/// refusing when HEAD is no longer `expected_head` (or, with
/// `require_clean`, when the tree became dirty since the caller checked).
pub(crate) fn reset_to_ref(
    wt: &Path,
    target: &str,
    expected_head: &str,
    require_clean: bool,
) -> Result<()> {
    if !is_commit_id(expected_head) || !is_commit_id(target) {
        return Err(CoreError::Backend(
            "worktree reset requires resolved commit IDs".into(),
        ));
    }
    let head_path = git_path(wt, "HEAD")?;
    let lock_path = PathBuf::from(format!("{}.lock", head_path.display()));
    let mut lock = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|e| CoreError::Backend(format!("cannot lock worktree HEAD: {e}")))?;
    let result = (|| {
        let h = head(wt)?;
        if h != expected_head {
            return Err(CoreError::Refused(format!(
                "worktree HEAD changed since safety check: was {expected_head}, now {h}"
            )));
        }
        if require_clean && is_dirty(wt)? {
            return Err(CoreError::Refused(
                "worktree became dirty after safety check".into(),
            ));
        }
        run(wt, &["read-tree", "--reset", "-u", target])?;
        run(wt, &["clean", "-fd"])?;
        writeln!(lock, "{target}")
            .and_then(|_| lock.sync_all())
            .map_err(|e| CoreError::Backend(format!("writing {}: {e}", lock_path.display())))?;
        std::fs::rename(&lock_path, &head_path)
            .map_err(|e| CoreError::Backend(format!("committing worktree HEAD: {e}")))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&lock_path);
    }
    result
}

/// `.worktreeinclude` is committed at `rev` (treehouse would seed ignored
/// files from it, which the native pool does not do yet).
pub(crate) fn has_worktree_include(repo: &Path, rev: &str) -> bool {
    let spec = format!("{rev}:.worktreeinclude");
    succeeds(repo, &["cat-file", "-e", &spec])
}

fn nul_list(data: &[u8]) -> Vec<String> {
    data.split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// The untracked files of a recovered worktree, or why it cannot be proven
/// to hold no other edits (hidden flags, tracked changes, dirty submodules).
pub(crate) fn recovery_worktree(dir: &Path) -> std::result::Result<Vec<String>, String> {
    let flags = exec(dir, &["ls-files", "-v", "-z"])
        .ok()
        .filter(Out::ok)
        .ok_or("cannot verify tracked changes")?;
    for entry in nul_list(&flags.stdout) {
        let tag = entry.as_bytes()[0];
        if tag == b'S' || tag.is_ascii_lowercase() {
            return Err("tracked files are marked skip-worktree or assume-unchanged, which hides their edits; clear those flags and check them".into());
        }
    }
    let stages = exec(dir, &["ls-files", "--stage", "-z"])
        .ok()
        .filter(Out::ok)
        .ok_or("cannot verify submodules")?;
    for entry in nul_list(&stages.stdout) {
        if !entry.starts_with("160000 ") {
            continue;
        }
        let name = entry
            .split_once('\t')
            .map(|(_, n)| n.to_string())
            .ok_or("cannot verify submodules")?;
        let sub = dir.join(&name);
        match std::fs::metadata(sub.join(".git")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot verify submodule {name}: {e}")),
            Ok(_) => {}
        }
        let files = recovery_worktree(&sub).map_err(|r| format!("submodule {name}: {r}"))?;
        if !files.is_empty() {
            return Err(format!(
                "submodule {name} has untracked files; inspect and return it by name"
            ));
        }
    }
    let tracked = exec(
        dir,
        &[
            "diff",
            "--name-only",
            "--ignore-submodules=none",
            "HEAD",
            "--",
        ],
    )
    .ok()
    .filter(Out::ok)
    .ok_or("cannot verify tracked changes")?;
    if !tracked.text().is_empty() {
        return Err(
            "tracked changes (including submodule contents) are present; commit or preserve them"
                .into(),
        );
    }
    let untracked = exec(dir, &["ls-files", "--others", "--exclude-standard", "-z"])
        .ok()
        .filter(Out::ok)
        .ok_or("cannot verify untracked files")?;
    Ok(nul_list(&untracked.stdout))
}

/// No commit reachable from HEAD is missing from every remote-tracking ref
/// and from the local `base` branch.
pub(crate) fn recovery_head_contained(dir: &Path, base: &str) -> bool {
    let mut args = vec!["rev-list", "-n", "1", "HEAD", "--not", "--remotes"];
    let r = format!("refs/heads/{base}");
    if !base.is_empty() && exact_ref_exists(dir, &r) {
        args.push(&r);
    }
    args.push("--");
    exec(dir, &args).is_ok_and(|o| o.ok() && o.text().is_empty())
}

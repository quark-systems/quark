//! Where a new slot goes: the built-in `<pool>/<slot>/<repo>` layout,
//! `unique_leaf` (`<pool>/<slot>/<repo>-<slot>`) or a `worktree_path`
//! template, ported from treehouse 3.1.2 (`internal/pool/worktree_path.go`).
//!
//! A template may use `{pool}`, `{slot}` (required), `{repo}` and
//! `{repo_parent}`, and one of `{pool}`/`{repo}` (slot names are per pool),
//! after `$VAR`/`${VAR}` expansion; a variable that expands to nothing is
//! refused. The result must be absolute, must not be or contain the
//! repository or the pool directory, inside the pool must be
//! `{pool}/{slot}/<name>`, must not sit in another repository's pool, and
//! must not sit inside the repository. Placement is read only when a slot is
//! created, so it never moves a slot already in the pool, and a templated
//! path that already exists is never adopted: the next slot name is tried.

use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use quark_core::{CoreError, Result};

use super::state;

const POOL: &str = "{pool}";
const SLOT: &str = "{slot}";
const REPO: &str = "{repo}";
const REPO_PARENT: &str = "{repo_parent}";
const PLACEHOLDERS: [&str; 4] = [POOL, SLOT, REPO, REPO_PARENT];

fn invalid(msg: String) -> CoreError {
    CoreError::Invalid(msg)
}

/// Refuse a `${` with no closing brace, or `${}`, before expansion would
/// quietly turn either into nothing.
fn check_env_references(template: &str) -> Result<()> {
    let b = template.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] != b'$' || b[i + 1] != b'{' {
            i += 1;
            continue;
        }
        let rest = &template[i + 2..];
        let end = rest.find(['}', '/', '\\', '{']);
        if end == Some(0) && rest.starts_with('}') {
            return Err(invalid(format!(
                "worktree path {template:?} references \"${{}}\", which names no environment variable and expands to nothing instead of a directory"
            )));
        }
        match end {
            Some(e) if rest.as_bytes()[e] == b'}' => i += e + 3,
            _ => {
                return Err(invalid(format!(
                    "worktree path {template:?} has a \"${{\" with no closing brace, so the reference expands to nothing instead of a directory"
                )))
            }
        }
    }
    Ok(())
}

/// `$VAR` and `${VAR}` expanded (Go's `os.Expand`), refusing any that is
/// empty.
fn expand_env(template: &str) -> Result<String> {
    check_env_references(template)?;
    let mut out = String::new();
    let mut empty: Vec<String> = Vec::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let name: String = if chars.peek() == Some(&'{') {
            chars.next();
            chars.by_ref().take_while(|c| *c != '}').collect()
        } else {
            let mut n = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    n.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if n.is_empty() {
                // A lone `$` (Go drops it, reading an empty name).
                continue;
            }
            n
        };
        let value = std::env::var(&name).unwrap_or_default();
        if value.is_empty() && !empty.contains(&name) {
            empty.push(name);
        }
        out.push_str(&value);
    }
    if !empty.is_empty() {
        return Err(invalid(format!(
            "worktree path {template:?} references ${}, which the environment leaves empty: an empty expansion drops a directory from the path, so the worktree would not be created where the template names",
            empty.join(", $")
        )));
    }
    Ok(out)
}

fn describe(template: &str, expanded: &str) -> String {
    if template == expanded {
        format!("{template:?}")
    } else {
        format!("{template:?} (expanded to {expanded:?})")
    }
}

/// Every `{...}` group in `s`.
fn placeholders(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match c {
            '{' => start = Some(i),
            '}' => {
                if let Some(st) = start.take() {
                    out.push(&s[st..=i]);
                }
            }
            _ => {}
        }
    }
    out
}

/// The template's own rules (before any slot name), returning it expanded.
/// An empty template is the built-in layout.
pub(super) fn validate_template(template: &str) -> Result<String> {
    if template.is_empty() {
        return Ok(String::new());
    }
    let expanded = expand_env(template)?;
    for found in placeholders(&expanded) {
        if !PLACEHOLDERS.contains(&found) {
            return Err(invalid(format!(
                "worktree path {} uses unknown placeholder {found} (supported: {})",
                describe(template, &expanded),
                PLACEHOLDERS.join(" ")
            )));
        }
    }
    if !expanded.contains(SLOT) {
        return Err(invalid(format!(
            "worktree path {} must contain {SLOT}: without it every pool slot resolves to the same directory",
            describe(template, &expanded)
        )));
    }
    let apart = |slot: &str| state::clean(Path::new(&expanded.replace(SLOT, slot)));
    if apart("slot-a") == apart("slot-b") {
        return Err(invalid(format!(
            "worktree path {} resolves to the same directory for every slot, so two slots would share one worktree; give {SLOT} a path segment that survives path cleaning",
            describe(template, &expanded)
        )));
    }
    if !expanded.contains(POOL) && !expanded.contains(REPO) {
        return Err(invalid(format!(
            "worktree path {} must contain one of {POOL} {REPO}: slot names are allocated per repository, so without one the first slot of every repository resolves to the same directory",
            describe(template, &expanded)
        )));
    }
    Ok(expanded)
}

/// Where slot `slot` of `repo`'s pool `pool` goes.
pub(super) fn resolve(
    repo: &Path,
    pool: &Path,
    slot: &str,
    template: &str,
    unique_leaf: bool,
) -> Result<PathBuf> {
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if template.is_empty() {
        let leaf = if unique_leaf {
            format!("{name}-{slot}")
        } else {
            name
        };
        return Ok(pool.join(slot).join(leaf));
    }
    let expanded = validate_template(template)?;
    let parent = repo.parent().unwrap_or(Path::new("/"));
    // One pass, as Go's strings.Replacer: a substituted value is never
    // read again for placeholders.
    let mut replaced = String::new();
    let mut rest = expanded.as_str();
    'outer: while !rest.is_empty() {
        for (ph, value) in [
            (POOL, pool.to_string_lossy()),
            (SLOT, slot.into()),
            (REPO, name.as_str().into()),
            (REPO_PARENT, parent.to_string_lossy()),
        ] {
            if let Some(r) = rest.strip_prefix(ph) {
                replaced.push_str(&value);
                rest = r;
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        replaced.push(c);
        rest = &rest[c.len_utf8()..];
    }
    let resolved = state::clean(Path::new(&replaced));
    if !resolved.is_absolute() {
        return Err(invalid(format!(
            "worktree path {template:?} resolves to the relative path {:?}; anchor it on {POOL}, {REPO_PARENT}, or an absolute path",
            resolved.display().to_string()
        )));
    }
    check_placement(template, &resolved, repo, pool, slot)?;
    Ok(resolved)
}

fn check_placement(
    template: &str,
    resolved: &Path,
    repo: &Path,
    pool: &Path,
    slot: &str,
) -> Result<()> {
    let canon_resolved = canonical_prefix(resolved).map_err(|e| {
        invalid(format!(
            "worktree path {template:?} resolves to {:?}, which cannot be checked: {e}",
            resolved.display().to_string()
        ))
    })?;
    let canon_repo = canonical_prefix(repo).map_err(|e| {
        invalid(format!(
            "cannot resolve the repository {:?} to check the worktree path: {e}",
            repo.display().to_string()
        ))
    })?;
    let canon_pool = canonical_prefix(pool).map_err(|e| {
        invalid(format!(
            "cannot resolve the pool directory {:?} to check the worktree path: {e}",
            pool.display().to_string()
        ))
    })?;
    let r = resolved.display().to_string();
    if path_contains(&canon_resolved, &canon_pool) {
        return Err(invalid(format!(
            "worktree path {template:?} resolves to {r:?}, which is or contains the pool directory {:?} holding the pool's state",
            pool.display().to_string()
        )));
    }
    if path_contains(&canon_resolved, &canon_repo) {
        return Err(invalid(format!(
            "worktree path {template:?} resolves to {r:?}, which is or contains the repository {:?}",
            repo.display().to_string()
        )));
    }
    if path_contains(&canon_pool, &canon_resolved) {
        let rel = lexical_rel(&canon_pool, &canon_resolved).unwrap_or_default();
        let segments: Vec<_> = rel.components().collect();
        if segments.len() != 2 || segments[0] != Component::Normal(slot.as_ref()) {
            return Err(invalid(format!(
                "worktree path {template:?} resolves to {r:?}; inside the pool directory it must be {POOL}/{SLOT}/<name> so pool state can be recovered from disk"
            )));
        }
        if !contains_lexically(pool, resolved) {
            return Err(invalid(format!(
                "worktree path {template:?} resolves to {r:?}, which reaches the pool directory {:?} by another name; spell an in-pool path through the pool directory itself (use {POOL}) so pool state and recovery agree on one path",
                pool.display().to_string()
            )));
        }
        return Ok(());
    }
    if let Some(foreign) = enclosing_pool(&canon_resolved) {
        return Err(invalid(format!(
            "worktree path {template:?} resolves to {r:?}, inside the treehouse pool directory {:?} of another repository, which would register the worktree as a slot of its own",
            foreign.display().to_string()
        )));
    }
    if path_contains(&canon_repo, &canon_resolved) {
        return Err(invalid(format!(
            "worktree path {template:?} resolves to {r:?}, inside the repository working tree {:?}; place worktrees outside the repository or under {POOL}",
            repo.display().to_string()
        )));
    }
    Ok(())
}

/// The first slot name from `first` (trying up to `max` names) whose
/// templated path does not exist yet.
pub(super) fn free_templated_slot(
    repo: &Path,
    pool: &Path,
    first: u64,
    max: usize,
    template: &str,
) -> Result<(String, PathBuf)> {
    let mut occupied = Vec::new();
    for n in first..first + max as u64 {
        let name = n.to_string();
        let path = resolve(repo, pool, &name, template, false)?;
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((name, path)),
            Err(e) => {
                return Err(CoreError::Backend(format!(
                    "checking {}: {e}",
                    path.display()
                )))
            }
            Ok(_) => {
                tracing::warn!(slot = %name, path = %path.display(), "skipped slot because its worktree path already exists");
                occupied.push(path);
            }
        }
    }
    let (Some(a), Some(b)) = (occupied.first(), occupied.last()) else {
        return Err(CoreError::Refused(
            "no worktree path candidates to check (max_trees = 0)".into(),
        ));
    };
    Err(CoreError::Refused(format!(
        "every worktree path this pool would create already exists ({} checked, {} through {}); treehouse only creates a worktree at a path it can own and never adopts an existing directory. List this repository's worktrees with 'git worktree list' in {} and see the README section on recovering missing pool state to decide what to do with them",
        occupied.len(),
        a.display(),
        b.display(),
        repo.display()
    )))
}

fn enclosing_pool(path: &Path) -> Option<PathBuf> {
    let mut cur = state::clean(path);
    loop {
        if state::is_pool_dir(&cur) {
            return Some(cur);
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_path_buf(),
            _ => return None,
        }
    }
}

/// `p` with its longest existing prefix resolved through symlinks.
pub(super) fn canonical_prefix(p: &Path) -> std::result::Result<PathBuf, String> {
    let p = state::clean(p);
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut head = p.clone();
    loop {
        match std::fs::canonicalize(&head) {
            Ok(mut c) => {
                for part in missing.iter().rev() {
                    c.push(part);
                }
                return Ok(c);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (head.parent(), head.file_name()) else {
                    return Err(format!("no existing directory found above {}", p.display()));
                };
                missing.push(name.to_os_string());
                head = parent.to_path_buf();
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// `child` relative to `parent` when it is inside it lexically.
fn lexical_rel(parent: &Path, child: &Path) -> Option<PathBuf> {
    state::clean(child)
        .strip_prefix(state::clean(parent))
        .ok()
        .map(Path::to_path_buf)
}

fn contains_lexically(parent: &Path, child: &Path) -> bool {
    lexical_rel(parent, child).is_some()
}

/// `child` is `parent` or inside it, lexically or through a directory that
/// is the same file as `parent`.
fn path_contains(parent: &Path, child: &Path) -> bool {
    if contains_lexically(parent, child) {
        return true;
    }
    let mut cur = state::clean(child);
    loop {
        if same_file(parent, &cur) {
            return true;
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_path_buf(),
            _ => return false,
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_and_unique_leaf() {
        let (repo, pool) = (Path::new("/src/app"), Path::new("/h/.treehouse/app-abc"));
        assert_eq!(
            resolve(repo, pool, "3", "", false).unwrap(),
            pool.join("3/app")
        );
        assert_eq!(
            resolve(repo, pool, "3", "", true).unwrap(),
            pool.join("3/app-3")
        );
    }

    #[test]
    fn template_rules() {
        for (t, want) in [
            ("{pool}/x/{repo}", "must contain {slot}"),
            ("/wt/{slot}", "must contain one of"),
            ("{pool}/{slot}/{name}", "unknown placeholder {name}"),
            (
                "{repo_parent}/{repo}-${QUARK_WT_UNSET_VAR}-{slot}",
                "leaves empty",
            ),
            ("{repo}-{slot}", "relative path"),
            ("${HOME", "no closing brace"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("app");
            let pool = tmp.path().join(".treehouse/app-abc");
            let err = resolve(&repo, &pool, "1", t, false)
                .unwrap_err()
                .to_string();
            assert!(err.contains(want), "{t}: {err}");
        }
    }

    #[test]
    fn template_placement() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let pool = tmp.path().join(".treehouse/app-abc");
        assert_eq!(
            resolve(&repo, &pool, "2", "{repo_parent}/{repo}-{slot}", false).unwrap(),
            tmp.path().join("app-2")
        );
        assert_eq!(
            resolve(&repo, &pool, "2", "{pool}/{slot}/w", false).unwrap(),
            pool.join("2/w")
        );
        for (t, want) in [
            ("{pool}/{slot}/a/b", "must be {pool}/{slot}/<name>"),
            (
                "{repo_parent}/{repo}/{slot}",
                "inside the repository working tree",
            ),
        ] {
            let err = resolve(&repo, &pool, "2", t, false)
                .unwrap_err()
                .to_string();
            assert!(err.contains(want), "{t}: {err}");
        }
    }

    #[test]
    fn occupied_templated_paths_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(tmp.path().join("app-1")).unwrap();
        let pool = tmp.path().join(".treehouse/app-abc");
        let (name, path) =
            free_templated_slot(&repo, &pool, 1, 4, "{repo_parent}/{repo}-{slot}").unwrap();
        assert_eq!((name.as_str(), path), ("2", tmp.path().join("app-2")));
        let err = free_templated_slot(&repo, &pool, 1, 1, "{repo_parent}/{repo}-{slot}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
    }
}

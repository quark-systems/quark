//! Freeing a recovered slot that still holds untracked files, ported from
//! treehouse 3.1.2 (`internal/pool/recovered.go`): once the slot is proven
//! otherwise safe (nothing runs in it, no tracked edits, HEAD reachable),
//! its untracked files are moved, never copied or deleted, into a retained
//! owner-only backup folder beside the pool,
//! `<pool root>/treehouse-recovered-backup-<pool name>-<slot>`, and the slot
//! goes back to the pool. A name already taken in the backup gets a `.N`
//! suffix; nothing there is ever overwritten.

use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Where slot `name` of `pool` keeps its recovered files.
pub(super) fn backup_dir(pool: &Path, name: &str) -> PathBuf {
    let pool_name = pool
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    pool.parent()
        .unwrap_or(Path::new("/"))
        .join(format!("treehouse-recovered-backup-{pool_name}-{name}"))
}

/// The backup folder of slot `name` when it exists and holds something
/// (shown in the status listing).
pub(super) fn backup_of(pool: &Path, name: &str) -> String {
    let dir = backup_dir(pool, name);
    let real = std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir());
    let nonempty = std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some());
    if real && nonempty {
        dir.to_string_lossy().into_owned()
    } else {
        String::new()
    }
}

fn ensure_real_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    if !std::fs::symlink_metadata(dir)?.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a real directory",
            dir.display()
        )));
    }
    Ok(())
}

fn ensure_owner_only(dir: &Path) -> std::io::Result<()> {
    let m = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid has no preconditions.
    if m.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::other(format!(
            "{} is not owned by the current user",
            dir.display()
        )));
    }
    if m.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            std::io::Error::other(format!(
                "cannot restrict {} to owner-only access: {e}",
                dir.display()
            ))
        })?;
    }
    Ok(())
}

/// Rename `src` to `dst`, failing with `AlreadyExists` rather than replace
/// anything (`renameat2(RENAME_NOREPLACE)` / `renamex_np(RENAME_EXCL)`).
fn move_no_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let s = CString::new(src.as_os_str().as_bytes())?;
    let d = CString::new(dst.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    // SAFETY: both arguments are valid NUL-terminated paths.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            s.as_ptr(),
            libc::AT_FDCWD,
            d.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: both arguments are valid NUL-terminated paths.
    let rc = unsafe { libc::renamex_np(s.as_ptr(), d.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let rc = {
        let _ = (s, d);
        return Err(std::io::Error::other(
            "recovery backup moves are unsupported on this platform",
        ));
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Move each untracked `paths` (relative, `/`-separated) of `wt` into
/// `backup`, keeping their directories.
pub(super) fn backup_untracked(backup: &Path, wt: &Path, paths: &[String]) -> std::io::Result<()> {
    ensure_real_dir(backup)?;
    ensure_owner_only(backup)?;
    for name in paths {
        let src = wt.join(name);
        let rel = Path::new(name);
        let mut dir = backup.to_path_buf();
        if let Some(parent) = rel.parent() {
            for part in parent.components() {
                dir.push(part);
                ensure_real_dir(&dir)?;
            }
        }
        let base = rel.file_name().unwrap_or_default().to_string_lossy();
        let dst = dir.join(base.as_ref());
        for n in 0.. {
            let free = if n == 0 {
                dst.clone()
            } else {
                dir.join(format!("{base}.{n}"))
            };
            match move_no_replace(&src, &free) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_without_replacing() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(wt.join("a")).unwrap();
        std::fs::write(wt.join("a/notes.txt"), "new").unwrap();
        std::fs::write(wt.join("top"), "t").unwrap();
        let pool = tmp.path().join(".treehouse/app-abc");
        let backup = backup_dir(&pool, "3");
        assert_eq!(
            backup,
            tmp.path()
                .join(".treehouse/treehouse-recovered-backup-app-abc-3")
        );
        std::fs::create_dir_all(backup.join("a")).unwrap();
        std::fs::write(backup.join("a/notes.txt"), "old").unwrap();
        backup_untracked(&backup, &wt, &["a/notes.txt".into(), "top".into()]).unwrap();
        assert_eq!(
            std::fs::read_to_string(backup.join("a/notes.txt")).unwrap(),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(backup.join("a/notes.txt.1")).unwrap(),
            "new"
        );
        assert_eq!(std::fs::read_to_string(backup.join("top")).unwrap(), "t");
        assert!(!wt.join("top").exists());
        let mode = std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(backup_of(&pool, "3"), backup.to_string_lossy());
        assert_eq!(backup_of(&pool, "4"), "");
    }
}

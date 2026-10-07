//! Disk used under a path.

use std::path::Path;

/// Bytes in the files under `path` (or `path` itself if it is a file).
/// Symlinks are not followed and unreadable entries are skipped, since
/// worktrees change while they are walked. A missing path is 0.
pub fn tree_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_files_and_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), [0u8; 100]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b"), [0u8; 23]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("link")).unwrap();

        assert_eq!(tree_size(dir.path()), 123);
        assert_eq!(tree_size(&dir.path().join("a")), 100);
        assert_eq!(tree_size(&dir.path().join("missing")), 0);
    }
}

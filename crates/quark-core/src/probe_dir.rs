//! An empty directory to run short-lived CLI checks in.
//!
//! A harness CLI run for a quota, version or model-list check starts in
//! whatever directory quarkd is in, and some (Antigravity's `agy`, built on
//! Gemini CLI, among them) index their working directory as a workspace
//! before they answer. quarkd launched by launchd sits in `/`, so such a
//! check walked the whole disk, and macOS asked for Desktop, Documents,
//! Downloads, removable and network volumes on behalf of the app that
//! started quarkd (QuarkBar). Checks run here instead: an empty directory
//! under the per-user temporary directory, which macOS does not protect.

use std::path::PathBuf;

/// `<temp>/quarkd-probe`, created if missing. Falls back to the temporary
/// directory itself if it cannot be created.
pub fn probe_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("quarkd-probe");
    match std::fs::create_dir_all(&dir) {
        Ok(()) => dir,
        Err(_) => std::env::temp_dir(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn exists_and_is_not_root() {
        let d = super::probe_dir();
        assert!(d.is_dir());
        assert_ne!(d, std::path::Path::new("/"));
    }
}

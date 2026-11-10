//! Opt-in APFS sharing (`apfs_sharing = "fresh"`), ported from treehouse
//! 3.1.2 (`gitvcs.ShareWorktreeFiles`, `internal/fileclone`).
//!
//! A *freshly created* git slot (never a reused one, which may still have
//! writers) replaces each large tracked file that is byte-identical to the
//! same path in the primary checkout with an independent copy-on-write
//! clone of it, so the two share disk blocks until either is edited. These
//! are clones, not hardlinks: editing or deleting one never touches the
//! other. Everything is checked first and anything unusual is kept as an
//! ordinary copy, with the reason counted in the report:
//!
//! - the pass is skipped (with a reason, never an error) off macOS, off
//!   APFS, across volumes, or when a git hook, fsmonitor or checkout filter
//!   could be writing to the slot;
//! - a file is kept when it is under 64 KiB, not a regular file, a
//!   hardlink, owned by someone else, carries flags or setuid/setgid/sticky
//!   bits, is sparse or compressed, has an ACL or extended attributes
//!   (stricter than treehouse, which copies xattrs over), or differs in
//!   content (SHA-256 of both).
//!
//! Each clone is made in a private staging directory beside the file,
//! verified (content, mode, times, owner), and renamed over the original
//! only if the original, its directory and the slot root are still the
//! ones checked. A slot whose git status, HEAD or index changed during the
//! pass, or whose staging could not be cleaned up, fails the acquisition
//! and is quarantined, as treehouse does.

use std::path::Path;

use quark_core::{CoreError, Result};

use super::git::{self, exec};

/// Files below this size are never worth a clone.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MINIMUM_SIZE: i64 = 64 * 1024;

/// What a sharing pass did.
#[derive(Debug, Default)]
pub(super) struct Report {
    pub cloned: usize,
    pub logical_bytes: u64,
    pub skipped: std::collections::BTreeMap<&'static str, usize>,
    /// Why the whole pass was skipped.
    pub reason: String,
    pub errors: Vec<String>,
}

impl Report {
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            ..Self::default()
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn skip(&mut self, reason: &'static str) {
        *self.skipped.entry(reason).or_default() += 1;
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.reason.is_empty() {
            return write!(f, "APFS sharing skipped: {}", self.reason);
        }
        write!(
            f,
            "APFS sharing: cloned={} logical_bytes={}",
            self.cloned, self.logical_bytes
        )?;
        for (reason, n) in &self.skipped {
            write!(f, " {reason}={n}")?;
        }
        for e in &self.errors {
            write!(f, " error={e:?}")?;
        }
        Ok(())
    }
}

/// Share `wt`'s tracked files with `repo`'s. `Err` means the slot changed
/// under the pass (or staging was left behind) and must be quarantined.
pub(super) fn share_worktree_files(repo: &Path, wt: &Path) -> Result<()> {
    let report = share(repo, wt)?;
    tracing::info!(slot = %wt.display(), "{report}");
    Ok(())
}

fn share(repo: &Path, wt: &Path) -> Result<Report> {
    if let Some(reason) = platform::filesystem_reason(repo, wt) {
        return Ok(Report::skipped(reason));
    }
    if let Some(reason) = writer_risk(repo, wt) {
        return Ok(Report::skipped(reason));
    }
    let Ok(listed) = exec(wt, &["ls-files", "-z"]) else {
        return Ok(Report::skipped("tracked paths cannot be enumerated"));
    };
    if !listed.ok() {
        return Ok(Report::skipped("tracked paths cannot be enumerated"));
    }
    if listed.stdout.is_empty() {
        return Ok(Report::default());
    }
    let paths: Vec<String> = listed
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    if let Some(reason) = filter_risk(wt, &listed.stdout) {
        return Ok(Report::skipped(reason));
    }
    let Some(before) = snapshot(wt) else {
        return Ok(Report::skipped("initial Git status cannot be verified"));
    };
    let report = platform::share(repo, wt, &paths)?;
    if snapshot(wt).as_ref() != Some(&before) {
        return Err(CoreError::Backend(
            "worktree status, HEAD or index changed during APFS sharing; inspect before use".into(),
        ));
    }
    Ok(report)
}

/// Status, HEAD and index, to prove the pass changed nothing git sees.
fn snapshot(wt: &Path) -> Option<(Vec<u8>, String, Vec<u8>)> {
    let status = exec(
        wt,
        &[
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )
    .ok()
    .filter(|o| o.ok())?;
    let head = git::head(wt).ok()?;
    let index = exec(wt, &["ls-files", "--stage", "-z"])
        .ok()
        .filter(|o| o.ok())?;
    Some((status.stdout, head, index.stdout))
}

/// A configured fsmonitor or an executable checkout hook could have started
/// a writer in either checkout.
fn writer_risk(repo: &Path, wt: &Path) -> Option<String> {
    for root in [repo, wt] {
        match exec(root, &["config", "--get", "core.fsmonitor"]) {
            Ok(o) if o.ok() => {
                let v = o.text().to_ascii_lowercase();
                if !matches!(
                    v.as_str(),
                    "" | "false" | "0" | "no" | "off" | "true" | "1" | "yes" | "on"
                ) {
                    return Some("Git fsmonitor hook may have started a writer".into());
                }
            }
            Ok(o) if o.code == Some(1) => {}
            _ => return Some("Git fsmonitor configuration cannot be verified".into()),
        }
        for hook in ["post-checkout", "reference-transaction"] {
            let rel = format!("hooks/{hook}");
            let Ok(path) = git::run(root, &["rev-parse", "--git-path", &rel]) else {
                return Some("Git hook configuration cannot be verified".into());
            };
            let path = Path::new(&path);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Ok(m) => {
                    use std::os::unix::fs::PermissionsExt;
                    if !m.is_file() || m.permissions().mode() & 0o111 != 0 {
                        return Some(format!("Git {hook} hook may have started a writer"));
                    }
                }
                Err(_) => return Some(format!("Git {hook} hook may have started a writer")),
            }
        }
    }
    None
}

/// A checkout filter on any tracked path could be writing to it.
fn filter_risk(wt: &Path, listed: &[u8]) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let child = Command::new("git")
        .args(["check-attr", "-z", "--stdin", "filter"])
        .current_dir(wt)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Some("checkout filters cannot be verified".into());
    };
    let input = listed.to_vec();
    let mut stdin = child.stdin.take()?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output();
    let _ = writer.join();
    let Ok(out) = out else {
        return Some("checkout filters cannot be verified".into());
    };
    if !out.status.success() {
        return Some("checkout filters cannot be verified".into());
    }
    let mut data = out.stdout.as_slice();
    if data.last() == Some(&0) {
        data = &data[..data.len() - 1];
    }
    let fields: Vec<&[u8]> = data.split(|b| *b == 0).collect();
    if fields.len() % 3 != 0 {
        return Some("checkout filter attributes are malformed".into());
    }
    for value in fields.iter().skip(2).step_by(3) {
        if *value != b"unspecified" && *value != b"unset" {
            return Some("checkout filter may have started a writer".into());
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(super) fn filesystem_reason(_: &Path, _: &Path) -> Option<String> {
        Some("requires macOS APFS".into())
    }

    pub(super) fn share(_: &Path, _: &Path, _: &[String]) -> Result<Report> {
        Ok(Report::skipped("requires macOS APFS"))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    use sha2::{Digest, Sha256};

    use super::*;

    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_NOOWNERCOPY: u32 = 0x0002;
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;

    extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, ty: libc::c_int) -> *mut libc::c_void;
        fn acl_free(obj: *mut libc::c_void) -> libc::c_int;
    }

    /// Fatal for the whole pass (the slot is quarantined).
    enum Fatal {
        Changed(String),
        Cleanup(String),
    }

    enum Outcome {
        Cloned(u64),
        Kept(&'static str),
    }

    type FileResult = std::result::Result<Outcome, std::result::Result<Fatal, String>>;

    fn os_err() -> String {
        std::io::Error::last_os_error().to_string()
    }

    fn cstr(s: &[u8]) -> std::result::Result<CString, String> {
        CString::new(s).map_err(|e| e.to_string())
    }

    fn openat(
        dir: libc::c_int,
        name: &CStr,
        flags: libc::c_int,
    ) -> std::result::Result<OwnedFd, String> {
        // SAFETY: `name` is NUL-terminated; the returned fd is owned below.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC, 0) };
        if fd < 0 {
            return Err(os_err());
        }
        // SAFETY: fd was just opened and is owned by nobody else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;

    /// Open `rel` below `root`, one component at a time, never following a
    /// symlink.
    fn open_rel_dir(root: libc::c_int, rel: &Path) -> std::result::Result<OwnedFd, String> {
        let mut fd = openat(root, c".", DIR_FLAGS)?;
        for c in rel.components() {
            match c {
                Component::Normal(n) => {
                    let name = cstr(n.as_bytes())?;
                    fd = openat(fd.as_raw_fd(), &name, DIR_FLAGS)?;
                }
                Component::CurDir => {}
                _ => return Err(format!("unsafe path {}", rel.display())),
            }
        }
        Ok(fd)
    }

    fn open_dir(path: &Path) -> std::result::Result<OwnedFd, String> {
        let abs = std::path::absolute(path).map_err(|e| e.to_string())?;
        let root = openat(libc::AT_FDCWD, c"/", DIR_FLAGS)?;
        let rel = abs.strip_prefix("/").unwrap_or(&abs);
        open_rel_dir(root.as_raw_fd(), rel)
    }

    fn fstat(fd: libc::c_int) -> std::result::Result<libc::stat, String> {
        // SAFETY: an all-zero stat is a valid out buffer.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd is open; st is a valid out pointer.
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            return Err(os_err());
        }
        Ok(st)
    }

    fn fstatfs_type(fd: libc::c_int) -> Option<(String, libc::fsid_t)> {
        // SAFETY: an all-zero statfs is a valid out buffer.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: fd is open; st is a valid out pointer.
        if unsafe { libc::fstatfs(fd, &mut st) } != 0 {
            return None;
        }
        // SAFETY: f_fstypename is NUL-terminated within its array.
        let name = unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) };
        Some((name.to_string_lossy().into_owned(), st.f_fsid))
    }

    fn open_regular(dir: libc::c_int, leaf: &CStr) -> Option<(File, libc::stat)> {
        let fd = openat(
            dir,
            leaf,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
        .ok()?;
        let st = fstat(fd.as_raw_fd()).ok()?;
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return None;
        }
        Some((File::from(fd), st))
    }

    fn has_acl(fd: libc::c_int) -> bool {
        // SAFETY: fd is open; a non-null result is freed with acl_free.
        let acl = unsafe { acl_get_fd_np(fd, ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            return std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT);
        }
        // SAFETY: acl came from acl_get_fd_np.
        unsafe { acl_free(acl) };
        true
    }

    /// Whether `fd` carries any extended attribute (errors count as yes).
    fn has_xattrs(fd: libc::c_int) -> bool {
        // SAFETY: a null buffer with size 0 asks only for the length.
        let n = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, 0) };
        n != 0
    }

    fn hash(f: &mut File) -> std::result::Result<[u8; 32], String> {
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 128 * 1024];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(h.finalize().into());
            }
            h.update(&buf[..n]);
        }
    }

    fn same_identity(a: &libc::stat, b: &libc::stat) -> bool {
        a.st_dev == b.st_dev
            && a.st_ino == b.st_ino
            && a.st_size == b.st_size
            && (a.st_mtime, a.st_mtime_nsec) == (b.st_mtime, b.st_mtime_nsec)
            && (a.st_ctime, a.st_ctime_nsec) == (b.st_ctime, b.st_ctime_nsec)
            && a.st_nlink == b.st_nlink
    }

    fn same_dir(a: libc::c_int, b: libc::c_int) -> bool {
        match (fstat(a), fstat(b)) {
            (Ok(x), Ok(y)) => x.st_dev == y.st_dev && x.st_ino == y.st_ino,
            _ => false,
        }
    }

    pub(super) fn filesystem_reason(source: &Path, destination: &Path) -> Option<String> {
        let (Ok(s), Ok(d)) = (open_dir(source), open_dir(destination)) else {
            return Some("filesystem cannot be verified".into());
        };
        let (Some((st, sid)), Some((dt, did))) =
            (fstatfs_type(s.as_raw_fd()), fstatfs_type(d.as_raw_fd()))
        else {
            return Some("filesystem cannot be verified".into());
        };
        if st != "apfs" || dt != "apfs" {
            return Some("requires APFS on both paths".into());
        }
        // SAFETY: fsid_t is two i32s on Darwin (its fields are private).
        let (sid, did): ([i32; 2], [i32; 2]) = unsafe {
            (
                std::mem::transmute::<libc::fsid_t, [i32; 2]>(sid),
                std::mem::transmute::<libc::fsid_t, [i32; 2]>(did),
            )
        };
        if sid != did {
            return Some("source and destination are on different volumes".into());
        }
        None
    }

    pub(super) fn share(source: &Path, destination: &Path, paths: &[String]) -> Result<Report> {
        let src = match open_dir(source) {
            Ok(f) => f,
            Err(e) => {
                return Ok(Report::skipped(format!(
                    "source path unavailable or symlinked: {e}"
                )))
            }
        };
        let dst = match open_dir(destination) {
            Ok(f) => f,
            Err(e) => {
                return Ok(Report::skipped(format!(
                    "destination path unavailable or symlinked: {e}"
                )))
            }
        };
        let (Ok(ss), Ok(ds)) = (fstat(src.as_raw_fd()), fstat(dst.as_raw_fd())) else {
            return Ok(Report::skipped(
                "source or destination identity cannot be verified",
            ));
        };
        if ss.st_dev != ds.st_dev {
            return Ok(Report::skipped(
                "source and destination are on different volumes",
            ));
        }
        if ss.st_ino == ds.st_ino {
            return Ok(Report::skipped(
                "source and destination are the same directory",
            ));
        }
        let mut report = Report::default();
        for path in paths {
            let rel = Path::new(path);
            let safe = rel.components().all(|c| matches!(c, Component::Normal(_)));
            if !safe || rel.file_name().is_none() {
                report.skip("unsafe_path");
                continue;
            }
            match share_file(src.as_raw_fd(), dst.as_raw_fd(), destination, &ds, rel) {
                Ok(Outcome::Cloned(bytes)) => {
                    report.cloned += 1;
                    report.logical_bytes += bytes;
                }
                Ok(Outcome::Kept(reason)) => report.skip(reason),
                Err(Ok(Fatal::Changed(why))) | Err(Ok(Fatal::Cleanup(why))) => {
                    return Err(CoreError::Backend(why));
                }
                Err(Err(e)) => {
                    report.skip("kept_after_error");
                    if report.errors.len() < 3 {
                        report.errors.push(format!("{path}: {e}"));
                    }
                }
            }
        }
        Ok(report)
    }

    fn share_file(
        source: libc::c_int,
        destination: libc::c_int,
        destination_path: &Path,
        root_stat: &libc::stat,
        rel: &Path,
    ) -> FileResult {
        let parent = rel.parent().unwrap_or(Path::new(""));
        let leaf = cstr(rel.file_name().unwrap_or_default().as_bytes()).map_err(Err)?;
        let Ok(dp) = open_rel_dir(destination, parent) else {
            return Ok(Outcome::Kept("destination_missing_or_symlink"));
        };
        let Some((mut dst, old)) = open_regular(dp.as_raw_fd(), &leaf) else {
            return Ok(Outcome::Kept("destination_not_regular_or_symlink"));
        };
        if old.st_size < MINIMUM_SIZE {
            return Ok(Outcome::Kept("below_threshold"));
        }
        let Ok(sp) = open_rel_dir(source, parent) else {
            return Ok(Outcome::Kept("source_missing_or_symlink"));
        };
        let Some((mut src, donor)) = open_regular(sp.as_raw_fd(), &leaf) else {
            return Ok(Outcome::Kept("source_not_regular_or_symlink"));
        };
        if old.st_dev != donor.st_dev {
            return Ok(Outcome::Kept("different_volume"));
        }
        if old.st_ino == donor.st_ino {
            return Ok(Outcome::Kept("same_file"));
        }
        if old.st_nlink != 1 || donor.st_nlink != 1 {
            return Ok(Outcome::Kept("hardlink"));
        }
        // SAFETY: geteuid has no preconditions.
        let me = unsafe { libc::geteuid() };
        if old.st_uid != donor.st_uid || old.st_gid != donor.st_gid || old.st_uid != me {
            return Ok(Outcome::Kept("different_owner"));
        }
        if old.st_flags != 0
            || donor.st_flags != 0
            || old.st_mode & 0o7000 != 0
            || donor.st_mode & 0o7000 != 0
        {
            return Ok(Outcome::Kept("unsupported_flags_or_mode"));
        }
        if old.st_size != donor.st_size {
            return Ok(Outcome::Kept("different_content"));
        }
        if old.st_blocks * 512 < old.st_size || donor.st_blocks * 512 < donor.st_size {
            return Ok(Outcome::Kept("sparse_or_compressed"));
        }
        if has_acl(dst.as_raw_fd()) || has_acl(src.as_raw_fd()) || has_acl(dp.as_raw_fd()) {
            return Ok(Outcome::Kept("acl"));
        }
        if has_xattrs(dst.as_raw_fd()) || has_xattrs(src.as_raw_fd()) {
            return Ok(Outcome::Kept("xattr"));
        }
        let expected = hash(&mut dst).map_err(Err)?;
        if hash(&mut src).map_err(Err)? != expected {
            return Ok(Outcome::Kept("different_content"));
        }

        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| Err(e.to_string()))?;
        let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let staging = cstr(format!(".treehouse-sharing-{name}").as_bytes()).map_err(Err)?;
        // SAFETY: dp is open; staging is NUL-terminated.
        if unsafe { libc::mkdirat(dp.as_raw_fd(), staging.as_ptr(), 0o700) } != 0 {
            return Err(Err(os_err()));
        }
        let result = (|| -> FileResult {
            let temp = openat(dp.as_raw_fd(), &staging, DIR_FLAGS).map_err(Err)?;
            // SAFETY: src and temp are open; the name is NUL-terminated.
            let rc = unsafe {
                libc::fclonefileat(
                    src.as_raw_fd(),
                    temp.as_raw_fd(),
                    c"clone".as_ptr(),
                    CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
                )
            };
            if rc != 0 {
                return Err(Err(format!("clonefile: {}", os_err())));
            }
            let staged =
                openat(temp.as_raw_fd(), c"clone", libc::O_RDWR | libc::O_NOFOLLOW).map_err(Err)?;
            let mut staged = File::from(staged);
            if hash(&mut staged).map_err(Err)? != expected {
                return Ok(Outcome::Kept("clone_changed_content"));
            }
            // SAFETY: staged is open.
            if unsafe { libc::fchmod(staged.as_raw_fd(), old.st_mode & 0o7777) } != 0 {
                return Err(Err(os_err()));
            }
            let times = [
                libc::timespec {
                    tv_sec: old.st_atime,
                    tv_nsec: old.st_atime_nsec,
                },
                libc::timespec {
                    tv_sec: old.st_mtime,
                    tv_nsec: old.st_mtime_nsec,
                },
            ];
            // Hashing can update atime: restore both after the last read.
            // SAFETY: staged is open; times holds two timespecs.
            if unsafe { libc::futimens(staged.as_raw_fd(), times.as_ptr()) } != 0 {
                return Err(Err(os_err()));
            }
            let now = fstat(staged.as_raw_fd()).map_err(Err)?;
            if now.st_mode != old.st_mode
                || now.st_uid != old.st_uid
                || now.st_gid != old.st_gid
                || now.st_size != old.st_size
                || (now.st_mtime, now.st_mtime_nsec) != (old.st_mtime, old.st_mtime_nsec)
                || now.st_flags != 0
                || now.st_nlink != 1
                || has_acl(staged.as_raw_fd())
                || has_xattrs(staged.as_raw_fd())
            {
                return Err(Err("cloning did not preserve destination metadata".into()));
            }
            staged.sync_all().map_err(|e| Err(e.to_string()))?;

            // The original, its name, its directory and the slot root are
            // still the ones checked.
            let unchanged = fstat(dst.as_raw_fd()).is_ok_and(|s| same_identity(&old, &s))
                && named_stat(dp.as_raw_fd(), &leaf).is_some_and(|s| same_identity(&old, &s))
                && open_dir(destination_path).is_ok_and(|f| {
                    fstat(f.as_raw_fd())
                        .is_ok_and(|s| s.st_dev == root_stat.st_dev && s.st_ino == root_stat.st_ino)
                })
                && open_rel_dir(destination, parent)
                    .is_ok_and(|f| same_dir(f.as_raw_fd(), dp.as_raw_fd()));
            if !unchanged {
                return Err(Ok(Fatal::Changed(format!(
                    "destination changed during APFS sharing: {}",
                    rel.display()
                ))));
            }
            // SAFETY: both directories are open; names are NUL-terminated.
            let rc = unsafe {
                libc::renameat(
                    temp.as_raw_fd(),
                    c"clone".as_ptr(),
                    dp.as_raw_fd(),
                    leaf.as_ptr(),
                )
            };
            if rc != 0 {
                return Err(Err(os_err()));
            }
            Ok(Outcome::Cloned(old.st_size as u64))
        })();
        // Staging is always removed; a leftover is fatal.
        let cleanup = (|| -> std::result::Result<(), String> {
            if let Ok(temp) = openat(dp.as_raw_fd(), &staging, DIR_FLAGS) {
                // SAFETY: temp is open; the name is NUL-terminated.
                let rc = unsafe { libc::unlinkat(temp.as_raw_fd(), c"clone".as_ptr(), 0) };
                if rc != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
                    return Err(os_err());
                }
            }
            // SAFETY: dp is open; staging is NUL-terminated.
            if unsafe { libc::unlinkat(dp.as_raw_fd(), staging.as_ptr(), libc::AT_REMOVEDIR) } != 0
            {
                return Err(os_err());
            }
            Ok(())
        })();
        if let Err(e) = cleanup {
            return Err(Ok(Fatal::Cleanup(format!(
                "APFS sharing staging cleanup incomplete for {}: {e}",
                rel.display()
            ))));
        }
        result
    }

    fn named_stat(dir: libc::c_int, leaf: &CStr) -> Option<libc::stat> {
        // SAFETY: an all-zero stat is a valid out buffer.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: dir is open; leaf is NUL-terminated; st is valid.
        let rc = unsafe { libc::fstatat(dir, leaf.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        (rc == 0).then_some(st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn writers_and_filters_skip_the_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-q"]);
        std::fs::write(repo.join("a"), "a").unwrap();
        git(repo, &["add", "a"]);
        git(repo, &["commit", "-q", "-m", "a"]);
        assert_eq!(writer_risk(repo, repo), None);
        assert_eq!(filter_risk(repo, b"a\0"), None);

        std::fs::write(repo.join(".gitattributes"), "a filter=lfs\n").unwrap();
        assert_eq!(
            filter_risk(repo, b"a\0").as_deref(),
            Some("checkout filter may have started a writer")
        );

        git(repo, &["config", "core.fsmonitor", "/usr/bin/watcher"]);
        assert!(writer_risk(repo, repo).unwrap().contains("fsmonitor"));
        git(repo, &["config", "core.fsmonitor", "false"]);
        let hook = repo.join(".git/hooks/post-checkout");
        std::fs::write(&hook, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        assert!(writer_risk(repo, repo).unwrap().contains("post-checkout"));
    }

    #[test]
    fn report_lines() {
        assert_eq!(
            Report::skipped("requires macOS APFS").to_string(),
            "APFS sharing skipped: requires macOS APFS"
        );
        let mut r = Report {
            cloned: 2,
            logical_bytes: 300_000,
            ..Report::default()
        };
        r.skip("below_threshold");
        r.skip("below_threshold");
        r.skip("acl");
        assert_eq!(
            r.to_string(),
            "APFS sharing: cloned=2 logical_bytes=300000 acl=1 below_threshold=2"
        );
    }
}

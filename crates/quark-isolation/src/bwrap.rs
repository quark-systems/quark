//! Linux bubblewrap argv.
//!
//! The root is bound read-only, `/dev` is the host's (so ptys and the
//! controlling terminal keep working inside tmux), `/proc` is fresh for the
//! new pid namespace, and `/tmp` and `/var/tmp` stay writable. Writable
//! paths are then bound read-write and readable carve-outs read-only on top,
//! so a carve-out wins inside a writable tree. With network off the process
//! gets its own network namespace, which has only a loopback device.

use std::path::{Path, PathBuf};

use crate::policy::Resolved;

/// Writable on every host, as on macOS.
const TEMP_DIRS: [&str; 2] = ["/tmp", "/var/tmp"];

fn base() -> Vec<String> {
    [
        "--die-with-parent",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--ro-bind",
        "/",
        "/",
        "--dev-bind",
        "/dev",
        "/dev",
        "--proc",
        "/proc",
    ]
    .map(String::from)
    .to_vec()
}

/// The flags [`argv`] uses, running `true`, to check bwrap works here.
pub(crate) fn probe_args() -> Vec<String> {
    let mut a = base();
    a.extend(["--unshare-net", "--", "true"].map(String::from));
    a
}

/// `bwrap` and its flags, up to and including `--`; the command follows.
pub(crate) fn argv(exe: &Path, r: &Resolved) -> Vec<String> {
    let mut a = vec![exe.display().to_string()];
    a.extend(base());
    let temps = TEMP_DIRS
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.is_dir() && !r.writable.contains(p));
    for p in temps.chain(r.writable.iter().cloned()) {
        bind(&mut a, "--bind", &p);
    }
    for p in &r.readable {
        bind(&mut a, "--ro-bind", p);
    }
    if !r.network {
        a.push("--unshare-net".into());
    }
    a.push("--chdir".into());
    a.push(r.cwd.display().to_string());
    a.push("--".into());
    a
}

fn bind(a: &mut Vec<String>, flag: &str, p: &Path) {
    let p = p.display().to_string();
    a.extend([flag.to_string(), p.clone(), p]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carve_outs_follow_writable_binds() {
        let r = Resolved {
            cwd: "/w".into(),
            writable: vec!["/w".into(), "/cache".into()],
            readable: vec!["/w/.git".into()],
            network: false,
        };
        let a = argv(Path::new("/usr/bin/bwrap"), &r).join(" ");
        assert!(a.starts_with("/usr/bin/bwrap --die-with-parent"));
        let w = a.find("--bind /w /w").unwrap();
        let c = a.find("--bind /cache /cache").unwrap();
        let ro = a.find("--ro-bind /w/.git /w/.git").unwrap();
        assert!(w < c && c < ro);
        assert!(a.ends_with("--unshare-net --chdir /w --"));
    }

    #[test]
    fn network_on_keeps_the_host_namespace() {
        let r = Resolved {
            cwd: "/w".into(),
            writable: vec!["/w".into()],
            readable: vec![],
            network: true,
        };
        assert!(!argv(Path::new("bwrap"), &r).contains(&"--unshare-net".to_string()));
    }
}

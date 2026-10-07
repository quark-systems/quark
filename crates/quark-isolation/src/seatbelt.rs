//! macOS Seatbelt profile and `sandbox-exec` argv.
//!
//! The profile allows everything by default and then denies, so harnesses
//! and toolchains keep the Mach services, sysctls and file reads they rely
//! on. In Seatbelt the last matching rule wins, so the order is: deny all
//! writes, allow writes under the writable paths, temp dirs and `/dev`,
//! then deny writes under the read-only carve-outs. With network off, IP
//! traffic is denied and Unix sockets still work.
//!
//! Paths are passed as `-D` parameters, never spliced into the profile, so
//! no path can change its meaning.

use std::path::Path;

use crate::policy::Resolved;

/// Writable on every host: `/tmp`, `/var/tmp` and the per-user temp and
/// cache dirs under `/var/folders` (`$TMPDIR`), plus devices.
const ALWAYS_WRITABLE: &str = r#"(subpath "/private/tmp") (subpath "/private/var/tmp") (subpath "/private/var/folders") (subpath "/dev")"#;

const NO_NETWORK: &str = r#"(deny network-outbound (remote ip "*:*"))
(deny network-inbound (local ip "*:*"))
(deny network-bind (local ip "*:*"))
"#;

/// The profile for `r`, with paths as `W<n>` and `R<n>` parameters.
pub(crate) fn profile(r: &Resolved) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    p.push_str("(allow file-write* ");
    p.push_str(ALWAYS_WRITABLE);
    for i in 0..r.writable.len() {
        p.push_str(&format!(" (subpath (param \"W{i}\"))"));
    }
    p.push_str(")\n");
    if !r.readable.is_empty() {
        p.push_str("(deny file-write*");
        for i in 0..r.readable.len() {
            p.push_str(&format!(" (subpath (param \"R{i}\"))"));
        }
        p.push_str(")\n");
    }
    if !r.network {
        p.push_str(NO_NETWORK);
    }
    p
}

/// `sandbox-exec` with the profile and its parameters; the command follows.
pub(crate) fn argv(exe: &Path, r: &Resolved) -> Vec<String> {
    let mut a = vec![exe.display().to_string(), "-p".into(), profile(r)];
    let params = r
        .writable
        .iter()
        .enumerate()
        .map(|(i, p)| format!("W{i}={}", p.display()))
        .chain(
            r.readable
                .iter()
                .enumerate()
                .map(|(i, p)| format!("R{i}={}", p.display())),
        );
    for kv in params {
        a.push("-D".into());
        a.push(kv);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(network: bool) -> Resolved {
        Resolved {
            cwd: "/w".into(),
            writable: vec!["/w".into(), "/cache dir".into()],
            readable: vec!["/w/.git".into()],
            network,
        }
    }

    #[test]
    fn profile_orders_denies_after_allows() {
        let p = profile(&resolved(false));
        let deny_all = p.find("(deny file-write*)").unwrap();
        let allow = p.find("(allow file-write* ").unwrap();
        let carve = p
            .find("(deny file-write* (subpath (param \"R0\")))")
            .unwrap();
        assert!(deny_all < allow && allow < carve);
        assert!(p.contains("(subpath (param \"W0\")) (subpath (param \"W1\")))"));
        assert!(p.contains("(deny network-outbound (remote ip \"*:*\"))"));
        assert!(!profile(&resolved(true)).contains("network"));
    }

    #[test]
    fn paths_travel_as_parameters() {
        let a = argv(Path::new("/usr/bin/sandbox-exec"), &resolved(false));
        assert_eq!(a[0], "/usr/bin/sandbox-exec");
        assert_eq!(a[1], "-p");
        assert!(!a[2].contains("/cache dir"));
        assert_eq!(
            a[3..],
            ["-D", "W0=/w", "-D", "W1=/cache dir", "-D", "R0=/w/.git"]
        );
    }
}

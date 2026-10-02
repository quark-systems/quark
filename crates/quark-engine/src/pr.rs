//! PR poll records the engine writes when a task's PR is registered.
//!
//! `state/<id>.pr-poll` is five lines: provider, url, host, path, number.
//! `state/<id>.pr-poll-merge-notified` is a version tag plus provider, host,
//! path, number, written once the merge outcome was delivered. Both formats are
//! owned by the engine's `bin/fm-pr-lib.sh`; anything else is refused.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::workspace::read_optional;
use crate::{Error, Result};

const MERGE_NOTIFIED_V1: &str = "fm-pr-poll-merge-notified-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Github,
    Gitlab,
}

impl Provider {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(Self::Github),
            "gitlab" => Some(Self::Gitlab),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrPollRecord {
    pub provider: Provider,
    pub url: String,
    pub host: String,
    /// Repository path, e.g. `owner/repo`.
    pub path: String,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeNotified {
    pub provider: Provider,
    pub host: String,
    pub path: String,
    pub number: u64,
}

pub fn read_poll(path: &Path) -> Result<Option<PrPollRecord>> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    parse_poll(&bytes).map(Some)
}

pub fn read_merge_notified(path: &Path) -> Result<Option<MergeNotified>> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    parse_merge_notified(&bytes).map(Some)
}

pub fn parse_poll(bytes: &[u8]) -> Result<PrPollRecord> {
    const WHAT: &str = "PR poll record";
    let [provider, url, host, path, number] = lines::<5>(bytes, WHAT)?;
    let provider = Provider::parse(provider).ok_or_else(|| bad(WHAT, "unknown provider"))?;
    let number = parse_number(number, WHAT)?;
    check_identity(host, path, WHAT)?;
    // The engine requires the URL to be reconstructible from its components.
    if !url.starts_with(&format!("https://{host}/{path}/")) || !url.ends_with(&format!("/{number}"))
    {
        return Err(bad(WHAT, "url does not match host, path and number"));
    }
    Ok(PrPollRecord {
        provider,
        url: url.to_string(),
        host: host.to_string(),
        path: path.to_string(),
        number,
    })
}

pub fn parse_merge_notified(bytes: &[u8]) -> Result<MergeNotified> {
    const WHAT: &str = "merge-notified marker";
    let [version, provider, host, path, number] = lines::<5>(bytes, WHAT)?;
    if version != MERGE_NOTIFIED_V1 {
        return Err(bad(WHAT, &format!("unknown version {version:?}")));
    }
    let provider = Provider::parse(provider).ok_or_else(|| bad(WHAT, "unknown provider"))?;
    check_identity(host, path, WHAT)?;
    Ok(MergeNotified {
        provider,
        host: host.to_string(),
        path: path.to_string(),
        number: parse_number(number, WHAT)?,
    })
}

fn lines<'a, const N: usize>(bytes: &'a [u8], what: &'static str) -> Result<[&'a str; N]> {
    let text = std::str::from_utf8(bytes).map_err(|_| bad(what, "not UTF-8"))?;
    let body = text
        .strip_suffix('\n')
        .ok_or_else(|| bad(what, "missing trailing newline"))?;
    let parts: Vec<&str> = body.split('\n').collect();
    parts
        .try_into()
        .map_err(|v: Vec<&str>| bad(what, &format!("expected {N} lines, found {}", v.len())))
}

fn parse_number(s: &str, what: &'static str) -> Result<u64> {
    match s.parse::<u64>() {
        Ok(n) if n > 0 && !s.starts_with('0') => Ok(n),
        _ => Err(bad(what, "number is not a positive integer")),
    }
}

fn check_identity(host: &str, path: &str, what: &'static str) -> Result<()> {
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
    let path_ok = !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        });
    if host_ok && path_ok {
        Ok(())
    } else {
        Err(bad(what, "invalid host or path"))
    }
}

fn bad(what: &'static str, detail: &str) -> Error {
    Error::Malformed {
        what,
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_poll_record() {
        let r = parse_poll(
            b"github\nhttps://github.com/quark-systems/quark/pull/12\ngithub.com\nquark-systems/quark\n12\n",
        )
        .unwrap();
        assert_eq!(r.provider, Provider::Github);
        assert_eq!(r.number, 12);
        assert_eq!(r.path, "quark-systems/quark");
    }

    #[test]
    fn gitlab_nested_group() {
        let r = parse_poll(
            b"gitlab\nhttps://gitlab.com/g/sub/repo/-/merge_requests/3\ngitlab.com\ng/sub/repo\n3\n",
        )
        .unwrap();
        assert_eq!(r.provider, Provider::Gitlab);
    }

    #[test]
    fn refuses_legacy_and_doctored_records() {
        // Pre-provider sidecar: URL first, one line short.
        assert!(parse_poll(b"https://github.com/a/b/pull/1\ngithub.com\na/b\n1\n").is_err());
        // URL pointing elsewhere.
        assert!(
            parse_poll(b"github\nhttps://evil.example/a/b/pull/1\ngithub.com\na/b\n1\n").is_err()
        );
        // Extra line, bad number, traversal.
        assert!(
            parse_poll(b"github\nhttps://github.com/a/b/pull/1\ngithub.com\na/b\n1\nx\n").is_err()
        );
        assert!(
            parse_poll(b"github\nhttps://github.com/a/b/pull/01\ngithub.com\na/b\n01\n").is_err()
        );
        assert!(
            parse_poll(b"github\nhttps://github.com/../b/pull/1\ngithub.com\n../b\n1\n").is_err()
        );
    }

    #[test]
    fn merge_notified_marker() {
        let m = parse_merge_notified(b"fm-pr-poll-merge-notified-v1\ngithub\ngithub.com\na/b\n7\n")
            .unwrap();
        assert_eq!(m.number, 7);
        assert!(parse_merge_notified(
            b"fm-pr-poll-merge-notified-v2\ngithub\ngithub.com\na/b\n7\n"
        )
        .is_err());
    }

    #[test]
    fn absent_files_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_poll(&dir.path().join("x.pr-poll")).unwrap(), None);
        assert_eq!(read_merge_notified(&dir.path().join("x.m")).unwrap(), None);
    }
}

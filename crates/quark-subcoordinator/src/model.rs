//! What a sub-coordinator is: its charter, where it lives, what it runs on
//! and which projects it holds.

use std::path::{Component, Path, PathBuf};

use quark_core::{CoreError, HostId, ProjectId, Result};
use quark_runtime::RuntimeSpec;
use serde::{Deserialize, Serialize};

/// Longest sub-coordinator id.
pub const MAX_ID: usize = 40;

/// Ids are path- and session-safe: lower-case letters, digits and `-`,
/// starting with a letter or digit.
pub fn check_id(id: &ProjectId) -> Result<()> {
    let s = id.as_str();
    let ok = !s.is_empty()
        && s.len() <= MAX_ID
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!(
            "sub-coordinator id {s:?}: use up to {MAX_ID} lower-case letters, digits and dashes"
        )))
    }
}

/// What work routes to the sub-coordinator and how it should work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Charter {
    /// One line: the kind of work this sub-coordinator takes.
    pub scope: String,
    /// Standing instructions, read at every launch.
    #[serde(default)]
    pub instructions: String,
}

/// Where the sub-coordinator lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub host: HostId,
    pub runtime: RuntimeSpec,
    /// Its home directory on that host, absolute.
    pub home: PathBuf,
}

impl Placement {
    pub fn validate(&self) -> Result<()> {
        self.runtime.validate()?;
        if !self.home.is_absolute() {
            return Err(CoreError::Invalid(format!(
                "home {} is not an absolute path",
                self.home.display()
            )));
        }
        if self.home.components().count() < 3
            || self
                .home
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(CoreError::Invalid(format!(
                "home {} must be a plain path at least two levels below /",
                self.home.display()
            )));
        }
        Ok(())
    }

    /// Whether `other` shares this home or sits inside or above it.
    pub fn overlaps(&self, other: &Placement) -> bool {
        self.host == other.host
            && (self.home.starts_with(&other.home) || other.home.starts_with(&self.home))
    }
}

/// The harness the sub-coordinator's agent runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Harness manifest id or alias.
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// A project the sub-coordinator holds, cloned on its host from `origin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSource {
    pub name: String,
    pub origin: String,
}

impl ProjectSource {
    /// Refuses names that are not one plain path component and origins
    /// git could read as an option or a remote helper.
    pub fn validate(&self) -> Result<()> {
        let n = &self.name;
        let name_ok = !n.is_empty()
            && !n.starts_with('.')
            && !n.starts_with('-')
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
        if !name_ok {
            return Err(CoreError::Invalid(format!("project name {n:?}")));
        }
        let o = &self.origin;
        let plain = !o.is_empty()
            && !o.starts_with('-')
            && !o.chars().any(|c| c.is_whitespace() || c.is_control());
        let lower = o.to_ascii_lowercase();
        let known = ["https://", "http://", "ssh://", "git://", "file://"]
            .iter()
            .any(|p| lower.starts_with(p))
            || Path::new(o).is_absolute()
            || scp_like(o);
        if !plain || !known || lower.contains("::") {
            return Err(CoreError::Invalid(format!(
                "origin {o:?} for project {n}: use an https, ssh, git or file URL, \
                 user@host:path, or an absolute path"
            )));
        }
        Ok(())
    }
}

/// `user@host:path` or `host:path`, git's scp-like syntax.
fn scp_like(o: &str) -> bool {
    match o.split_once(':') {
        Some((host, path)) => {
            !host.is_empty()
                && !path.is_empty()
                && !host.contains('/')
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-@".contains(c))
        }
        None => false,
    }
}

/// Everything recorded when a sub-coordinator is registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub id: ProjectId,
    pub charter: Charter,
    pub placement: Placement,
    pub profile: Profile,
    #[serde(default)]
    pub projects: Vec<ProjectSource>,
}

impl Registration {
    pub fn validate(&self) -> Result<()> {
        check_id(&self.id)?;
        if self.charter.scope.trim().is_empty() {
            return Err(CoreError::Invalid("the charter needs a scope".into()));
        }
        self.placement.validate()?;
        let mut names = std::collections::BTreeSet::new();
        for p in &self.projects {
            p.validate()?;
            if !names.insert(&p.name) {
                return Err(CoreError::Invalid(format!(
                    "project {} listed twice",
                    p.name
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(name: &str, origin: &str) -> ProjectSource {
        ProjectSource {
            name: name.into(),
            origin: origin.into(),
        }
    }

    #[test]
    fn ids() {
        assert!(check_id(&ProjectId::from("web-2")).is_ok());
        for bad in ["", "Web", "a_b", "-a", "a/b", &"x".repeat(41)] {
            assert!(check_id(&ProjectId::from(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn origins() {
        for ok in [
            "git@github.com:quark-systems/quark.git",
            "https://github.com/o/r.git",
            "ssh://git@host/r.git",
            "/srv/git/r.git",
            "file:///srv/git/r.git",
        ] {
            assert!(src("r", ok).validate().is_ok(), "{ok}");
        }
        for bad in [
            "--upload-pack=x",
            "ext::sh -c x",
            "relative/path",
            "https://h/a b",
            "",
        ] {
            assert!(src("r", bad).validate().is_err(), "{bad}");
        }
        for bad in ["", ".git", "a/b", "-x"] {
            assert!(src(bad, "/srv/r").validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn homes() {
        let p = |home: &str| Placement {
            host: HostId::from("h"),
            runtime: RuntimeSpec::Local,
            home: home.into(),
        };
        assert!(p("/srv/quark/web").validate().is_ok());
        assert!(p("relative").validate().is_err());
        assert!(p("/srv").validate().is_err());
        assert!(p("/srv/../etc").validate().is_err());
        assert!(p("/srv/q").overlaps(&p("/srv/q/web")));
        assert!(!p("/srv/q/a").overlaps(&p("/srv/q/b")));
    }
}

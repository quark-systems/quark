//! In-memory implementations of the contracts.
//!
//! For unit tests, and for a subsystem whose real dependency has not landed
//! yet (the worker protocol writes to [`MemoryEventLog`] until
//! `quark-eventlog` exists). None of them touch the filesystem.

mod event_log;
mod hosts;
mod session;
mod verify;
mod worktree;

pub use event_log::MemoryEventLog;
pub use hosts::MemoryHosts;
pub use session::{FakeSessions, Pty};
pub use verify::{FakeGuard, FakeVerify};
pub use worktree::FakeWorktrees;

use async_trait::async_trait;

use crate::harness::{HarnessManifest, HarnessRegistry};
use crate::persona::{PersonaPack, PersonaSource};
use crate::{CoreError, ProjectId, Result};

/// A fixed list of harness manifests.
#[derive(Debug, Clone, Default)]
pub struct StaticHarnesses(pub Vec<HarnessManifest>);

#[async_trait]
impl HarnessRegistry for StaticHarnesses {
    async fn list(&self) -> Result<Vec<HarnessManifest>> {
        Ok(self.0.clone())
    }
}

/// A fixed set of packs; every Project uses `default` unless overridden.
#[derive(Debug, Clone, Default)]
pub struct StaticPersonas {
    pub packs: Vec<PersonaPack>,
    pub default: String,
    pub overrides: std::collections::BTreeMap<ProjectId, String>,
}

#[async_trait]
impl PersonaSource for StaticPersonas {
    async fn packs(&self) -> Result<Vec<PersonaPack>> {
        Ok(self.packs.clone())
    }

    async fn for_project(&self, project: &ProjectId) -> Result<PersonaPack> {
        let id = self.overrides.get(project).unwrap_or(&self.default);
        self.packs
            .iter()
            .find(|p| &p.id == id)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("persona {id}")))
    }
}

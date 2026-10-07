//! Identifiers shared by every contract.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Host-independent event id (UUIDv7), so logs from several hosts merge
/// without renumbering. Appending an event whose id is already in the log is
/// a no-op that returns the existing [`crate::Seq`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub Uuid);

impl EventId {
    /// A fresh time-ordered id.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
    };
}

string_id!(
    /// A machine that runs Quark work: the local Mac, an SSH host, a hosted
    /// or private-cloud runtime. Stable across restarts.
    HostId
);
string_id!(
    /// A Quark Project, as quarkd names it.
    ProjectId
);
string_id!(
    /// A task, stable for its whole life including relaunches.
    TaskId
);

impl ProjectId {
    /// The project of events about the engine itself rather than one
    /// Project, such as slice mode changes.
    pub const ENGINE: &'static str = "_engine";

    pub fn engine() -> Self {
        Self::new(Self::ENGINE)
    }
}

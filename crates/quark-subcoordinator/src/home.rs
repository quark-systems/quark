//! A sub-coordinator's home on its host: the layout, the instructions its
//! agent reads, and seeding.
//!
//! ```text
//! <home>/.quark-subcoordinator.json  identity: which sub-coordinator owns the home
//! <home>/CHARTER.md                  scope and standing instructions
//! <home>/BRIEF.md                    what the current agent was started with
//! <home>/inbox/                      messages from the parent, one file each
//! <home>/inbox/handled/              messages the agent has acted on
//! <home>/outbox/parent.status        the parent channel, append-only lines
//! <home>/inherited/                  configuration pushed by the parent
//! <home>/projects/<name>/            the projects, cloned on the host
//! ```

use std::path::{Path, PathBuf};

use quark_core::{CoreError, ProjectId, Result};
use quark_runtime::{fs, Cmd, Exec};
use serde::{Deserialize, Serialize};

use crate::model::Registration;

/// The identity file's schema.
pub const IDENTITY_SCHEMA: &str = "quark-subcoordinator.v1";

/// Paths inside one home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub home: PathBuf,
}

impl Layout {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    pub fn identity(&self) -> PathBuf {
        self.home.join(".quark-subcoordinator.json")
    }

    pub fn charter(&self) -> PathBuf {
        self.home.join("CHARTER.md")
    }

    pub fn brief(&self) -> PathBuf {
        self.home.join("BRIEF.md")
    }

    pub fn inbox(&self) -> PathBuf {
        self.home.join("inbox")
    }

    pub fn handled(&self) -> PathBuf {
        self.inbox().join("handled")
    }

    pub fn channel(&self) -> PathBuf {
        self.home.join("outbox").join("parent.status")
    }

    pub fn inherited(&self) -> PathBuf {
        self.home.join("inherited")
    }

    pub fn projects(&self) -> PathBuf {
        self.home.join("projects")
    }

    /// The inbox file for message `n` (its order) with id `id`.
    pub fn message(&self, n: usize, id: &str, suffix: &str) -> PathBuf {
        self.inbox().join(message_name(n, id, suffix))
    }
}

/// `0007-<id>.<suffix>`: sorts in send order.
pub fn message_name(n: usize, id: &str, suffix: &str) -> String {
    format!("{n:04}-{id}.{suffix}")
}

/// The message id inside an inbox file name, if it is one of ours.
pub fn message_id(name: &str) -> Option<&str> {
    let (_, rest) = name.split_once('-')?;
    let (id, _) = rest.split_once('.')?;
    (!id.is_empty()).then_some(id)
}

/// Who owns a home, written when it is seeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub schema: String,
    pub id: ProjectId,
    /// The host the parent engine runs on.
    pub parent: String,
}

/// The charter file.
pub fn charter_text(r: &Registration) -> String {
    let mut out = format!("# {}\n\nScope: {}\n", r.id, r.charter.scope.trim());
    if !r.charter.instructions.trim().is_empty() {
        out.push('\n');
        out.push_str(r.charter.instructions.trim());
        out.push('\n');
    }
    out
}

/// What the agent is started with: who it is, where things are, and the
/// contract for its inbox and parent channel.
pub fn brief(r: &Registration, note: Option<&str>) -> String {
    let l = Layout::new(&r.placement.home);
    let mut out = String::new();
    if let Some(note) = note.filter(|n| !n.trim().is_empty()) {
        out.push_str(note.trim());
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "You are the coordinator for \"{}\". Scope: {}\n",
        r.id,
        r.charter.scope.trim()
    ));
    if !r.charter.instructions.trim().is_empty() {
        out.push('\n');
        out.push_str(r.charter.instructions.trim());
        out.push('\n');
    }
    out.push_str(&format!("\nYour home is {}.", r.placement.home.display()));
    if r.projects.is_empty() {
        out.push_str(" You hold no projects yet.\n");
    } else {
        let names: Vec<&str> = r.projects.iter().map(|p| p.name.as_str()).collect();
        out.push_str(&format!(
            " Your projects are cloned in {}: {}.\n",
            l.projects().display(),
            names.join(", ")
        ));
    }
    out.push_str(&format!(
        "\nWork arrives only from your parent. Its messages are files in {inbox}, oldest first by name. \
         Read each one and act on it, then move it into {handled} keeping its name; that is how your parent \
         knows you took it. A file ending in .handoff.json moves work items into your queue. \
         Check the inbox now and whenever you are told a message arrived. With nothing in it, wait.\n\
         \nConfiguration your parent shares is in {inherited}; re-read it when told it changed.\n\
         \nReport to your parent by appending one line per event to {channel}, such as \
         `working: <note>`, `blocked: <reason>`, `needs-decision [key=<slug>]: <question>`, \
         `learned: <fact>` or `done: <summary or PR url>`. Never edit or remove earlier lines.\n",
        inbox = l.inbox().display(),
        handled = l.handled().display(),
        inherited = l.inherited().display(),
        channel = l.channel().display(),
    ));
    out
}

fn arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Create the home on its host, or check an existing one is this
/// sub-coordinator's, then clone any project not there yet. Safe to run
/// again. Never takes over a non-empty directory it did not create, and
/// never replaces a project clone with a different origin.
pub async fn seed(exec: &dyn Exec, r: &Registration, parent: &str) -> Result<()> {
    let l = Layout::new(&r.placement.home);
    match fs::read(exec, &l.identity()).await? {
        Some(bytes) => {
            let id: Identity = serde_json::from_slice(&bytes).map_err(|e| {
                CoreError::Refused(format!(
                    "{} is not a readable sub-coordinator identity: {e}",
                    l.identity().display()
                ))
            })?;
            if id.id != r.id {
                return Err(CoreError::Refused(format!(
                    "{} belongs to sub-coordinator {}",
                    l.home.display(),
                    id.id
                )));
            }
        }
        None => {
            if !fs::list(exec, &l.home).await?.is_empty() {
                return Err(CoreError::Refused(format!(
                    "{} already has files and is not a sub-coordinator home",
                    l.home.display()
                )));
            }
        }
    }
    fs::mkdir_all(
        exec,
        &[
            &l.handled(),
            l.channel().parent().unwrap_or(&l.home),
            &l.inherited(),
            &l.projects(),
        ],
    )
    .await?;
    if fs::read(exec, &l.channel()).await?.is_none() {
        fs::write(exec, &l.channel(), b"").await?;
    }
    let identity = Identity {
        schema: IDENTITY_SCHEMA.into(),
        id: r.id.clone(),
        parent: parent.into(),
    };
    let json = serde_json::to_vec_pretty(&identity)
        .map_err(|e| CoreError::Backend(format!("identity: {e}")))?;
    fs::write(exec, &l.identity(), &json).await?;
    fs::write(exec, &l.charter(), charter_text(r).as_bytes()).await?;
    for p in &r.projects {
        let dir = l.projects().join(&p.name);
        if fs::exists(exec, &dir).await? {
            let out = exec
                .run(&Cmd::new([
                    "git",
                    "-C",
                    &arg(&dir),
                    "remote",
                    "get-url",
                    "origin",
                ]))
                .await?
                .check(&format!("reading the origin of {}", dir.display()))?;
            let have = out.stdout_str().trim().to_string();
            if have != p.origin {
                return Err(CoreError::Refused(format!(
                    "{} is a clone of {have}, not {}",
                    dir.display(),
                    p.origin
                )));
            }
            continue;
        }
        exec.run(
            &Cmd::new(["git", "clone", "--quiet", "--", &p.origin, &arg(&dir)])
                .timeout(std::time::Duration::from_secs(15 * 60)),
        )
        .await?
        .check(&format!("cloning {} into {}", p.origin, dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_names_round_trip() {
        let n = message_name(7, "abc123", "handoff.json");
        assert_eq!(n, "0007-abc123.handoff.json");
        assert_eq!(message_id(&n), Some("abc123"));
        assert_eq!(message_id("notes.txt"), None);
    }
}

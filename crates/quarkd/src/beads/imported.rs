//! Beads for tasks imported from another tool's backlog (firstmate): one
//! bead per task, created with its status, brief and links, then linked to
//! the tasks it waits on and gated when a person holds it.

use std::path::Path;

use serde::Deserialize;

use super::{Beads, BeadsError};
use crate::firstmate_import::{SeedIssue, SeedStatus};

#[derive(Deserialize)]
struct Created {
    id: String,
}

impl Beads {
    /// Creates `issue` as a bead and returns its id. Its waits on other
    /// tasks are added afterwards with [`Beads::import_wait`].
    pub async fn import_issue(
        &self,
        home: &Path,
        project_id: &str,
        issue: &SeedIssue,
    ) -> Result<String, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        let tmp = |body: &str| -> Result<tempfile::NamedTempFile, BeadsError> {
            let file = tempfile::Builder::new()
                .prefix("quark-import-")
                .suffix(".md")
                .tempfile_in(&dir)
                .map_err(|e| BeadsError::Invalid(e.to_string()))?;
            std::fs::write(file.path(), body).map_err(|e| BeadsError::Invalid(e.to_string()))?;
            Ok(file)
        };
        let body = tmp(&issue.description)?;
        let body_path = body.path().to_string_lossy().into_owned();
        let design = issue.design.as_deref().map(tmp).transpose()?;
        let design_path = design
            .as_ref()
            .map(|f| f.path().to_string_lossy().into_owned());
        let labels = issue.labels.join(",");
        let priority = issue.priority.to_string();
        let mut meta = serde_json::json!({ "firstmate_task": issue.key });
        if let Some(pr) = &issue.pull_request {
            meta["pull_request"] = pr.as_str().into();
        }
        let meta = meta.to_string();
        let mut args = vec![
            "create",
            "--type",
            "task",
            "--title",
            &issue.title,
            "--body-file",
            &body_path,
            "--labels",
            &labels,
            "--priority",
            &priority,
            "--metadata",
            &meta,
        ];
        if let Some(path) = &design_path {
            args.extend(["--design-file", path]);
        }
        let created: Created = self.json(&dir, &args).await?;
        let id = created.id;
        match &issue.status {
            SeedStatus::Open => {}
            SeedStatus::InProgress => {
                self.bd(&dir, &["update", &id, "--status", "in_progress"], &[])
                    .await?;
            }
            SeedStatus::Closed { reason } => {
                self.bd(&dir, &["close", &id, "--reason", reason], &[])
                    .await?;
            }
        }
        if let Some(hold) = &issue.hold {
            let title: String = format!("Captain hold: {hold}").chars().take(200).collect();
            self.bd(
                &dir,
                &[
                    "gate", "create", "--type", "human", "--blocks", &id, "--title", &title,
                    "--reason", hold,
                ],
                &[],
            )
            .await?;
        }
        Ok(id)
    }

    /// Records that bead `id` waits on bead `on`.
    pub async fn import_wait(
        &self,
        home: &Path,
        project_id: &str,
        id: &str,
        on: &str,
    ) -> Result<(), BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        self.bd(&dir, &["dep", "add", id, on], &[]).await.map(drop)
    }
}

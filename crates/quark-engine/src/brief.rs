//! A task's brief as dispatched: `data/<id>/brief.md`, and the rendered
//! launch brief `data/<id>/launch-brief.md` when the engine kept one.
//!
//! Both live in the engine's `data/`, which outlives the task's worker and
//! its `state/` records, so a finished task's brief can still be read.

use std::path::Path;
use std::time::SystemTime;

use crate::workspace::{read_optional, Workspace};
use crate::{Error, Result};

/// The rendered launch brief beside `brief.md`.
pub const LAUNCH_FILE: &str = "launch-brief.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// `brief.md` as the coordinator wrote it.
    pub text: String,
    /// `launch-brief.md`, the brief as the worker was started with it.
    pub launch: Option<String>,
    /// When `brief.md` was last written, in Unix seconds.
    pub written_at: Option<i64>,
}

/// Reads a task's brief. `None` when the task has no `brief.md`.
pub fn read(ws: &Workspace, task_id: &str) -> Result<Option<Brief>> {
    let path = ws.brief_path(task_id)?;
    let Some(bytes) = read_optional(&path)? else {
        return Ok(None);
    };
    let launch = path.parent().map(|d| d.join(LAUNCH_FILE));
    let launch = match launch {
        Some(p) => read_optional(&p)?.map(|b| String::from_utf8_lossy(&b).into_owned()),
        None => None,
    };
    Ok(Some(Brief {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        launch,
        written_at: modified(&path)?,
    }))
}

fn modified(path: &Path) -> Result<Option<i64>> {
    let meta = std::fs::metadata(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_brief_and_its_launch_brief() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path(), "/e");
        assert_eq!(read(&ws, "t1").unwrap(), None);
        let data = dir.path().join("data/t1");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("brief.md"), "# Task\n").unwrap();
        let b = read(&ws, "t1").unwrap().unwrap();
        assert_eq!(b.text, "# Task\n");
        assert_eq!(b.launch, None);
        assert!(b.written_at.is_some());
        std::fs::write(data.join(LAUNCH_FILE), "You are a crewmate.\n").unwrap();
        let b = read(&ws, "t1").unwrap().unwrap();
        assert_eq!(b.launch.as_deref(), Some("You are a crewmate.\n"));
    }

    #[test]
    fn refuses_ids_that_leave_data() {
        let ws = Workspace::new("/h", "/e");
        assert!(read(&ws, "../t1").is_err());
    }
}

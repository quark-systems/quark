//! The spawn fields of a task's metadata record, `state/<id>.meta`.
//!
//! `bin/fm-spawn.sh` owns the format: one `key=value` per line, rewritten on
//! every spawn and relaunch. Only the fields that say which agent the worker
//! was started with are read here; every other key is ignored.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::workspace::read_optional;
use crate::Result;

/// Which agent a task's current worker was started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnMeta {
    /// `spawn_gen`: new on every spawn and relaunch, e.g. `s1790000000.4242.17`.
    pub generation: String,
    pub harness: String,
    /// `None` when the engine recorded the harness default.
    pub model: Option<String>,
    /// `None` when the engine recorded the harness default.
    pub effort: Option<String>,
    /// The repo checkout the worker branched from.
    pub project: Option<String>,
    /// `kind`, e.g. `ship`, `scout` or `secondmate`.
    pub kind: Option<String>,
}

impl SpawnMeta {
    /// When the worker was spawned, in Unix seconds, read from the
    /// generation's `s<seconds>.` prefix.
    pub fn spawned_at(&self) -> Option<i64> {
        self.generation
            .strip_prefix('s')?
            .split('.')
            .next()?
            .parse()
            .ok()
    }

    /// The project's registry name: the last component of its checkout path.
    pub fn project_name(&self) -> Option<&str> {
        let p = self.project.as_deref()?.trim_end_matches('/');
        p.rsplit('/').next().filter(|n| !n.is_empty())
    }
}

/// Reads a task's spawn fields. `None` when the record is absent or names no
/// harness or generation yet (a provisional record mid-spawn).
pub fn read(path: &Path) -> Result<Option<SpawnMeta>> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    Ok(parse(&String::from_utf8_lossy(&bytes)))
}

pub fn parse(text: &str) -> Option<SpawnMeta> {
    let value = |key: &str| {
        text.lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v.trim())
            .next_back()
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let explicit = |key: &str| value(key).filter(|v| v != "default");
    Some(SpawnMeta {
        generation: value("spawn_gen")?,
        harness: value("harness")?,
        model: explicit("model"),
        effort: explicit("effort"),
        project: value("project"),
        kind: value("kind"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = "window=fm:3\nendpoint_task_id=t1\nworktree=/w/t1\nproject=/h/projects/quark\n\
                        harness=codex\nkind=ship\nmode=direct-pr\ntasktmp=/tmp/x\nmodel=gpt-5.6-luna\n\
                        effort=default\nspawn_gen=s1790000000.4242.17\n";

    #[test]
    fn reads_the_spawn_fields() {
        let m = parse(META).unwrap();
        assert_eq!(m.harness, "codex");
        assert_eq!(m.model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(m.effort, None, "default is the harness default");
        assert_eq!(m.generation, "s1790000000.4242.17");
        assert_eq!(m.spawned_at(), Some(1_790_000_000));
        assert_eq!(m.project_name(), Some("quark"));
        assert_eq!(m.kind.as_deref(), Some("ship"));
    }

    #[test]
    fn provisional_records_are_not_spawns() {
        assert_eq!(parse("window=fm:3\nharness=claude\n"), None);
        assert_eq!(parse("spawn_gen=s1.2.3\n"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(&dir.path().join("t1.meta")).unwrap(), None);
    }
}

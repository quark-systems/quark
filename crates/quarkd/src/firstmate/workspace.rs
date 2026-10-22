//! Taking firstmate out of the workspaces it made.
//!
//! Before firstmate was retired, a Project workspace was a firstmate
//! secondmate: a clone of firstmate's engine (`AGENTS.md`, `CLAUDE.md`,
//! `.claude/` with its hooks and skills, `bin/`, ...) with its `data/` and
//! `state/`, and Quark's own files beside them. The coordinator started in
//! it still loaded firstmate's instructions and skills, whose scripts no
//! longer run. The command-center workspace was a firstmate primary, with
//! the same `data/` and `state/`.
//!
//! [`retire`] runs when quarkd starts. Each workspace under
//! `<home>/workspaces` that still carries firstmate has those files moved,
//! not deleted, to `<home>/backups/firstmate/<workspace>-<time>/`, and a
//! Project workspace is then claimed as a native one
//! (`.quark/workspace.json`). What Quark keeps is left where it is: the
//! Project repo checkout (`project/`), the code clones (`repos/`, or
//! `projects/` where firstmate cloned them), `.quark/`, `config/`,
//! `verify/`, `.mcp.json` and `.claude/settings.local.json`. A backup is a
//! firstmate home that `quarkd import firstmate` can still read.
//!
//! A cleaned workspace has no firstmate marker left, so the next start
//! leaves it alone. A coordinator already running keeps what it loaded until
//! it is restarted.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::provision::Layout;

/// Files only a firstmate secondmate home has.
const SECONDMATE_MARKERS: &[&str] = &[".fm-secondmate-home", ".fm-secondmate-parent"];
/// A file only a firstmate primary home has.
const PRIMARY_MARKER: &str = "data/secondmates.md";

/// What Quark keeps at the top of a workspace.
const KEEP: &[&str] = &[
    "project",
    "projects",
    crate::engine::native_project::REPOS_DIR,
    crate::engine::native_project::STATE_DIR,
    "config",
    "verify",
    "coordinators",
    ".mcp.json",
    ".claude",
];
/// What Quark keeps in `.claude/`: the coordinator's MCP and hook settings.
const KEEP_CLAUDE: &[&str] = &["settings.local.json"];

/// firstmate's own top-level files, for a clone whose git index cannot be
/// read. The files its git tracks are moved too.
const FIRSTMATE_FILES: &[&str] = &[
    ".git",
    ".agents",
    ".bob",
    ".codex",
    ".cursor",
    ".gitattributes",
    ".github",
    ".gitignore",
    ".greptile",
    ".grok",
    ".no-mistakes",
    ".no-mistakes.yaml",
    ".omp",
    ".opencode",
    ".pi",
    ".tasks.toml",
    ".lavish",
    ".tools",
    "AGENTS.md",
    "CLAUDE.md",
    "CONTRIBUTING.md",
    "GROK_BOT.md",
    "LICENSE",
    "README.md",
    "VISION.md",
    "assets",
    "bin",
    "data",
    "docs",
    "skills",
    "state",
    "tests",
    ".fm-secondmate-home",
    ".fm-secondmate-parent",
];

/// A workspace that had firstmate taken out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retired {
    pub workspace: PathBuf,
    pub backup: PathBuf,
    /// What was moved, relative to the workspace.
    pub moved: Vec<String>,
}

/// Take firstmate out of every workspace under `layout`. Never fails: a
/// workspace that cannot be cleaned is logged and left for the next start.
pub fn retire(layout: &Layout) -> Vec<Retired> {
    let Ok(entries) = std::fs::read_dir(layout.home.join("workspaces")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let stamp = stamp();
    let mut out = Vec::new();
    for dir in dirs {
        match retire_one(layout, &dir, &stamp) {
            Ok(Some(r)) => {
                tracing::info!(
                    workspace = %r.workspace.display(),
                    backup = %r.backup.display(),
                    moved = r.moved.len(),
                    "moved firstmate's files out of a workspace"
                );
                out.push(r);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(
                workspace = %dir.display(),
                error = %e,
                "could not move firstmate's files out of a workspace"
            ),
        }
    }
    out
}

fn retire_one(layout: &Layout, ws: &Path, stamp: &str) -> std::io::Result<Option<Retired>> {
    let secondmate = SECONDMATE_MARKERS.iter().any(|m| ws.join(m).exists());
    let primary = ws.join(PRIMARY_MARKER).exists();
    if !secondmate && !primary {
        return Ok(None);
    }
    let name = ws
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut names: BTreeSet<String> = FIRSTMATE_FILES.iter().map(|s| s.to_string()).collect();
    if secondmate {
        names.extend(tracked(ws));
    }
    names.retain(|n| !KEEP.contains(&n.as_str()));
    let mut moving: Vec<String> = names
        .into_iter()
        .filter(|n| ws.join(n).symlink_metadata().is_ok())
        .collect();
    let claude = ws.join(".claude");
    if secondmate {
        if let Ok(rd) = std::fs::read_dir(&claude) {
            let mut extra: Vec<String> = rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !KEEP_CLAUDE.contains(&n.as_str()))
                .map(|n| format!(".claude/{n}"))
                .collect();
            extra.sort();
            moving.extend(extra);
        }
    }

    let backup = layout
        .home
        .join("backups/firstmate")
        .join(format!("{name}-{stamp}"));
    std::fs::create_dir_all(&backup)?;
    let mut moved = Vec::new();
    for rel in &moving {
        let to = backup.join(rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(ws.join(rel), &to)?;
        moved.push(rel.clone());
    }
    std::fs::write(backup.join("QUARK-BACKUP.md"), note(ws, &moved))?;
    if secondmate && name.starts_with("prj_") {
        crate::engine::native_project::adopt(ws, &name)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }
    Ok(Some(Retired {
        workspace: ws.to_path_buf(),
        backup,
        moved,
    }))
}

/// The top-level names firstmate's clone at `ws` tracks.
fn tracked(ws: &Path) -> Vec<String> {
    if !ws.join(".git").exists() {
        return Vec::new();
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(["ls-files", "-z"])
        .output();
    let Some(out) = out.ok().filter(|o| o.status.success()) else {
        return Vec::new();
    };
    out.stdout
        .split(|b| *b == 0)
        .filter_map(|f| std::str::from_utf8(f).ok())
        .filter_map(|f| f.split('/').next())
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect()
}

fn note(ws: &Path, moved: &[String]) -> String {
    let mut s = format!(
        "# firstmate files from {}\n\n\
         quarkd moved these out of the workspace when it started, since Quark no longer runs \
         firstmate. Nothing else was changed. To put a file back, move it to the same path in \
         the workspace. `quarkd import firstmate <this folder>` reads it as a firstmate home.\n\n",
        ws.display()
    );
    for m in moved {
        s.push_str(&format!("- `{m}`\n"));
    }
    s
}

/// The current UTC time as `YYYYMMDDTHHMMSSZ`.
fn stamp() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A secondmate workspace as firstmate seeded it, with Quark's files.
    fn secondmate(ws: &Path) {
        write(&ws.join("AGENTS.md"), "firstmate\n");
        write(&ws.join("bin/fm-ahoy.sh"), "#!/bin/sh\n");
        write(&ws.join(".agents/skills/ahoy/SKILL.md"), "ahoy\n");
        write(&ws.join(".claude/settings.json"), "{}\n");
        write(&ws.join("extra-tracked.md"), "only git knows\n");
        git(ws, &["init", "-q"]);
        git(ws, &["add", "."]);
        std::os::unix::fs::symlink("../.agents/skills", ws.join(".claude/skills")).unwrap();
        write(&ws.join(".fm-secondmate-home"), "prj_1\n");
        write(&ws.join("data/backlog.md"), "## Queued\n");
        write(&ws.join("state/x.status"), "running\n");
        write(&ws.join(".claude/settings.local.json"), "{\"hooks\":{}}\n");
        write(&ws.join(".mcp.json"), "{}\n");
        write(&ws.join(".quark/coordinator.md"), "prompt\n");
        write(&ws.join("config/gates.json"), "{}\n");
        write(&ws.join("project/memory/0001.md"), "Rebase first.\n");
        write(
            &ws.join("projects/quark/.git/HEAD"),
            "ref: refs/heads/main\n",
        );
    }

    #[test]
    fn moves_firstmate_out_and_keeps_quark() {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::new(home.path());
        let ws = layout.workspace("prj_1");
        secondmate(&ws);

        let done = retire(&layout);
        assert_eq!(done.len(), 1);
        let backup = &done[0].backup;
        assert!(backup.starts_with(home.path().join("backups/firstmate")));
        for gone in [
            "AGENTS.md",
            "bin",
            ".agents",
            ".git",
            "data",
            "state",
            "extra-tracked.md",
            ".fm-secondmate-home",
            ".claude/settings.json",
            ".claude/skills",
        ] {
            assert!(
                ws.join(gone).symlink_metadata().is_err(),
                "{gone} still in the workspace"
            );
            assert!(
                backup.join(gone).symlink_metadata().is_ok(),
                "{gone} not backed up"
            );
        }
        for kept in [
            ".claude/settings.local.json",
            ".mcp.json",
            ".quark/coordinator.md",
            "config/gates.json",
            "project/memory/0001.md",
            "projects/quark/.git/HEAD",
        ] {
            assert!(ws.join(kept).exists(), "{kept} was moved");
        }
        let claim = std::fs::read_to_string(ws.join(".quark/workspace.json")).unwrap();
        assert!(claim.contains("prj_1"));
        assert!(std::fs::read_to_string(backup.join("QUARK-BACKUP.md"))
            .unwrap()
            .contains("`AGENTS.md`"));

        // Nothing firstmate is left, so a second start changes nothing.
        assert!(retire(&layout).is_empty());
    }

    #[test]
    fn the_command_center_keeps_its_records_and_clones() {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::new(home.path());
        let cmd = layout.command_workspace();
        write(&cmd.join("data/secondmates.md"), "prj_1\n");
        write(&cmd.join("state/home-summary.json"), "{}\n");
        write(&cmd.join("coordinators/prj_1.json"), "{}\n");
        write(&cmd.join("repos/quark/.git/HEAD"), "x\n");
        write(&cmd.join("projects/quark/.git/HEAD"), "x\n");

        let done = retire(&layout);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].moved, ["data", "state"]);
        assert!(cmd.join("coordinators/prj_1.json").exists());
        assert!(cmd.join("repos/quark/.git/HEAD").exists());
        assert!(cmd.join("projects/quark/.git/HEAD").exists());
        assert!(!cmd.join(".quark/workspace.json").exists());
    }

    #[test]
    fn a_native_workspace_is_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let layout = Layout::new(home.path());
        let ws = layout.workspace("prj_2");
        write(
            &ws.join(".quark/workspace.json"),
            "{\"project_id\":\"prj_2\"}",
        );
        write(&ws.join("repos/site/.git/HEAD"), "x\n");
        write(&ws.join("AGENTS.md"), "the user's own\n");
        assert!(retire(&layout).is_empty());
        assert!(ws.join("AGENTS.md").exists());
    }
}

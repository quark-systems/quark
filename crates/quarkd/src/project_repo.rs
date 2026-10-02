//! The Project repo: a local bare git repo per Project that holds its durable,
//! reviewable record (spec ADR-10).
//!
//! ```text
//! ~/.quark/projects/<project-id>.git     bare repo, the source of truth
//! <workspace>/project/                   checkout inside the Project workspace
//!   project.yaml     goal, workspace sources, default agent config, trackers, delivery
//!   dispatch.yaml    rules and default (compiled to the engine's dispatch profiles later)
//!   instructions.md  Project-level guidance for the coordinator and workers
//!   memory/          one file per entry, with evidence and date (empty at creation)
//!   library/         files you add and artifacts agents produce
//! ```
//!
//! Files are rendered from the [`Project`] row. YAML string values are written
//! as JSON strings, which are valid double-quoted YAML scalars, so no value can
//! break the document structure.

use std::path::{Path, PathBuf};
use std::process::Command;

use quark_systems::{AgentConfig, DeliveryPolicy, DispatchPreset, Project};

pub const SCHEMA: &str = "quark.project.v1";

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} exists but is not a Project repo checkout")]
    Unexpected(PathBuf),
}

/// Rendered Project repo files, relative path first.
pub fn render(p: &Project) -> Vec<(&'static str, String)> {
    vec![
        ("project.yaml", project_yaml(p)),
        ("dispatch.yaml", dispatch_yaml(p)),
        ("instructions.md", instructions_md(p)),
        ("memory/.gitkeep", String::new()),
        ("library/.gitkeep", String::new()),
    ]
}

/// Create the bare repo and its checkout with the initial files, commit and
/// push. Safe to call again after a partial failure: an existing bare repo
/// with history is kept, and a missing checkout is cloned from it.
pub fn init(bare: &Path, checkout: &Path, p: &Project) -> Result<(), RepoError> {
    if !bare.join("HEAD").is_file() {
        mkdir(bare.parent().unwrap_or(Path::new("/")))?;
        git(
            None,
            &["init", "--quiet", "--bare", "-b", "main", path_str(bare)?],
        )?;
    }
    if checkout.exists() {
        if !checkout.join(".git").exists() {
            return Err(RepoError::Unexpected(checkout.to_path_buf()));
        }
    } else {
        git(
            None,
            &["clone", "--quiet", path_str(bare)?, path_str(checkout)?],
        )?;
    }
    if git(
        Some(checkout),
        &["rev-parse", "--verify", "--quiet", "HEAD"],
    )
    .is_ok()
    {
        return Ok(());
    }
    git(Some(checkout), &["checkout", "--quiet", "-B", "main"])?;
    for (rel, body) in render(p) {
        let path = checkout.join(rel);
        if let Some(dir) = path.parent() {
            mkdir(dir)?;
        }
        std::fs::write(&path, body).map_err(|source| RepoError::Io {
            path: path.clone(),
            source,
        })?;
    }
    git(Some(checkout), &["add", "--all"])?;
    let message = format!("Create Project {}", p.name);
    let mut args = vec!["-c", "commit.gpgsign=false"];
    // Use the person's git identity when they have one; otherwise commit as Quark.
    if git(Some(checkout), &["config", "user.email"]).is_err() {
        args.extend(["-c", "user.name=Quark", "-c", "user.email=quark@localhost"]);
    }
    args.extend(["commit", "--quiet", "-m", &message]);
    git(Some(checkout), &args)?;
    git(Some(checkout), &["push", "--quiet", "origin", "HEAD:main"])?;
    Ok(())
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, RepoError> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    let out = cmd
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|source| RepoError::Io {
            path: PathBuf::from("git"),
            source,
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(RepoError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

fn mkdir(dir: &Path) -> Result<(), RepoError> {
    std::fs::create_dir_all(dir).map_err(|source| RepoError::Io {
        path: dir.to_path_buf(),
        source,
    })
}

fn path_str(p: &Path) -> Result<&str, RepoError> {
    p.to_str()
        .ok_or_else(|| RepoError::Unexpected(p.to_path_buf()))
}

/// A YAML double-quoted scalar.
fn q(s: &str) -> String {
    serde_json::to_string(s).expect("strings serialize")
}

fn project_yaml(p: &Project) -> String {
    let mut y = String::new();
    y.push_str(&format!("schema: {}\n", q(SCHEMA)));
    y.push_str(&format!("id: {}\n", q(&p.id)));
    y.push_str(&format!("name: {}\n", q(&p.name)));
    match p.goal.as_deref() {
        Some(g) => y.push_str(&format!("goal: {}\n", q(g))),
        None => y.push_str("goal: null\n"),
    }
    y.push_str(&format!("created_at: {}\n", q(&p.created_at)));
    y.push_str("workspace:\n  sources:\n");
    for r in &p.repos {
        y.push_str(&format!(
            "    - name: {}\n      url: {}\n",
            q(r.name.as_deref().unwrap_or_default()),
            q(&r.url)
        ));
    }
    y.push_str("agent_config:\n");
    y.push_str(&agent_yaml(p.agent_config.as_ref(), "  "));
    let trackers: Vec<_> = p.repos.iter().filter_map(|r| github_repo(&r.url)).collect();
    if trackers.is_empty() {
        y.push_str("trackers: []\n");
    } else {
        y.push_str("trackers:\n");
        for t in trackers {
            y.push_str(&format!(
                "  - kind: {}\n    repo: {}\n",
                q("github_issues"),
                q(&t)
            ));
        }
    }
    let delivery = match p.delivery.unwrap_or(DeliveryPolicy::Gated) {
        DeliveryPolicy::Gated => "gated",
        DeliveryPolicy::Direct => "direct",
    };
    y.push_str(&format!(
        "delivery:\n  policy: {}\n  standing_approval: false\n",
        q(delivery)
    ));
    y
}

fn agent_yaml(a: Option<&AgentConfig>, indent: &str) -> String {
    let Some(a) = a else {
        return format!("{indent}harness: null\n");
    };
    let mut y = format!("{indent}harness: {}\n", q(&a.harness));
    if let Some(m) = &a.model {
        y.push_str(&format!("{indent}model: {}\n", q(m)));
    }
    if let Some(e) = &a.effort {
        y.push_str(&format!("{indent}effort: {}\n", q(e)));
    }
    y
}

/// One flow-style dispatch profile.
fn profile(a: Option<&AgentConfig>, effort: Option<&str>) -> String {
    let Some(a) = a else {
        return "{}".into();
    };
    let mut parts = vec![format!("harness: {}", q(&a.harness))];
    if let Some(m) = &a.model {
        parts.push(format!("model: {}", q(m)));
    }
    if let Some(e) = effort.or(a.effort.as_deref()) {
        parts.push(format!("effort: {}", q(e)));
    }
    format!("{{ {} }}", parts.join(", "))
}

fn dispatch_yaml(p: &Project) -> String {
    let preset = p.dispatch_preset.unwrap_or(DispatchPreset::Single);
    let agent = p.agent_config.as_ref();
    let mut y = String::new();
    y.push_str(&format!(
        "# Dispatch rules for this Project, created from the {} preset.\n",
        q(preset.as_str())
    ));
    y.push_str("# With provider none, the coordinator picks the rule for each task.\n");
    y.push_str("classifier:\n  provider: \"none\"\n");
    y.push_str("default_select: \"ordered\"\n");
    match preset {
        DispatchPreset::Single => y.push_str("rules: []\n"),
        DispatchPreset::LightTrivial => {
            y.push_str("rules:\n");
            y.push_str("  - name: \"trivial-edit\"\n");
            y.push_str(
                "    when: \"A trivial mechanical edit such as a rename, typo or one-line fix.\"\n",
            );
            y.push_str(&format!(
                "    use:\n      - {}\n",
                profile(agent, Some("low"))
            ));
        }
    }
    y.push_str(&format!("default:\n  - {}\n", profile(agent, None)));
    y
}

fn instructions_md(p: &Project) -> String {
    let mut m = format!("# {}\n\n## Goal\n\n", p.name);
    match p.goal.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
        Some(g) => m.push_str(&format!("{g}\n")),
        None => m.push_str("Not stated yet.\n"),
    }
    m.push_str("\n## Repositories\n\n");
    for r in &p.repos {
        m.push_str(&format!(
            "- `{}`: {}\n",
            r.name.as_deref().unwrap_or_default(),
            r.url
        ));
    }
    m.push_str(
        "\n## Guidance\n\n\
         Project-level guidance for the coordinator and every worker goes here: \
         conventions, constraints, and what done means for this Project.\n\n\
         Learnings accepted from finished tasks land in `memory/`, one file per entry.\n",
    );
    m
}

/// `owner/repo` for a github.com clone URL.
fn github_repo(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/'))
        .then(|| format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_systems::{ProjectStatus, RepoSource};

    fn project() -> Project {
        Project {
            id: "prj_1".into(),
            name: "Quark \"MVP\"".into(),
            goal: Some("Ship J2:\nproject creation".into()),
            workspace_path: None,
            status: ProjectStatus::Provisioning,
            status_detail: None,
            repos: vec![
                RepoSource {
                    url: "https://github.com/quark-systems/quark.git".into(),
                    name: Some("quark".into()),
                },
                RepoSource {
                    url: "/srv/local.git".into(),
                    name: Some("local".into()),
                },
            ],
            agent_config: Some(AgentConfig {
                harness: "claude-code".into(),
                model: Some("claude-sonnet-5".into()),
                effort: Some("medium".into()),
            }),
            dispatch_preset: Some(DispatchPreset::LightTrivial),
            delivery: Some(DeliveryPolicy::Direct),
            project_repo_path: None,
            created_at: "2026-10-02T00:00:00Z".into(),
            updated_at: "2026-10-02T00:00:00Z".into(),
        }
    }

    #[test]
    fn project_yaml_quotes_every_value() {
        let y = project_yaml(&project());
        assert!(y.contains("name: \"Quark \\\"MVP\\\"\"\n"), "{y}");
        assert!(y.contains("goal: \"Ship J2:\\nproject creation\"\n"), "{y}");
        assert!(y.contains(
            "    - name: \"quark\"\n      url: \"https://github.com/quark-systems/quark.git\"\n"
        ));
        assert!(y.contains("  - kind: \"github_issues\"\n    repo: \"quark-systems/quark\"\n"));
        assert!(y.contains("  policy: \"direct\"\n"));
        assert!(y.contains("  harness: \"claude-code\"\n  model: \"claude-sonnet-5\"\n"));
    }

    #[test]
    fn dispatch_presets() {
        let mut p = project();
        let y = dispatch_yaml(&p);
        assert!(y.contains("  - name: \"trivial-edit\""), "{y}");
        assert!(
            y.contains(
                "      - { harness: \"claude-code\", model: \"claude-sonnet-5\", effort: \"low\" }"
            ),
            "{y}"
        );
        assert!(y.ends_with("default:\n  - { harness: \"claude-code\", model: \"claude-sonnet-5\", effort: \"medium\" }\n"), "{y}");
        p.dispatch_preset = None;
        assert!(dispatch_yaml(&p).contains("rules: []\n"));
    }

    #[test]
    fn github_trackers() {
        assert_eq!(
            github_repo("git@github.com:o/r.git").as_deref(),
            Some("o/r")
        );
        assert_eq!(
            github_repo("https://github.com/o/r").as_deref(),
            Some("o/r")
        );
        assert_eq!(github_repo("https://gitlab.com/o/r"), None);
        assert_eq!(github_repo("https://github.com/o/r/tree/x"), None);
    }

    #[test]
    fn init_commits_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("projects/prj_1.git");
        let checkout = dir.path().join("ws/project");
        std::fs::create_dir_all(dir.path().join("ws")).unwrap();
        let p = project();
        init(&bare, &checkout, &p).unwrap();
        for (rel, _) in render(&p) {
            assert!(checkout.join(rel).is_file(), "{rel}");
        }
        let log = git(Some(&bare), &["log", "--format=%s", "main"]).unwrap();
        assert_eq!(log, "Create Project Quark \"MVP\"");
        let files = git(Some(&bare), &["ls-tree", "-r", "--name-only", "main"]).unwrap();
        assert!(files.contains("memory/.gitkeep") && files.contains("instructions.md"));

        // A lost checkout is recloned; nothing new is committed.
        std::fs::remove_dir_all(&checkout).unwrap();
        init(&bare, &checkout, &p).unwrap();
        init(&bare, &checkout, &p).unwrap();
        assert!(checkout.join("project.yaml").is_file());
        let count = git(Some(&bare), &["rev-list", "--count", "main"]).unwrap();
        assert_eq!(count, "1");
    }
}

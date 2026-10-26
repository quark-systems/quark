//! Filing diagnostics as beads: `quarkd triage`.
//!
//! Runs [`crate::diagnose`] and keeps one bead per finding in a Beads
//! database (Quark's own work queue): a new finding becomes a bug, one
//! already filed and still open gets a comment when it was seen again, and
//! one that was closed but came back is reopened. A bead is matched to its
//! finding by `external_ref = diag:<fingerprint>`, so a bead filed on one
//! Mac is the same bead on another. Labels `diagnostics`,
//! `diag:<fingerprint>` and `host:<host>` make them easy to list.
//!
//! A Quark trigger runs it on a schedule; someone (or an agent) then picks
//! the beads up like any other work.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::diagnose::{Diagnostics, Finding, Severity};

/// Longest bead title.
const TITLE_CHARS: usize = 120;

/// A bead already filed for a finding.
#[derive(Debug, Clone, Deserialize)]
pub struct Filed {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub external_ref: Option<String>,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
}

/// What triage does for one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Create {
        fingerprint: String,
        title: String,
        priority: u8,
        labels: Vec<String>,
        body: String,
    },
    Comment {
        id: String,
        text: String,
    },
    Reopen {
        id: String,
        text: String,
    },
}

/// The external ref a finding's bead carries.
pub fn external_ref(fingerprint: &str) -> String {
    format!("diag:{fingerprint}")
}

/// The steps that bring `filed` up to date with `report`, for findings at
/// `min` or worse.
pub fn plan(report: &Diagnostics, filed: &[Filed], min: Severity) -> Vec<Step> {
    let mut steps = Vec::new();
    for f in report.findings.iter().filter(|f| f.severity >= min) {
        let key = external_ref(&f.fingerprint);
        let seen = format!(
            "Seen again on {}: {} time(s) between {} and {}.",
            report.host, f.count, f.first_seen, f.last_seen
        );
        match filed
            .iter()
            .find(|b| b.external_ref.as_deref() == Some(&key))
        {
            None => steps.push(Step::Create {
                fingerprint: f.fingerprint.clone(),
                title: title(f),
                priority: match f.severity {
                    Severity::Error => 1,
                    Severity::Warn => 2,
                    Severity::Info => 3,
                },
                labels: vec!["diagnostics".into(), key, format!("host:{}", report.host)],
                body: body(report, f),
            }),
            Some(b) if b.status == "closed" => {
                let closed = b.closed_at.as_deref().unwrap_or(&b.updated_at);
                if f.last_seen.as_str() > closed {
                    steps.push(Step::Reopen {
                        id: b.id.clone(),
                        text: format!("Came back after it was closed. {seen}"),
                    });
                }
            }
            Some(b) => {
                if f.last_seen > b.updated_at {
                    steps.push(Step::Comment {
                        id: b.id.clone(),
                        text: seen,
                    });
                }
            }
        }
    }
    steps
}

fn title(f: &Finding) -> String {
    let t = format!("[diag] {}", f.summary.lines().next().unwrap_or_default());
    match t.char_indices().nth(TITLE_CHARS) {
        Some((i, _)) => format!("{}…", &t[..i]),
        None => t,
    }
}

fn body(report: &Diagnostics, f: &Finding) -> String {
    let mut out = format!(
        "Filed by `quarkd triage` on {} ({}).\n\n\
         - Severity: {:?}\n- Source: {}\n- Seen: {} time(s), {} .. {}\n",
        report.host, report.home, f.severity, f.source, f.count, f.first_seen, f.last_seen
    );
    if !f.projects.is_empty() {
        out.push_str(&format!("- Projects: {}\n", f.projects.join(", ")));
    }
    out.push_str(&format!(
        "- Fingerprint: {}\n\n{}\n\nSamples:\n",
        f.fingerprint, f.summary
    ));
    for s in &f.samples {
        out.push_str("\n```\n");
        out.push_str(s);
        out.push_str("\n```\n");
    }
    out.push_str(&format!(
        "\nReproduce the report with `quarkd diagnose --home {} --hours 24`.\n",
        report.home
    ));
    out
}

/// Runs `bd` against one database.
pub struct Bd {
    pub bin: PathBuf,
    pub beads_dir: PathBuf,
}

impl Bd {
    fn run(&self, args: &[&str], stdin: Option<&str>) -> anyhow::Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .env("BEADS_DIR", &self.beads_dir)
            .env("BD_NON_INTERACTIVE", "1")
            .env("NO_COLOR", "1")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if std::env::var_os("BEADS_ACTOR").is_none() {
            cmd.env("BEADS_ACTOR", "quark");
        }
        if let Some(dir) = self.beads_dir.parent() {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("could not run {}: {e}", self.bin.display()))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            anyhow::bail!(
                "bd {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Every bead labelled `diagnostics`, open or closed.
    pub fn filed(&self) -> anyhow::Result<Vec<Filed>> {
        let out = self.run(
            &[
                "list",
                "--label",
                "diagnostics",
                "--all",
                "--limit",
                "0",
                "--json",
            ],
            None,
        )?;
        if out.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&out)?)
    }

    /// Carries out one step; returns the bead it touched.
    pub fn apply(&self, step: &Step) -> anyhow::Result<String> {
        match step {
            Step::Create {
                fingerprint,
                title,
                priority,
                labels,
                body,
            } => {
                let p = priority.to_string();
                let labels = labels.join(",");
                let ext = external_ref(fingerprint);
                let id = self.run(
                    &[
                        "create",
                        title,
                        "--type",
                        "bug",
                        "--priority",
                        &p,
                        "--labels",
                        &labels,
                        "--external-ref",
                        &ext,
                        "--body-file",
                        "-",
                        "--silent",
                    ],
                    Some(body),
                )?;
                Ok(id.trim().to_string())
            }
            Step::Comment { id, text } => {
                self.run(&["comments", "add", id, text], None)?;
                Ok(id.clone())
            }
            Step::Reopen { id, text } => {
                self.run(&["reopen", id], None)?;
                self.run(&["comments", "add", id, text], None)?;
                Ok(id.clone())
            }
        }
    }

    /// `bd dolt pull` / `bd dolt push`, for a database shared between Macs.
    pub fn dolt(&self, verb: &str) -> anyhow::Result<()> {
        self.run(&["dolt", verb], None).map(drop)
    }
}

/// Plans and, unless `dry_run`, applies triage for `report`. Prints what it
/// does to stdout and returns how many steps it took.
pub fn triage(
    report: &Diagnostics,
    bd: &Bd,
    min: Severity,
    dry_run: bool,
    sync: bool,
) -> anyhow::Result<usize> {
    if sync && !dry_run {
        if let Err(e) = bd.dolt("pull") {
            eprintln!("quarkd triage: {e}; filing against the local copy");
        }
    }
    let filed = bd.filed()?;
    let steps = plan(report, &filed, min);
    for step in &steps {
        let what = match step {
            Step::Create { title, .. } => format!("file {title}"),
            Step::Comment { id, .. } => format!("note {id}"),
            Step::Reopen { id, .. } => format!("reopen {id}"),
        };
        if dry_run {
            println!("would {what}");
            continue;
        }
        match bd.apply(step) {
            Ok(id) => println!("{what} ({id})"),
            Err(e) => eprintln!("quarkd triage: could not {what}: {e}"),
        }
    }
    if sync && !dry_run && !steps.is_empty() {
        if let Err(e) = bd.dolt("push") {
            eprintln!("quarkd triage: {e}");
        }
    }
    Ok(steps.len())
}

/// `BEADS_DIR` of a Project in `home`, the default for `--project`.
pub fn project_beads_dir(home: &Path, project_id: &str) -> PathBuf {
    crate::beads::beads_dir(home, project_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::StoreSizes;

    fn finding(fp: &str, severity: Severity, last: &str) -> Finding {
        Finding {
            fingerprint: fp.into(),
            severity,
            source: "log".into(),
            summary: format!("trouble {fp}"),
            count: 2,
            first_seen: "2026-10-26T00:00:00Z".into(),
            last_seen: last.into(),
            projects: vec!["prj_1".into()],
            samples: vec!["sample".into()],
        }
    }

    fn report(findings: Vec<Finding>) -> Diagnostics {
        Diagnostics {
            home: "/h".into(),
            host: "mac".into(),
            version: "0".into(),
            generated_at: "2026-10-26T12:00:00Z".into(),
            since: "2026-10-25T12:00:00Z".into(),
            last_log_at: None,
            sizes: StoreSizes {
                quark_db_bytes: 0,
                events_db_bytes: 0,
                logs_bytes: 0,
            },
            findings,
        }
    }

    fn filed(id: &str, fp: &str, status: &str, updated: &str) -> Filed {
        Filed {
            id: id.into(),
            status: status.into(),
            external_ref: Some(external_ref(fp)),
            updated_at: updated.into(),
            closed_at: (status == "closed").then(|| updated.into()),
        }
    }

    #[test]
    fn plan_files_notes_and_reopens() {
        let r = report(vec![
            finding("new", Severity::Error, "2026-10-26T10:00:00Z"),
            finding("open-seen", Severity::Warn, "2026-10-26T10:00:00Z"),
            finding("open-stale", Severity::Warn, "2026-10-26T10:00:00Z"),
            finding("back", Severity::Warn, "2026-10-26T10:00:00Z"),
            finding("fixed", Severity::Warn, "2026-10-26T10:00:00Z"),
            finding("minor", Severity::Info, "2026-10-26T10:00:00Z"),
        ]);
        let beads = [
            filed("q-1", "open-seen", "open", "2026-10-26T09:00:00Z"),
            filed("q-2", "open-stale", "in_progress", "2026-10-26T11:00:00Z"),
            filed("q-3", "back", "closed", "2026-10-26T09:00:00Z"),
            filed("q-4", "fixed", "closed", "2026-10-26T11:00:00Z"),
        ];
        let steps = plan(&r, &beads, Severity::Warn);
        assert_eq!(steps.len(), 3);
        match &steps[0] {
            Step::Create {
                title,
                priority,
                labels,
                body,
                ..
            } => {
                assert_eq!(title, "[diag] trouble new");
                assert_eq!(*priority, 1);
                assert_eq!(labels, &["diagnostics", "diag:new", "host:mac"]);
                assert!(body.contains("Projects: prj_1"));
            }
            s => panic!("{s:?}"),
        }
        assert!(matches!(&steps[1], Step::Comment { id, .. } if id == "q-1"));
        assert!(matches!(&steps[2], Step::Reopen { id, .. } if id == "q-3"));
    }
}

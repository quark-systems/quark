//! Decisions as `decision` beads. Each Quark decision in a Project with a
//! Beads database is mirrored as one `decision` bead (its `external_ref` is
//! `quark:<decision id>`). While the decision is open, a human gate blocks
//! the bead, and the bead blocks every bead the decision says it blocks, so
//! `bd ready` does not hand that work out. Answering closes both, in Quark
//! or in Beads: closing the bead (or resolving its gate) with the answer as
//! the reason answers the decision in Quark.
//!
//! Standing rules become Beads memories (`quark-rule-<id>`), so `bd prime`
//! shows every agent the rules in force.
//!
//! [`crate::api::decision_beads`] decides what to write; this module only
//! reads and writes the database.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use quark_systems::{Decision, DecisionBrief};
use serde::Deserialize;

use super::{Beads, BeadsError, CLOSED};

/// `external_ref` prefix that marks a bead as a Quark decision's mirror.
pub const REF_PREFIX: &str = "quark:";

/// Memory key prefix of a standing rule.
pub const RULE_KEY_PREFIX: &str = "quark-rule-";

/// Label on a decision bead whose answer became a standing rule.
pub const RULE_LABEL: &str = "standing-rule";

/// Longest close reason written to Beads, in bytes.
const MAX_REASON: usize = 1000;

/// A decision bead as Quark needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionBead {
    pub id: String,
    /// The Quark decision it mirrors.
    pub decision_id: String,
    pub closed: bool,
    /// The answer, when someone closed it in Beads.
    pub close_reason: Option<String>,
    /// Its human gate, and the gate's answer when resolved in Beads.
    pub gate: Option<String>,
    pub gate_closed: bool,
    pub gate_reason: Option<String>,
    pub labels: Vec<String>,
    /// Whether the asker's outcome has been noted on it.
    pub outcome_noted: bool,
}

impl DecisionBead {
    /// The answer given in Beads, if it was answered there.
    pub fn beads_answer(&self) -> Option<String> {
        let pick = |s: &Option<String>| {
            s.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        if self.closed {
            pick(&self.close_reason).or_else(|| pick(&self.gate_reason))
        } else if self.gate_closed {
            pick(&self.gate_reason)
        } else {
            None
        }
    }
}

#[derive(Debug, Deserialize)]
struct Listed {
    id: String,
    status: String,
    #[serde(default)]
    close_reason: Option<String>,
    #[serde(default)]
    external_ref: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Created {
    id: String,
}

fn meta_str(m: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    match m.get(key)? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        v => Some(v.to_string()),
    }
}

/// The decision beads among `beads`, joined with their gates in `gates`.
fn decision_beads(beads: Vec<Listed>, gates: &[Listed]) -> Vec<DecisionBead> {
    let gates: HashMap<&str, &Listed> = gates.iter().map(|g| (g.id.as_str(), g)).collect();
    beads
        .into_iter()
        .filter_map(|b| {
            let decision_id = b
                .external_ref
                .as_deref()?
                .strip_prefix(REF_PREFIX)?
                .to_string();
            let gate = meta_str(&b.metadata, "gate");
            let g = gate.as_deref().and_then(|g| gates.get(g));
            Some(DecisionBead {
                decision_id,
                closed: b.status == CLOSED,
                close_reason: b.close_reason,
                gate_closed: g.is_some_and(|g| g.status == CLOSED),
                gate_reason: g.and_then(|g| g.close_reason.clone()),
                gate,
                labels: b.labels,
                outcome_noted: meta_str(&b.metadata, "outcome").is_some(),
                id: b.id,
            })
        })
        .collect()
}

/// A decision's bead description: the context, the options with their
/// consequences, the recommendation, who asked, what waits, the evidence,
/// and how to answer from Beads.
pub fn description(d: &Decision) -> String {
    let b: &DecisionBrief = &d.brief;
    let mut out = Vec::new();
    if let Some(c) = b.context.as_deref() {
        out.push(c.trim().to_string());
    }
    if !b.options.is_empty() {
        let mut lines = vec!["Options:".to_string()];
        for o in &b.options {
            let rec = if b.recommended.as_deref() == Some(o.label.as_str()) {
                " (recommended)"
            } else {
                ""
            };
            lines.push(match o.consequence.as_deref() {
                Some(c) => format!("- **{}**{rec}: {c}", o.label),
                None => format!("- **{}**{rec}", o.label),
            });
        }
        out.push(lines.join("\n"));
    }
    if let Some(w) = b.recommended_why.as_deref() {
        out.push(format!("Why recommended: {w}"));
    }
    let mut who = Vec::new();
    if let Some(a) = b.asked_by.as_deref() {
        who.push(format!("Asked by: {a}"));
    }
    if !b.blocks.is_empty() {
        who.push(format!("Blocks: {}", b.blocks.join(", ")));
    }
    if !who.is_empty() {
        out.push(who.join("\n"));
    }
    if !b.evidence.is_empty() {
        let mut lines = vec!["Evidence:".to_string()];
        for e in &b.evidence {
            lines.push(match e.url.as_deref() {
                Some(u) => format!("- [{}]({u})", e.label),
                None => format!("- {}", e.label),
            });
        }
        out.push(lines.join("\n"));
    }
    out.push(format!(
        "Decision D-{} in Quark. Answer it there, or resolve the gate that blocks this bead with the answer as the reason (`bd gate resolve <gate> --reason <answer>`).",
        d.number
    ));
    out.join("\n\n")
}

/// The note recording an answer given in Quark.
pub fn answer_note(d: &Decision) -> String {
    let by = d.answered_by.as_deref().unwrap_or("someone");
    let via = match d.answered_via.as_deref() {
        Some("rule") => " under a standing rule".to_string(),
        Some("beads") => " in Beads".to_string(),
        Some(v) => format!(" in the {v}"),
        None => String::new(),
    };
    let mut s = format!(
        "Answered by {by}{via}: {}",
        d.answer.as_deref().unwrap_or("").trim()
    );
    if let Some(w) = d.answer_why.as_deref() {
        s.push_str(&format!("\nWhy: {w}"));
    }
    s
}

/// Whether `s` looks like a bead id (`qk-41`, `bd-a3f8e9`).
fn bead_like(s: &str) -> bool {
    let Some((prefix, rest)) = s.split_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && !rest.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

fn reason(text: &str) -> String {
    let t = text.trim();
    if t.len() <= MAX_REASON {
        return t.to_string();
    }
    let mut end = MAX_REASON;
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &t[..end])
}

impl Beads {
    /// Every decision bead in the Project's database.
    pub async fn decision_beads(
        &self,
        home: &Path,
        project_id: &str,
    ) -> Result<Vec<DecisionBead>, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        // `bd` prints `null` for an empty list.
        let beads: Option<Vec<Listed>> = self
            .json(
                &dir,
                &["list", "--all", "--type", "decision", "--limit", "0"],
            )
            .await?;
        let gates: Option<Vec<Listed>> = self.json(&dir, &["gate", "list", "--all"]).await?;
        Ok(decision_beads(
            beads.unwrap_or_default(),
            &gates.unwrap_or_default(),
        ))
    }

    /// Creates the bead for `d`. An open decision gets a human gate, and
    /// blocks the beads it names; an answered one is closed with its answer.
    pub async fn create_decision_bead(
        &self,
        home: &Path,
        d: &Decision,
    ) -> Result<String, BeadsError> {
        let dir = Self::ready_dir(home, &d.project_id)?;
        let ext = format!("{REF_PREFIX}{}", d.id);
        let desc = description(d);
        let mut labels = "quark".to_string();
        if d.made_rule_id.is_some() {
            labels.push(',');
            labels.push_str(RULE_LABEL);
        }
        let created: Created = self
            .json(
                &dir,
                &[
                    "create",
                    "--type",
                    "decision",
                    "--title",
                    &d.question,
                    "--description",
                    &desc,
                    "--external-ref",
                    &ext,
                    "--labels",
                    &labels,
                ],
            )
            .await?;
        let bead = created.id;
        if d.state == quark_systems::DecisionState::Open {
            let title = format!("Waiting on D-{}: {}", d.number, d.question);
            let gate: Created = self
                .json(
                    &dir,
                    &[
                        "gate",
                        "create",
                        "--type",
                        "human",
                        "--blocks",
                        &bead,
                        "--title",
                        &title,
                        "--reason",
                        "A person answers this decision in Quark or in Beads",
                    ],
                )
                .await?;
            let meta = format!("gate={}", gate.id);
            self.bd(&dir, &["update", &bead, "--set-metadata", &meta], &[])
                .await?;
            for blocked in d.brief.blocks.iter().filter(|b| bead_like(b)) {
                // A name that is not a bead here is left alone.
                if let Err(e) = self.bd(&dir, &["dep", "add", blocked, &bead], &[]).await {
                    tracing::debug!(bead = %blocked, error = %e, "not blocking a bead on a decision");
                }
            }
        } else {
            self.close_decision_bead(home, d, &bead, None).await?;
        }
        Ok(bead)
    }

    /// Records the answer to `d` on its bead and closes the bead and its
    /// gate.
    pub async fn close_decision_bead(
        &self,
        home: &Path,
        d: &Decision,
        bead: &str,
        gate: Option<(&str, bool)>,
    ) -> Result<(), BeadsError> {
        let dir = Self::ready_dir(home, &d.project_id)?;
        let answer = reason(d.answer.as_deref().unwrap_or("Answered in Quark"));
        self.bd(&dir, &["note", bead, &answer_note(d)], &[]).await?;
        if let Some((gate, false)) = gate {
            self.bd(&dir, &["gate", "resolve", gate, "--reason", &answer], &[])
                .await?;
        }
        self.bd(&dir, &["close", bead, "--reason", &answer], &[])
            .await?;
        Ok(())
    }

    /// Notes what the asker did with the answer, once.
    pub async fn note_decision_outcome(
        &self,
        home: &Path,
        project_id: &str,
        bead: &str,
        outcome: &str,
    ) -> Result<(), BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        self.bd(
            &dir,
            &["note", bead, &format!("What happened: {outcome}")],
            &[],
        )
        .await?;
        self.bd(
            &dir,
            &["update", bead, "--set-metadata", "outcome=noted"],
            &[],
        )
        .await?;
        Ok(())
    }

    /// Labels a decision bead whose answer became a standing rule.
    pub async fn label_rule_decision(
        &self,
        home: &Path,
        project_id: &str,
        bead: &str,
    ) -> Result<(), BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        self.bd(&dir, &["label", "add", bead, RULE_LABEL], &[])
            .await?;
        Ok(())
    }

    /// Makes the Project's `quark-rule-*` memories match `rules` (id, text):
    /// remembers new and changed rules, forgets ones no longer in force.
    /// Returns whether anything changed; the caller announces it with
    /// [`super::memories_changed`].
    pub async fn sync_rule_memories(
        &self,
        home: &Path,
        project_id: &str,
        rules: &[(String, String)],
    ) -> Result<bool, BeadsError> {
        let dir = Self::ready_dir(home, project_id)?;
        let raw: BTreeMap<String, serde_json::Value> = self.json(&dir, &["memories"]).await?;
        let have: BTreeMap<&str, String> = raw
            .iter()
            .filter(|(k, _)| k.starts_with(RULE_KEY_PREFIX))
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or_default().to_string()))
            .collect();
        let want: BTreeMap<String, String> = rules
            .iter()
            .map(|(id, text)| (format!("{RULE_KEY_PREFIX}{id}"), rule_memory(text)))
            .collect();
        let mut changed = false;
        for (key, text) in &want {
            if have.get(key.as_str()) != Some(text) {
                self.bd(&dir, &["remember", "--key", key, "--", text], &[])
                    .await?;
                changed = true;
            }
        }
        for key in have.keys().filter(|k| !want.contains_key(**k)) {
            self.bd(&dir, &["forget", key], &[]).await?;
            changed = true;
        }
        Ok(changed)
    }
}

/// How a standing rule reads as a memory.
pub fn rule_memory(text: &str) -> String {
    format!(
        "Standing rule (decide without asking, then log it in Quark): {}",
        text.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use quark_systems::{DecisionOption, EvidenceLink};

    fn listed(id: &str, status: &str, ext: Option<&str>, meta: serde_json::Value) -> Listed {
        Listed {
            id: id.into(),
            status: status.into(),
            close_reason: None,
            external_ref: ext.map(Into::into),
            labels: vec![],
            metadata: serde_json::from_value(meta).unwrap(),
        }
    }

    #[test]
    fn only_quark_decisions_are_read_and_joined_with_their_gates() {
        let mut closed_gate = listed("qk-g1", "closed", None, serde_json::json!({}));
        closed_gate.close_reason = Some("Yes".into());
        let got = decision_beads(
            vec![
                listed(
                    "qk-1",
                    "open",
                    Some("quark:d1"),
                    serde_json::json!({"gate": "qk-g1"}),
                ),
                listed("qk-2", "open", Some("gh-12"), serde_json::json!({})),
                listed("qk-3", "open", None, serde_json::json!({})),
            ],
            &[closed_gate],
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].decision_id, "d1");
        assert!(got[0].gate_closed);
        assert_eq!(got[0].beads_answer().as_deref(), Some("Yes"));
    }

    #[test]
    fn an_answer_in_beads_is_the_close_reason_or_the_gate_reason() {
        let mut b = DecisionBead {
            id: "qk-1".into(),
            decision_id: "d1".into(),
            closed: false,
            close_reason: None,
            gate: Some("qk-g".into()),
            gate_closed: false,
            gate_reason: None,
            labels: vec![],
            outcome_noted: false,
        };
        assert_eq!(b.beads_answer(), None);
        b.closed = true;
        b.close_reason = Some("  ".into());
        assert_eq!(b.beads_answer(), None, "closed with no reason is no answer");
        b.close_reason = Some("Wait a week".into());
        assert_eq!(b.beads_answer().as_deref(), Some("Wait a week"));
    }

    #[test]
    fn the_description_carries_the_brief() {
        let d = Decision {
            id: "d1".into(),
            number: 14,
            question: "Switch slice 2?".into(),
            brief: DecisionBrief {
                context: Some("Shadow agreed.".into()),
                options: vec![
                    DecisionOption {
                        label: "Switch now".into(),
                        consequence: Some("Merges #95".into()),
                    },
                    DecisionOption {
                        label: "Wait".into(),
                        consequence: None,
                    },
                ],
                recommended: Some("Switch now".into()),
                recommended_why: Some("No disagreements.".into()),
                asked_by: Some("coordinator".into()),
                blocks: vec!["qk-44".into()],
                evidence: vec![EvidenceLink {
                    label: "Report".into(),
                    url: Some("https://x.test".into()),
                }],
            },
            ..Default::default()
        };
        let text = description(&d);
        assert!(text.starts_with("Shadow agreed."));
        assert!(text.contains("- **Switch now** (recommended): Merges #95"));
        assert!(
            text.contains("- **Wait**\n")
                || text.contains("- **Wait**\n\n")
                || text.contains("- **Wait**")
        );
        assert!(text.contains("Blocks: qk-44"));
        assert!(text.contains("- [Report](https://x.test)"));
        assert!(text.ends_with("(`bd gate resolve <gate> --reason <answer>`)."));
    }

    #[test]
    fn answers_name_who_where_and_why() {
        let d = Decision {
            answer: Some("Yes".into()),
            answered_by: Some("matt".into()),
            answered_via: Some("phone".into()),
            answer_why: Some("Green for a week.".into()),
            ..Default::default()
        };
        assert_eq!(
            answer_note(&d),
            "Answered by matt in the phone: Yes\nWhy: Green for a week."
        );
    }

    #[test]
    fn only_bead_like_names_are_linked() {
        assert!(bead_like("qk-44"));
        assert!(bead_like("bd-a3f8e9.1"));
        assert!(!bead_like("https://github.com/o/r/pull/95"));
        assert!(!bead_like("t-1chjg x"));
        assert!(!bead_like("plain"));
    }

    #[test]
    fn long_reasons_are_cut_on_a_character() {
        let long = "é".repeat(MAX_REASON);
        let r = reason(&long);
        assert!(r.len() <= MAX_REASON + "…".len());
        assert!(r.ends_with('…'));
    }
}

//! Decisions: questions held for a person, their answers, what happened
//! next, and the standing rules answers can create.
//!
//! One lifecycle: asked (`open`), answered, acted on, and kept in the log.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionState {
    /// Asked and waiting on a person.
    #[default]
    Open,
    /// Answered; the asker has not yet recorded what it did.
    Answered,
    /// Answered and acted on: `outcome` says what happened.
    Acted,
}

impl DecisionState {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionState::Open => "open",
            DecisionState::Answered => "answered",
            DecisionState::Acted => "acted",
        }
    }

    /// Unknown text reads as open.
    pub fn parse(s: &str) -> DecisionState {
        match s {
            "answered" => DecisionState::Answered,
            "acted" => DecisionState::Acted,
            _ => DecisionState::Open,
        }
    }
}

/// One answer the asker offers, and what picking it leads to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct DecisionOption {
    pub label: String,
    /// What happens when this option is picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consequence: Option<String>,
}

/// Accepts a bare label as well as `{label, consequence}`, so a producer
/// that only lists labels still yields options.
impl<'de> Deserialize<'de> for DecisionOption {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Label(String),
            Full {
                label: String,
                #[serde(default)]
                consequence: Option<String>,
            },
        }
        Ok(match Repr::deserialize(d)? {
            Repr::Label(label) => DecisionOption {
                label,
                consequence: None,
            },
            Repr::Full { label, consequence } => DecisionOption { label, consequence },
        })
    }
}

/// A link to something that informs a decision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EvidenceLink {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What the asker attaches to a question so a person can answer it in one
/// look. Every engine produces this same shape: firstmate's coordinator
/// through its hold record, the native coordinator through `ask_user`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DecisionBrief {
    /// Background a person needs to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<DecisionOption>,
    /// The label of the option the asker recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    /// Why the asker recommends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_why: Option<String>,
    /// Who asked: `coordinator`, a worker's task id, or a gate's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_by: Option<String>,
    /// What waits on the answer: pull request URLs, task or issue ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceLink>,
}

impl DecisionBrief {
    pub fn is_empty(&self) -> bool {
        *self == DecisionBrief::default()
    }

    /// Trims every text and drops blank ones, blank options and blank
    /// links, so stored briefs hold only what a person can read.
    pub fn normalized(mut self) -> DecisionBrief {
        fn text(s: Option<String>) -> Option<String> {
            s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        }
        self.context = text(self.context);
        self.recommended = text(self.recommended);
        self.recommended_why = text(self.recommended_why);
        self.asked_by = text(self.asked_by);
        self.options = self
            .options
            .into_iter()
            .filter_map(|o| {
                Some(DecisionOption {
                    label: text(Some(o.label))?,
                    consequence: text(o.consequence),
                })
            })
            .collect();
        self.blocks = self
            .blocks
            .into_iter()
            .filter_map(|b| text(Some(b)))
            .collect();
        self.evidence = self
            .evidence
            .into_iter()
            .filter_map(|e| {
                Some(EvidenceLink {
                    label: text(Some(e.label))?,
                    url: text(e.url),
                })
            })
            .collect();
        self
    }
}

/// A question held for a person, and everything that followed it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Decision {
    pub id: String,
    /// Per-Project number, shown as `D-<number>`.
    pub number: i64,
    pub project_id: String,
    pub task_id: Option<String>,
    pub question: String,
    pub state: DecisionState,
    /// Context, options and recommendation from whoever asked.
    #[serde(default)]
    pub brief: DecisionBrief,
    pub answer: Option<String>,
    pub answered_by: Option<String>,
    /// Where the answer came from: `app`, `phone`, `chat`, or `rule` for an
    /// agent deciding under a standing rule.
    #[serde(default)]
    pub answered_via: Option<String>,
    /// The answerer's reason, kept for the log.
    #[serde(default)]
    pub answer_why: Option<String>,
    pub opened_at: String,
    pub answered_at: Option<String>,
    /// What the asker did with the answer.
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub acted_at: Option<String>,
    /// The standing rule an agent decided this under.
    #[serde(default)]
    pub rule_id: Option<String>,
    /// The standing rule this answer created.
    #[serde(default)]
    pub made_rule_id: Option<String>,
    /// The `decision` bead that mirrors it in the Project's Beads database,
    /// once there is one.
    #[serde(default)]
    pub bead_id: Option<String>,
}

/// An answer to an open decision.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AnswerDecision {
    /// The answer, as the worker or coordinator will read it.
    pub answer: String,
    /// Who is answering: one line of at most 128 bytes. Stored with the
    /// decision and recorded with the engine's own record of the answer.
    /// Absent or null means the daemon's own user (`$USER`).
    #[serde(default)]
    pub answered_by: Option<String>,
    /// Why, for the log. Sent to the asker with the answer.
    #[serde(default)]
    pub why: Option<String>,
    /// Where the answer was given: `app` (the default), `phone` or `chat`.
    /// Answers given in Beads are recorded as `beads`.
    #[serde(default)]
    pub via: Option<String>,
    /// Turn the answer into a standing rule with this text, so matching
    /// cases are decided without asking.
    #[serde(default)]
    pub make_rule: Option<String>,
}

/// What the asker did with an answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ActOnDecision {
    pub outcome: String,
}

/// A decision an agent made itself under a standing rule, for the log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RecordRuleDecision {
    /// The rule; taken from the request path, so a body may omit it.
    #[serde(default)]
    pub rule_id: String,
    pub question: String,
    pub answer: String,
    /// The agent that decided: `coordinator` or a worker's task id.
    pub decided_by: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub brief: DecisionBrief,
    /// What it did, when already done.
    #[serde(default)]
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// Made from an answer.
    #[default]
    Answer,
    /// The Project's standing approval to merge green pull requests.
    MergeApproval,
}

/// A standing rule: matching cases are decided without asking, and each one
/// is logged as decided under the rule.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StandingRule {
    pub id: String,
    pub project_id: String,
    pub kind: RuleKind,
    /// What the rule allows, in the answerer's words.
    pub text: String,
    /// The decision whose answer made it.
    pub decision_id: Option<String>,
    pub created_by: Option<String>,
    pub created_at: Option<String>,
    /// Set once revoked; a revoked rule decides nothing.
    pub revoked_at: Option<String>,
    pub revoked_by: Option<String>,
    /// When its text was last changed, and by whom.
    #[serde(default)]
    pub changed_at: Option<String>,
    #[serde(default)]
    pub changed_by: Option<String>,
    /// How many decisions were logged under it.
    pub applied: i64,
}

/// New words for a standing rule.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChangeRule {
    pub text: String,
    /// Who is changing it; absent means the daemon's own user.
    #[serde(default)]
    pub changed_by: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_read_labels_or_objects() {
        let b: DecisionBrief = serde_json::from_value(serde_json::json!({
            "options": ["Wait", {"label": "Switch", "consequence": "Merges #95"}]
        }))
        .unwrap();
        assert_eq!(b.options[0].label, "Wait");
        assert_eq!(b.options[0].consequence, None);
        assert_eq!(b.options[1].consequence.as_deref(), Some("Merges #95"));
    }

    #[test]
    fn normalized_drops_blanks() {
        let b = DecisionBrief {
            context: Some("  ".into()),
            options: vec![
                DecisionOption {
                    label: " ".into(),
                    consequence: None,
                },
                DecisionOption {
                    label: " Yes ".into(),
                    consequence: Some("".into()),
                },
            ],
            blocks: vec!["".into(), "#95".into()],
            ..Default::default()
        }
        .normalized();
        assert_eq!(b.context, None);
        assert_eq!(
            b.options,
            vec![DecisionOption {
                label: "Yes".into(),
                consequence: None
            }]
        );
        assert_eq!(b.blocks, vec!["#95".to_string()]);
    }

    #[test]
    fn state_round_trip() {
        for s in [
            DecisionState::Open,
            DecisionState::Answered,
            DecisionState::Acted,
        ] {
            assert_eq!(DecisionState::parse(s.as_str()), s);
        }
    }
}

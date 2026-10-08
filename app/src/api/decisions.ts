// Decisions: questions held for a person, their answers, what happened next,
// and the standing rules answers make.

import { enc, req } from "./client";

/** Asked (`open`), answered, then acted on once the asker records what it did. */
export type DecisionState = "open" | "answered" | "acted";
export interface DecisionOption { label: string; /** What happens when it is picked. */ consequence?: string | null }
export interface EvidenceLink { label: string; url?: string | null }
/** What the asker attached: the same shape from firstmate's coordinator and the native one. */
export interface DecisionBrief {
  context?: string | null;
  options?: DecisionOption[];
  /** The label of the recommended option. */
  recommended?: string | null;
  recommended_why?: string | null;
  /** `coordinator`, a worker's task id, `quarkd`, or a gate's name. */
  asked_by?: string | null;
  /** Pull request URLs, task or issue ids waiting on the answer. */
  blocks?: string[];
  evidence?: EvidenceLink[];
}
export interface Decision {
  id: string;
  /** Per-Project number, shown as `D-<number>`. */
  number?: number;
  project_id: string; task_id?: string | null; question: string;
  state: DecisionState; answer?: string | null; opened_at: string;
  brief?: DecisionBrief;
  /** Who answered and when; null while open. */
  answered_by?: string | null; answered_at?: string | null;
  /** `app`, `phone`, `chat`, or `rule` when an agent decided under a standing rule. */
  answered_via?: string | null;
  answer_why?: string | null;
  /** What the asker did with the answer. */
  outcome?: string | null; acted_at?: string | null;
  /** The rule an agent decided this under. */
  rule_id?: string | null;
  /** The rule this answer made. */
  made_rule_id?: string | null;
}
export interface AnswerDecision {
  answer: string; answered_by?: string | null;
  /** Why, for the log; sent to the asker with the answer. */
  why?: string | null;
  via?: "app" | "phone" | "chat" | null;
  /** Make the answer a standing rule with this text. */
  make_rule?: string | null;
}
export type RuleKind = "answer" | "merge_approval";
export interface StandingRule {
  id: string; project_id: string; kind: RuleKind; text: string;
  decision_id?: string | null; created_by?: string | null; created_at?: string | null;
  revoked_at?: string | null; revoked_by?: string | null;
  /** Decisions logged under it. */
  applied: number;
}

export const decisionsApi = {
  /** Open, answered and acted-on decisions, so the log can show who answered and what happened. */
  decisions: () => req<Decision[]>("GET", "/v1/decisions"),
  decision: (id: string) => req<Decision>("GET", `/v1/decisions/${enc(id)}`),
  /** Answers an open decision and returns it answered. `answered_by` defaults to the daemon's user. */
  answerDecision: (id: string, body: AnswerDecision) => req<Decision>("POST", `/v1/decisions/${enc(id)}:answer`, body),
  /** Standing rules in force (with `includeRevoked`, revoked ones too), including each Project's standing approval. */
  rules: (includeRevoked = false) => req<StandingRule[]>("GET", `/v1/rules${includeRevoked ? "?include_revoked=true" : ""}`),
  revokeRule: (id: string) => req<StandingRule>("POST", `/v1/rules/${enc(id)}:revoke`),
};

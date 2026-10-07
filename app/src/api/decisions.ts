// Decisions a task is waiting on.

import { enc, req } from "./client";

export type DecisionState = "open" | "answered";
export interface Decision {
  id: string; project_id: string; task_id?: string | null; question: string;
  state: DecisionState; answer?: string | null; opened_at: string;
  /** Who answered and when; null while open. */
  answered_by?: string | null; answered_at?: string | null;
}
export interface AnswerDecision { answer: string; answered_by?: string | null }

export const decisionsApi = {
  /** Open and answered decisions, so the inbox can show who answered. */
  decisions: () => req<Decision[]>("GET", "/v1/decisions"),
  /** Answers an open decision and returns it answered. `answered_by` defaults to the daemon's user. */
  answerDecision: (id: string, body: AnswerDecision) => req<Decision>("POST", `/v1/decisions/${enc(id)}:answer`, body),
};

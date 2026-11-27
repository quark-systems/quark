// References to Quark's own objects (a decision, a worker, a pull request) in text a person sent.
// The dock sends `About "<title>" (#/<route>):` on a message's first line so the coordinator can
// follow it; the transcript reads that line back and shows the object as a card, never the raw
// link. Kept pure so it is easy to test; `ui/ObjectRef.tsx` draws the card.
import type { Decision, PullRequest, Task } from "./api";
import { decisionLabel } from "./decisions";
import { href, parseRoute, type Route } from "./nav";
import { prStateMeta } from "./prs";
import { taskTone, TONE_LABEL, type Tone } from "./ui/tone";

/** What a message is about, as its first line names it. */
export interface AboutLine { title: string; link: string }

// `About "<title>" (#/<route>):` then a newline. The title may hold quotes, so it runs up to the
// last `" (#/` on the line.
const ABOUT = /^About "(.*)" \((#\/[^\s)]*)\):[ \t]*(?:\r?\n|$)/;

/** The line a message opens with when it is about one object, and the rest of the message. */
export function parseAbout(text: string): { about: AboutLine; body: string } | null {
  const m = ABOUT.exec(text);
  return m ? { about: { title: m[1], link: m[2] }, body: text.slice(m[0].length) } : null;
}

/** The first line a message about `about` opens with; `parseAbout` reads it back. */
export function aboutLine(about: AboutLine): string {
  return `About "${about.title}" (${about.link}):`;
}

export type RefKind = "decision" | "worker" | "pull-request" | "page";

export const REF_KIND_LABEL: Record<RefKind, string> = {
  decision: "Decision", worker: "Worker", "pull-request": "Pull request", page: "Page",
};

/** An object as its card shows it. */
export interface ResolvedRef {
  kind: RefKind;
  /** A short handle beside the kind: `D-14`, `#95`. */
  tag?: string;
  title: string;
  /** Its state, in the glossary's words, with the tone of its dot. */
  status?: { label: string; tone: Tone };
  /** Where a click opens it, inside its project. */
  href: string;
  /** The project whose issue ids the title may name. */
  project: string | null;
}

type Lookup = {
  tasks: Record<string, Pick<Task, "project_id" | "title" | "state">>;
  pullRequests: Record<string, Pick<PullRequest, "project_id" | "title" | "repo" | "number" | "state">>;
  decisions: Record<string, Pick<Decision, "id" | "number" | "project_id" | "question" | "state">>;
};

const DECISION_STATUS: Record<Decision["state"], { label: string; tone: Tone }> = {
  open: { label: "Needs you", tone: "needs-you" },
  answered: { label: "Answered", tone: "ready" },
  acted: { label: "Acted on", tone: "idle" },
  with_coordinator: { label: "With the coordinator", tone: "parked" },
};

const PR_TONE: Record<PullRequest["state"], Tone> = { open: "ready", draft: "parked", merged: "idle", closed: "idle" };

/**
 * The object `about` links to, with its live title and state when the store has it. One the store
 * does not know keeps the title the message gave and opens the link as written.
 */
export function resolveRef(about: AboutLine, s: Lookup): ResolvedRef {
  const route: Route = parseRoute(about.link);
  const given = { title: about.title, href: about.link, project: "project" in route ? (route.project as string | undefined) ?? null : null };
  if ((route.name === "decisions" || route.name === "inbox") && route.id) {
    const d = s.decisions[route.id];
    if (!d) return { kind: "decision", ...given };
    return {
      kind: "decision", tag: decisionLabel(d), title: d.question, status: DECISION_STATUS[d.state],
      href: href({ name: "decisions", project: d.project_id, id: d.id }), project: d.project_id,
    };
  }
  if (route.name === "task") {
    const t = s.tasks[route.id];
    if (!t) return { kind: "worker", ...given };
    const tone = taskTone(t.state);
    return { kind: "worker", title: t.title, status: { label: TONE_LABEL[tone], tone }, href: href({ name: "task", id: route.id }), project: t.project_id };
  }
  if (route.name === "pr" || (route.name === "pulls" && route.id)) {
    const p = s.pullRequests[route.id!];
    if (!p) return { kind: "pull-request", ...given };
    return {
      kind: "pull-request", tag: `#${p.number}`, title: p.title ?? `${p.repo}#${p.number}`,
      status: { label: prStateMeta[p.state].label, tone: PR_TONE[p.state] },
      href: href({ name: "pulls", project: p.project_id, id: route.id! }), project: p.project_id,
    };
  }
  return { kind: "page", ...given };
}

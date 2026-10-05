// The rule editor's draft: cleaning, comparing and checking it, kept pure so it is easy to test.
import type { DispatchProfile, DispatchRuleSpec, DispatchRulesDraft } from "./api";

const text = (s: string | null | undefined) => s?.trim() || null;

/** A profile as it is saved: trimmed, blank values null, the engine's own fields kept only when set. */
export function cleanProfile(p: DispatchProfile): DispatchProfile {
  return {
    harness: p.harness.trim(), model: text(p.model), effort: text(p.effort), pool: text(p.pool),
    ...(text(p.provider) ? { provider: text(p.provider) } : {}),
    ...(p.floor ? { floor: p.floor } : {}),
    ...(text(p.pricing) ? { pricing: text(p.pricing) } : {}),
  };
}

function cleanRule(r: DispatchRuleSpec): DispatchRuleSpec {
  return {
    name: text(r.name), when: r.when.trim(), candidates: r.candidates.map(cleanProfile), select: r.select ?? null,
    ...(text(r.why) ? { why: text(r.why) } : {}),
    ...(text(r.approval) ? { approval: text(r.approval) } : {}),
    ...(r.floor ? { floor: r.floor } : {}),
  };
}

/** The draft as it is saved, and a copy that shares nothing with `d`. */
export function cleanDraft(d: DispatchRulesDraft): DispatchRulesDraft {
  return JSON.parse(JSON.stringify({
    default_select: d.default_select ?? null, rules: d.rules.map(cleanRule), default: d.default.map(cleanProfile),
  }));
}

export function sameDraft(a: DispatchRulesDraft, b: DispatchRulesDraft): boolean {
  return JSON.stringify(cleanDraft(a)) === JSON.stringify(cleanDraft(b));
}

/** What `POST /v1/harnesses:validate` is asked about a candidate; equal keys get one answer. */
export function profileKey(p: DispatchProfile): string {
  const c = cleanProfile(p);
  return JSON.stringify([c.harness, c.model, c.effort, c.pool]);
}

/** Every candidate of the draft: each rule's, then the default's. */
export function allProfiles(d: DispatchRulesDraft): DispatchProfile[] {
  return [...d.rules.flatMap((r) => r.candidates), ...d.default];
}

/** `at` is a rule's index, or "default". */
export interface Problem { at: number | "default"; message: string }

/** What the daemon would refuse on save, found before asking it. */
export function problems(d: DispatchRulesDraft): Problem[] {
  const out: Problem[] = [];
  const names = new Map<string, number>();
  d.rules.forEach((r, i) => {
    const name = text(r.name);
    if (name) {
      if (names.has(name)) out.push({ at: i, message: `Rule ${names.get(name)! + 1} is already named “${name}”.` });
      else names.set(name, i);
    }
    if (!r.when.trim()) out.push({ at: i, message: "Say when the rule applies." });
    if (!r.candidates.length) out.push({ at: i, message: "A rule needs at least one candidate." });
    if (r.candidates.some((c) => !c.harness.trim())) out.push({ at: i, message: "Pick a harness for every candidate." });
  });
  if (!d.default.length) out.push({ at: "default", message: "The default needs at least one candidate." });
  if (d.default.some((c) => !c.harness.trim())) out.push({ at: "default", message: "Pick a harness for every candidate." });
  return out;
}

/** `list` with the item at `i` moved by `delta`; unchanged when it would leave the list. */
export function move<T>(list: T[], i: number, delta: number): T[] {
  const j = i + delta;
  if (i < 0 || i >= list.length || j < 0 || j >= list.length) return list;
  const next = [...list];
  [next[i], next[j]] = [next[j], next[i]];
  return next;
}

/** `harness:model (effort effort)`, as dispatch records label an agent. */
export function profileLabel(p: DispatchProfile): string {
  return p.harness + (p.model ? `:${p.model}` : "") + (p.effort ? ` (${p.effort} effort)` : "");
}

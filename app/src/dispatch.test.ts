import { describe, expect, it } from "vitest";
import type { DispatchRulesDraft } from "./api";
import { allProfiles, cleanDraft, move, problems, profileKey, profileLabel, sameDraft } from "./dispatch";

const draft = (): DispatchRulesDraft => ({
  default_select: "ordered",
  rules: [
    { name: "trivial-edit", when: "A rename or a typo.", select: null,
      candidates: [{ harness: "claude-code", model: "claude-sonnet-5", effort: "low", pool: null }, { harness: "codex", model: "gpt-5.5" }] },
    { name: null, when: "Fresh news.", candidates: [{ harness: "pi", provider: "x", floor: { scope: "all_models", min_percent: 20 } }], why: "live web", approval: "captain" },
  ],
  default: [{ harness: "claude-code", effort: "medium" }],
});

describe("dispatch draft", () => {
  it("cleans to what is saved and keeps the engine's own fields", () => {
    const d = draft();
    d.rules[0].name = "  trivial-edit ";
    d.rules[0].candidates[1].model = "  ";
    d.rules[0].candidates[1].pool = "";
    const c = cleanDraft(d);
    expect(c.rules[0]).toEqual({
      name: "trivial-edit", when: "A rename or a typo.", select: null,
      candidates: [{ harness: "claude-code", model: "claude-sonnet-5", effort: "low", pool: null }, { harness: "codex", model: null, effort: null, pool: null }],
    });
    expect(c.rules[1]).toMatchObject({ why: "live web", approval: "captain" });
    expect(c.rules[1].candidates[0]).toEqual({ harness: "pi", model: null, effort: null, pool: null, provider: "x", floor: { scope: "all_models", min_percent: 20 } });
    c.rules[0].candidates[0].harness = "pi";
    expect(d.rules[0].candidates[0].harness).toBe("claude-code");
  });

  it("compares drafts as they would be saved", () => {
    const a = draft(), b = draft();
    b.rules[0].candidates[0].pool = " ";
    b.rules[1].name = "";
    expect(sameDraft(a, b)).toBe(true);
    b.rules = move(b.rules, 0, 1);
    expect(sameDraft(a, b)).toBe(false);
  });

  it("keys a candidate by what the harness is asked", () => {
    expect(profileKey({ harness: "codex", model: " m ", effort: "" })).toBe(profileKey({ harness: "codex", model: "m", pricing: "budget" }));
    expect(profileKey({ harness: "codex", model: "m" })).not.toBe(profileKey({ harness: "codex", model: "m", pool: "max" }));
    expect(allProfiles(draft()).map((p) => p.harness)).toEqual(["claude-code", "codex", "pi", "claude-code"]);
    expect(profileLabel({ harness: "codex", model: "gpt-5.5", effort: "low" })).toBe("codex:gpt-5.5 (low effort)");
    expect(profileLabel({ harness: "bob" })).toBe("bob");
  });

  it("finds what the daemon would refuse", () => {
    expect(problems(draft())).toEqual([]);
    const d = draft();
    d.rules[1] = { name: "trivial-edit", when: " ", candidates: [] };
    d.rules.push({ when: "x", candidates: [{ harness: "" }] });
    d.default = [];
    expect(problems(d)).toEqual([
      { at: 1, message: "Rule 1 is already named “trivial-edit”." },
      { at: 1, message: "Say when the rule applies." },
      { at: 1, message: "A rule needs at least one candidate." },
      { at: 2, message: "Pick a harness for every candidate." },
      { at: "default", message: "The default needs at least one candidate." },
    ]);
  });

  it("moves an item and leaves the list alone at its ends", () => {
    const l = ["a", "b", "c"];
    expect(move(l, 0, 1)).toEqual(["b", "a", "c"]);
    expect(move(l, 2, -1)).toEqual(["a", "c", "b"]);
    expect(move(l, 0, -1)).toBe(l);
    expect(move(l, 2, 1)).toBe(l);
  });
});

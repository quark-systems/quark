import { describe, expect, it } from "vitest";
import { promptTitle, screenPrompt } from "./screenPrompt";

// Exactly what quark-supervisor's `Prompt::question` writes.
const Q = "My terminal shows a prompt waiting for a keypress (a permission or approval request, or a menu), so I cannot go on until it is answered."
  + "\n\n```\nBash command\n  npm test -- export\nDo you want to proceed?\n```"
  + "\n\nChoices:\n1. Yes\n2. Yes, and don't ask again for npm test\n3. No, and tell Claude what to do differently (esc)"
  + "\n\nAnswer with the number of the choice (or its label); Quark selects it on my screen.";

describe("screenPrompt", () => {
  it("reads the context and the choices", () => {
    const p = screenPrompt(Q)!;
    expect(p.options).toEqual(["Yes", "Yes, and don't ask again for npm test", "No, and tell Claude what to do differently (esc)"]);
    expect(p.context).toEqual(["Bash command", "npm test -- export", "Do you want to proceed?"]);
    expect(promptTitle(p)).toBe("Do you want to proceed?");
  });
  it("ignores other questions", () => {
    expect(screenPrompt("Keep the full answer history per decision?")).toBeNull();
  });
  it("works without context lines", () => {
    const q = Q.replace(/\n\n```[\s\S]*?```/, "");
    const p = screenPrompt(q)!;
    expect(p.context).toEqual([]);
    expect(p.options).toHaveLength(3);
    expect(promptTitle(p)).toMatch(/waiting for a choice/);
  });
});

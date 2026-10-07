import { describe, expect, it } from "vitest";
import { shortModel } from "./agentLine";

describe("shortModel", () => {
  it("drops prefixes and suffixes a card has no room for", () => {
    expect(shortModel("claude-opus-5-5")).toBe("opus-5-5");
    expect(shortModel("claude-sonnet-4-5-20250929")).toBe("sonnet-4-5");
    expect(shortModel("anthropic.claude-haiku-4-5")).toBe("haiku-4-5");
    expect(shortModel("claude-opus-5-5[1m]")).toBe("opus-5-5");
    expect(shortModel("openai/gpt-5.5")).toBe("gpt-5.5");
    expect(shortModel("gpt-5-codex")).toBe("gpt-5-codex");
  });
});

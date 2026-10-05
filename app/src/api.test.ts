import { afterEach, describe, expect, it, vi } from "vitest";
import { api, ApiError, NotAvailable } from "./api";

const respond = (status: number, body?: unknown, type = "application/json") =>
  vi.stubGlobal("fetch", vi.fn(async () => new Response(body === undefined ? null : typeof body === "string" ? body : JSON.stringify(body),
    { status, headers: body === undefined ? {} : { "content-type": type } })));

afterEach(() => vi.unstubAllGlobals());

describe("api errors", () => {
  it("reads a bare 404 as a route the daemon does not serve yet", async () => {
    respond(404);
    await expect(api.transcript("t1")).rejects.toBeInstanceOf(NotAvailable);
  });
  it("reads 405 and 501 the same way", async () => {
    respond(405);
    await expect(api.cancel("t1")).rejects.toBeInstanceOf(NotAvailable);
    respond(501);
    await expect(api.relaunch("t1")).rejects.toBeInstanceOf(NotAvailable);
  });
  it("keeps a daemon not_found as a real error", async () => {
    respond(404, { error: { code: "not_found", message: "resource not found" } });
    const e = await api.task("nope").catch((x) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect(e.code).toBe("not_found");
  });
  it("surfaces the daemon's error message", async () => {
    respond(409, { error: { code: "conflict", message: "task already finished" } });
    await expect(api.cancel("t1")).rejects.toThrow("task already finished (409)");
  });
  it("treats 202 as success with no body", async () => {
    respond(202);
    await expect(api.steer("t1", "hi")).resolves.toBeUndefined();
  });
});

describe("decisions", () => {
  it("answers with who answered and returns the decision", async () => {
    const answered = { id: "d 1", project_id: "p", question: "q", state: "answered", answer: "yes", answered_by: "matt", opened_at: "t", answered_at: "t" };
    respond(200, answered);
    await expect(api.answerDecision("d 1", { answer: "yes", answered_by: "matt" })).resolves.toEqual(answered);
    const [url, init] = vi.mocked(fetch).mock.calls[0];
    expect(String(url)).toMatch(/\/v1\/decisions\/d%201:answer$/);
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({ answer: "yes", answered_by: "matt" });
  });
});

describe("memory", () => {
  it("accepts a proposal with edited text under its Project", async () => {
    const accepted = { id: "mpr 1", project_id: "p", text: "edited", evidence: { files: [] }, source: "worker", state: "accepted", proposed_at: "t" };
    respond(200, accepted);
    await expect(api.acceptMemoryProposal("p", "mpr 1", { text: "edited" })).resolves.toEqual(accepted);
    const [url, init] = vi.mocked(fetch).mock.calls[0];
    expect(String(url)).toMatch(/\/v1\/projects\/p\/memory\/proposals\/mpr%201:accept$/);
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({ text: "edited" });
  });
  it("filters proposals by state", async () => {
    respond(200, []);
    await api.memoryProposals("p", "proposed");
    expect(String(vi.mocked(fetch).mock.calls[0][0])).toMatch(/\/v1\/projects\/p\/memory\/proposals\?state=proposed$/);
  });
});

describe("dispatch test", () => {
  it("posts the description to the Project's dispatch:test", async () => {
    const result = { project_id: "p 1", decided_by: "coordinator", summary: "s", rule: null, chosen: null, candidates: [],
      resolution: { status: "off", notes: [] }, classifier: { provider: "none" } };
    respond(200, result);
    await expect(api.testDispatch("p 1", "Rename a field")).resolves.toEqual(result);
    const [url, init] = vi.mocked(fetch).mock.calls[0];
    expect(String(url)).toMatch(/\/v1\/projects\/p%201\/dispatch:test$/);
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({ description: "Rename a field" });
  });
  it("sends unsaved rules as the draft to test", async () => {
    respond(200, {});
    const draft = { default_select: null, rules: [], default: [{ harness: "codex" }] };
    await api.testDispatch("p", "Rename a field", draft);
    expect(JSON.parse(String(vi.mocked(fetch).mock.calls[0][1]?.body))).toEqual({ description: "Rename a field", draft });
  });
  it("loads the rules and saves them against the revision they were loaded at", async () => {
    const rules = { project_id: "p 1", revision: "abc", commit: "def", classifier: { provider: "none" }, default_select: "ordered" as const,
      rules: [{ name: "big", when: "A big feature.", candidates: [{ harness: "claude-code", effort: "high" }] }], default: [{ harness: "codex" }] };
    respond(200, rules);
    await expect(api.dispatchRules("p 1")).resolves.toEqual(rules);
    expect(String(vi.mocked(fetch).mock.calls[0][0])).toMatch(/\/v1\/projects\/p%201\/dispatch$/);
    respond(200, rules);
    const { default_select, rules: list, default: fallback } = rules;
    await api.saveDispatchRules("p 1", { default_select, rules: list, default: fallback }, "abc");
    const [url, init] = vi.mocked(fetch).mock.calls[0];
    expect(String(url)).toMatch(/\/v1\/projects\/p%201\/dispatch$/);
    expect(init?.method).toBe("PUT");
    expect(JSON.parse(String(init?.body))).toEqual({ revision: "abc", default_select, rules: list, default: fallback });
  });
});

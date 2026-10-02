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

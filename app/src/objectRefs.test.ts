import { describe, expect, it } from "vitest";
import { aboutLine, parseAbout, resolveRef } from "./objectRefs";
import { dockMessage } from "./shell/Dock";

const s = {
  tasks: { t1: { project_id: "quark", title: "attention-model", state: "running" as const } },
  pullRequests: { pr1: { project_id: "web", title: "Fix login", repo: "o/web", number: 4, state: "merged" as const } },
  decisions: { dec_1: { id: "dec_1", number: 14, project_id: "quark", question: "Rebuild on Tempo's real tables?", state: "open" as const } },
};

describe("parseAbout", () => {
  it("reads back the line the dock sends, titles with quotes included", () => {
    const text = dockMessage("Does this keep it read-only?", { projectId: "p", pinned: true, about: 'Is "lake" read-only?', link: "#/p/p/decisions/dec_1" });
    expect(parseAbout(text)).toEqual({ about: { title: 'Is "lake" read-only?', link: "#/p/p/decisions/dec_1" }, body: "Does this keep it read-only?" });
    expect(aboutLine({ title: "x", link: "#/t/t1" })).toBe('About "x" (#/t/t1):');
  });
  it("leaves other messages alone", () => {
    expect(parseAbout("About the login fix: why?")).toBeNull();
    expect(parseAbout('see About "x" (#/t/t1):\nhi')).toBeNull();
  });
});

describe("resolveRef", () => {
  it("shows a decision with its number, live question and state, opened in its project", () => {
    expect(resolveRef({ title: "old words", link: "#/inbox/dec_1" }, s)).toEqual({
      kind: "decision", tag: "D-14", title: "Rebuild on Tempo's real tables?", status: { label: "Needs you", tone: "needs-you" },
      href: "#/p/quark/decisions/dec_1", project: "quark",
    });
  });
  it("shows workers and pull requests", () => {
    expect(resolveRef({ title: "x", link: "#/t/t1" }, s)).toMatchObject({ kind: "worker", title: "attention-model", status: { label: "Busy" } });
    expect(resolveRef({ title: "x", link: "#/pr/pr1" }, s)).toMatchObject({ kind: "pull-request", tag: "#4", status: { label: "Merged" }, href: "#/p/web/prs/pr1" });
  });
  it("keeps the message's title and link for an object it does not know", () => {
    expect(resolveRef({ title: "Gone?", link: "#/p/quark/decisions/dec_9" }, s)).toEqual({ kind: "decision", title: "Gone?", href: "#/p/quark/decisions/dec_9", project: "quark" });
    expect(resolveRef({ title: "Metrics", link: "#/p/quark/metrics" }, s)).toMatchObject({ kind: "page" });
  });
});

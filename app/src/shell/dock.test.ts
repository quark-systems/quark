import { describe, expect, it } from "vitest";
import { dockContext, dockMessage } from "./Dock";

const s = {
  tasks: { t1: { project_id: "quark", title: "attention-model" } },
  pullRequests: { pr1: { project_id: "web", title: null, repo: "o/web", number: 4 } },
  decisions: { d1: { project_id: "quark", question: "Switch slice 2?" } },
};

describe("dockContext", () => {
  it("carries the worker, PR or decision on screen and its project", () => {
    expect(dockContext({ name: "task", id: "t1" }, s, "web")).toEqual({ projectId: "quark", pinned: true, about: "attention-model", link: "#/t/t1" });
    expect(dockContext({ name: "pr", id: "pr1" }, s, null)).toMatchObject({ projectId: "web", about: "o/web#4" });
    expect(dockContext({ name: "inbox", id: "d1" }, s, null)).toMatchObject({ projectId: "quark", about: "Switch slice 2?" });
    expect(dockContext({ name: "metrics", project: "web" }, s, "quark")).toEqual({ projectId: "web", pinned: true, about: null, link: null });
  });
  it("falls back to the last project when the screen has none", () => {
    expect(dockContext({ name: "accounts" }, s, "quark")).toEqual({ projectId: "quark", pinned: false, about: null, link: null });
    expect(dockContext({ name: "task", id: "gone" }, s, "web")).toMatchObject({ projectId: "web", pinned: false });
  });
});

describe("dockMessage", () => {
  it("puts what the message is about on its first line", () => {
    expect(dockMessage("Why is this slow?", { projectId: "quark", pinned: true, about: "attention-model", link: "#/t/t1" }))
      .toBe('About "attention-model" (#/t/t1):\nWhy is this slow?');
    expect(dockMessage("Plan the week", { projectId: "quark", pinned: false, about: null, link: null })).toBe("Plan the week");
  });
});

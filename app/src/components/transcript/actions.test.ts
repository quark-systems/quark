import { describe, expect, it } from "vitest";
import type { ToolInfo, TranscriptItem } from "../../api";
import { actionTitle, coordinatorAction } from "./actions";
import type { Step, StepStatus } from "./turns";

const shell = (command: string, status: StepStatus = "ok", tool: Partial<ToolInfo> | null = {}): Step => {
  const call: TranscriptItem = {
    id: 1, role: "tool_call", text: JSON.stringify({ command }), tool_name: "Bash", is_error: false, truncated: false,
    tool: tool ? { kind: "shell", title: `Run ${command}`, command, ...tool } : undefined,
  };
  return { key: 1, call, status };
};

describe("coordinatorAction", () => {
  it("recognizes workers, scouts and second mates being started", () => {
    expect(coordinatorAction(shell("bin/fm-spawn.sh event-stream projects/quark --mode no-mistakes --yolo off")))
      .toEqual({ kind: "spawn", task: "event-stream", role: "worker", relaunch: false });
    expect(coordinatorAction(shell('cd "$FM_HOME" && ./bin/fm-spawn.sh ui-audit projects/quark --scout')))
      .toMatchObject({ task: "ui-audit", role: "scout" });
    expect(coordinatorAction(shell("/opt/fm/bin/fm-spawn.sh web-home --secondmate"))).toMatchObject({ role: "second mate" });
    const relaunch = coordinatorAction(shell("bin/fm-spawn.sh event-stream --relaunch"))!;
    expect(relaunch).toMatchObject({ relaunch: true });
    expect(actionTitle(relaunch)).toBe("Relaunched worker");
  });

  it("recognizes a question held for the person, with its reason", () => {
    expect(coordinatorAction(shell('bin/fm-captain-hold.sh hold decision-records --reason "Keep ADRs in docs/ or a wiki?"')))
      .toEqual({ kind: "decision", task: "decision-records", reason: "Keep ADRs in docs/ or a wiki?" });
    expect(coordinatorAction(shell("bin/fm-captain-hold.sh hold x --reason='quoted'"))).toMatchObject({ reason: "quoted" });
    expect(coordinatorAction(shell("bin/fm-captain-hold.sh hold x"))).toEqual({ kind: "decision", task: "x", reason: undefined });
    expect(actionTitle({ kind: "decision", task: "x" })).toBe("Asked you to decide");
  });

  it("reads the command from the raw input when the daemon recorded no summary", () => {
    expect(coordinatorAction(shell("bin/fm-spawn.sh a dir", "ok", null))).toMatchObject({ kind: "spawn", task: "a" });
  });

  it("ignores failed calls, other tools and lookalikes", () => {
    expect(coordinatorAction(shell("bin/fm-spawn.sh a dir", "error"))).toBeNull();
    expect(coordinatorAction(shell("sed -n 1,40p bin/fm-spawn.sh"))).toBeNull();
    expect(coordinatorAction(shell("bin/fm-captain-hold.sh release x"))).toBeNull();
    expect(coordinatorAction(shell("bin/fm-spawn.sh a", "ok", { kind: "read" }))).toBeNull();
  });

  it("reads the native coordinator's ask_user calls as decisions, with the number it was filed under", () => {
    const ask = (input: unknown, result?: string, status: StepStatus = "ok"): Step => ({
      key: 2, status,
      call: { id: 2, role: "tool_call", text: JSON.stringify(input), tool_name: "mcp__quark-coordinator__ask_user", is_error: false, truncated: false, tool: { kind: "other", title: "quark-coordinator: ask_user" } },
      result: result === undefined ? undefined : { id: 3, role: "tool_result", text: result, is_error: false, truncated: false },
    });
    const input = { key: "cache", question: "Redis or memory?", context: "Sessions need a store.", options: ["Redis", { label: "Memory", consequence: "Lost on restart" }], recommended: "Redis" };
    expect(coordinatorAction(ask(input, "Asked the user (decision 12). Their answer wakes you; keep the rest of the work moving."))).toEqual({
      kind: "ask", key: "cache", question: "Redis or memory?", context: "Sessions need a store.",
      options: [{ label: "Redis" }, { label: "Memory", consequence: "Lost on restart" }], recommended: "Redis", number: 12,
    });
    expect(coordinatorAction(ask(input))).toMatchObject({ kind: "ask", number: undefined });
    expect(actionTitle(coordinatorAction(ask(input))!)).toBe("Asked you to decide");
    expect(coordinatorAction(ask(input, "refused", "error"))).toBeNull();
    expect(coordinatorAction(ask({ key: "k" }))).toBeNull();
  });
});

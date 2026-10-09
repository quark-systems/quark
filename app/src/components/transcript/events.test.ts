import { describe, expect, it } from "vitest";
import { engineCommandTitle, engineEvent, isListening, wakeTitle } from "./events";
import type { Step } from "./turns";

// Copied from a coordinator session: Claude Code delivers the Stop hook's rewake like this.
const STOP_WAKE = `<task-notification>
<summary>Stop hook feedback</summary>
</task-notification>
<system-reminder>
Stop hook blocking error from command "Stop": firstmate watcher wake - one supervision event needs a handling turn now.
stale: quark:fm-q-task-done-on-merge
Run bin/fm-wake-drain.sh first, handle the wake, then run its exact WAKE_ACK_REQUIRED --ack-through command.

</system-reminder>`;

const shell = (command: string, status: Step["status"] = "running"): Step => ({
  key: 1, status,
  call: { id: 1, role: "tool_call", text: JSON.stringify({ command }), is_error: false, truncated: false, tool: { kind: "shell", title: `Run ${command}`, command } },
});

describe("engineEvent", () => {
  it("reads a watcher wake from the Stop hook as one plain line", () => {
    const e = engineEvent(STOP_WAKE);
    expect(e).toMatchObject({ kind: "wake", title: "fm-q-task-done-on-merge stopped responding" });
    expect(e?.raw).toBe(STOP_WAKE);
  });

  it("names each kind of wake", () => {
    expect(wakeTitle("signal: quark:fix-login")).toBe("Update from fix-login");
    expect(wakeTitle("heartbeat")).toBe("Routine project check");
    expect(wakeTitle("check: pr-merged quark#12")).toBe("Check: pr-merged quark#12");
  });

  it("flags a monitoring failure", () => {
    const e = engineEvent("<system-reminder>Stop hook blocking error: firstmate watcher auto-arm FAILED twice</system-reminder>");
    expect(e?.kind).toBe("warning");
  });

  it("reads background-task notices", () => {
    const e = engineEvent("<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>cargo test finished</summary>\n</task-notification>");
    expect(e).toMatchObject({ kind: "background", title: "Background task: cargo test finished (completed)" });
  });

  it("reads firstmate's marked operational inputs", () => {
    expect(engineEvent("\u2063FIRSTMATE_OP: v1 away-supervisor: 2 PRs ready")?.title).toBe("Update while you were away");
    expect(engineEvent("\u2063FIRSTMATE_OP: v1 launch-brief: do the thing")).toBeNull();
  });

  it("leaves what a person typed alone, even next to engine tags", () => {
    expect(engineEvent("Merge #12 when it is green")).toBeNull();
    expect(engineEvent("why does it say <system-reminder>?")).toBeNull();
    expect(engineEvent("<system-reminder>context</system-reminder>\nplease rebase")).toBeNull();
  });
});

describe("firstmate commands", () => {
  const H = "H=/Users/m/.quark/workspaces/prj_1; export FM_HOME=$H; ";
  it("titles them plainly", () => {
    expect(engineCommandTitle(shell(`${H}$H/bin/fm-wake-drain.sh`))).toBe("Read new project events");
    expect(engineCommandTitle(shell(`${H}$H/bin/fm-wake-drain.sh --ack-through 12 --generation 3`))).toBe("Marked project events handled");
    expect(engineCommandTitle(shell("cargo test"))).toBeNull();
  });

  it("knows which ones only wait for events", () => {
    expect(isListening(shell(`${H}$H/bin/fm-watch-arm.sh`))).toBe(true);
    expect(isListening(shell("bin/fm-watch.sh --once"))).toBe(true);
    expect(isListening(shell(`${H}$H/bin/fm-wake-drain.sh`))).toBe(false);
  });
});

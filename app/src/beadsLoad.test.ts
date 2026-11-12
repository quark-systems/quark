import { describe, expect, it, vi } from "vitest";
import { api, type BeadsStatus } from "./api";
import { getState, loadBeads, setBeads } from "./store";

const status = (state: BeadsStatus["state"]): BeadsStatus => ({ project_id: "p1", state });

describe("loadBeads", () => {
  it("keeps a status that arrived while the request was in flight", async () => {
    let answer!: (b: BeadsStatus) => void;
    const spy = vi.spyOn(api, "beads").mockReturnValue(new Promise((r) => { answer = r; }));
    const loading = loadBeads("p1");
    setBeads(status("ready")); // the beads.status event, newer than the answer below
    answer(status("setting_up"));
    await loading;
    expect(getState().beads.p1.state).toBe("ready");
    spy.mockRestore();
  });
});

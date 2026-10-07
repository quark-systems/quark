import { describe, expect, it } from "vitest";
import { Persona } from "./api";
import { labels } from "./persona";

const kitchen: Persona = {
  id: "kitchen-brigade", name: "Kitchen brigade", builtin: true, address: "chef", voice: "", vocabulary: [],
  roles: { coordinator: "expo", worker: "line cook", decision: "chef's call" },
  ui_labels: { tasks: "Tickets" },
};

describe("labels", () => {
  it("uses the pack's role names and app labels", () => {
    const l = labels(kitchen);
    expect(l.role("coordinator")).toBe("expo");
    expect(l.Role("worker")).toBe("Line cook");
    expect(l.ui("tasks", "Tasks")).toBe("Tickets");
  });

  it("falls back to neutral names without a pack or a label", () => {
    const none = labels(null);
    expect(none.Role("coordinator")).toBe("Coordinator");
    expect(none.role("sub_coordinator")).toBe("sub-coordinator");
    expect(none.ui("memory", "Memory")).toBe("Memory");
    expect(labels(kitchen).role("user")).toBe("user");
    expect(labels(kitchen).ui("memory", "Memory")).toBe("Memory");
  });
});

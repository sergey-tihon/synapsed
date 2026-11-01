import { describe, expect, it } from "vitest";
import { validateInspectorCommand } from "../src/inspector-command.js";
import { device, characteristic, snapshot } from "./fixtures.js";

const binding = { deviceId: "monitor", characteristicId: "brightness" };

describe("validateInspectorCommand", () => {
  it("parses a typed numeric set through characteristic metadata", () => {
    expect(validateInspectorCommand(snapshot(), binding, "set", "37")).toEqual({
      command: { op: "set", value: 37 },
      error: null,
    });
  });

  it("rejects malformed, out-of-range and unsupported commands", () => {
    expect(validateInspectorCommand(snapshot(), binding, "set", "not-number").error).toMatch(/decimal/i);
    expect(validateInspectorCommand(snapshot(), binding, "set", "101").error).toMatch(/bounds/i);
    expect(validateInspectorCommand(snapshot(), null, "set", "2").error).toContain("Select a device");
  });

  it("validates step syntax and returns action commands", () => {
    expect(validateInspectorCommand(snapshot(), binding, "step", "-3").command).toEqual({ op: "step", steps: -3 });
    expect(validateInspectorCommand(snapshot(), binding, "step", "1.5").error).toContain("whole step");
    const action = snapshot([device([characteristic("action", null)])]);
    expect(validateInspectorCommand(action, binding, "invoke").command).toEqual({ op: "invoke" });
  });

  it("keeps default behavior as an unset command", () => {
    expect(validateInspectorCommand(snapshot(), binding, "default")).toEqual({ command: null, error: null });
  });
});

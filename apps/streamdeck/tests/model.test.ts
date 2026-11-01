import { describe, expect, it } from "vitest";
import { clampDialTicks, mergeValidatedCommand } from "../src/model.js";

describe("Stream Deck model helpers", () => {
  it("clamps dial ticks to the supported range", () => {
    expect(clampDialTicks(4.9)).toBe(4);
    expect(clampDialTicks(101)).toBe(100);
    expect(clampDialTicks(-101)).toBe(-100);
    expect(Number.isNaN(clampDialTicks(Number.NaN))).toBe(true);
  });

  it("merges or removes one command without disturbing bindings or the other slot", () => {
    const settings = {
      binding: { deviceId: "monitor", characteristicId: "brightness" },
      pressBinding: { deviceId: "monitor", characteristicId: "power" },
      pressCommand: { op: "toggle" } as const,
    };
    expect(mergeValidatedCommand(settings, "key", { op: "set", value: 31 })).toEqual({
      ...settings,
      keyCommand: { op: "set", value: 31 },
    });
    expect(mergeValidatedCommand({ ...settings, keyCommand: { op: "step", steps: 1 } }, "key", null)).toEqual(settings);
    expect(mergeValidatedCommand(settings, "press", null)).toEqual({
      binding: settings.binding,
      pressBinding: settings.pressBinding,
    });
  });
});

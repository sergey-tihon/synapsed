import { describe, expect, it } from "vitest";
import { isBinding, settingsConform } from "../src/settings.js";

const binding = { deviceId: "monitor", characteristicId: "brightness" };

describe("settings validation", () => {
  it("accepts exact device and characteristic bindings", () => {
    expect(isBinding(binding)).toBe(true);
    expect(isBinding({ ...binding, extra: true })).toBe(false);
    expect(isBinding({ deviceId: "../monitor", characteristicId: "brightness" })).toBe(false);
    expect(isBinding(null)).toBe(false);
  });

  it("accepts supported saved commands and rejects malformed values", () => {
    expect(settingsConform({ binding, keyCommand: { op: "set", value: 42 } })).toBe(true);
    expect(settingsConform({ binding: binding as never, keyCommand: { op: "unknown" } as never })).toBe(false);
    expect(settingsConform({ pressBinding: { deviceId: "Bad ID", characteristicId: "power" } as never })).toBe(false);
  });
});

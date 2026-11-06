import { CharacteristicValueError, type Characteristic } from "@synapsed/client";
import { describe, expect, it } from "vitest";
import { commandForMode, commandModes, initialCommand, modeOf, parseSetValue } from "../src/commands.js";
import { inspectorCharacteristic } from "../src/catalog.js";
import { characteristic } from "./fixtures.js";

const inspect = (item: Characteristic) => inspectorCharacteristic(item);

describe("command mode model", () => {
  it("offers boolean modes and no modes for read-only characteristics", () => {
    expect(commandModes(inspect(characteristic("boolean", true))!)).toEqual(["toggle", "on", "off"]);
    expect(commandModes(inspect(characteristic("number", 50, "ok", false))!)).toEqual([]);
  });

  it("defaults an unstepped number to its observed set value", () => {
    const item = characteristic("number", 62);
    const unstepped = {
      ...item,
      spec: { ...item.spec, step: null },
    } as Characteristic;
    const c = inspect(unstepped)!;
    expect(commandModes(c)).toEqual(["set"]);
    expect(initialCommand(c)).toEqual({ op: "set", value: 62 });
  });

  it("uses the declared minimum when an unstepped number has no observation", () => {
    const item = characteristic("number", null);
    const bounded = {
      ...item,
      spec: { ...item.spec, minimum: 62, step: null },
    } as Characteristic;
    expect(initialCommand(inspect(bounded)!)).toEqual({ op: "set", value: 62 });
  });

  it("retains step magnitude when changing direction and maps set booleans", () => {
    const c = inspect(characteristic())!;
    expect(commandForMode(c, "decrease", { op: "step", steps: 3 })).toEqual({ op: "step", steps: -3 });
    expect(modeOf({ op: "set", value: false })).toBe("off");
  });

  it("parses set values against metadata and rejects malformed numbers", () => {
    const c = inspect(characteristic())!;
    expect(parseSetValue(c, "37")).toEqual({ op: "set", value: 37 });
    expect(() => parseSetValue(c, "abc")).toThrow(CharacteristicValueError);
  });
});

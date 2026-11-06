import { describe, expect, it } from "vitest";
import { clampDialTicks } from "../src/model.js";

describe("Stream Deck model helpers", () => {
  it("clamps dial ticks to the supported range", () => {
    expect(clampDialTicks(4.9)).toBe(4);
    expect(clampDialTicks(101)).toBe(100);
    expect(clampDialTicks(-101)).toBe(-100);
    expect(Number.isNaN(clampDialTicks(Number.NaN))).toBe(true);
  });
});

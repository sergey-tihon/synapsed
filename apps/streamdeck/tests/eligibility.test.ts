import { describe, expect, it } from "vitest";
import { keyEligible, pressEligible, rotateEligible } from "../src/eligibility.js";
import { characteristic } from "./fixtures.js";

const withoutStep = () => {
  const item = characteristic();
  return {
    ...item,
    spec: { ...item.spec, step: null },
  } as typeof item;
};

describe("action characteristic eligibility", () => {
  it("requires write access for every action kind", () => {
    const readOnly = characteristic("number", 50, "ok", false);
    expect(keyEligible(readOnly)).toBe(false);
    expect(rotateEligible(readOnly)).toBe(false);
    expect(pressEligible(readOnly)).toBe(false);
  });

  it("allows stepped numbers and enums for rotation, but not unstepped numbers", () => {
    expect(keyEligible(withoutStep())).toBe(true);
    expect(rotateEligible(withoutStep())).toBe(false);
    expect(rotateEligible(characteristic("enum", "low"))).toBe(true);
  });

  it("allows only boolean and action characteristics for press", () => {
    expect(pressEligible(characteristic("boolean", true))).toBe(true);
    expect(pressEligible(characteristic("action", null))).toBe(true);
    expect(pressEligible(characteristic("number", 50))).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import type { ActionSettings } from "../src/settings.js";
import { renderDisplay } from "../src/render.js";
import { characteristic, device, snapshot } from "./fixtures.js";

const binding = { deviceId: "monitor", characteristicId: "brightness" };
const render = (
  settings: ActionSettings = {},
  state = snapshot(),
  connection: "connected" | "connecting" | "disconnected" | "incompatible" = "connected",
  pending = false,
) => renderDisplay(settings, state, connection, pending);

describe("renderDisplay", () => {
  it("renders configure and invalid settings states", () => {
    expect(render().title).toBe("Synapsed\nConfigure");
    expect(render({ keyCommand: { op: "bad" } as never }).title).toBe("Synapsed\nUnsupported");
  });

  it("renders connection and missing-binding states", () => {
    expect(render({ binding }, snapshot(), "incompatible").title).toContain("Incompatible");
    expect(render({ binding }, snapshot([])).title).toBe("Synapsed · Missing\nMissing");
    expect(render({ binding }, snapshot(), "connecting").title).toContain("Connecting");
    expect(render({ binding }, snapshot(), "disconnected").title).toContain("Service Offline");
  });

  it("marks unavailable device and characteristic observations as stale", () => {
    expect(render({ binding }, snapshot([device(undefined, "unavailable")])).title).toContain("Device unavailable");
    expect(render({ binding }, snapshot([device([characteristic("number", 50, "unavailable")])])).title).toContain(
      "Unavailable",
    );
    expect(render({ binding }, snapshot([device([characteristic("number", 50, "error")])])).title).toContain(
      "Characteristic error",
    );
  });

  it("reports driver conflict and unsupported saved commands", () => {
    const conflict = device();
    conflict.error = { code: "driver_conflict", message: "conflict" };
    expect(render({ binding }, snapshot([conflict])).title).toContain("Driver conflict");
    expect(render({ binding, keyCommand: { op: "toggle" } }, snapshot()).title).toContain("Unsupported");
  });

  it("renders a missing or unsupported secondary press binding", () => {
    expect(render({ binding, pressBinding: { deviceId: "missing", characteristicId: "power" } }).title).toContain(
      "Missing press binding",
    );
    expect(render({ binding, pressCommand: { op: "set", value: "bad" } }, snapshot()).title).toContain(
      "Unsupported press",
    );
  });

  it("includes a secondary boolean value and pending status", () => {
    const state = snapshot([
      device([characteristic(), { ...characteristic("boolean", true), id: "power", label: "Power" }]),
    ]);
    const output = render(
      { binding, pressBinding: { deviceId: "monitor", characteristicId: "power" } },
      state,
      "connected",
      true,
    );
    expect(output.title).toContain("Power: On");
    expect(output.title).toContain("Pending");
  });

  it("renders bounded numeric and enum feedback", () => {
    const number = render({ binding }, snapshot());
    expect(number.feedback.bar.enabled).toBe(true);
    expect(number.feedback.bar.value).toBe(50);
    const enumState = snapshot([device([characteristic("enum", "high")])]);
    const enumeration = render({ binding }, enumState);
    expect(enumeration.title).toContain("High");
    expect(enumeration.feedback.bar.enabled).toBe(true);
    expect(enumeration.feedback.bar.value).toBe(100);
  });

  it("renders action and text values without a feedback range", () => {
    const action = render({ binding }, snapshot([device([characteristic("action", null)])]));
    expect(action.title).toContain("Action");
    expect(action.feedback.bar.enabled).toBe(false);
    const text = render({ binding }, snapshot([device([characteristic("string", "hello")])]));
    expect(text.title).toContain("hello");
    expect(text.feedback.bar.enabled).toBe(false);
  });
});

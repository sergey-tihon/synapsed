import { describe, expect, it } from "vitest";
import { renderDial, renderKey } from "../src/render.js";
import { characteristic, device, snapshot } from "./fixtures.js";

const key = (
  settings: Parameters<typeof renderKey>[0] = {},
  state = snapshot(),
  connection = "connected",
  pending = false,
) => renderKey(settings, state, connection as "connected" | "connecting" | "disconnected" | "incompatible", pending);

describe("Key rendering", () => {
  it("shows device, characteristic and observed value, with custom title replacing the labels", () => {
    expect(key({ deviceId: "monitor", characteristicId: "brightness", command: { op: "step", steps: 1 } })).toBe(
      "Monitor\nBrightness\n50 %",
    );
    expect(
      key({
        deviceId: "monitor",
        characteristicId: "brightness",
        command: { op: "step", steps: 1 },
        title: "Desk up",
      }),
    ).toBe("Desk up\n50 %");
  });
  it("shows a compact target while preserving stale readings", () => {
    const settings = { deviceId: "monitor", characteristicId: "brightness", command: { op: "set", value: 80 } };
    expect(key(settings)).toBe("Monitor\nBrightness\nto 80%");
    expect(key(settings, snapshot(), "disconnected")).toBe("Monitor\nBrightness\n50 % (stale) · to 80%");
  });

  it("shows configure, unsupported commands and missing command states", () => {
    expect(key()).toBe("Synapsed\nConfigure");
    expect(key({ deviceId: "monitor", characteristicId: "brightness" })).toBe("Monitor\nBrightness\nChoose action");
    expect(key({ title: 7 } as never)).toBe("Synapsed\nUnsupported");
  });

  it("retains stale readings and appends pending status", () => {
    const settings = { deviceId: "monitor", characteristicId: "brightness", command: { op: "step", steps: 1 } };
    expect(key(settings, snapshot(), "disconnected").split("\n").at(-1)).toBe("50 % (stale)");
    expect(key(settings, snapshot(), "connected", true)).toBe("Monitor\nBrightness\n50 % · Pending");
  });

  it("omits the value line for healthy action characteristics", () => {
    const state = snapshot([device([characteristic("action", null)])]);
    expect(key({ deviceId: "monitor", characteristicId: "brightness", command: { op: "invoke" } }, state)).toBe(
      "Monitor\nBrightness",
    );
  });
});

describe("Dial rendering", () => {
  it("shows a title, authoritative value and bounded numeric feedback", () => {
    const output = renderDial({ deviceId: "monitor", characteristicId: "brightness" }, snapshot(), "connected", false);
    expect(output.title).toBe("Monitor · Brightness");
    expect(output.current).toBe("50 %");
    expect(output.bar).toEqual({ value: 50, enabled: true });
  });

  it("includes the same-device boolean press reading in the title", () => {
    const state = snapshot([
      device([characteristic(), { ...characteristic("boolean", true), id: "power", label: "Power" }]),
    ]);
    const output = renderDial(
      {
        deviceId: "monitor",
        characteristicId: "brightness",
        pressCharacteristicId: "power",
        pressCommand: { op: "toggle" },
      },
      state,
      "connected",
      false,
    );
    expect(output.title).toBe("Monitor · Brightness · Power: On");
  });
});

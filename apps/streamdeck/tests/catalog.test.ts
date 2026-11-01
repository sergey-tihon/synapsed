import { describe, expect, it } from "vitest";
import { buildCatalog } from "../src/catalog.js";
import type { Selection } from "../src/protocol.js";
import { device, snapshot } from "./fixtures.js";

const selection: Selection = {
  deviceId: "monitor",
  characteristicId: "brightness",
  pressDeviceId: "gone",
  pressCharacteristicId: "power",
};

describe("buildCatalog", () => {
  it("retains missing saved IDs while resolving the available primary characteristic", () => {
    const result = buildCatalog(snapshot(), "connected", selection);
    expect(result.selection).toEqual(selection);
    expect(result.primary?.label).toBe("Brightness");
    expect(result.press).toBeNull();
  });

  it("includes unavailable devices and their characteristics for recovery", () => {
    const result = buildCatalog(
      snapshot([device(undefined, "unavailable")]),
      "disconnected",
      selection,
      "offline config",
    );
    expect(result.devices[0]?.status).toBe("unavailable");
    expect(result.primary?.status).toBe("ok");
    expect(result.selection.deviceId).toBe("monitor");
  });

  it("preserves device groups and characteristic group membership", () => {
    const grouped = device([{ ...snapshot().devices[0]!.characteristics[0]!, group: "general" }], "online");
    grouped.groups = [{ id: "general", label: "General" }];
    const result = buildCatalog(snapshot([grouped]), "connected", selection);
    expect(result.devices[0]?.groups).toEqual([{ id: "general", label: "General" }]);
    expect(result.devices[0]?.characteristics[0]?.group).toBe("general");
  });
  it("handles an empty device catalog without dropping selections", () => {
    const result = buildCatalog(snapshot([]), "connected", selection);
    expect(result.primary).toBeNull();
    expect(result.selection).toEqual(selection);
  });
});

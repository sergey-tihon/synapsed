import { describe, expect, it } from "vitest";
import { buildCatalog } from "../src/catalog.js";
import { device, snapshot } from "./fixtures.js";

describe("buildCatalog", () => {
  it("returns connection, configuration errors and the available device catalog", () => {
    const result = buildCatalog(snapshot(), "connected", "offline config");
    expect(result.connection).toBe("connected");
    expect(result.configurationError).toBe("offline config");
    expect(result.devices[0]?.characteristics[0]?.label).toBe("Brightness");
  });

  it("includes unavailable devices and their characteristics for recovery", () => {
    const result = buildCatalog(snapshot([device(undefined, "unavailable")]), "disconnected");
    expect(result.devices[0]?.status).toBe("unavailable");
    expect(result.devices[0]?.characteristics[0]?.status).toBe("ok");
    expect(result.configurationError).toBeNull();
  });

  it("preserves device groups and characteristic group membership", () => {
    const grouped = device([{ ...snapshot().devices[0]!.characteristics[0]!, group: "general" }], "online");
    grouped.groups = [{ id: "general", label: "General" }];
    const result = buildCatalog(snapshot([grouped]), "connected");
    expect(result.devices[0]?.groups).toEqual([{ id: "general", label: "General" }]);
    expect(result.devices[0]?.characteristics[0]?.group).toBe("general");
  });

  it("handles an empty device catalog", () => {
    expect(buildCatalog(snapshot([]), "connected").devices).toEqual([]);
  });
});

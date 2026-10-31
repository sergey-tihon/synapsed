import { describe, expect, it } from "vitest";
import type { InventoryDriver, InventoryNode } from "@synapsed/client";
import { applyFilters, driverBadge, groupByAttachment, type InventoryTreeNode } from "../src/inventory";
import { DEFAULT_HARDWARE_FILTERS } from "../src/router";

const genericDriver: InventoryDriver = {
  id: "generic-ddc",
  version: "0.1.0",
  class: "generic",
  deviceId: "display.rd280u",
};
const specializedDriver: InventoryDriver = {
  id: "headset-driver",
  version: "1.2.0",
  class: "specialized",
  deviceId: "audio.headset",
};

function node(
  id: string,
  kind: InventoryNode["kind"],
  parentId: string | null,
  attachment: InventoryNode["attachment"],
  overrides: Partial<InventoryNode> = {},
): InventoryNode {
  return {
    id,
    parentId,
    kind,
    label: id,
    detail: null,
    vendorId: null,
    productId: null,
    speedMbps: null,
    connected: true,
    attachment,
    relation: "exact",
    rule: null,
    driver: null,
    ...overrides,
  };
}

const nodes: InventoryNode[] = [
  node("host", "host", null, "host", { label: "This machine" }),
  node("controller", "controller", "host", "host", { label: "USB controller" }),
  node("dock", "dock", "controller", "dock", { label: "CalDigit dock" }),
  node("dock-hub", "hub", "dock", "dock", { label: "Dock hub" }),
  node("dock-usb", "usb_device", "dock-hub", "dock", { label: "Stream Deck", vendorId: "0fd9", productId: "0084" }),
  node("display", "display", "controller", "host", { label: "RD280U", driver: genericDriver }),
  node("monitor-hub", "hub", "display", "monitor", {
    label: "Monitor companion hub",
    relation: "inferred",
    rule: "driver_link",
  }),
  node("monitor-usb", "usb_device", "monitor-hub", "monitor", {
    label: "Monitor HID",
    vendorId: "0bda",
    productId: "1100",
  }),
  node("bt-adapter", "bluetooth_adapter", "controller", "host", { label: "Bluetooth adapter" }),
  node("bt-device", "bluetooth_device", "bt-adapter", "host", { label: "Headset", driver: specializedDriver }),
];

function flatten(tree: InventoryTreeNode): string[] {
  return [tree.node.id, ...tree.children.flatMap(flatten)];
}

describe("hardware inventory helpers", () => {
  it("keeps topology ancestors for matches without retaining unrelated branches", () => {
    const result = applyFilters(nodes, {
      ...DEFAULT_HARDWARE_FILTERS,
      kinds: ["bluetooth"],
      driver: "specialized",
      connected: true,
      q: "headset",
    });
    expect(result.map((entry) => entry.id)).toEqual(["host", "controller", "bt-adapter", "bt-device"]);
  });

  it("applies attachment and text filters while preserving parent context", () => {
    const result = applyFilters(nodes, {
      ...DEFAULT_HARDWARE_FILTERS,
      attachment: "dock",
      kinds: ["usb"],
      q: "0fd9:0084",
    });
    expect(result.map((entry) => entry.id)).toEqual(["host", "controller", "dock", "dock-hub", "dock-usb"]);
  });

  it("builds host, per-dock and per-monitor trees at attachment boundaries", () => {
    const groups = groupByAttachment(nodes);
    expect(groups.host).toHaveLength(1);
    expect(flatten(groups.host[0]!)).toEqual(["host", "controller", "display", "bt-adapter", "bt-device"]);
    expect(groups.docks.map((group) => group.label)).toEqual(["CalDigit dock"]);
    expect(flatten(groups.docks[0]!.tree)).toEqual(["dock", "dock-hub", "dock-usb"]);
    expect(groups.monitors.map((group) => group.label)).toEqual(["RD280U"]);
    expect(flatten(groups.monitors[0]!.tree)).toEqual(["display", "monitor-hub", "monitor-usb"]);
  });

  it("describes specialized, generic and missing driver states in text", () => {
    expect(driverBadge(specializedDriver)).toEqual({
      label: "Specialized driver · headset-driver",
      className: "specialized",
    });
    expect(driverBadge(genericDriver)).toEqual({ label: "Generic driver · generic-ddc", className: "generic" });
    expect(driverBadge(null)).toEqual({ label: "No driver", className: "none" });
  });
});

import type { InventoryDriver, InventoryNode } from "@synapsed/client";
import type { HardwareFilters } from "./router";

export interface InventoryTreeNode {
  node: InventoryNode;
  children: InventoryTreeNode[];
}

export interface DockGroup {
  id: string;
  label: string;
  tree: InventoryTreeNode;
}

export interface AttachmentGroups {
  host: InventoryTreeNode[];
  docks: DockGroup[];
  monitors: DockGroup[];
}

export interface DriverBadgeModel {
  label: string;
  className: "specialized" | "generic" | "none";
}

function matchesKind(node: InventoryNode, selected: HardwareFilters["kinds"]): boolean {
  if (!selected.length) return true;
  return selected.some((kind) => {
    switch (kind) {
      case "usb":
        return node.kind === "dock" || node.kind === "hub" || node.kind === "usb_device";
      case "display":
        return node.kind === "display";
      case "bluetooth":
        return node.kind === "bluetooth_adapter" || node.kind === "bluetooth_device";
    }
  });
}

function matchesDriver(node: InventoryNode, filter: HardwareFilters["driver"]): boolean {
  if (filter === "any") return true;
  if (filter === "none") return node.driver === null;
  return node.driver?.class === filter;
}

function matchesQuery(node: InventoryNode, query: string): boolean {
  const normalized = query.trim().toLowerCase();
  if (!normalized) return true;
  const searchable = [
    node.label,
    node.detail,
    node.kind,
    node.vendorId && node.productId ? `${node.vendorId}:${node.productId}` : null,
    node.vendorId,
    node.productId,
    node.driver?.id,
    node.driver?.version,
    node.driver?.deviceId,
  ]
    .filter((value): value is string => value !== null && value !== undefined)
    .join(" ")
    .toLowerCase();
  return searchable.includes(normalized);
}

function matchesNode(node: InventoryNode, filters: HardwareFilters): boolean {
  return (
    matchesKind(node, filters.kinds) &&
    (filters.attachment === "all" || node.attachment === filters.attachment) &&
    matchesDriver(node, filters.driver) &&
    (!filters.connected || node.connected) &&
    matchesQuery(node, filters.q)
  );
}

/** Keeps every ancestor needed to explain the topology of a matching node. */
export function applyFilters(nodes: readonly InventoryNode[], filters: HardwareFilters): InventoryNode[] {
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const kept = new Set<string>();
  for (const node of nodes) {
    if (!matchesNode(node, filters)) continue;
    let current: InventoryNode | undefined = node;
    while (current && !kept.has(current.id)) {
      kept.add(current.id);
      current = current.parentId === null ? undefined : byId.get(current.parentId);
    }
  }
  return nodes.filter((node) => kept.has(node.id));
}

function descendantsByParent(nodes: readonly InventoryNode[]): Map<string, InventoryNode[]> {
  const children = new Map<string, InventoryNode[]>();
  for (const node of nodes) {
    if (node.parentId === null) continue;
    const group = children.get(node.parentId) ?? [];
    group.push(node);
    children.set(node.parentId, group);
  }
  return children;
}

function buildTree(
  root: InventoryNode,
  childrenByParent: ReadonlyMap<string, readonly InventoryNode[]>,
  include: (node: InventoryNode) => boolean,
): InventoryTreeNode {
  return {
    node: root,
    children: (childrenByParent.get(root.id) ?? [])
      .filter(include)
      .map((child) => buildTree(child, childrenByParent, include)),
  };
}

/** Splits the filtered topology at dock and monitor attachment boundaries. */
export function groupByAttachment(nodes: readonly InventoryNode[]): AttachmentGroups {
  const children = descendantsByParent(nodes);
  const hostRoots = nodes.filter((node) => node.kind === "host" && node.attachment === "host");
  const dockRoots = nodes.filter((node) => node.kind === "dock");
  const monitorRoots = nodes.filter((node) => node.kind === "display");
  return {
    host: hostRoots.map((root) => buildTree(root, children, (child) => child.attachment === "host")),
    docks: dockRoots.map((root) => ({
      id: root.id,
      label: root.label,
      // A daisy-chained dock is its own group; stop at it so its subtree is not repeated here.
      tree: buildTree(root, children, (child) => child.attachment === "dock" && child.kind !== "dock"),
    })),
    monitors: monitorRoots.map((root) => ({
      id: root.id,
      label: root.label,
      tree: buildTree(root, children, (child) => child.attachment === "monitor" && child.kind !== "display"),
    })),
  };
}

export function driverBadge(driver: InventoryDriver | null): DriverBadgeModel {
  if (!driver) return { label: "No driver", className: "none" };
  return {
    label: `${driver.class === "specialized" ? "Specialized" : "Generic"} driver · ${driver.id}`,
    className: driver.class,
  };
}

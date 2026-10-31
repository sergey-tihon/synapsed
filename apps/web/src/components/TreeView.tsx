import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import type { InventoryNodeKind } from "@synapsed/client";
import type { InventoryTreeNode } from "../inventory";
import { href } from "../router";
import DriverBadge from "./DriverBadge";

interface Props {
  id: string;
  label: string;
  roots: InventoryTreeNode[];
}

interface VisibleRow {
  id: string;
  parentId: string | null;
  depth: number;
}

const kindLabels: Record<InventoryNodeKind, string> = {
  host: "HOST",
  controller: "CTRL",
  dock: "DOCK",
  hub: "HUB",
  usb_device: "USB",
  display: "DISPLAY",
  bluetooth_adapter: "BT ADAPTER",
  bluetooth_device: "BT DEVICE",
};

function collectExpandable(nodes: readonly InventoryTreeNode[], ids = new Set<string>()): Set<string> {
  for (const tree of nodes) {
    if (tree.children.length) ids.add(tree.node.id);
    collectExpandable(tree.children, ids);
  }
  return ids;
}

function flattenVisible(
  nodes: readonly InventoryTreeNode[],
  expanded: ReadonlySet<string>,
  depth = 1,
  parentId: string | null = null,
  rows: VisibleRow[] = [],
): VisibleRow[] {
  for (const tree of nodes) {
    rows.push({ id: tree.node.id, parentId, depth });
    if (expanded.has(tree.node.id)) flattenVisible(tree.children, expanded, depth + 1, tree.node.id, rows);
  }
  return rows;
}

function findTreeNode(nodes: readonly InventoryTreeNode[], id: string): InventoryTreeNode | undefined {
  for (const tree of nodes) {
    if (tree.node.id === id) return tree;
    const child = findTreeNode(tree.children, id);
    if (child) return child;
  }
  return undefined;
}

function detail(node: InventoryTreeNode["node"]): string {
  const identifiers = node.vendorId && node.productId ? `${node.vendorId}:${node.productId}` : "";
  const speed = node.speedMbps === null ? "" : `${node.speedMbps} Mb/s`;
  const known = [identifiers, speed].filter(Boolean);
  const supplemental = (node.detail ?? "").split(" · ").filter((part) => part && !known.includes(part));
  return [...known, ...supplemental].join(" · ");
}

function ruleLabel(node: InventoryTreeNode["node"]): string {
  switch (node.rule) {
    case "usb4_tunnel":
      return "USB4 tunnel";
    case "vendor_match":
      return "vendor match";
    case "driver_link":
      return "driver link";
    case null:
      return "inferred topology";
  }
}

export default function TreeView(props: Props) {
  const allExpandable = createMemo(() => [...collectExpandable(props.roots)]);
  const initiallyExpanded = new Set(props.roots.filter((root) => root.children.length).map((root) => root.node.id));
  const [expanded, setExpanded] = createSignal<ReadonlySet<string>>(initiallyExpanded);
  const [activeId, setActiveId] = createSignal(props.roots[0]?.node.id ?? "");
  const rowElements = new Map<string, HTMLLIElement>();
  let knownExpandable = new Set(allExpandable());
  const visibleRows = createMemo(() => flattenVisible(props.roots, expanded()));
  const allExpanded = () => allExpandable().length > 0 && allExpandable().every((id) => expanded().has(id));

  createEffect(
    () => ({
      expandable: allExpandable(),
      visible: visibleRows(),
      activeId: activeId(),
    }),
    ({ expandable, visible, activeId: currentActiveId }) => {
      const added = expandable.filter((id) => !knownExpandable.has(id));
      if (added.length) {
        knownExpandable = new Set([...knownExpandable, ...added]);
        setExpanded((current) => new Set([...current, ...added]));
      }
      if (!visible.some((row) => row.id === currentActiveId)) setActiveId(visible[0]?.id ?? "");
    },
  );

  function toggle(id: string) {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function focusRow(id: string) {
    setActiveId(id);
    queueMicrotask(() => rowElements.get(id)?.focus());
  }

  function toggleAll() {
    setExpanded(allExpanded() ? new Set<string>() : new Set(allExpandable()));
  }

  function onKeyDown(event: KeyboardEvent) {
    const target = event.target instanceof Element ? event.target.closest<HTMLElement>("[role='treeitem']") : null;
    const id = target?.dataset.nodeId;
    if (!id) return;
    const rows = visibleRows();
    const index = rows.findIndex((row) => row.id === id);
    const current = rows[index];
    if (!current) return;
    let nextId: string | undefined;
    if (event.key === "ArrowDown") nextId = rows[index + 1]?.id;
    else if (event.key === "ArrowUp") nextId = rows[index - 1]?.id;
    else if (event.key === "Home") nextId = rows[0]?.id;
    else if (event.key === "End") nextId = rows[rows.length - 1]?.id;
    else if (event.key === "ArrowRight") {
      const treeNode = findTreeNode(props.roots, id);
      if (treeNode?.children.length && !expanded().has(id)) toggle(id);
      else if (treeNode?.children.length) nextId = treeNode.children[0]?.node.id;
    } else if (event.key === "ArrowLeft") {
      if (expanded().has(id)) toggle(id);
      else nextId = current.parentId ?? undefined;
    } else if (event.key === "Enter") {
      target?.querySelector<HTMLAnchorElement>("a[data-controls-link]")?.click();
    } else {
      return;
    }
    event.preventDefault();
    if (nextId) focusRow(nextId);
  }

  function renderNode(tree: InventoryTreeNode, depth: number, position: number, setSize: number) {
    const node = tree.node;
    const hasChildren = tree.children.length > 0;
    const isExpanded = () => expanded().has(node.id);
    const badge = node.driver;
    const nodeDetail = detail(node);
    const relation = node.relation === "inferred" ? `Inferred · ${ruleLabel(node)}` : "Exact topology";
    let element: HTMLLIElement | undefined;
    onCleanup(() => {
      if (rowElements.get(node.id) === element) rowElements.delete(node.id);
    });
    return (
      <li
        id={`${props.id}-${node.id}`}
        ref={(value) => {
          element = value;
          rowElements.set(node.id, value);
        }}
        class="tree-item"
        role="treeitem"
        tabindex={activeId() === node.id ? 0 : -1}
        aria-level={depth}
        aria-posinset={position}
        aria-setsize={setSize}
        aria-expanded={hasChildren ? (isExpanded() ? "true" : "false") : undefined}
        aria-label={`${node.label}, ${kindLabels[node.kind]}`}
        data-node-id={node.id}
        data-relation={node.relation}
        title={relation}
        onFocus={() => setActiveId(node.id)}
        onClick={(event) => {
          if (event.target instanceof Element && event.target.closest("[role='treeitem']") === event.currentTarget)
            focusRow(node.id);
        }}
      >
        <div class="tree-row">
          <button
            type="button"
            class="tree-toggle"
            tabindex="-1"
            disabled={!hasChildren}
            aria-label={
              hasChildren ? `${isExpanded() ? "Collapse" : "Expand"} ${node.label}` : `${node.label} has no children`
            }
            onClick={() => hasChildren && toggle(node.id)}
          >
            {hasChildren ? (isExpanded() ? "−" : "+") : "·"}
          </button>
          <span class="hardware-icon" aria-hidden="true">
            {kindLabels[node.kind]}
          </span>
          <span class="tree-title">
            <strong>{node.label}</strong>
            <Show when={nodeDetail}>
              <small>{nodeDetail}</small>
            </Show>
            {node.relation === "inferred" ? <small class="relation-label">Inferred · {ruleLabel(node)}</small> : null}
          </span>
          <DriverBadge driver={badge} />
          {badge?.deviceId ? (
            <a
              href={href({ page: "device", deviceId: badge.deviceId, characteristicId: null })}
              data-controls-link="true"
              tabindex="-1"
              class="tree-link"
              onClick={(event) => event.stopPropagation()}
            >
              Open controls →
            </a>
          ) : null}
        </div>
        <Show when={hasChildren && isExpanded()}>
          <ul role="group">
            <For each={tree.children}>
              {(child, index) => renderNode(child, depth + 1, index() + 1, tree.children.length)}
            </For>
          </ul>
        </Show>
      </li>
    );
  }

  return (
    <div class="tree-frame" onKeyDown={onKeyDown}>
      <div class="tree-toolbar">
        <span class="tree-count">{visibleRows().length} shown</span>
        <Show when={allExpandable().length > 0}>
          <button
            type="button"
            class="tree-expand-toggle"
            aria-label={`${allExpanded() ? "Collapse" : "Expand"} all in ${props.label}`}
            onClick={toggleAll}
          >
            {allExpanded() ? "Collapse all" : "Expand all"}
          </button>
        </Show>
      </div>
      <ul class="inventory-tree" role="tree" aria-label={props.label}>
        <For each={props.roots}>{(root, index) => renderNode(root, 1, index() + 1, props.roots.length)}</For>
      </ul>
      <Show when={props.roots.length === 0}>
        <p class="muted">No hardware in this group matches the current filters.</p>
      </Show>
    </div>
  );
}

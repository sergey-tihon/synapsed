import { createEffect, createMemo, createSignal, For, onSettled, Show } from "solid-js";
import type { InventoryResponse } from "@synapsed/client";
import { applyFilters, groupByAttachment } from "../inventory";
import type { HardwareFilters } from "../router";
import FilterBar from "../components/FilterBar";
import SourceStatus from "../components/SourceStatus";
import TreeView from "../components/TreeView";
import { useSession } from "../session";

interface Props {
  filters: HardwareFilters;
  onFiltersChange(filters: HardwareFilters): void;
}

export default function Hardware(props: Props) {
  const session = useSession();
  const [inventory, setInventory] = createSignal<InventoryResponse | null>(null);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  let requestController: AbortController | undefined;
  let requestId = 0;
  let revisionKey = session.snapshot() ? `${session.snapshot()?.node.instanceId}:${session.snapshot()?.revision}` : "";

  async function refresh(): Promise<void> {
    requestController?.abort();
    const controller = new AbortController();
    requestController = controller;
    const request = ++requestId;
    const currentInstance = session.snapshot()?.node.instanceId;
    if (inventory() && currentInstance && inventory()?.instanceId !== currentInstance) setInventory(null);
    setLoading(true);
    try {
      const next = await session.getInventory({ signal: controller.signal });
      if (request !== requestId || controller.signal.aborted) return;
      if (session.snapshot() && next.instanceId !== session.snapshot()?.node.instanceId)
        throw new Error("Inventory belongs to a previous daemon instance.");
      setInventory(next);
      setError("");
    } catch (reason) {
      if (request === requestId && !controller.signal.aborted)
        setError(reason instanceof Error ? reason.message : "Inventory request failed.");
    } finally {
      if (request === requestId) setLoading(false);
    }
  }

  createEffect(
    () => {
      const snapshot = session.snapshot();
      return snapshot ? `${snapshot.node.instanceId}:${snapshot.revision}` : null;
    },
    (nextKey) => {
      if (!nextKey || nextKey === revisionKey) return;
      revisionKey = nextKey;
      void refresh();
    },
  );

  onSettled(() => {
    if (typeof window === "undefined") return;
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5_000);
    return () => {
      window.clearInterval(timer);
      requestController?.abort();
      ++requestId;
    };
  });

  const visibleNodes = createMemo(() => {
    const current = inventory();
    if (!current) return [];
    return applyFilters(current.nodes, props.filters);
  });
  const groups = createMemo(() => groupByAttachment(visibleNodes()));
  const counts = createMemo(() => {
    const nodes = inventory()?.nodes ?? [];
    const physical = nodes.filter((node) => node.kind !== "host" && node.kind !== "controller");
    return {
      nodes: physical.length,
      displays: nodes.filter((node) => node.kind === "display").length,
      connected: physical.filter((node) => node.connected).length,
      driverLinked: physical.filter((node) => node.driver !== null).length,
    };
  });

  return (
    <div class="page-stack hardware-page">
      <header class="page-title hardware-title">
        <div>
          <p class="eyebrow">LOCAL · READ ONLY</p>
          <h1>Hardware</h1>
          <p>Explore connected devices, display paths, and inferred topology.</p>
        </div>
        <div class="hardware-header-actions">
          <span class="badge" data-state={error() || session.connection() !== "connected" ? "warn" : "online"}>
            {session.connection() !== "connected" || error()
              ? "Stale inventory"
              : loading()
                ? "Refreshing…"
                : "Live · 5 s"}
          </span>
          <button type="button" class="refresh-button" onClick={() => void refresh()} disabled={loading()}>
            <span aria-hidden="true">↻</span> Refresh
          </button>
        </div>
      </header>
      <FilterBar filters={props.filters} onChange={props.onFiltersChange} />
      <Show when={error()}>
        <p class="notice" role="alert">
          Inventory refresh failed: {error()} {inventory() ? "Showing the last successful inventory." : ""}
        </p>
      </Show>
      <Show
        when={inventory()}
        fallback={
          <section class="panel">
            <p>{loading() ? "Reading hardware inventory…" : "No hardware inventory received."}</p>
          </section>
        }
      >
        <>
          <div class="hardware-summary" aria-label="Inventory summary">
            <div>
              <strong>{counts().nodes}</strong>
              <span>Hardware nodes</span>
            </div>
            <div>
              <strong>{counts().displays}</strong>
              <span>Displays</span>
            </div>
            <div>
              <strong>{counts().connected}</strong>
              <span>Connected</span>
            </div>
            <div>
              <strong>{counts().driverLinked}</strong>
              <span>Driver linked</span>
            </div>
          </div>
          <SourceStatus sources={inventory()?.sources ?? []} />
          <Show when={groups().monitors.length > 0}>
            <section class="hardware-section hardware-section--focus" aria-labelledby="monitor-heading">
              <div class="section-heading">
                <div>
                  <p class="eyebrow">DISPLAY PATHS</p>
                  <h2 id="monitor-heading">Monitors</h2>
                </div>
                <span class="badge">{groups().monitors.length}</span>
              </div>
              <div class="hardware-groups">
                <For each={groups().monitors}>
                  {(monitor) => (
                    <article class="topology-group topology-group--monitor">
                      <TreeView
                        id={`hardware-monitor-${monitor.id}`}
                        label={`${monitor.label} topology`}
                        roots={[monitor.tree]}
                      />
                    </article>
                  )}
                </For>
              </div>
            </section>
          </Show>
          <Show when={visibleNodes().length === 0}>
            <section class="panel">
              <p>No hardware matches these filters.</p>
            </section>
          </Show>
          <Show when={groups().host.length > 0}>
            <section class="hardware-section" aria-labelledby="host-hardware-heading">
              <div class="section-heading">
                <div>
                  <p class="eyebrow">ATTACHMENT</p>
                  <h2 id="host-hardware-heading">This machine</h2>
                </div>
                <span class="badge">
                  {groups().host.length} root{groups().host.length === 1 ? "" : "s"}
                </span>
              </div>
              <TreeView id="hardware-host" label="Hardware attached to this machine" roots={groups().host} />
            </section>
          </Show>
          <Show when={groups().docks.length > 0}>
            <section class="hardware-section" aria-labelledby="dock-heading">
              <div class="section-heading">
                <div>
                  <p class="eyebrow">ATTACHMENT</p>
                  <h2 id="dock-heading">Docks</h2>
                </div>
                <span class="badge">{groups().docks.length}</span>
              </div>
              <For each={groups().docks}>
                {(dock) => (
                  <article class="topology-group">
                    <h3>{dock.label}</h3>
                    <TreeView id={`hardware-dock-${dock.id}`} label={`${dock.label} topology`} roots={[dock.tree]} />
                  </article>
                )}
              </For>
            </section>
          </Show>
        </>
      </Show>
    </div>
  );
}

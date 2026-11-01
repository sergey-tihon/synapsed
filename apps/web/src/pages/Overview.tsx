import { createEffect, createMemo, createSignal, For, onSettled, Show } from "solid-js";
import type { InventoryResponse } from "@synapsed/client";
import { DEFAULT_HARDWARE_FILTERS, href } from "../router";
import { useSession } from "../session";

export default function Overview() {
  const session = useSession();
  const [inventory, setInventory] = createSignal<InventoryResponse | null>(null);
  const [inventoryError, setInventoryError] = createSignal("");
  const [now, setNow] = createSignal(Date.now());
  const snapshot = () => session.snapshot();
  const problems = createMemo(() => {
    const current = snapshot();
    if (!current) return 0;
    return (
      current.drivers.filter((driver) => driver.state !== "online").length +
      current.devices.filter((device) => device.status !== "online").length +
      current.devices.reduce(
        (count, device) =>
          count + device.characteristics.filter((characteristic) => characteristic.status !== "ok").length,
        0,
      )
    );
  });
  const inventoryCount = createMemo(() => {
    const current = inventory();
    const instanceId = snapshot()?.node.instanceId;
    if (!current || (instanceId && current.instanceId !== instanceId)) return null;
    return current.nodes.filter((node) => node.kind !== "host").length;
  });

  let inventoryController: AbortController | undefined;
  let requestedInstance = "";
  createEffect(
    () => snapshot()?.node.instanceId ?? null,
    (instanceId) => {
      if (!instanceId || instanceId === requestedInstance) return;
      requestedInstance = instanceId;
      inventoryController?.abort();
      const controller = new AbortController();
      inventoryController = controller;
      void session
        .getInventory({ signal: controller.signal })
        .then((response) => {
          if (!controller.signal.aborted && response.instanceId === session.snapshot()?.node.instanceId) {
            setInventory(response);
            setInventoryError("");
          }
        })
        .catch((error: unknown) => {
          if (!controller.signal.aborted)
            setInventoryError(error instanceof Error ? error.message : "Inventory request failed.");
        });
    },
  );

  onSettled(() => {
    if (typeof window === "undefined") return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => {
      window.clearInterval(timer);
      inventoryController?.abort();
    };
  });

  const status = () => session.statusResponse();
  const uptime = () => {
    const received = status();
    if (!received) return "Not received";
    const seconds = received.response.uptimeSeconds + Math.max(0, Math.floor((now() - received.receivedAt) / 1_000));
    return `${seconds.toLocaleString()} seconds${session.connection() === "connected" ? "" : " · stale"}`;
  };

  return (
    <div class="page-stack">
      <header class="page-title">
        <p class="eyebrow">LOCAL SERVICE</p>
        <h1>Overview</h1>
        <p>Current daemon state, installed drivers and discovered hardware.</p>
      </header>
      <section class="panel" aria-labelledby="node-heading">
        <div class="section-heading">
          <div>
            <p class="eyebrow">SERVICE</p>
            <h2 id="node-heading">Node</h2>
          </div>
          <span class="badge">{session.connection() === "connected" ? "Live" : "Retained snapshot · stale"}</span>
        </div>
        <Show when={session.serviceError()}>
          <p class="error">{session.serviceError()}</p>
        </Show>
        <Show when={snapshot()} fallback={<p class="muted">Waiting for a valid service snapshot.</p>}>
          <dl class="node-info">
            <dt>Name</dt>
            <dd>{snapshot()?.node.name}</dd>
            <dt>API</dt>
            <dd>{snapshot()?.apiVersion}</dd>
            <dt>Node ID</dt>
            <dd>{snapshot()?.node.nodeId}</dd>
            <dt>Instance ID</dt>
            <dd>{snapshot()?.node.instanceId}</dd>
            <dt>Revision</dt>
            <dd>{snapshot()?.revision}</dd>
            <dt>Uptime</dt>
            <dd>{uptime()}</dd>
          </dl>
          <Show when={session.statusError()}>
            <p class="error">Status request: {session.statusError()}</p>
          </Show>
        </Show>
      </section>

      <section class="metric-grid" aria-label="Current counts">
        <article class="metric-card">
          <span class="metric-label">Devices</span>
          <strong>{snapshot()?.devices.length ?? "—"}</strong>
          <a href={href({ page: "devices" })}>View devices</a>
        </article>
        <article class="metric-card">
          <span class="metric-label">Hardware nodes</span>
          <strong>{inventoryCount() ?? "—"}</strong>
          <span class="muted">
            {inventoryCount() === null
              ? "Inventory not received"
              : session.connection() === "connected"
                ? "Last inventory · current"
                : "Last inventory · stale"}
          </span>
          <a href={href({ page: "hardware", filters: DEFAULT_HARDWARE_FILTERS })}>Explore hardware</a>
          {inventoryError() ? <small class="error">Inventory unavailable: {inventoryError()}</small> : null}
        </article>
        <article class="metric-card">
          <span class="metric-label">Problems</span>
          <strong data-state={problems() ? "warning" : "ok"}>{snapshot() ? problems() : "—"}</strong>
          <span class="muted">Drivers, devices and characteristics</span>
        </article>
      </section>

      <section class="panel" aria-labelledby="drivers-heading">
        <div class="section-heading">
          <div>
            <p class="eyebrow">INSTALLED</p>
            <h2 id="drivers-heading">Drivers</h2>
          </div>
          <a href={href({ page: "logs", level: "all" })}>Open logs →</a>
        </div>
        <Show when={snapshot()} fallback={<p class="muted">Driver catalog not received.</p>}>
          <Show
            when={snapshot()?.drivers.length}
            fallback={<p class="muted">No driver packages installed. No controls are manufactured.</p>}
          >
            <ul class="driver-list">
              <For each={snapshot()?.drivers ?? []}>
                {(driver) => (
                  <li>
                    <div class="driver-row">
                      <div>
                        <strong>{driver.id}</strong>
                        <span class="muted">{driver.version}</span>
                      </div>
                      <span class="badge" data-state={driver.state}>
                        {driver.state}
                      </span>
                    </div>
                    {driver.error ? (
                      <p class="error">
                        {driver.error.code}: {driver.error.message}
                      </p>
                    ) : null}
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
      </section>

      <nav class="quick-links" aria-label="Explore Synapsed">
        <a href="#/hardware">
          <span>01</span>
          <strong>Hardware</strong>
          <small>Explore USB, display and Bluetooth topology</small>
        </a>
        <a href="#/devices">
          <span>02</span>
          <strong>Devices</strong>
          <small>Open published characteristic controls</small>
        </a>
        <a href="#/logs">
          <span>03</span>
          <strong>Logs</strong>
          <small>Review recent service messages</small>
        </a>
      </nav>
    </div>
  );
}

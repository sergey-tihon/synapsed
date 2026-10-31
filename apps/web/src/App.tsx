import { createEffect, createMemo, createSignal, For, onSettled, Show } from "solid-js";
import {
  createClient,
  isCommandApplicable,
  type CharacteristicCommand,
  type ClientStatus,
  type LogEntry,
  type Snapshot,
  type StatusResponse,
  type SynapsedClient,
} from "@synapsed/client";
import CharacteristicCard from "./CharacteristicCard";
import { characteristicAnchor, scrollToCharacteristic } from "./anchors";
import "./App.css";

export default function App() {
  const [snapshot, setSnapshot] = createSignal<Snapshot | null>(null);
  const [statusResponse, setStatusResponse] = createSignal<StatusResponse | null>(null);
  const [connection, setConnection] = createSignal<ClientStatus>("connecting");
  const [serviceError, setServiceError] = createSignal("");
  const [statusError, setStatusError] = createSignal("");
  const [logs, setLogs] = createSignal<LogEntry[]>([]);
  const [level, setLevel] = createSignal("all");
  const [pending, setPending] = createSignal<ReadonlySet<string>>(new Set());
  const [results, setResults] = createSignal<ReadonlyMap<string, string>>(new Map());
  const [hash, setHash] = createSignal("");
  const devices = createMemo(() => snapshot()?.devices.map((device) => device.id) ?? []);
  const filteredLogs = createMemo(() => logs().filter((entry) => level() === "all" || entry.level === level()));
  const connected = () => connection() === "connected";
  let client: SynapsedClient | undefined;
  let alive = false;
  let sawSocketSnapshot = false;
  let lastInstance: string | undefined;
  let statusRequest = 0;
  let scrolledHash = "";
  const requests = new AbortController();

  function recordResult(key: string, message: string) {
    setResults((current) => {
      const next = new Map(current);
      next.set(key, message);
      return next;
    });
  }
  async function execute(deviceId: string, characteristicId: string, command: CharacteristicCommand): Promise<boolean> {
    const key = characteristicAnchor(deviceId, characteristicId);
    const currentDevice = snapshot()?.devices.find((device) => device.id === deviceId);
    const currentCharacteristic = currentDevice?.characteristics.find(
      (characteristic) => characteristic.id === characteristicId,
    );
    if (!alive || !client || !connected() || pending().has(key)) return false;
    if (
      currentDevice?.status !== "online" ||
      !currentCharacteristic ||
      !isCommandApplicable(currentCharacteristic, command)
    ) {
      recordResult(key, "Command unavailable: the device or current metadata has changed.");
      return false;
    }
    setPending((current) => new Set([...current, key]));
    recordResult(key, "");
    try {
      // Only accepted stream observations render confirmed values. A command's
      // HTTP response must never overwrite a later stream publication.
      await client.execute(deviceId, characteristicId, command, { signal: requests.signal });
      if (!alive) return false;
      recordResult(key, "Command confirmed by the service; displayed values come from observations.");
      return true;
    } catch (error) {
      if (alive)
        recordResult(key, `Command failed: ${error instanceof Error ? error.message : "Unknown service error."}`);
      return false;
    } finally {
      if (alive)
        setPending((current) => {
          const next = new Set(current);
          next.delete(key);
          return next;
        });
    }
  }

  onSettled(() => {
    alive = true;
    const onHashChange = () => {
      scrolledHash = "";
      setHash(window.location.hash);
    };
    window.addEventListener("hashchange", onHashChange);
    onHashChange();
    try {
      client = createClient({ baseUrl: window.location.origin });
    } catch (error) {
      setConnection("incompatible");
      setServiceError(error instanceof Error ? error.message : "The local endpoint is invalid.");
      return () => {
        alive = false;
        requests.abort();
        window.removeEventListener("hashchange", onHashChange);
      };
    }
    const fetchStatus = () => {
      const sequence = ++statusRequest;
      void client!
        .getStatus({ signal: requests.signal })
        .then((response) => {
          if (alive && sequence === statusRequest && response.node.instanceId === lastInstance) {
            setStatusResponse(response);
            setStatusError("");
          }
        })
        .catch((error: unknown) => {
          if (alive && sequence === statusRequest)
            setStatusError(error instanceof Error ? error.message : "Status request failed.");
        });
    };
    const unsubscribe = client.subscribe({
      onSnapshot(next) {
        if (!alive) return;
        sawSocketSnapshot = true;
        if (next.node.instanceId !== lastInstance) {
          lastInstance = next.node.instanceId;
          setStatusResponse(null);
          setLogs([]);
          setResults(new Map());
        }
        setSnapshot(next);
        setServiceError("");
      },
      onLogs(entries) {
        if (alive) setLogs(entries.slice(-200));
      },
      onLog(entry) {
        if (alive)
          setLogs((current) =>
            current.some((item) => item.id === entry.id) ? current : [...current, entry].slice(-200),
          );
      },
      onStatus(next) {
        if (!alive) return;
        setConnection(next);
        if (next === "connected") fetchStatus();
      },
      onError(error) {
        if (alive) setServiceError(error.message);
      },
    });
    void client
      .getSnapshot({ signal: requests.signal })
      .then((next) => {
        if (alive && !sawSocketSnapshot) setSnapshot(next);
      })
      .catch((error: unknown) => {
        if (alive && !sawSocketSnapshot)
          setServiceError(error instanceof Error ? error.message : "Snapshot request failed.");
      });
    return () => {
      alive = false;
      ++statusRequest;
      requests.abort();
      unsubscribe();
      client?.close();
      window.removeEventListener("hashchange", onHashChange);
    };
  });

  createEffect(
    () => ({ currentHash: hash(), currentSnapshot: snapshot() }),
    ({ currentHash }) => {
      if (currentHash && scrolledHash !== currentHash && scrollToCharacteristic(currentHash))
        scrolledHash = currentHash;
    },
  );

  return (
    <>
      <header class="page-header">
        <div>
          <h1>Synapsed</h1>
          <p>Local devices, confirmed state.</p>
        </div>
        <nav aria-label="Page sections">
          <a href="#status">Status</a>
          <a href="#devices">Devices</a>
          <a href="#logs">Logs</a>
        </nav>
      </header>
      <main>
        <section id="status" aria-labelledby="status-heading">
          <div class="row">
            <h2 id="status-heading">Status</h2>
            <span class="badge" role="status">
              {connection() === "connected"
                ? "Online"
                : connection() === "disconnected"
                  ? "Offline · reconnecting"
                  : connection() === "incompatible"
                    ? "Incompatible"
                    : "Connecting"}
            </span>
          </div>
          <Show when={!connected()}>
            <p class="notice">
              {connection() === "incompatible"
                ? "Protocol or endpoint policy is incompatible. Correct the local service configuration and reload this page."
                : "Waiting for a valid service snapshot. Retained readings and logs are stale; mutations are disabled."}
            </p>
          </Show>
          <Show when={serviceError()}>
            <p class="error" role="alert">
              {serviceError()}
            </p>
          </Show>
          <Show when={snapshot()} fallback={<p>No node information received yet.</p>}>
            <dl class="node-info">
              <dt>Node</dt>
              <dd>{snapshot()?.node.name}</dd>
              <dt>API</dt>
              <dd>{snapshot()?.apiVersion}</dd>
              <dt>Node ID</dt>
              <dd>{snapshot()?.node.nodeId}</dd>
              <dt>Instance ID</dt>
              <dd>{snapshot()?.node.instanceId}</dd>
              <dt>Revision</dt>
              <dd>{snapshot()?.revision}</dd>
              <dt>Uptime (at status request)</dt>
              <dd>
                {statusResponse()
                  ? `${statusResponse()?.uptimeSeconds} seconds${connected() ? "" : " · stale"}`
                  : "Not received"}
              </dd>
            </dl>
            <Show when={statusError()}>
              <p class="error">Status request: {statusError()}</p>
            </Show>
            <h3>Installed drivers</h3>
            <Show
              when={snapshot()?.drivers.length}
              fallback={<p>No driver packages installed. No controls are manufactured.</p>}
            >
              <ul class="drivers">
                <For each={snapshot()?.drivers ?? []}>
                  {(driver) => (
                    <li>
                      <strong>{driver.id}</strong>{" "}
                      <span>
                        {driver.version} · {driver.state}
                      </span>
                      <Show when={driver.error}>
                        <p class="error">
                          {driver.error?.code}: {driver.error?.message}
                        </p>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </Show>
        </section>
        <section id="devices" aria-labelledby="devices-heading">
          <h2 id="devices-heading">Devices</h2>
          <Show when={snapshot()} fallback={<p>Waiting for the device catalog…</p>}>
            <Show
              when={devices().length}
              fallback={
                <p>
                  No devices discovered. Check installed driver status, hardware availability and permissions above.
                </p>
              }
            >
              <For each={devices()}>
                {(deviceId) => {
                  const device = createMemo(() => snapshot()!.devices.find((item) => item.id === deviceId)!);
                  const ids = createMemo(() => device().characteristics.map((item) => item.id));
                  return (
                    <div class="device">
                      <div class="row">
                        <h3>{device().label}</h3>
                        <span class="badge">
                          {device().status}
                          {connected() ? "" : " · stale"}
                        </span>
                      </div>
                      <p class="muted">
                        {deviceId} · kind {device().kind} · driver {device().driver}
                      </p>
                      <Show when={device().error}>
                        <p class="error">
                          {device().error?.code}: {device().error?.message}
                        </p>
                      </Show>
                      <Show when={!ids().length}>
                        <p>No characteristics declared.</p>
                      </Show>
                      <div class="characteristics">
                        <For each={ids()}>
                          {(characteristicId) => (
                            <CharacteristicCard
                              deviceId={deviceId}
                              characteristic={device().characteristics.find((item) => item.id === characteristicId)!}
                              available={connected() && device().status === "online"}
                              stale={!connected() || device().status !== "online"}
                              pending={pending().has(characteristicAnchor(deviceId, characteristicId))}
                              result={results().get(characteristicAnchor(deviceId, characteristicId))}
                              execute={(command) => execute(deviceId, characteristicId, command)}
                            />
                          )}
                        </For>
                      </div>
                    </div>
                  );
                }}
              </For>
            </Show>
          </Show>
          <Show
            when={
              hash().startsWith("#device=") &&
              snapshot() &&
              !snapshot()?.devices.some((device) =>
                device.characteristics.some(
                  (characteristic) => characteristicAnchor(device.id, characteristic.id) === hash(),
                ),
              )
            }
          >
            <p class="notice">
              The linked characteristic is missing from the current catalog. Nothing has been rebound.
            </p>
          </Show>
        </section>
        <section id="logs" aria-labelledby="logs-heading">
          <div class="row">
            <h2 id="logs-heading">Logs</h2>
            <label>
              Level{" "}
              <select value={level()} onChange={(event) => setLevel(event.currentTarget.value)}>
                <option value="all">All levels</option>
                <option value="debug">Debug</option>
                <option value="info">Info</option>
                <option value="warn">Warning</option>
                <option value="error">Error</option>
              </select>
            </label>
          </div>
          <p class="muted">
            Recent and live service logs · retaining at most 200 entries{connected() ? "" : " · stale"}
          </p>
          <Show
            when={filteredLogs().length}
            fallback={<p>{logs().length ? "No entries match this level." : "No log entries received."}</p>}
          >
            <ol class="logs">
              <For each={filteredLogs()}>
                {(entry) => (
                  <li>
                    <div class="log-heading">
                      <time datetime={entry.timestamp}>{entry.timestamp}</time>
                      <span class="badge">{entry.level}</span>
                      <span>{entry.target}</span>
                    </div>
                    <p>{entry.message}</p>
                  </li>
                )}
              </For>
            </ol>
          </Show>
        </section>
      </main>
      <footer>
        Loopback-only client. Hardware and observations are owned by the local service and installed drivers.
      </footer>
    </>
  );
}

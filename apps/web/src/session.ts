import { createContext, createStore, onCleanup, onSettled, reconcile, useContext, createSignal } from "solid-js";
import {
  createClient,
  isCommandApplicable,
  type CharacteristicChange,
  type CharacteristicCommand,
  type ClientStatus,
  type InventoryResponse,
  type LogEntry,
  type Snapshot,
  type StatusResponse,
  type SynapsedClient,
} from "@synapsed/client";

export interface CommandResult {
  message: string;
  revision: string;
}

export interface SessionReducerState {
  instanceId: string | null;
  logs: LogEntry[];
  changes: CharacteristicChange[];
  results: ReadonlyMap<string, CommandResult>;
}

function compareDecimal(left: string, right: string): number {
  if (left.length !== right.length) return left.length - right.length;
  return left === right ? 0 : left < right ? -1 : 1;
}

export function mergeLogs(current: readonly LogEntry[], incoming: readonly LogEntry[]): LogEntry[] {
  const byId = new Map(current.map((entry) => [entry.id, entry]));
  for (const entry of incoming) if (!byId.has(entry.id)) byId.set(entry.id, entry);
  return [...byId.values()].sort((left, right) => compareDecimal(left.id, right.id)).slice(-200);
}

export function appendChanges(
  current: readonly CharacteristicChange[],
  incoming: readonly CharacteristicChange[],
): CharacteristicChange[] {
  return [...current, ...incoming].slice(-200);
}

export function expireResultsOnRevision(
  results: ReadonlyMap<string, CommandResult>,
  revision: string,
): ReadonlyMap<string, CommandResult> {
  let expired = false;
  const next = new Map<string, CommandResult>();
  for (const [key, result] of results) {
    if (compareDecimal(revision, result.revision) > 0) expired = true;
    else next.set(key, result);
  }
  return expired ? next : results;
}

export function resetForInstance(state: SessionReducerState, instanceId: string): SessionReducerState {
  if (state.instanceId === instanceId) return state;
  return { instanceId, logs: [], changes: [], results: new Map() };
}

export function characteristicKey(deviceId: string, characteristicId: string): string {
  return JSON.stringify([deviceId, characteristicId]);
}

export interface SynapsedSession {
  snapshot(): Snapshot | null;
  connection(): ClientStatus;
  serviceError(): string;
  statusResponse(): { response: StatusResponse; receivedAt: number } | null;
  statusError(): string;
  logs(): LogEntry[];
  changes(): CharacteristicChange[];
  pending(): ReadonlySet<string>;
  results(): ReadonlyMap<string, CommandResult>;
  execute(deviceId: string, characteristicId: string, command: CharacteristicCommand): Promise<boolean>;
  getInventory(options?: { signal?: AbortSignal }): Promise<InventoryResponse>;
}

export const SessionContext = createContext<SynapsedSession>();

export function useSession(): SynapsedSession {
  return useContext(SessionContext);
}

export function createSynapsedSession(): SynapsedSession {
  const [state, setState] = createStore<{ snapshot: Snapshot | null }>({ snapshot: null });
  const [connection, setConnection] = createSignal<ClientStatus>("connecting");
  const [serviceError, setServiceError] = createSignal("");
  const [statusResponse, setStatusResponse] = createSignal<{ response: StatusResponse; receivedAt: number } | null>(
    null,
  );
  const [statusError, setStatusError] = createSignal("");
  const [logs, setLogs] = createSignal<LogEntry[]>([]);
  const [changes, setChanges] = createSignal<CharacteristicChange[]>([]);
  const [pending, setPending] = createSignal<ReadonlySet<string>>(new Set());
  const [results, setResults] = createSignal<ReadonlyMap<string, CommandResult>>(new Map());
  const pendingOperations = new Map<string, number>();
  const operationRequests = new Map<number, AbortController>();
  let reducerState: SessionReducerState = { instanceId: null, logs: [], changes: [], results: new Map() };
  let client: SynapsedClient | undefined;
  let unsubscribe: (() => void) | undefined;
  let statusController: AbortController | undefined;
  let nextOperation = 0;
  let statusRequest = 0;
  let alive = false;

  const publishPending = () => setPending(new Set(pendingOperations.keys()));
  const recordResult = (key: string, message: string, revision: string) => {
    const next = new Map(reducerState.results);
    next.set(key, { message, revision });
    reducerState = { ...reducerState, results: next };
    setResults(next);
  };

  const clearResult = (key: string) => {
    if (!reducerState.results.has(key)) return;
    const next = new Map(reducerState.results);
    next.delete(key);
    reducerState = { ...reducerState, results: next };
    setResults(next);
  };

  async function fetchStatus(instanceId: string): Promise<void> {
    const currentClient = client;
    if (!currentClient || !alive) return;
    statusController?.abort();
    const controller = new AbortController();
    statusController = controller;
    const request = ++statusRequest;
    try {
      const response = await currentClient.getStatus({ signal: controller.signal });
      if (
        alive &&
        request === statusRequest &&
        !controller.signal.aborted &&
        state.snapshot?.node.instanceId === instanceId &&
        response.node.instanceId === instanceId
      ) {
        setStatusResponse({ response, receivedAt: Date.now() });
        setStatusError("");
      }
    } catch (error) {
      if (alive && request === statusRequest && !controller.signal.aborted)
        setStatusError(error instanceof Error ? error.message : "Status request failed.");
    }
  }

  function acceptSnapshot(next: Snapshot): void {
    const reset = resetForInstance(reducerState, next.node.instanceId);
    if (reset !== reducerState) {
      statusController?.abort();
      for (const controller of operationRequests.values()) controller.abort();
      operationRequests.clear();
      pendingOperations.clear();
      publishPending();
      setStatusResponse(null);
      setStatusError("");
    }
    const nextResults = expireResultsOnRevision(reset.results, next.revision);
    reducerState = { ...reset, results: nextResults };
    setLogs(reducerState.logs);
    setChanges(reducerState.changes);
    setState((draft) => {
      if (draft.snapshot === null) {
        draft.snapshot = next;
      } else {
        // Solid 2's default reconcile key is "id" for nested rows.
        reconcile(next)(draft.snapshot);
      }
    });
    setServiceError("");
    if (connection() === "connected" && statusResponse()?.response.node.instanceId !== next.node.instanceId)
      void fetchStatus(next.node.instanceId);
  }

  async function execute(deviceId: string, characteristicId: string, command: CharacteristicCommand): Promise<boolean> {
    const currentClient = client;
    const currentSnapshot = state.snapshot;
    const key = characteristicKey(deviceId, characteristicId);
    const device = currentSnapshot?.devices.find((candidate) => candidate.id === deviceId);
    const characteristic = device?.characteristics.find((candidate) => candidate.id === characteristicId);
    if (!alive || !currentClient || connection() !== "connected" || !currentSnapshot) return false;
    if (
      device?.status !== "online" ||
      characteristic?.status !== "ok" ||
      !characteristic ||
      !isCommandApplicable(characteristic, command)
    ) {
      recordResult(key, "Command unavailable: review the current device state and metadata.", currentSnapshot.revision);
      return false;
    }
    if (pendingOperations.has(key)) return false;
    clearResult(key);

    const operation = ++nextOperation;
    const controller = new AbortController();
    operationRequests.set(operation, controller);
    pendingOperations.set(key, operation);
    publishPending();
    try {
      const observed = await currentClient.execute(deviceId, characteristicId, command, { signal: controller.signal });
      if (!alive || controller.signal.aborted || reducerState.instanceId !== observed.node.instanceId) return false;
      recordResult(key, "Command confirmed by the service; values remain observed state.", observed.revision);
      return true;
    } catch (error) {
      if (alive && !controller.signal.aborted && reducerState.instanceId === currentSnapshot.node.instanceId) {
        recordResult(
          key,
          `Command failed: ${error instanceof Error ? error.message : "Unknown service error."}`,
          state.snapshot?.revision ?? currentSnapshot.revision,
        );
      }
      return false;
    } finally {
      operationRequests.delete(operation);
      if (pendingOperations.get(key) === operation) {
        pendingOperations.delete(key);
        publishPending();
      }
    }
  }

  onSettled(() => {
    if (typeof window === "undefined") return;
    alive = true;
    try {
      client = createClient({ baseUrl: window.location.origin });
    } catch (error) {
      setConnection("incompatible");
      setServiceError(error instanceof Error ? error.message : "The local endpoint is invalid.");
      return;
    }
    unsubscribe = client.subscribe({
      onSnapshot(next) {
        if (alive) acceptSnapshot(next);
      },
      onLogs(entries) {
        if (alive) {
          reducerState = { ...reducerState, logs: mergeLogs(reducerState.logs, entries) };
          setLogs(reducerState.logs);
        }
      },
      onLog(entry) {
        if (alive) {
          reducerState = { ...reducerState, logs: mergeLogs(reducerState.logs, [entry]) };
          setLogs(reducerState.logs);
        }
      },
      onCharacteristicChanged(change) {
        if (alive) {
          reducerState = { ...reducerState, changes: appendChanges(reducerState.changes, [change]) };
          setChanges(reducerState.changes);
        }
      },
      onStatus(next) {
        if (!alive) return;
        setConnection(next);
        if (next === "connected" && state.snapshot) void fetchStatus(state.snapshot.node.instanceId);
      },
      onError(error) {
        if (alive) setServiceError(error.message);
      },
    });
    void client.getSnapshot().catch((error: unknown) => {
      if (alive && !state.snapshot)
        setServiceError(error instanceof Error ? error.message : "Snapshot request failed.");
    });
    return () => dispose();
  });

  function dispose(): void {
    if (!alive) return;
    alive = false;
    ++statusRequest;
    statusController?.abort();
    for (const controller of operationRequests.values()) controller.abort();
    operationRequests.clear();
    pendingOperations.clear();
    unsubscribe?.();
    client?.close();
    client = undefined;
  }

  onCleanup(dispose);

  return {
    snapshot: () => state.snapshot,
    connection,
    serviceError,
    statusResponse,
    statusError,
    logs,
    changes,
    pending,
    results,
    execute,
    getInventory(options) {
      if (!alive || !client) return Promise.reject(new Error("The Synapsed service connection is not ready."));
      return client.getInventory(options);
    },
  };
}

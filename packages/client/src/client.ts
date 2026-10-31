import type {
  ApiError,
  CharacteristicCommand,
  DeviceResponse,
  LogEntry,
  LogsResponse,
  ServerEvent,
  Snapshot,
  StatusResponse,
} from "./generated/index.js";
import {
  ProtocolValidationError,
  validateCharacteristicCommand,
  validateDeviceResponse,
  validateErrorResponse,
  validateLogsResponse,
  validateServerEvent,
  validateSnapshot,
  validateStatusResponse,
} from "./generated/index.js";

export type ClientStatus = "connecting" | "connected" | "disconnected" | "incompatible";
export interface RequestOptions {
  signal?: AbortSignal;
}
export interface SubscriptionCallbacks {
  onSnapshot(snapshot: Snapshot): void;
  onLogs(entries: LogEntry[]): void;
  onLog(entry: LogEntry): void;
  /** Non-connected status explicitly marks retained snapshots/logs stale. */
  onStatus(status: ClientStatus): void;
  onError(error: Error): void;
}
export interface SynapsedClient {
  getStatus(options?: RequestOptions): Promise<StatusResponse>;
  getSnapshot(options?: RequestOptions): Promise<Snapshot>;
  getDevice(deviceId: string, options?: RequestOptions): Promise<DeviceResponse>;
  execute(
    deviceId: string,
    characteristicId: string,
    command: CharacteristicCommand,
    options?: RequestOptions,
  ): Promise<Snapshot>;
  getLogs(options?: RequestOptions): Promise<LogsResponse>;
  subscribe(callbacks: SubscriptionCallbacks): () => void;
  close(): void;
}

export class EndpointPolicyError extends Error {
  constructor() {
    super("The Synapsed endpoint must be an HTTP loopback origin without credentials, path, query or fragment.");
    this.name = "EndpointPolicyError";
  }
}
export class ApiRequestError extends Error {
  constructor(
    readonly status: number,
    readonly apiError: ApiError,
  ) {
    super(apiError.message);
    this.name = "ApiRequestError";
  }
}
export class RequestTimeoutError extends Error {
  constructor() {
    super("The Synapsed request timed out after five seconds.");
    this.name = "RequestTimeoutError";
  }
}
export class StaleResponseError extends Error {
  constructor() {
    super("The Synapsed response belongs to an obsolete connection or daemon instance.");
    this.name = "StaleResponseError";
  }
}

/** Validates the original spelling as well as URL's canonicalized hostname. */
export function validateBaseUrl(baseUrl: string): URL {
  const match = /^http:\/\/(\[[0-9a-f:]+\]|localhost|[0-9.]+)(?::[0-9]+)?\/?$/i.exec(baseUrl);
  if (!match) throw new EndpointPolicyError();
  let url: URL;
  try {
    url = new URL(baseUrl);
  } catch {
    throw new EndpointPolicyError();
  }
  const literalHost = match[1]!;
  const ipv4 = literalHost.split(".");
  const loopbackV4 =
    ipv4.length === 4 &&
    ipv4[0] === "127" &&
    ipv4.every((part) => /^(0|[1-9][0-9]{0,2})$/.test(part) && Number(part) <= 255);
  if (
    !(
      literalHost.toLowerCase() === "localhost" ||
      loopbackV4 ||
      (literalHost.startsWith("[") && url.hostname === "[::1]")
    )
  ) {
    throw new EndpointPolicyError();
  }
  return new URL(url.origin);
}

const TIMEOUT_MS = 5_000;
const RECONNECT_MS = [1_000, 2_000, 4_000, 8_000, 10_000] as const;
const LOG_LIMIT = 200;
function newer(a: string, b: string): boolean {
  // Validated canonical decimal strings: numeric order without precision loss.
  return a.length > b.length || (a.length === b.length && a > b);
}
function abortError(): DOMException {
  return new DOMException("The operation was aborted.", "AbortError");
}
function closedError(): Error {
  return new Error("The Synapsed client is closed.");
}
function networkError(): Error {
  return new Error("The local Synapsed service is unavailable.");
}

interface SocketSession {
  socket: WebSocket;
  generation: number;
  instance: string | null;
  initialTimer: number | undefined;
  dispose(): void;
}

class NativeClient implements SynapsedClient {
  private readonly baseUrl: URL;
  private readonly subscribers = new Map<number, SubscriptionCallbacks>();
  private readonly requests = new Set<AbortController>();
  private nextSubscriber = 0;
  private generation = 0;
  private instance: string | null = null;
  private snapshot: Snapshot | null = null;
  private logs: LogEntry[] = [];
  private lastLogId: string | null = null;
  private hasLogs = false;
  private status: ClientStatus = "disconnected";
  private session: SocketSession | null = null;
  private reconnectTimer: number | undefined;
  private reconnectAttempt = 0;
  private terminalError: Error | null = null;
  private closed = false;

  constructor(baseUrl: string) {
    this.baseUrl = validateBaseUrl(baseUrl);
  }

  getStatus(options?: RequestOptions): Promise<StatusResponse> {
    return this.request(
      "/api/v1/status",
      (data) => {
        const result = validateStatusResponse(data);
        this.acceptHttpInstance(result.node.instanceId);
        return result;
      },
      options,
    );
  }
  getSnapshot(options?: RequestOptions): Promise<Snapshot> {
    return this.request("/api/v1/devices", (data) => this.acceptHttpSnapshot(validateSnapshot(data)), options);
  }
  getDevice(deviceId: string, options?: RequestOptions): Promise<DeviceResponse> {
    return this.request(
      `/api/v1/devices/${encodeURIComponent(deviceId)}`,
      (data) => {
        const result = validateDeviceResponse(data);
        this.acceptHttpInstance(result.node.instanceId);
        // A delayed individual-device response must not regress an accepted whole state.
        if (this.snapshot && !newer(result.revision, this.snapshot.revision)) {
          const device = this.snapshot.devices.find((candidate) => candidate.id === deviceId);
          if (!device) throw new StaleResponseError();
          return { node: this.snapshot.node, revision: this.snapshot.revision, device };
        }
        return result;
      },
      options,
    );
  }
  async execute(
    deviceId: string,
    characteristicId: string,
    command: CharacteristicCommand,
    options?: RequestOptions,
  ): Promise<Snapshot> {
    validateCharacteristicCommand(command);
    return this.request(
      `/api/v1/devices/${encodeURIComponent(deviceId)}/characteristics/${encodeURIComponent(characteristicId)}`,
      (data) => this.acceptHttpSnapshot(validateSnapshot(data)),
      options,
      command,
    );
  }
  getLogs(options?: RequestOptions): Promise<LogsResponse> {
    return this.request(
      "/api/v1/logs",
      (data) => {
        const result = validateLogsResponse(data);
        this.acceptHttpInstance(result.node.instanceId);
        // HTTP is a read result, not a stream ring replacement: do not erase newer live logs.
        return result;
      },
      options,
    );
  }

  subscribe(callbacks: SubscriptionCallbacks): () => void {
    if (this.closed) throw closedError();
    const id = this.nextSubscriber++;
    this.subscribers.set(id, callbacks);
    if (!this.session && !this.reconnectTimer && !this.terminalError) this.connect();
    else callbacks.onStatus(this.status);
    if (this.subscribers.has(id) && this.snapshot) callbacks.onSnapshot(this.snapshot);
    if (this.subscribers.has(id) && this.hasLogs) callbacks.onLogs([...this.logs]);
    if (this.subscribers.has(id) && this.terminalError) callbacks.onError(this.terminalError);
    let subscribed = true;
    return () => {
      if (!subscribed) return;
      subscribed = false;
      this.subscribers.delete(id);
      if (this.subscribers.size === 0) {
        this.stopConnection();
        this.abortRequests();
        this.generation += 1;
        if (!this.terminalError) this.status = "disconnected";
      }
    };
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.subscribers.clear();
    this.stopConnection();
    this.abortRequests();
    this.generation += 1;
  }

  private emit(invoke: (callbacks: SubscriptionCallbacks) => void): void {
    for (const [id, callbacks] of this.subscribers) {
      if (this.closed) break;
      if (this.subscribers.has(id)) invoke(callbacks);
    }
  }
  private setStatus(status: ClientStatus): void {
    if (this.status === status) return;
    this.status = status;
    this.emit((callbacks) => callbacks.onStatus(status));
  }
  private abortRequests(): void {
    for (const controller of this.requests) controller.abort();
  }
  private ensureUsable(): void {
    if (this.closed) throw closedError();
    if (this.terminalError) throw this.terminalError;
  }

  private async request<T>(
    path: string,
    validate: (value: unknown) => T,
    options?: RequestOptions,
    command?: CharacteristicCommand,
  ): Promise<T> {
    this.ensureUsable();
    if (options?.signal?.aborted) throw options.signal.reason ?? abortError();
    const generation = this.generation;
    const controller = new AbortController();
    this.requests.add(controller);
    const onAbort = () => controller.abort(options?.signal?.reason ?? abortError());
    options?.signal?.addEventListener("abort", onAbort, { once: true });
    let timedOut = false;
    const timeout = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, TIMEOUT_MS);
    try {
      const response = await fetch(new URL(path, this.baseUrl), {
        method: command ? "POST" : "GET",
        credentials: "omit",
        redirect: "error",
        signal: controller.signal,
        ...(command ? { headers: { "Content-Type": "application/json" }, body: JSON.stringify(command) } : {}),
      });
      if (this.closed || generation !== this.generation) {
        controller.abort();
        throw this.closed ? closedError() : new StaleResponseError();
      }
      let data: unknown;
      try {
        data = await response.json();
      } catch (error) {
        if (controller.signal.aborted) throw error;
        if (this.closed) throw closedError();
        if (generation !== this.generation) throw new StaleResponseError();
        throw new ProtocolValidationError(
          !response.ok
            ? "ErrorResponse"
            : command || path === "/api/v1/devices"
              ? "Snapshot"
              : path === "/api/v1/status"
                ? "StatusResponse"
                : path === "/api/v1/logs"
                  ? "LogsResponse"
                  : "DeviceResponse",
        );
      }
      if (this.closed) throw closedError();
      if (generation !== this.generation) throw new StaleResponseError();
      if (controller.signal.aborted) throw controller.signal.reason ?? abortError();
      if (!response.ok) {
        const { error } = validateErrorResponse(data);
        throw new ApiRequestError(response.status, error);
      }
      return validate(data);
    } catch (error) {
      if (timedOut) throw new RequestTimeoutError();
      if (options?.signal?.aborted) throw options.signal.reason ?? abortError();
      if (
        error instanceof ProtocolValidationError ||
        (error instanceof ApiRequestError &&
          (error.status === 403 ||
            error.apiError.code === "forbidden_origin" ||
            error.apiError.code === "forbidden_host"))
      ) {
        this.failPermanently(error);
      }
      if (error instanceof Error) throw error;
      throw networkError();
    } finally {
      clearTimeout(timeout);
      options?.signal?.removeEventListener("abort", onAbort);
      this.requests.delete(controller);
    }
  }

  private adoptInstance(instance: string): void {
    if (this.instance === instance) return;
    // HTTP's first epoch establishes ordering; replacing it invalidates outstanding work.
    if (this.instance !== null) this.generation += 1;
    this.instance = instance;
    this.snapshot = null;
    this.logs = [];
    this.lastLogId = null;
    this.hasLogs = false;
  }
  private acceptHttpInstance(instance: string): void {
    if (this.instance === instance) return;
    if (this.instance !== null && this.subscribers.size > 0) {
      // A delayed HTTP reply cannot elect a daemon epoch over the subscribed stream.
      if (this.session) this.disconnect(this.session, new StaleResponseError());
      throw new StaleResponseError();
    }
    this.adoptInstance(instance);
  }
  private acceptHttpSnapshot(snapshot: Snapshot): Snapshot {
    this.acceptHttpInstance(snapshot.node.instanceId);
    this.acceptSnapshot(snapshot);
    return this.snapshot!;
  }
  private acceptSnapshot(snapshot: Snapshot): void {
    if (this.snapshot && !newer(snapshot.revision, this.snapshot.revision)) return;
    this.snapshot = snapshot;
    this.emit((callbacks) => callbacks.onSnapshot(snapshot));
  }

  private connect(): void {
    if (this.closed || this.terminalError || this.subscribers.size === 0 || this.session) return;
    this.setStatus("connecting");
    if (this.closed || this.subscribers.size === 0) return;
    const url = new URL("/api/v1/events", this.baseUrl);
    url.protocol = "ws:";
    let socket: WebSocket;
    try {
      socket = new WebSocket(url);
    } catch {
      this.failPermanently(new Error("This runtime cannot establish a native Synapsed WebSocket."));
      return;
    }
    const session: SocketSession = {
      socket,
      generation: this.generation,
      instance: null,
      initialTimer: undefined,
      dispose: () => {
        socket.removeEventListener("message", onMessage);
        socket.removeEventListener("error", onError);
        socket.removeEventListener("close", onClose);
        clearTimeout(session.initialTimer);
        session.initialTimer = undefined;
        socket.close();
      },
    };
    const onMessage = (event: MessageEvent<unknown>) => this.receive(session, event.data);
    const onError = () => {
      if (!this.current(session)) return;
      this.disconnect(session, networkError());
      // Native WebSocket deliberately hides HTTP handshake status. An ordinary same-origin
      // read exposes a 403 policy envelope, without sending/retrying any command.
      if (this.closed || this.terminalError || this.subscribers.size === 0) return;
      void this.request("/api/v1/status", validateStatusResponse).catch(() => {});
    };
    const onClose = (event: CloseEvent) => {
      if (!this.current(session)) return;
      if ([1002, 1003, 1007, 1008, 1009].includes(event.code)) {
        this.failPermanently(new Error("The Synapsed event stream was rejected by protocol or access policy."));
      } else this.disconnect(session, networkError());
    };
    this.session = session;
    socket.addEventListener("message", onMessage);
    socket.addEventListener("error", onError);
    socket.addEventListener("close", onClose);
    session.initialTimer = setTimeout(() => {
      if (this.current(session)) this.disconnect(session, new RequestTimeoutError());
    }, TIMEOUT_MS);
  }
  private current(session: SocketSession): boolean {
    return !this.closed && this.session === session && session.generation === this.generation;
  }
  private receive(session: SocketSession, data: unknown): void {
    if (!this.current(session)) return;
    let event: ServerEvent;
    try {
      if (typeof data !== "string" || new Blob([data]).size > 1_048_576) {
        throw new ProtocolValidationError("ServerEvent");
      }
      let parsed: unknown;
      try {
        parsed = JSON.parse(data);
      } catch {
        throw new ProtocolValidationError("ServerEvent");
      }
      event = validateServerEvent(parsed);
      if (session.instance === null && event.type !== "snapshot") {
        throw new ProtocolValidationError("ServerEvent");
      }
      if (
        event.type === "snapshot" &&
        session.instance !== null &&
        event.snapshot.node.instanceId !== session.instance
      ) {
        throw new ProtocolValidationError("ServerEvent");
      }
    } catch (error) {
      this.failPermanently(error instanceof Error ? error : new ProtocolValidationError("ServerEvent"));
      return;
    }
    if (event.type === "snapshot") {
      if (session.instance === null) {
        // A first socket elects the epoch over requests started without a known instance.
        if (this.instance === null) this.generation += 1;
        this.adoptInstance(event.snapshot.node.instanceId);
        session.generation = this.generation;
        session.instance = event.snapshot.node.instanceId;
        clearTimeout(session.initialTimer);
        session.initialTimer = undefined;
        this.reconnectAttempt = 0;
        this.setStatus("connected");
        if (!this.current(session)) return;
      }
      this.acceptSnapshot(event.snapshot);
    } else if (event.type === "logs") {
      this.replaceLogs(event.entries);
    } else {
      if (this.lastLogId !== null && !newer(event.entry.id, this.lastLogId)) return;
      this.lastLogId = event.entry.id;
      this.logs.push(event.entry);
      if (this.logs.length > LOG_LIMIT) this.logs.shift();
      this.hasLogs = true;
      this.emit((callbacks) => callbacks.onLog(event.entry));
    }
  }
  private replaceLogs(entries: LogEntry[]): void {
    // Initial ring replaces the visible ring even on same-instance reconnect.
    const sorted = [...entries].sort((a, b) => (a.id === b.id ? 0 : newer(a.id, b.id) ? 1 : -1));
    this.logs = sorted.filter((entry, index) => index === 0 || entry.id !== sorted[index - 1]!.id).slice(-LOG_LIMIT);
    this.lastLogId = this.logs.at(-1)?.id ?? null;
    this.hasLogs = true;
    this.emit((callbacks) => callbacks.onLogs([...this.logs]));
  }
  private disconnect(session: SocketSession, error: Error): void {
    if (!this.current(session)) return;
    this.session = null;
    session.dispose();
    this.generation += 1;
    this.setStatus("disconnected");
    this.emit((callbacks) => callbacks.onError(error));
    if (this.closed || this.terminalError || this.subscribers.size === 0) return;
    const delay = RECONNECT_MS[Math.min(this.reconnectAttempt, RECONNECT_MS.length - 1)]!;
    this.reconnectAttempt += 1;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = undefined;
      this.connect();
    }, delay);
  }
  private stopConnection(): void {
    clearTimeout(this.reconnectTimer);
    this.reconnectTimer = undefined;
    const session = this.session;
    this.session = null;
    session?.dispose();
  }
  private failPermanently(error: Error): void {
    if (this.closed || this.terminalError) return;
    this.terminalError = error;
    this.stopConnection();
    this.abortRequests();
    this.generation += 1;
    this.setStatus("incompatible");
    this.emit((callbacks) => callbacks.onError(error));
  }
}

/** One client owns HTTP work and a demand-driven, shared native event stream. */
export function createClient(options: { baseUrl: string }): SynapsedClient {
  return new NativeClient(options.baseUrl);
}

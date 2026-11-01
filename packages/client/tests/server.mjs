import { createServer } from "node:http";
import { EventEmitter } from "node:events";
import { WebSocketServer } from "ws";
import { createClient } from "../dist/index.js";

export const INSTANCE_A = "22222222-2222-4222-8222-222222222222";
export const INSTANCE_B = "33333333-3333-4333-8333-333333333333";

export function characteristic(overrides = {}) {
  return {
    id: "measurement",
    label: "Measurement",
    group: null,
    error: null,
    status: "ok",
    value: 4.5,
    spec: { kind: "number", minimum: 0, maximum: 100, step: 0.5, integer: false, unit: "cm" },
    access: { read: true, write: true, notify: true },
    ...overrides,
  };
}
export function snapshot(revision = "0", instanceId = INSTANCE_A, value = 4.5) {
  return {
    apiVersion: 1,
    revision,
    node: { nodeId: "11111111-1111-4111-8111-111111111111", instanceId, name: "Isolated test node" },
    drivers: [{ id: "fixture", version: "0.1.0", state: "online", error: null }],
    devices: [
      {
        id: "instrument",
        driver: "fixture",
        kind: "instrument",
        label: "Instrument",
        status: "online",
        error: null,
        groups: [],
        characteristics: [characteristic({ value })],
      },
    ],
  };
}
export function log(id, message = "Observed test state") {
  return { id: String(id), timestamp: "2026-10-31T12:00:00Z", level: "info", target: "fixture", message };
}
export function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

/** Real HTTP/WS peers; route overrides create controlled response races, not transport mocks. */
export async function serverFixture(t) {
  const fixture = {
    current: snapshot(),
    inventory: {
      apiVersion: 1,
      instanceId: INSTANCE_A,
      generatedAt: "2026-10-31T12:00:00Z",
      sources: ["usb", "thunderbolt", "ddc", "bluetooth"].map((kind) => ({ kind, state: "ok", message: null })),
      nodes: [
        {
          id: "n1234567890abcdef",
          parentId: null,
          kind: "host",
          label: "Isolated test node",
          detail: null,
          vendorId: null,
          productId: null,
          speedMbps: null,
          connected: true,
          attachment: "host",
          relation: "exact",
          rule: null,
          driver: null,
        },
      ],
    },
    recent: [],
    sockets: new Set(),
    clients: new Set(),
    connectionCount: 0,
    mutationCount: 0,
    requests: [],
    routes: new Map(),
    onConnection: null,
    rejectUpgrade: false,
  };
  const events = new EventEmitter();
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = Buffer.concat(chunks).toString();
    const record = { method: request.method, url: request.url, body };
    fixture.requests.push(record);
    events.emit("request", record);
    const reply = (data, status = 200) => {
      response.writeHead(status, { "Content-Type": "application/json" });
      response.end(JSON.stringify(data));
    };
    const route = fixture.routes.get(`${request.method} ${request.url}`);
    if (route) {
      await route({ request, response, reply, body });
      return;
    }
    if (request.url === "/api/v1/status") {
      reply({
        apiVersion: 1,
        node: fixture.current.node,
        revision: fixture.current.revision,
        uptimeSeconds: 1,
        drivers: fixture.current.drivers,
      });
    } else if (request.method === "GET" && request.url === "/api/v1/logs") {
      reply({ node: fixture.current.node, entries: fixture.recent });
    } else if (request.method === "GET" && request.url === "/api/v1/devices") {
      reply(fixture.current);
    } else if (request.method === "GET" && request.url === "/api/v1/inventory") {
      reply(fixture.inventory);
    } else if (request.url === "/api/v1/devices/instrument" && request.method === "GET") {
      reply({ node: fixture.current.node, revision: fixture.current.revision, device: fixture.current.devices[0] });
    } else if (request.method === "POST" && request.url === "/api/v1/devices/instrument/characteristics/measurement") {
      fixture.mutationCount += 1;
      const command = JSON.parse(body);
      const before = fixture.current.devices[0].characteristics[0].value;
      // Stateful relative operation: the result depends on the server's previous observation.
      if (command.op !== "step") {
        reply(
          { error: { code: "unsupported_operation", message: "Only relative fixture operations are enabled." } },
          422,
        );
        return;
      }
      const value = Math.max(0, Math.min(100, before + command.steps * 0.5));
      fixture.current = snapshot(String(BigInt(fixture.current.revision) + 1n), fixture.current.node.instanceId, value);
      fixture.broadcast({ type: "snapshot", snapshot: fixture.current });
      reply(fixture.current);
    } else {
      reply({ error: { code: "unknown_device", message: "Unknown declared device." } }, 404);
    }
  });
  const ws = new WebSocketServer({ noServer: true });
  server.on("upgrade", (request, socket, head) => {
    if (fixture.rejectUpgrade) {
      socket.end("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
      return;
    }
    ws.handleUpgrade(request, socket, head, (peer) => ws.emit("connection", peer, request));
  });
  ws.on("connection", (peer) => {
    fixture.connectionCount += 1;
    fixture.sockets.add(peer);
    peer.on("close", () => {
      fixture.sockets.delete(peer);
      events.emit("change");
    });
    if (fixture.onConnection) fixture.onConnection(peer);
    else {
      peer.send(JSON.stringify({ type: "snapshot", snapshot: fixture.current }));
      peer.send(JSON.stringify({ type: "logs", entries: fixture.recent }));
    }
    events.emit("change");
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  fixture.baseUrl = `http://127.0.0.1:${server.address().port}`;
  fixture.client = () => {
    const client = createClient({ baseUrl: fixture.baseUrl });
    fixture.clients.add(client);
    return client;
  };
  fixture.broadcast = (event) => {
    for (const peer of fixture.sockets) if (peer.readyState === 1) peer.send(JSON.stringify(event));
  };
  fixture.dropSockets = () => {
    for (const peer of fixture.sockets) peer.close(1012, "Daemon restart");
  };
  fixture.until = (predicate, timeout = 8_000) => until(events, predicate, timeout);
  fixture.waitRequest = (method, url) =>
    new Promise((resolve) => {
      const found = fixture.requests.find((request) => request.method === method && request.url === url);
      if (found) {
        resolve(found);
        return;
      }
      const listener = (request) => {
        if (request.method === method && request.url === url) {
          events.off("request", listener);
          resolve(request);
        }
      };
      events.on("request", listener);
    });
  t.after(async () => {
    for (const client of fixture.clients) client.close();
    for (const peer of fixture.sockets) peer.terminate();
    server.closeAllConnections();
    await Promise.all([new Promise((resolve) => server.close(resolve)), new Promise((resolve) => ws.close(resolve))]);
  });
  return fixture;
}

function until(events, predicate, timeout) {
  if (predicate()) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      events.off("change", check);
      reject(new Error("Expected client transition was not observed."));
    }, timeout);
    const check = () => {
      if (!predicate()) return;
      clearTimeout(timer);
      events.off("change", check);
      resolve();
    };
    events.on("change", check);
  });
}

export function observer(client) {
  const events = new EventEmitter();
  const state = { snapshots: [], rings: [], liveLogs: [], changes: [], statuses: [], errors: [], visibleLogs: [] };
  const unsubscribe = client.subscribe({
    onSnapshot(value) {
      state.snapshots.push(value);
      events.emit("change");
    },
    onLogs(entries) {
      state.rings.push(entries);
      state.visibleLogs = [...entries];
      events.emit("change");
    },
    onLog(entry) {
      state.liveLogs.push(entry);
      state.visibleLogs = [...state.visibleLogs, entry].slice(-200);
      events.emit("change");
    },
    onCharacteristicChanged(change) {
      state.changes.push(change);
      events.emit("change");
    },
    onStatus(status) {
      state.statuses.push(status);
      events.emit("change");
    },
    onError(error) {
      state.errors.push(error);
      events.emit("change");
    },
  });
  return {
    ...state,
    state,
    unsubscribe,
    until: (predicate, timeout = 8_000) => until(events, () => predicate(state), timeout),
  };
}

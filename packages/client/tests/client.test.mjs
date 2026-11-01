import assert from "node:assert/strict";
import { test } from "node:test";
import { scheduler, setTimeout as delay } from "node:timers/promises";
import {
  ApiRequestError,
  EndpointPolicyError,
  ProtocolValidationError,
  RequestTimeoutError,
  StaleResponseError,
  createClient,
  validateBaseUrl,
} from "../dist/index.js";
import { deferred, INSTANCE_A, INSTANCE_B, log, observer, serverFixture, snapshot } from "./server.mjs";

const DEVICE_ROUTE = "/api/v1/devices/instrument";
const WRITE_ROUTE = "/api/v1/devices/instrument/characteristics/measurement";

test("endpoint policy rejects remote, credentialed, normalized numeric and non-origin URLs", () => {
  for (const url of [
    "http://127.0.0.1:24816",
    "http://127.23.0.8/",
    "http://localhost:3000/",
    "http://[::1]:24816",
    "http://[0:0:0:0:0:0:0:1]/",
  ]) {
    assert.equal(validateBaseUrl(url).protocol, "http:");
  }
  for (const url of [
    "https://localhost",
    "http://example.com",
    "http://192.168.1.1",
    "http://0.0.0.0",
    "http://[::]",
    "http://[::ffff:127.0.0.1]",
    "http://user:secret@localhost",
    "http://localhost/api",
    "http://localhost/?",
    "http://localhost/#",
    "http://127.1",
    "http://2130706433",
    "http://0x7f000001",
    "http://127.000.0.1",
    "http://localhost./",
    " http://localhost",
    "http://localhost\\@example.com",
  ]) {
    assert.throws(() => createClient({ baseUrl: url }), EndpointPolicyError, url);
  }
});

test("native HTTP validates each response, encodes IDs and exposes bounded API errors", async (t) => {
  const server = await serverFixture(t);
  const client = server.client();
  server.current.extraFutureField = "accepted additive response";
  assert.equal((await client.getStatus()).node.instanceId, INSTANCE_A);
  assert.equal((await client.getSnapshot()).extraFutureField, "accepted additive response");
  assert.equal((await client.getDevice("instrument")).device.characteristics[0].value, 4.5);
  server.recent = [log(1)];
  assert.equal((await client.getLogs()).entries[0].id, "1");
  assert.equal((await client.getInventory()).nodes[0].kind, "host");
  const result = await client.execute("instrument", "measurement", { op: "step", steps: -2 });
  assert.equal(result.devices[0].characteristics[0].value, 3.5);
  assert.equal(server.mutationCount, 1);
  assert.equal(server.connectionCount, 0, "HTTP alone must not open a socket");
  await assert.rejects(client.getDevice("missing/with ?#%"), (error) => {
    assert.ok(error instanceof ApiRequestError);
    assert.equal(error.status, 404);
    assert.equal(error.apiError.code, "unknown_device");
    return true;
  });
  await assert.rejects(client.execute("missing/id", "field/?#", { op: "invoke" }), ApiRequestError);
  assert.ok(server.requests.some((request) => request.url === "/api/v1/devices/missing%2Fwith%20%3F%23%25"));
  assert.ok(
    server.requests.some((request) => request.url === "/api/v1/devices/missing%2Fid/characteristics/field%2F%3F%23"),
  );
  const before = server.requests.length;
  await assert.rejects(
    client.execute("instrument", "measurement", { op: "step", steps: 101 }),
    ProtocolValidationError,
  );
  await assert.rejects(
    client.execute("instrument", "measurement", { op: "toggle", extra: "rejected" }),
    ProtocolValidationError,
  );
  assert.equal(server.requests.length, before, "invalid commands are not sent");
});

test("inventory responses cannot cross a subscribed daemon instance epoch", async (t) => {
  const server = await serverFixture(t);
  const client = server.client();
  const state = observer(client);
  await state.until((value) => value.snapshots.at(-1)?.node.instanceId === INSTANCE_A);

  server.current = snapshot("0", INSTANCE_B, 55);
  server.inventory = { ...server.inventory, instanceId: INSTANCE_B };
  await assert.rejects(client.getInventory(), StaleResponseError);
  await state.until((value) => value.snapshots.at(-1)?.node.instanceId === INSTANCE_B);

  const inventory = await client.getInventory();
  assert.equal(inventory.instanceId, INSTANCE_B);
  assert.equal(state.state.snapshots.at(-1).node.instanceId, inventory.instanceId);
  assert.equal(server.connectionCount, 2);
});

test("one demanded socket waits for an actual initial snapshot and serves late subscribers", async (t) => {
  const server = await serverFixture(t);
  server.onConnection = () => {};
  const client = server.client();
  const first = observer(client);
  await server.until(() => server.connectionCount === 1);
  assert.deepEqual(first.state.statuses, ["connecting"]);
  assert.equal(first.state.snapshots.length, 0);
  server.broadcast({ type: "snapshot", snapshot: server.current });
  server.broadcast({ type: "logs", entries: [] });
  await first.until((state) => state.rings.length === 1);
  assert.ok(first.state.statuses.includes("connected"));
  const second = observer(client);
  assert.equal(second.state.snapshots[0].revision, "0");
  assert.deepEqual(second.state.statuses, ["connected"]);
  assert.deepEqual(second.state.rings, [[]]);
  assert.equal(server.connectionCount, 1);
  first.unsubscribe();
  server.current = snapshot("1");
  server.broadcast({ type: "snapshot", snapshot: server.current });
  await second.until((state) => state.snapshots.at(-1)?.revision === "1");
  assert.equal(first.state.snapshots.length, 1);
  second.unsubscribe();
  await server.until(() => server.sockets.size === 0);
  await delay(1_100);
  assert.equal(server.connectionCount, 1, "last unsubscribe prevents reconnect");
});

test("characteristic change events dispatch without modifying log state", async (t) => {
  const server = await serverFixture(t);
  const state = observer(server.client());
  await state.until((value) => value.rings.length === 1);
  const change = {
    revision: "1",
    timestamp: "2026-11-01T09:08:29.123Z",
    deviceId: "instrument",
    characteristicId: "measurement",
    value: 5,
    status: "ok",
    error: null,
  };
  server.broadcast({ type: "characteristic_changed", change });
  await state.until((value) => value.changes.length === 1);
  assert.deepEqual(state.state.changes, [change]);
  assert.deepEqual(state.state.rings, [[]]);
  assert.deepEqual(state.state.liveLogs, []);
  assert.deepEqual(state.state.visibleLogs, []);
});

test("decimal snapshot ordering discards 9 after 10, equal revisions and precision-loss updates", async (t) => {
  const server = await serverFixture(t);
  server.current = snapshot("9");
  const state = observer(server.client());
  await state.until((value) => value.snapshots.length === 1);
  server.broadcast({ type: "snapshot", snapshot: snapshot("10", INSTANCE_A, 10) });
  server.broadcast({ type: "snapshot", snapshot: snapshot("9", INSTANCE_A, 9) });
  server.broadcast({ type: "snapshot", snapshot: snapshot("10", INSTANCE_A, 99) });
  server.broadcast({ type: "snapshot", snapshot: snapshot("9007199254740993", INSTANCE_A, 12) });
  server.broadcast({ type: "snapshot", snapshot: snapshot("9007199254740992", INSTANCE_A, 13) });
  server.broadcast({ type: "log", entry: log(1) });
  await state.until((value) => value.liveLogs.length === 1);
  assert.deepEqual(
    state.state.snapshots.map((value) => value.revision),
    ["9", "10", "9007199254740993"],
  );
  assert.equal(state.state.snapshots.at(-1).devices[0].characteristics[0].value, 12);
});

test("late mutation, snapshot and device HTTP replies cannot regress live authoritative state", async (t) => {
  const server = await serverFixture(t);
  server.current = snapshot("9", INSTANCE_A, 40);
  const client = server.client();
  const state = observer(client);
  await state.until((value) => value.snapshots.length === 1);
  const release = deferred();
  t.after(() => release.resolve());
  server.routes.set(`POST ${WRITE_ROUTE}`, async ({ reply }) => {
    server.mutationCount += 1;
    // An earlier completed mutation's response is delayed while a later observation arrives.
    const old = snapshot("10", INSTANCE_A, 40.5);
    await release.promise;
    reply(old);
  });
  server.routes.set("GET /api/v1/devices", async ({ reply }) => {
    await release.promise;
    reply(snapshot("9", INSTANCE_A, 40));
  });
  server.routes.set(`GET ${DEVICE_ROUTE}`, async ({ reply }) => {
    await release.promise;
    const old = snapshot("9", INSTANCE_A, 40);
    reply({ node: old.node, revision: old.revision, device: old.devices[0] });
  });
  const write = client.execute("instrument", "measurement", { op: "step", steps: 1 });
  const read = client.getSnapshot();
  const device = client.getDevice("instrument");
  await Promise.all([
    server.waitRequest("POST", WRITE_ROUTE),
    server.waitRequest("GET", "/api/v1/devices"),
    server.waitRequest("GET", DEVICE_ROUTE),
  ]);
  server.current = snapshot("11", INSTANCE_A, 55);
  server.broadcast({ type: "snapshot", snapshot: server.current });
  await state.until((value) => value.snapshots.at(-1)?.revision === "11");
  release.resolve();
  assert.equal((await write).revision, "11");
  assert.equal((await read).devices[0].characteristics[0].value, 55);
  assert.equal((await device).device.characteristics[0].value, 55);
  assert.deepEqual(
    state.state.snapshots.map((value) => value.revision),
    ["9", "11"],
  );
  assert.equal(server.mutationCount, 1);
});

test("restart adopts only the new socket epoch, invalidates old work and resets log sequences", async (t) => {
  const server = await serverFixture(t);
  server.current = snapshot("10");
  server.recent = [log(9)];
  const client = server.client();
  const state = observer(client);
  await state.until((value) => value.rings.length === 1);
  const release = deferred();
  t.after(() => release.resolve());
  server.routes.set(`POST ${WRITE_ROUTE}`, async ({ reply }) => {
    server.mutationCount += 1;
    await release.promise;
    reply(snapshot("11", INSTANCE_A, 5));
  });
  const write = client.execute("instrument", "measurement", { op: "step", steps: 1 });
  const rejected = assert.rejects(write, StaleResponseError);
  await server.waitRequest("POST", WRITE_ROUTE);
  server.current = snapshot("0", INSTANCE_B, 8);
  server.recent = [log(1, "Restart ring")];
  server.dropSockets();
  await state.until((value) => value.snapshots.at(-1)?.node.instanceId === INSTANCE_B);
  await state.until((value) => value.rings.length === 2);
  release.resolve();
  await rejected;
  assert.ok(state.state.statuses.includes("disconnected"));
  assert.deepEqual(
    state.state.rings.at(-1).map((entry) => entry.id),
    ["1"],
  );
  server.broadcast({ type: "log", entry: log(2) });
  await state.until((value) => value.liveLogs.length === 1);
  assert.equal(server.mutationCount, 1, "a restart never replays the pending command");
});

for (const [name, status, body, sendHeadersEarly] of [
  ["old snapshot", 200, JSON.stringify(snapshot("10", INSTANCE_A, 40)), false],
  ["malformed success", 200, JSON.stringify({ apiVersion: 2 }), true],
  ["malformed JSON", 200, "obsolete malformed {response", true],
  [
    "old policy denial",
    403,
    JSON.stringify({
      error: {
        code: "forbidden_origin",
        message: "The request origin is not permitted.",
      },
    }),
    false,
  ],
]) {
  test(`first socket epoch discards pending unknown-instance HTTP ${name} without disrupting its stream`, async (t) => {
    const server = await serverFixture(t);
    server.onConnection = () => {};
    const client = server.client();
    const state = observer(client);
    await server.until(() => server.connectionCount === 1);
    const release = deferred();
    const arrived = deferred();
    t.after(() => release.resolve());
    server.routes.set("GET /api/v1/devices", async ({ response }) => {
      if (sendHeadersEarly) {
        response.writeHead(status, { "Content-Type": "application/json" });
        response.write(body.slice(0, 1));
      }
      arrived.resolve();
      await release.promise;
      if (!sendHeadersEarly) response.writeHead(status, { "Content-Type": "application/json" });
      response.end(sendHeadersEarly ? body.slice(1) : body);
    });
    const rejected = assert.rejects(client.getSnapshot(), StaleResponseError);
    await arrived.promise;
    server.current = snapshot("0", INSTANCE_B, 55);
    server.broadcast({ type: "snapshot", snapshot: server.current });
    server.broadcast({ type: "logs", entries: [] });
    await state.until((value) => value.rings.length === 1);
    release.resolve();
    await rejected;
    assert.deepEqual(state.state.statuses, ["connecting", "connected"]);
    assert.deepEqual(state.state.errors, [], "obsolete data cannot poison or disconnect the elected epoch");
    server.routes.delete("GET /api/v1/devices");
    server.current = snapshot("1", INSTANCE_B, 56);
    server.broadcast({ type: "snapshot", snapshot: server.current });
    server.broadcast({ type: "log", entry: log(1) });
    await state.until((value) => value.liveLogs.length === 1);
    assert.equal(state.state.snapshots.at(-1).revision, "1", "generation advancement keeps the current socket live");
    assert.equal((await client.getSnapshot()).node.instanceId, INSTANCE_B);
    const late = observer(client);
    assert.deepEqual(late.state.statuses, ["connected"]);
    assert.equal(late.state.snapshots[0].devices[0].characteristics[0].value, 56);
    assert.deepEqual(
      late.state.rings[0].map((entry) => entry.id),
      ["1"],
    );
    assert.equal(server.connectionCount, 1);
  });
}

test("a different-instance HTTP reply while subscribed is rejected, then the socket elects the epoch", async (t) => {
  const server = await serverFixture(t);
  const client = server.client();
  const state = observer(client);
  await state.until((value) => value.snapshots.length === 1);
  server.current = snapshot("0", INSTANCE_B, 7);
  await assert.rejects(client.getSnapshot(), StaleResponseError);
  assert.equal(state.state.snapshots.at(-1).node.instanceId, INSTANCE_A);
  assert.ok(state.state.statuses.includes("disconnected"));
  await state.until((value) => value.snapshots.at(-1)?.node.instanceId === INSTANCE_B);
  assert.equal(server.connectionCount, 2);
});

test("initial and reconnect log rings replace, deduplicate and cap cached late-subscriber logs", async (t) => {
  const server = await serverFixture(t);
  server.recent = [log(10), log(9), log(10)];
  const client = server.client();
  const state = observer(client);
  await state.until((value) => value.rings.length === 1);
  assert.deepEqual(
    state.state.rings[0].map((entry) => entry.id),
    ["9", "10"],
  );
  server.broadcast({ type: "log", entry: log(9) });
  server.broadcast({ type: "log", entry: log(10) });
  for (let id = 11; id <= 215; id += 1) server.broadcast({ type: "log", entry: log(id) });
  await state.until((value) => value.liveLogs.at(-1)?.id === "215");
  const late = observer(client);
  assert.equal(late.state.rings[0].length, 200);
  assert.equal(late.state.rings[0][0].id, "16");
  assert.equal(late.state.rings[0].at(-1).id, "215");
  server.recent = [log(214), log(215), log(216)];
  server.dropSockets();
  await state.until((value) => value.rings.length === 2);
  assert.deepEqual(
    state.state.visibleLogs.map((entry) => entry.id),
    ["214", "215", "216"],
  );
  server.broadcast({ type: "log", entry: log(216) });
  server.broadcast({ type: "log", entry: log(217) });
  await state.until((value) => value.liveLogs.at(-1)?.id === "217");
  assert.deepEqual(
    state.state.visibleLogs.map((entry) => entry.id),
    ["214", "215", "216", "217"],
  );
});

test("five-second mutation timeout and caller abort never resend a command", { timeout: 12_000 }, async (t) => {
  const server = await serverFixture(t);
  const client = server.client();
  const release = deferred();
  const secondArrived = deferred();
  t.after(() => release.resolve());
  server.routes.set(`POST ${WRITE_ROUTE}`, async ({ reply }) => {
    server.mutationCount += 1;
    if (server.mutationCount === 2) secondArrived.resolve();
    await release.promise;
    reply(snapshot("1"));
  });
  await assert.rejects(client.execute("instrument", "measurement", { op: "step", steps: 1 }), RequestTimeoutError);
  assert.equal(server.mutationCount, 1);
  const controller = new AbortController();
  const aborted = client.execute("instrument", "measurement", { op: "step", steps: -1 }, { signal: controller.signal });
  const rejected = assert.rejects(aborted, (error) => error.name === "AbortError");
  await secondArrived.promise;
  controller.abort();
  await rejected;
  const before = server.requests.length;
  await assert.rejects(client.getSnapshot({ signal: controller.signal }), (error) => error.name === "AbortError");
  assert.equal(server.requests.length, before);
  release.resolve();
  await delay(1_100);
  assert.equal(server.mutationCount, 2);
});

test("last unsubscribe aborts owned HTTP, clears the socket and prohibits late callbacks", async (t) => {
  const server = await serverFixture(t);
  const client = server.client();
  const state = observer(client);
  await state.until((value) => value.snapshots.length === 1);
  const release = deferred();
  t.after(() => release.resolve());
  server.routes.set("GET /api/v1/devices", async ({ reply }) => {
    await release.promise;
    reply(snapshot("2"));
  });
  const request = client.getSnapshot();
  const rejected = assert.rejects(request);
  await server.waitRequest("GET", "/api/v1/devices");
  state.unsubscribe();
  await rejected;
  const count = state.state.snapshots.length + state.state.statuses.length + state.state.errors.length;
  release.resolve();
  await server.until(() => server.sockets.size === 0);
  await delay(1_100);
  assert.equal(state.state.snapshots.length + state.state.statuses.length + state.state.errors.length, count);
  assert.equal(server.connectionCount, 1);
  client.close();
  client.close();
  await assert.rejects(client.getStatus(), /closed/);
  assert.throws(() => observer(client), /closed/);
});

test(
  "an accepted socket without an initial snapshot times out and reconnects only while demanded",
  { timeout: 12_000 },
  async (t) => {
    const server = await serverFixture(t);
    server.onConnection = () => {};
    const state = observer(server.client());
    await state.until((value) => value.errors.some((error) => error instanceof RequestTimeoutError));
    assert.ok(!state.state.statuses.includes("connected"));
    server.onConnection = (peer) => peer.send(JSON.stringify({ type: "snapshot", snapshot: server.current }));
    await state.until((value) => value.statuses.includes("connected"));
    assert.equal(server.connectionCount, 2);
  },
);

for (const [name, payload] of [
  ["malformed JSON", "{not JSON"],
  ["incompatible API", JSON.stringify({ type: "snapshot", snapshot: { ...snapshot(), apiVersion: 2 } })],
  ["noncanonical revision", JSON.stringify({ type: "snapshot", snapshot: snapshot("01") })],
  ["logs before initial snapshot", JSON.stringify({ type: "logs", entries: [] })],
  ["binary frame", Buffer.from("not a text event")],
]) {
  test(`${name} visibly terminates the event stream without reconnect`, async (t) => {
    const server = await serverFixture(t);
    server.onConnection = (peer) => peer.send(payload);
    const state = observer(server.client());
    await state.until((value) => value.statuses.includes("incompatible"));
    assert.ok(state.state.errors[0] instanceof ProtocolValidationError);
    assert.ok(!state.state.errors[0].message.includes("not JSON"));
    await delay(1_100);
    assert.equal(server.connectionCount, 1);
  });
}

test("later stream frames cannot switch instance without reconnect", async (t) => {
  const server = await serverFixture(t);
  const state = observer(server.client());
  await state.until((value) => value.snapshots.length === 1);
  server.broadcast({ type: "snapshot", snapshot: snapshot("1", INSTANCE_B) });
  await state.until((value) => value.statuses.includes("incompatible"));
  assert.equal(state.state.snapshots.at(-1).node.instanceId, INSTANCE_A);
});

for (const [name, response] of [
  ["malformed success response", { apiVersion: 2 }],
  ["malformed error envelope", { error: { code: "invented", message: "Rejected body must not be exposed" } }],
]) {
  test(`${name} terminates HTTP and subscribed stream visibly`, async (t) => {
    const server = await serverFixture(t);
    const client = server.client();
    const state = observer(client);
    await state.until((value) => value.snapshots.length === 1);
    server.routes.set("GET /api/v1/status", ({ reply }) => reply(response, name.includes("error") ? 503 : 200));
    await assert.rejects(client.getStatus(), ProtocolValidationError);
    assert.ok(state.state.statuses.includes("incompatible"));
    await delay(1_100);
    assert.equal(server.connectionCount, 1);
  });
}

test("HTTP 403 and opaque WebSocket handshake denial stop reconnect through validated policy error", async (t) => {
  const server = await serverFixture(t);
  server.rejectUpgrade = true;
  server.routes.set("GET /api/v1/status", ({ reply }) =>
    reply(
      {
        error: { code: "forbidden_origin", message: "The request origin is not permitted." },
      },
      403,
    ),
  );
  const client = server.client();
  const state = observer(client);
  await state.until((value) => value.statuses.includes("incompatible"));
  assert.ok(state.state.errors.some((error) => error instanceof ApiRequestError && error.status === 403));
  const requests = server.requests.length;
  await delay(1_100);
  assert.equal(server.requests.length, requests);
  await assert.rejects(client.getStatus(), ApiRequestError);
});

test("close with active work rejects reads and prevents every subsequent socket callback", async (t) => {
  const server = await serverFixture(t);
  const client = server.client();
  const state = observer(client);
  await state.until((value) => value.snapshots.length === 1);
  const release = deferred();
  t.after(() => release.resolve());
  server.routes.set("GET /api/v1/logs", async ({ reply }) => {
    await release.promise;
    reply({ node: server.current.node, entries: [log(1)] });
  });
  const request = client.getLogs();
  const rejected = assert.rejects(request);
  await server.waitRequest("GET", "/api/v1/logs");
  client.close();
  server.broadcast({ type: "snapshot", snapshot: snapshot("2") });
  server.broadcast({ type: "log", entry: log(2) });
  release.resolve();
  await rejected;
  await server.until(() => server.sockets.size === 0);
  assert.equal(state.state.snapshots.length, 1);
  assert.equal(state.state.liveLogs.length, 0);
});

test("reconnect delays grow 1/2/4/8/10 seconds and cap, then last-demand removal cancels them", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const server = await serverFixture(t);
  server.onConnection = (peer) => peer.close(1013, "Refresh the event stream");
  const state = observer(server.client());
  await state.until((value) => value.errors.length === 1);
  let connections = 1;
  for (const wait of [1_000, 2_000, 4_000, 8_000, 10_000, 10_000]) {
    t.mock.timers.tick(wait - 1);
    await scheduler.yield();
    assert.equal(server.connectionCount, connections, "reconnect must not start before its deadline");
    t.mock.timers.tick(1);
    connections += 1;
    await server.until(() => server.connectionCount === connections);
    await state.until((value) => value.errors.length === connections);
  }
  state.unsubscribe();
  t.mock.timers.tick(100_000);
  await scheduler.yield();
  assert.equal(server.connectionCount, connections);
});

test("standalone HTTP epoch changes invalidate outstanding old-generation responses", async (t) => {
  const server = await serverFixture(t);
  const client = server.client();
  await client.getSnapshot();
  const release = deferred();
  t.after(() => release.resolve());
  server.routes.set(`GET ${DEVICE_ROUTE}`, async ({ response }) => {
    await release.promise;
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end("obsolete malformed {response");
  });
  const oldRequest = client.getDevice("instrument");
  const rejected = assert.rejects(oldRequest, StaleResponseError);
  await server.waitRequest("GET", DEVICE_ROUTE);
  server.current = snapshot("0", INSTANCE_B, 9);
  assert.equal((await client.getSnapshot()).node.instanceId, INSTANCE_B);
  release.resolve();
  await rejected;
  assert.equal(
    (await client.getStatus()).node.instanceId,
    INSTANCE_B,
    "invalid obsolete data must not poison the current client",
  );
  assert.equal(server.connectionCount, 0);
});
test("oversized event frames permanently stop the event stream", async (t) => {
  const server = await serverFixture(t);
  server.onConnection = (peer) => peer.send(" ".repeat(1_048_577));
  const state = observer(server.client());
  await state.until((value) => value.statuses.includes("incompatible"));
  assert.ok(state.state.errors.some((error) => error instanceof ProtocolValidationError));
  assert.equal(server.connectionCount, 1);
});

test("protocol-invalid websocket close codes permanently stop reconnect", async (t) => {
  for (const code of [1002, 1003, 1007, 1008, 1009]) {
    const server = await serverFixture(t);
    server.onConnection = (peer) => {
      peer.send(JSON.stringify({ type: "snapshot", snapshot: server.current }));
      peer.close(code, "protocol rejection");
    };
    const state = observer(server.client());
    await state.until((value) => value.statuses.includes("incompatible"));
    assert.equal(server.connectionCount, 1, `close code ${code}`);
    assert.ok(state.state.errors.length > 0, `close code ${code}`);
  }
});
test("a device response is stale when a newer snapshot has removed that device", async (t) => {
  const server = await serverFixture(t);
  const client = server.client();
  await client.getSnapshot();
  const release = deferred();
  t.after(() => release.resolve());
  server.routes.set(`GET ${DEVICE_ROUTE}`, async ({ response }) => {
    await release.promise;
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ node: snapshot().node, revision: "0", device: snapshot().devices[0] }));
  });
  const delayedDevice = client.getDevice("instrument");
  const rejected = assert.rejects(delayedDevice, StaleResponseError);
  await server.waitRequest("GET", DEVICE_ROUTE);
  server.current = { ...snapshot("1"), devices: [] };
  await client.getSnapshot();
  release.resolve();
  await rejected;
});

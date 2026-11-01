import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdir, mkdtemp, writeFile, cp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "node:net";
import { request } from "node:http";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { createClient } from "../packages/client/dist/index.js";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const execute = promisify(execFile);
const { stdout: rustc } = await execute("rustc", ["-vV"]);
const host = rustc.match(/^host: (.+)$/m)?.[1];
const { stdout: metadata } = await execute("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"], {
  cwd: root,
});
const binaries = join(JSON.parse(metadata).target_directory, host, "debug");
const temporary = await mkdtemp(join(tmpdir(), "synapsed-no-package-smoke-"));
const plugins = join(temporary, "drivers");
const config = join(temporary, "config.toml");
await mkdir(plugins);
const reservation = createServer();
await new Promise((accept, reject) => {
  reservation.once("error", reject);
  reservation.listen(0, "127.0.0.1", accept);
});
const port = reservation.address().port;
await new Promise((accept) => reservation.close(accept));
const endpoint = `http://127.0.0.1:${port}`;
let daemon;
let exited;
let stderr = "";
const cli = (...args) => execute(join(binaries, "synapsectl"), ["--endpoint", endpoint, ...args], { timeout: 10_000 });
async function configure(extra = "") {
  await writeFile(
    config,
    `[server]\nlisten = "127.0.0.1:${port}"\nstate_dir = ${JSON.stringify(join(temporary, "state"))}\n[plugins]\ndirectory = ${JSON.stringify(plugins)}\n${extra}`,
  );
}
async function start() {
  stderr = "";
  daemon = spawn(join(binaries, "synapsed"), ["--config", config], { stdio: ["ignore", "pipe", "pipe"] });
  daemon.stdout.resume();
  daemon.stderr.on("data", (data) => {
    stderr = (stderr + data.toString()).slice(-32_768);
  });
  exited = new Promise((accept, reject) => {
    daemon.once("error", reject);
    daemon.once("exit", (code) => accept(code));
  });
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    if (daemon.exitCode !== null) throw new Error(`Daemon exited during startup: ${stderr}`);
    try {
      const response = await fetch(`${endpoint}/api/v1/status`, { signal: AbortSignal.timeout(500) });
      if (response.ok) return;
    } catch {
      /* Wait for this real process's listener, not a fabricated state. */
    }
    await delay(25);
  }
  throw new Error(`Daemon did not become ready: ${stderr}`);
}
async function stop() {
  if (!daemon || daemon.exitCode !== null) return;
  daemon.kill("SIGTERM");
  const code = await Promise.race([
    exited,
    delay(6_000).then(() => {
      throw new Error("Daemon shutdown exceeded its completion bound.");
    }),
  ]);
  assert.equal(code, 0, stderr);
  daemon = undefined;
}
async function firstSubscriptionSnapshot(client) {
  let remove;
  try {
    return await new Promise((accept, reject) => {
      const timer = setTimeout(() => reject(new Error("No immediate actual WS snapshot.")), 5_000);
      remove = client.subscribe({
        onSnapshot(snapshot) {
          clearTimeout(timer);
          accept(snapshot);
        },
        onStatus() {},
        onLogs() {},
        onLog() {},
        onError(error) {
          clearTimeout(timer);
          reject(error);
        },
      });
    });
  } finally {
    remove?.();
  }
}
async function watchInitial() {
  const child = spawn(join(binaries, "synapsectl"), ["--endpoint", endpoint, "--json", "watch"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stderr.resume();
  const end = new Promise((accept, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => accept(code));
  });
  try {
    const snapshot = await new Promise((accept, reject) => {
      let text = "";
      const timer = setTimeout(() => reject(new Error("CLI watch did not print its initial snapshot.")), 5_000);
      child.stdout.on("data", (data) => {
        text += data;
        const newline = text.indexOf("\n");
        if (newline !== -1) {
          clearTimeout(timer);
          try {
            accept(JSON.parse(text.slice(0, newline)));
          } catch (error) {
            reject(error);
          }
        }
      });
      child.once("exit", () => {
        clearTimeout(timer);
        reject(new Error("CLI watch exited before initial snapshot."));
      });
    });
    assert.deepEqual(snapshot.devices, []);
    child.kill("SIGINT");
    assert.equal(await end, 0);
  } finally {
    if (child.exitCode === null) child.kill("SIGKILL");
  }
}
try {
  await configure();
  await start();
  const first = JSON.parse((await cli("--json", "status")).stdout);
  const snapshot = JSON.parse((await cli("--json", "devices")).stdout);
  assert.deepEqual(first.drivers, []);
  assert.deepEqual(snapshot.devices, []);
  const client = createClient({ baseUrl: endpoint });
  try {
    assert.equal((await firstSubscriptionSnapshot(client)).node.instanceId, first.node.instanceId);
    const inventory = await client.getInventory();
    assert.ok(inventory.nodes.some((node) => node.kind === "host"));
    assert.equal(inventory.sources.find((source) => source.kind === "ddc")?.state, "not_scanned");
    const cliInventory = JSON.parse((await cli("--json", "inventory")).stdout);
    assert.ok(cliInventory.nodes.some((node) => node.kind === "host"));
  } finally {
    client.close();
  }
  await watchInitial();
  for (const origin of ["https://untrusted.example", "null"]) {
    assert.equal((await fetch(`${endpoint}/api/v1/devices`, { headers: { Origin: origin } })).status, 403);
  }
  const hostStatus = await new Promise((accept, reject) => {
    const outgoing = request(`${endpoint}/api/v1/devices`, { headers: { Host: "untrusted.example" } }, (response) => {
      response.resume();
      accept(response.statusCode);
    });
    outgoing.once("error", reject);
    outgoing.setTimeout(5_000, () => outgoing.destroy(new Error("Host-policy request timed out.")));
    outgoing.end();
  });
  assert.equal(hostStatus, 403);
  await assert.rejects(cli("get", "missing", "missing"), (error) => error.code === 1);
  await stop();
  await start();
  const second = JSON.parse((await cli("--json", "status")).stdout);
  assert.equal(second.node.nodeId, first.node.nodeId);
  assert.notEqual(second.node.instanceId, first.node.instanceId);
  await stop();
  for (const name of ["generic-ddc", "benq-rd280u"])
    await cp(join(root, "drivers", name), join(plugins, name), { recursive: true });
  await configure('disabled = ["generic-ddc", "benq-rd280u"]\n');
  await start();
  const disabled = JSON.parse((await cli("--json", "status")).stdout);
  assert.equal(disabled.drivers.length, 2);
  assert.ok(disabled.drivers.every((driver) => driver.state === "disabled"));
  assert.deepEqual(JSON.parse((await cli("--json", "devices")).stdout).devices, []);
  await stop();
  for (const invalid of [
    '[server]\nlisten = "0.0.0.0:24816"\n',
    'token = "unsupported"\n',
    '[monitor]\nprofile = "unsupported"\n',
  ]) {
    await writeFile(config, invalid);
    await assert.rejects(
      execute(join(binaries, "synapsed"), ["--config", config], { timeout: 6_000 }),
      (error) => error.code === 1,
    );
  }
  console.log(
    JSON.stringify({
      smoke: "passed",
      nodeStableAcrossRestart: true,
      instanceChanged: true,
      immediateWsSnapshot: true,
      cliWatch: true,
      disabledDrivers: 2,
      originHostDenied: true,
      obsoleteConfigRejected: true,
      inventory: true,
    }),
  );
} finally {
  if (daemon?.exitCode === null) {
    daemon.kill("SIGKILL");
    await exited.catch(() => {});
  }
  await rm(temporary, { recursive: true, force: true });
}

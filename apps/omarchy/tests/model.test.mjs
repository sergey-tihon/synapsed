import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../Model.js", import.meta.url), "utf8");
const model = {};
vm.runInNewContext(source, model, { filename: "Model.js" });

const characteristic = (kind, options = {}) => ({
  access: { write: true },
  spec: { kind, step: null, options: [], ...options },
});

test("controlFor maps supported characteristic metadata", () => {
  assert.equal(model.controlFor(characteristic("number", { minimum: 0, maximum: 100, step: 1 })), "slider");
  assert.equal(model.controlFor(characteristic("action")), "invoke");
  assert.equal(model.controlFor(characteristic("boolean")), "toggle");
  assert.equal(model.controlFor(characteristic("enum", { options: [{}, {}] })), "choice");
  assert.equal(model.controlFor(characteristic("enum", { options: [{}, {}, {}, {}, {}, {}] })), "stepper");
  assert.equal(model.controlFor(characteristic("number", { minimum: 0, maximum: 100 })), "slider");
  assert.equal(model.controlFor(characteristic("number", { step: 0.5 })), "stepper");
  assert.equal(model.controlFor(characteristic("number")), "web");
  assert.equal(model.controlFor(characteristic("string")), "web");
  assert.equal(model.controlFor({ ...characteristic("number"), access: { write: false } }), "readonly");
});

test("normalizeEndpoint accepts only supported loopback endpoints", () => {
  for (const endpoint of [
    "http://127.0.0.1:24816",
    "http://127.0.0.1:24816/",
    "http://localhost:24816",
    "http://localhost:24816/",
    "http://[::1]:24816",
    "http://[::1]:24816/",
  ]) {
    assert.equal(model.normalizeEndpoint(endpoint), endpoint.replace(/\/$/, ""));
  }
  for (const endpoint of [
    "https://127.0.0.1:24816",
    "http://example.com:24816",
    "http://user@127.0.0.1:24816",
    "http://127.0.0.1:24816/api",
    "http://127.0.0.1:0",
    "http://127.0.0.1:65536",
    "http://127.0.0.1:24816/?q=x",
    "http://127.0.0.1:24816/#fragment",
  ]) {
    assert.equal(model.normalizeEndpoint(endpoint), null);
  }
});

test("webUrl emits an encoded hardware control route", () => {
  assert.equal(
    model.webUrl("http://127.0.0.1:24816", "a&b", "c#d"),
    "http://127.0.0.1:24816/#/devices/a%26b?characteristic=c%23d",
  );
  assert.equal(model.webUrl("http://127.0.0.1:24816", "device", null), "http://127.0.0.1:24816/");
  assert.equal(model.webUrl("http://127.0.0.1:24816", null, null), "http://127.0.0.1:24816/");
  assert.equal(model.webUrl("http://127.0.0.1:24816", "", ""), "http://127.0.0.1:24816/");
});

test("setArgument and stepArgument preserve typed bounded commands", () => {
  assert.equal(model.setArgument({ minimum: 0, maximum: 100, integer: true }, 100.6), "100");
  assert.equal(model.setArgument({ minimum: 0, maximum: 1, integer: false }, 0.25), "0.25");
  assert.equal(model.stepArgument(250), 100);
  assert.equal(model.stepArgument(-3), -3);
});

test("commandArgs and watchArgs use fixed synapsectl argv", () => {
  const endpoint = "http://127.0.0.1:24816";
  assert.deepEqual(Array.from(model.commandArgs(endpoint, "step", "monitor", "brightness", -1)), [
    "synapsectl",
    "--endpoint",
    endpoint,
    "--json",
    "step",
    "monitor",
    "brightness",
    "-1",
  ]);
  assert.deepEqual(Array.from(model.commandArgs(endpoint, "toggle", "monitor", "power")), [
    "synapsectl",
    "--endpoint",
    endpoint,
    "--json",
    "toggle",
    "monitor",
    "power",
  ]);
  assert.deepEqual(Array.from(model.watchArgs(endpoint)), ["synapsectl", "--endpoint", endpoint, "--json", "watch"]);
});

test("parseSnapshot accepts only the expected snapshot envelope", () => {
  assert.equal(model.parseSnapshot("{}"), null);
  const snapshot = { apiVersion: 1, node: { name: "host" }, drivers: [], devices: [] };
  assert.equal(JSON.stringify(model.parseSnapshot(JSON.stringify(snapshot))), JSON.stringify(snapshot));
  assert.equal(model.parseSnapshot("not json"), null);
});

test("characteristicSections preserves declared groups and characteristic order", () => {
  const first = { id: "a", group: "general" };
  const middle = { id: "b", group: "sensors" };
  const second = { id: "c", group: "general" };
  const sections = model.characteristicSections({
    groups: [
      { id: "general", label: "General" },
      { id: "empty", label: "Empty" },
      { id: "sensors", label: "Sensors" },
    ],
    characteristics: [first, middle, second],
  });
  assert.deepEqual(JSON.parse(JSON.stringify(sections)), [
    { label: "General", characteristics: [first, second] },
    { label: "Sensors", characteristics: [middle] },
  ]);
});

test("characteristicSections keeps ungrouped and older snapshots in one null section", () => {
  const characteristics = [{ id: "a" }, { id: "b" }];
  assert.deepEqual(JSON.parse(JSON.stringify(model.characteristicSections({ groups: [], characteristics }))), [
    { label: null, characteristics },
  ]);
  assert.deepEqual(JSON.parse(JSON.stringify(model.characteristicSections({ characteristics: [] }))), [
    { label: null, characteristics: [] },
  ]);
});

test("characteristicSections puts unmatched characteristics in a trailing null section", () => {
  const grouped = { id: "a", group: "known" };
  const unmatched = { id: "b", group: "missing" };
  assert.deepEqual(
    JSON.parse(
      JSON.stringify(
        model.characteristicSections({
          groups: [{ id: "known", label: "Known" }],
          characteristics: [unmatched, grouped],
        }),
      ),
    ),
    [
      { label: "Known", characteristics: [grouped] },
      { label: null, characteristics: [unmatched] },
    ],
  );
});

test("supportsStep and formatValue follow characteristic metadata and observations", () => {
  assert.equal(model.supportsStep(characteristic("number", { step: 0.5 })), true);
  assert.equal(model.supportsStep(characteristic("number")), false);
  assert.equal(model.supportsStep(characteristic("enum")), true);
  assert.equal(model.supportsStep({ ...characteristic("enum"), access: { write: false } }), false);
  assert.equal(model.formatValue({ spec: { kind: "action" }, value: null, status: "ok" }), "Action");
  assert.equal(model.formatValue({ spec: { kind: "boolean" }, value: true, status: "ok" }), "On");
  assert.equal(
    model.formatValue({
      spec: { kind: "enum", options: [{ value: "x", label: "Example" }] },
      value: "x",
      status: "ok",
    }),
    "Example",
  );
  assert.equal(
    model.formatValue({ spec: { kind: "number", unit: "%" }, value: 42, status: "unavailable" }),
    "42 % · unavailable",
  );
  assert.equal(model.formatValue({ spec: { kind: "string" }, value: "", status: "ok" }), "Empty string");
});

test("problemDrivers and tooltip expose connection and driver problems", () => {
  const snapshot = {
    devices: [{}, {}],
    drivers: [
      { id: "generic-ddc", state: "online" },
      { id: "benq-rd280u", state: "ambiguous", error: { message: "two displays" } },
      { id: "other", state: "permission_denied" },
    ],
  };
  assert.deepEqual(Array.from(model.problemDrivers(snapshot)), [
    "benq-rd280u: ambiguous — two displays",
    "other: permission_denied",
  ]);
  assert.equal(
    model.tooltip("online", snapshot, model.problemDrivers(snapshot)),
    "Synapsed — Online · 2 devices · 2 driver errors",
  );
  assert.equal(model.tooltip("offline", snapshot, []), "Synapsed — Offline (stale)");
  assert.equal(model.tooltip("failed", null, []), "Synapsed — synapsectl unavailable");
});

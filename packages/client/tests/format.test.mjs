import assert from "node:assert/strict";
import { test } from "node:test";
import { describeSpec, enumOptionLabel, formatCharacteristicValue, statusSuffix } from "../dist/index.js";

const characteristic = (spec, value, status = "ok", error = null) => ({
  id: "sample",
  label: "Sample",
  spec,
  value,
  status,
  error,
  access: { read: true, write: true, notify: false },
});

test("formats all characteristic value kinds consistently", () => {
  assert.equal(formatCharacteristicValue(characteristic({ kind: "action" }, null)), "Action");
  assert.equal(
    formatCharacteristicValue(
      characteristic({ kind: "number", integer: false, minimum: 0, maximum: 100, step: 1, unit: "%" }, 42),
    ),
    "42 %",
  );
  assert.equal(formatCharacteristicValue(characteristic({ kind: "boolean" }, true)), "On");
  assert.equal(formatCharacteristicValue(characteristic({ kind: "boolean" }, false)), "Off");
  assert.equal(
    formatCharacteristicValue(characteristic({ kind: "enum", options: [{ label: "Warm", value: "warm" }] }, "warm")),
    "Warm",
  );
  assert.equal(
    formatCharacteristicValue(characteristic({ kind: "enum", options: [{ label: "Warm", value: "warm" }] }, "custom")),
    "custom",
  );
  assert.equal(formatCharacteristicValue(characteristic({ kind: "string", maxLength: 20 }, "")), "Empty string");
  assert.equal(formatCharacteristicValue(characteristic({ kind: "string", maxLength: 20 }, null)), "Not yet observed");
});

test("describes bounds, units, steps and enum labels", () => {
  assert.equal(
    describeSpec({ kind: "number", integer: true, minimum: 0, maximum: 100, step: 1, unit: "%" }),
    "0–100 %, step 1",
  );
  assert.equal(enumOptionLabel({ kind: "enum", options: [{ label: "Warm", value: "warm" }] }, "warm"), "Warm");
  assert.equal(describeSpec({ kind: "string", maxLength: 20 }), "Up to 20 characters");
});

test("returns a status suffix only for a non-healthy characteristic", () => {
  assert.equal(statusSuffix(characteristic({ kind: "boolean" }, false)), "");
  assert.equal(statusSuffix(characteristic({ kind: "boolean" }, null, "unavailable")), "Unavailable");
  assert.equal(
    statusSuffix(characteristic({ kind: "boolean" }, null, "error", { code: "ddc_error", message: "Read failed" })),
    "Read failed",
  );
});

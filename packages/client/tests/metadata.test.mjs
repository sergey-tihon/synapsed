import assert from "node:assert/strict";
import { test } from "node:test";
import {
  CharacteristicValueError,
  defaultCommand,
  isCommandApplicable,
  parseCharacteristicValue,
  supportsStep,
  valueConformsToSpec,
  validateCharacteristic,
} from "../dist/index.js";
import { characteristic } from "./server.mjs";

function declared(spec, writable = true, value = null) {
  return validateCharacteristic(
    characteristic({
      spec,
      value,
      status: value === null ? "unavailable" : "ok",
      access: { read: spec.kind !== "action", write: writable, notify: spec.kind !== "action" },
    }),
  );
}
const fractional = { kind: "number", minimum: -5, maximum: 5, step: 0.5, integer: false, unit: "cm" };
const integer = { kind: "number", minimum: 0, maximum: 10, step: 1, integer: true, unit: null };
const enumeration = {
  kind: "enum",
  options: [
    { value: "warm", label: "Warm" },
    { value: "cool", label: "Cool" },
  ],
};

test("numeric parsing accepts finite whole decimal arguments, fractional metadata and exact bounds", () => {
  for (const [input, value] of [
    ["1.25", 1.25],
    ["-4.5", -4.5],
    [" .5 ", 0.5],
    ["+2e-1", 0.2],
    ["5.", 5],
    ["-5", -5],
    ["0", 0],
  ]) {
    assert.equal(parseCharacteristicValue(fractional, input), value);
  }
  for (const input of [
    "",
    " ",
    "1.5junk",
    "1 2",
    "0x2",
    "Infinity",
    "NaN",
    "1e999",
    "-5.01",
    "5.01",
    '{"value":1}',
    "1,5",
    "1e",
    "--1",
  ]) {
    assert.throws(() => parseCharacteristicValue(fractional, input), CharacteristicValueError, input);
  }
  assert.equal(parseCharacteristicValue(integer, "2e0"), 2);
  assert.throws(() => parseCharacteristicValue(integer, "2.25"), CharacteristicValueError);
  assert.equal(
    valueConformsToSpec(fractional, 0.25),
    true,
    "step is a relative increment, not an invented set-value grid",
  );
  assert.equal(valueConformsToSpec(fractional, Infinity), false);
  assert.equal(valueConformsToSpec(fractional, NaN), false);
  assert.equal(valueConformsToSpec(integer, 1.5), false);
});

test("booleans and enums are exact while strings preserve literal whitespace, emptiness and Unicode", () => {
  const boolean = { kind: "boolean" };
  assert.equal(parseCharacteristicValue(boolean, "true"), true);
  assert.equal(parseCharacteristicValue(boolean, "false"), false);
  for (const input of ["TRUE", "False", "1", " true", "false ", ""]) {
    assert.throws(() => parseCharacteristicValue(boolean, input), CharacteristicValueError);
  }
  assert.equal(parseCharacteristicValue(enumeration, "warm"), "warm");
  for (const input of ["Warm", "warm ", "0", ""]) {
    assert.throws(() => parseCharacteristicValue(enumeration, input), CharacteristicValueError);
  }
  const string = { kind: "string", maxLength: 3 };
  for (const input of ["", " a ", "😀😀😀", "e\u0301", "{x}"]) {
    assert.equal(parseCharacteristicValue(string, input), input);
  }
  assert.throws(() => parseCharacteristicValue(string, "😀😀😀😀"), CharacteristicValueError);
  assert.equal(parseCharacteristicValue({ kind: "string", maxLength: 0 }, ""), "");
  assert.throws(() => parseCharacteristicValue({ kind: "action" }, ""), CharacteristicValueError);
});

test("command applicability respects read-only, missing-step and no-argument action metadata", () => {
  const stepped = declared(fractional);
  const readOnly = declared(fractional, false);
  const withoutStep = declared({ ...fractional, step: null });
  const boolean = declared({ kind: "boolean" });
  const enumValue = declared(enumeration);
  const string = declared({ kind: "string", maxLength: 10 });
  const action = declared({ kind: "action" });
  assert.equal(supportsStep(stepped), true);
  assert.equal(supportsStep(enumValue), true);
  assert.equal(supportsStep(readOnly), false);
  assert.equal(supportsStep(withoutStep), false);
  assert.equal(isCommandApplicable(stepped, { op: "step", steps: -100 }), true);
  assert.equal(isCommandApplicable(stepped, { op: "step", steps: 0 }), true);
  assert.equal(isCommandApplicable(enumValue, { op: "step", steps: 100 }), true);
  assert.equal(isCommandApplicable(readOnly, { op: "set", value: 1 }), false);
  assert.equal(isCommandApplicable(readOnly, { op: "step", steps: 1 }), false);
  assert.equal(isCommandApplicable(withoutStep, { op: "step", steps: 1 }), false);
  assert.equal(isCommandApplicable(withoutStep, { op: "set", value: 1.25 }), true);
  assert.equal(isCommandApplicable(boolean, { op: "toggle" }), true);
  assert.equal(isCommandApplicable(boolean, { op: "set", value: true }), true);
  assert.equal(isCommandApplicable(boolean, { op: "set", value: "true" }), false);
  assert.equal(isCommandApplicable(enumValue, { op: "set", value: "cool" }), true);
  assert.equal(isCommandApplicable(enumValue, { op: "set", value: "Cool" }), false);
  assert.equal(isCommandApplicable(string, { op: "set", value: "" }), true);
  assert.equal(isCommandApplicable(action, { op: "invoke" }), true);
  assert.equal(isCommandApplicable(action, { op: "set", value: "" }), false);
  assert.equal(isCommandApplicable(stepped, { op: "invoke" }), false);
  assert.equal(isCommandApplicable(stepped, { op: "toggle" }), false);
  assert.deepEqual(defaultCommand(stepped), { op: "step", steps: 1 });
  assert.deepEqual(defaultCommand(enumValue), { op: "step", steps: 1 });
  assert.deepEqual(defaultCommand(boolean), { op: "toggle" });
  assert.deepEqual(defaultCommand(action), { op: "invoke" });
  assert.equal(defaultCommand(readOnly), null);
  assert.equal(defaultCommand(string), null);
  assert.equal(defaultCommand(withoutStep), null);
});

test("saved command validation rejects unknown fields, partial types and out-of-range signed steps", () => {
  const numeric = declared(fractional);
  for (const command of [
    null,
    {},
    { op: "step" },
    { op: "step", steps: "1" },
    { op: "step", steps: 0.5 },
    { op: "step", steps: -101 },
    { op: "step", steps: 101 },
    { op: "step", steps: Infinity },
    { op: "step", steps: NaN },
    { op: "step", steps: 1, value: 2 },
    { op: "set", value: {} },
    { op: "set", value: Infinity },
    { op: "set", value: 1, unrelated: true },
    { op: "toggle", steps: 1 },
    { op: "invoke", value: null },
    { op: "unknown" },
  ]) {
    assert.equal(isCommandApplicable(numeric, command), false);
  }
  assert.equal(isCommandApplicable(declared({ kind: "boolean" }), { op: "toggle", extra: true }), false);
  assert.equal(isCommandApplicable(declared({ kind: "action" }), { op: "invoke", extra: true }), false);
});

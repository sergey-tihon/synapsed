import test from "node:test";
import assert from "node:assert/strict";
import { bindingKey, dispatchPrecondition } from "../src/model.ts";

test("bindingKey distinguishes and safely encodes pairs", () => {
  assert.equal(bindingKey("a", "b"), '["a","b"]');
  assert.notEqual(bindingKey("a:b", "c"), bindingKey("a", "b:c"));
});

test("dispatch preconditions require every current-state guard", () => {
  const valid = {
    live: true,
    hasClient: true,
    hasSnapshot: true,
    connected: true,
    deviceOnline: true,
    characteristicAvailable: true,
    commandApplicable: true,
    notBusy: true,
  };
  assert.equal(dispatchPrecondition(valid), true);
  for (const key of Object.keys(valid)) {
    assert.equal(dispatchPrecondition({ ...valid, [key]: false }), false, key);
  }
});

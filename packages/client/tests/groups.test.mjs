import assert from "node:assert/strict";
import { test } from "node:test";
import { characteristicSections } from "../dist/index.js";

const characteristic = (id, group) => ({ id, group });

test("returns one null section for ungrouped characteristics", () => {
  const characteristics = [characteristic("one", null), characteristic("two", null)];
  assert.deepEqual(characteristicSections([], characteristics), [{ group: null, characteristics }]);
  assert.deepEqual(characteristicSections([], []), [{ group: null, characteristics: [] }]);
});

test("keeps declared group order and characteristic declaration order", () => {
  const groups = [
    { id: "second", label: "Second" },
    { id: "first", label: "First" },
  ];
  const characteristics = [characteristic("a", "first"), characteristic("b", "second"), characteristic("c", "first")];
  assert.deepEqual(characteristicSections(groups, characteristics), [
    { group: groups[0], characteristics: [characteristics[1]] },
    { group: groups[1], characteristics: [characteristics[0], characteristics[2]] },
  ]);
});

test("omits declared groups with no characteristics", () => {
  const groups = [
    { id: "used", label: "Used" },
    { id: "empty", label: "Empty" },
  ];
  const item = characteristic("one", "used");
  assert.deepEqual(characteristicSections(groups, [item]), [{ group: groups[0], characteristics: [item] }]);
});

test("appends unmatched characteristics in a trailing null section", () => {
  const groups = [{ id: "declared", label: "Declared" }];
  const matched = characteristic("matched", "declared");
  const unmatched = characteristic("unmatched", "missing");
  assert.deepEqual(characteristicSections(groups, [matched, unmatched]), [
    { group: groups[0], characteristics: [matched] },
    { group: null, characteristics: [unmatched] },
  ]);
});

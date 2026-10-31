import type {
  Characteristic,
  CharacteristicCommand,
  CharacteristicSpec,
  CharacteristicValue,
} from "./generated/index.js";
import { isCharacteristicCommand } from "./generated/index.js";

/** A user-facing validation error that never includes the supplied value. */
export class CharacteristicValueError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CharacteristicValueError";
  }
}

/** Unicode scalar count, matching the wire contract rather than UTF-16 length. */
function stringLength(value: string): number {
  let length = 0;
  for (const _ of value) length += 1;
  return length;
}

/** Checks the declared value type and bounds; a step is not a set-value grid. */
export function valueConformsToSpec(spec: CharacteristicSpec, value: CharacteristicValue): boolean {
  switch (spec.kind) {
    case "number":
      return (
        typeof value === "number" &&
        Number.isFinite(value) &&
        (!spec.integer || Number.isInteger(value)) &&
        (spec.minimum === null || value >= spec.minimum) &&
        (spec.maximum === null || value <= spec.maximum)
      );
    case "boolean":
      return typeof value === "boolean";
    case "enum":
      return typeof value === "string" && spec.options.some((option) => option.value === value);
    case "string":
      return typeof value === "string" && stringLength(value) <= spec.maxLength;
    case "action":
      return false;
  }
}

/**
 * Parses a complete decimal number, exact boolean/enum, or literal string.
 * Strings are never trimmed; the empty string is a valid explicitly supplied value.
 * This does not grant write access: check isCommandApplicable before dispatch.
 */
export function parseCharacteristicValue(spec: CharacteristicSpec, text: string): CharacteristicValue {
  let value: CharacteristicValue;
  switch (spec.kind) {
    case "number": {
      const numberText = text.trim();
      if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(numberText)) {
        throw new CharacteristicValueError("Enter a complete decimal number.");
      }
      value = Number(numberText);
      break;
    }
    case "boolean":
      if (text !== "true" && text !== "false") {
        throw new CharacteristicValueError("Enter exactly true or false.");
      }
      value = text === "true";
      break;
    case "enum":
    case "string":
      value = text;
      break;
    case "action":
      throw new CharacteristicValueError("Actions accept invoke, not a value.");
  }
  if (!valueConformsToSpec(spec, value)) {
    throw new CharacteristicValueError("The value does not match the characteristic's declared type or bounds.");
  }
  return value;
}

/** Whether relative step commands are declared; no default numeric step is invented. */
export function supportsStep(characteristic: Characteristic): boolean {
  return (
    characteristic.access.write &&
    (characteristic.spec.kind === "enum" ||
      (characteristic.spec.kind === "number" && characteristic.spec.step !== null))
  );
}

/** Full saved-command shape/type validation plus metadata applicability; availability remains a UI/server check. */
export function isCommandApplicable(
  characteristic: Characteristic,
  command: unknown,
): command is CharacteristicCommand {
  if (!characteristic.access.write || !isCharacteristicCommand(command)) return false;
  switch (command.op) {
    case "set":
      return valueConformsToSpec(characteristic.spec, command.value);
    case "step":
      return supportsStep(characteristic);
    case "toggle":
      return characteristic.spec.kind === "boolean";
    case "invoke":
      return characteristic.spec.kind === "action";
  }
}

/** Key/press default; null means display-only, not an implicit set operation. */
export function defaultCommand(characteristic: Characteristic): CharacteristicCommand | null {
  if (!characteristic.access.write) return null;
  if (supportsStep(characteristic)) return { op: "step", steps: 1 };
  if (characteristic.spec.kind === "boolean") return { op: "toggle" };
  if (characteristic.spec.kind === "action") return { op: "invoke" };
  return null;
}

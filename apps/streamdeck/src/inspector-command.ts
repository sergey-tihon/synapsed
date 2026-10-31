import {
  isCommandApplicable,
  parseCharacteristicValue,
  type CharacteristicCommand,
  type Snapshot,
} from "@synapsed/client";
import { resolveBinding } from "./render.js";
import { isBinding } from "./settings.js";

export type ValidationResult = { command: CharacteristicCommand | null; error: string | null };

export function validateInspectorCommand(
  snapshot: Snapshot | undefined,
  binding: unknown,
  op: unknown,
  text?: unknown,
): ValidationResult {
  let command: CharacteristicCommand | null = null;
  try {
    if (!isBinding(binding)) throw new Error("Select a device and characteristic first.");
    const resolved = resolveBinding(snapshot, binding);
    if (!resolved) throw new Error("The selected characteristic is missing.");
    const characteristic = resolved.characteristic;
    switch (op) {
      case "default":
        break;
      case "set":
        if (typeof text !== "string" || text.length > 4096) throw new Error("Enter a bounded characteristic value.");
        command = { op: "set", value: parseCharacteristicValue(characteristic.spec, text) };
        break;
      case "step":
        if (typeof text !== "string" || !/^[+-]?\d+$/.test(text))
          throw new Error("Enter a whole step count from -100 to 100.");
        command = { op: "step", steps: Number(text) };
        break;
      case "toggle":
        command = { op: "toggle" };
        break;
      case "invoke":
        command = { op: "invoke" };
        break;
      default:
        throw new Error("Select a supported operation.");
    }
    if (command && !isCommandApplicable(characteristic, command))
      throw new Error("The command is not supported by current metadata.");
    return { command, error: null };
  } catch (failure) {
    return {
      command: null,
      error: failure instanceof Error ? failure.message : "Invalid characteristic command.",
    };
  }
}

import {
  CharacteristicValueError,
  parseCharacteristicValue,
  type CharacteristicCommand,
  type CharacteristicSpec,
} from "@synapsed/client";
import type { InspectorCharacteristic } from "./catalog.js";

export type CommandMode = "toggle" | "on" | "off" | "invoke" | "increase" | "decrease" | "set";

export function commandModes(characteristic: InspectorCharacteristic): CommandMode[] {
  if (!characteristic.access.write) return [];
  switch (characteristic.spec.kind) {
    case "boolean":
      return ["toggle", "on", "off"];
    case "action":
      return ["invoke"];
    case "enum":
      return ["increase", "decrease", "set"];
    case "number":
      return characteristic.spec.step !== null ? ["increase", "decrease", "set"] : ["set"];
    case "string":
      return ["set"];
  }
}

export function modeLabel(characteristic: InspectorCharacteristic, mode: CommandMode): string {
  switch (characteristic.spec.kind) {
    case "boolean":
      return mode === "toggle" ? "Toggle" : mode === "on" ? "Turn on" : "Turn off";
    case "action":
      return "Run";
    case "enum":
      return mode === "increase" ? "Next option" : mode === "decrease" ? "Previous option" : "Set to";
    case "number":
      return mode === "increase" ? "Increase" : mode === "decrease" ? "Decrease" : "Set to";
    case "string":
      return "Set to";
  }
}

export function modeOf(command: CharacteristicCommand | undefined): CommandMode | undefined {
  if (!command) return undefined;
  switch (command.op) {
    case "toggle":
      return "toggle";
    case "invoke":
      return "invoke";
    case "step":
      return command.steps > 0 ? "increase" : command.steps < 0 ? "decrease" : undefined;
    case "set":
      if (typeof command.value === "boolean") return command.value ? "on" : "off";
      return "set";
  }
}

export function commandForMode(
  characteristic: InspectorCharacteristic,
  mode: CommandMode,
  previous?: CharacteristicCommand,
): CharacteristicCommand | undefined {
  switch (mode) {
    case "toggle":
      return { op: "toggle" };
    case "invoke":
      return { op: "invoke" };
    case "on":
      return { op: "set", value: true };
    case "off":
      return { op: "set", value: false };
    case "increase":
    case "decrease": {
      const magnitude = previous?.op === "step" ? Math.abs(previous.steps) : 1;
      return { op: "step", steps: mode === "increase" ? magnitude : -magnitude };
    }
    case "set":
      switch (characteristic.spec.kind) {
        case "enum": {
          const observed = characteristic.value;
          const option =
            typeof observed === "string" && characteristic.spec.options.some((item) => item.value === observed)
              ? observed
              : characteristic.spec.options[0]?.value;
          return option === undefined ? undefined : { op: "set", value: option };
        }
        case "number": {
          const candidates: number[] = [];
          if (typeof characteristic.value === "number") candidates.push(characteristic.value);
          if (characteristic.spec.minimum !== null) candidates.push(characteristic.spec.minimum);
          candidates.push(0);
          for (const candidate of candidates) {
            try {
              return parseSetValue(characteristic, String(candidate));
            } catch (error) {
              if (!(error instanceof CharacteristicValueError)) throw error;
            }
          }
          return undefined;
        }
        case "string":
          return typeof characteristic.value === "string" ? { op: "set", value: characteristic.value } : undefined;
        case "boolean":
        case "action":
          return undefined;
      }
  }
}

export function initialCommand(characteristic: InspectorCharacteristic): CharacteristicCommand | undefined {
  const firstMode = commandModes(characteristic)[0];
  return firstMode ? commandForMode(characteristic, firstMode) : undefined;
}

export function parseSetValue(characteristic: InspectorCharacteristic, text: string): CharacteristicCommand {
  return {
    op: "set",
    value: parseCharacteristicValue(characteristic.spec as CharacteristicSpec, text),
  };
}

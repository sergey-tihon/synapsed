import type { CharacteristicCommand, ClientStatus, DeviceStatus } from "@synapsed/client";
import type { ActionSettings, Binding } from "./settings.js";
import { ID_PATTERN } from "./settings.js";
import type { InspectorCharacteristic } from "./catalog.js";
export type Selection = {
  deviceId: string;
  characteristicId: string;
  pressDeviceId: string;
  pressCharacteristicId: string;
};

export function selectionFromSettings(settings: ActionSettings): Selection {
  return {
    deviceId: settings.binding?.deviceId ?? "",
    characteristicId: settings.binding?.characteristicId ?? "",
    pressDeviceId: settings.pressBinding?.deviceId ?? "",
    pressCharacteristicId: settings.pressBinding?.characteristicId ?? "",
  };
}

export function isSelection(value: unknown): value is Selection {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const item = value as Record<string, unknown>;
  return ["deviceId", "characteristicId", "pressDeviceId", "pressCharacteristicId"].every(
    (key) => typeof item[key] === "string" && (item[key] === "" || ID_PATTERN.test(item[key] as string)),
  );
}

export type InspectorRequest =
  | { event: "catalog"; selection: Selection }
  | {
      event: "validateCommand";
      requestId: number;
      slot: "key" | "press";
      binding: Binding | null;
      op: "default" | "set" | "step" | "toggle" | "invoke";
      text?: string;
    };

export type InspectorDevice = {
  id: string;
  label: string;
  kind: string;
  driver: string;
  status: DeviceStatus;
  characteristics: InspectorCharacteristic[];
};

export type InspectorCatalog = {
  event: "catalog";
  connection: ClientStatus;
  configurationError: string | null;
  devices: InspectorDevice[];
  selection: Selection;
  primary: InspectorCharacteristic | null;
  press: InspectorCharacteristic | null;
};

export type InspectorCommandValidation = {
  event: "commandValidation";
  requestId: number;
  slot: "key" | "press";
  command: CharacteristicCommand | null;
  error: string | null;
};

export type InspectorMessage = InspectorCatalog | InspectorCommandValidation;

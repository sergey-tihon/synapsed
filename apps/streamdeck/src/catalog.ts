import {
  defaultCommand,
  supportsStep,
  type Characteristic,
  type CharacteristicCommand,
  type CharacteristicStatus,
  type CharacteristicValue,
  type ClientStatus,
  type ErrorCode,
  type Snapshot,
} from "@synapsed/client";
import type { InspectorDevice } from "./protocol.js";
export type InspectorSpec =
  | {
      kind: "number";
      minimum: number | null;
      maximum: number | null;
      step: number | null;
      integer: boolean;
      unit: string | null;
    }
  | { kind: "boolean" }
  | { kind: "enum"; options: { value: string; label: string }[] }
  | { kind: "string"; maxLength: number }
  | { kind: "action" };
export type InspectorCharacteristic = {
  id: string;
  label: string;
  group: string | null;
  spec: InspectorSpec;
  access: { read: boolean; write: boolean; notify: boolean };
  value: CharacteristicValue | null;
  status: CharacteristicStatus;
  error: { code: ErrorCode; message: string } | null;
  operations: ("set" | "step" | "toggle" | "invoke")[];
  defaultCommand: CharacteristicCommand | null;
};

/** Removes additive metadata before sending a characteristic across the SDK's JSON boundary. */
export function inspectorCharacteristic(characteristic: Characteristic): InspectorCharacteristic;
export function inspectorCharacteristic(characteristic: undefined): null;
export function inspectorCharacteristic(characteristic: Characteristic | undefined): InspectorCharacteristic | null {
  if (!characteristic) return null;
  const spec = characteristic.spec;
  const metadata =
    spec.kind === "number"
      ? {
          kind: spec.kind,
          minimum: spec.minimum,
          maximum: spec.maximum,
          step: spec.step,
          integer: spec.integer,
          unit: spec.unit,
        }
      : spec.kind === "enum"
        ? { kind: spec.kind, options: spec.options.map(({ value, label }) => ({ value, label })) }
        : spec.kind === "string"
          ? { kind: spec.kind, maxLength: spec.maxLength }
          : { kind: spec.kind };
  const operations: InspectorCharacteristic["operations"] = [];
  if (characteristic.access.write && spec.kind !== "action") operations.push("set");
  if (supportsStep(characteristic)) operations.push("step");
  if (characteristic.access.write && spec.kind === "boolean") operations.push("toggle");
  if (characteristic.access.write && spec.kind === "action") operations.push("invoke");
  return {
    id: characteristic.id,
    label: characteristic.label,
    group: characteristic.group,
    spec: metadata,
    access: {
      read: characteristic.access.read,
      write: characteristic.access.write,
      notify: characteristic.access.notify,
    },
    value: characteristic.value,
    status: characteristic.status,
    operations,
    defaultCommand: defaultCommand(characteristic),
    error: characteristic.error ? { code: characteristic.error.code, message: characteristic.error.message } : null,
  } as InspectorCharacteristic;
}

export type Catalog = {
  devices: InspectorDevice[];
  connection: ClientStatus;
  configurationError: string | null;
};

export function buildCatalog(
  snapshot: Snapshot | undefined,
  connection: ClientStatus,
  configurationError?: string,
): Catalog {
  const devices = (snapshot?.devices ?? []).map((device) => ({
    id: device.id,
    label: device.label,
    kind: device.kind,
    driver: device.driver,
    groups: device.groups.map(({ id, label }) => ({ id, label })),
    status: device.status,
    characteristics: device.characteristics.map((characteristic) => inspectorCharacteristic(characteristic)),
  }));
  return {
    connection,
    devices,
    configurationError: configurationError ?? null,
  };
}

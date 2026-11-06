import {
  formatCharacteristicValue,
  isCommandApplicable,
  type Characteristic,
  type CharacteristicCommand,
  type ClientStatus,
  type Device,
  type Snapshot,
} from "@synapsed/client";
import type { InspectorCharacteristic } from "./catalog.js";
import type { InspectorDevice } from "./protocol.js";
import type { FeedbackPayload } from "@elgato/streamdeck";
import { keyEligible, rotateEligible } from "./eligibility.js";
import {
  customTitle,
  dialSettingsConform,
  keyBinding,
  keySettingsConform,
  pressBinding,
  rotateBinding,
  type Binding,
  type DialSettings,
  type KeySettings,
} from "./settings.js";

export function resolveBinding(
  snapshot: Snapshot | undefined,
  binding: Binding | undefined,
): { device: Device; characteristic: Characteristic } | undefined {
  if (!snapshot || !binding) return undefined;
  const device = snapshot.devices.find((item) => item.id === binding.deviceId);
  const characteristic = device?.characteristics.find((item) => item.id === binding.characteristicId);
  return device && characteristic ? { device, characteristic } : undefined;
}

export function readingText(characteristic: Characteristic): string {
  return formatCharacteristicValue(characteristic);
}

function unavailableText(
  device: { status: Device["status"]; error?: Device["error"] },
  characteristic: {
    status: Characteristic["status"];
    error: Characteristic["error"] | InspectorCharacteristic["error"];
  },
): string | undefined {
  const error = device.error ?? characteristic.error;
  if (error?.code === "driver_conflict" || error?.code === "ambiguous_device") return "Driver conflict";
  if (device.status !== "online") return "Device unavailable";
  if (characteristic.status !== "ok") return characteristic.status === "error" ? "Characteristic error" : "Unavailable";
  return undefined;
}

export type DisplayFeedback = {
  title: string;
  current: string;
  minimum: { value: string; enabled: boolean };
  maximum: { value: string; enabled: boolean };
  bar: { value: number; enabled: boolean };
};

type RenderParts = {
  header: string[];
  valueLine?: string;
  resolved?: { device: Device; characteristic: Characteristic };
  minimum: string;
  maximum: string;
  percent: number;
  bounded: boolean;
};

function serviceState(connection: ClientStatus): string | undefined {
  if (connection === "incompatible") return "Incompatible";
  if (connection === "connecting") return "Connecting";
  if (connection === "disconnected") return "Service Offline";
  return undefined;
}

export function inspectorReading(
  connection: ClientStatus,
  device: InspectorDevice,
  characteristic: InspectorCharacteristic,
): string {
  const value = formatCharacteristicValue(characteristic as unknown as Characteristic);
  const problem = serviceState(connection) ?? unavailableText(device, characteristic);
  if (!problem) return `Current: ${value}`;
  return characteristic.value === null ? problem : `Last known: ${value} (stale) · ${problem}`;
}

function renderParts(
  settings: { title?: string },
  conform: boolean,
  binding: Binding | undefined,
  snapshot: Snapshot | undefined,
  connection: ClientStatus,
  pending: boolean,
  eligible: (characteristic: Characteristic) => boolean,
  command?: CharacteristicCommand,
  commandRequired = false,
): RenderParts {
  const title = customTitle(settings);
  let header = ["Synapsed"];
  let valueLine: string | undefined = "Configure";
  let resolved: RenderParts["resolved"];
  let minimum = "";
  let maximum = "";
  let percent = 0;
  let bounded = false;

  if (!conform) {
    valueLine = "Unsupported";
  } else if (!binding) {
    header = [title ?? "Synapsed"];
  } else {
    resolved = resolveBinding(snapshot, binding);
    const service = serviceState(connection);
    if (!resolved) {
      header = title ? [title] : [binding.deviceId, binding.characteristicId];
      valueLine = service ?? "Missing";
    } else {
      const { device, characteristic } = resolved;
      header = title ? [title] : [device.label, characteristic.label];
      const unavailable = service ?? unavailableText(device, characteristic);
      if (!eligible(characteristic)) {
        valueLine = "Unsupported";
      } else if (
        commandRequired &&
        characteristic.access.write &&
        (!command || !isCommandApplicable(characteristic, command))
      ) {
        valueLine = "Choose action";
      } else if (unavailable) {
        valueLine =
          characteristic.value !== null && characteristic.spec.kind !== "action"
            ? `${readingText(characteristic)} (stale)`
            : unavailable;
      } else if (characteristic.spec.kind !== "action") {
        valueLine = readingText(characteristic);
      } else {
        valueLine = undefined;
      }

      if (characteristic.spec.kind === "number") {
        const spec = characteristic.spec;
        if (
          spec.minimum !== null &&
          spec.maximum !== null &&
          spec.maximum > spec.minimum &&
          typeof characteristic.value === "number" &&
          Number.isFinite(characteristic.value)
        ) {
          bounded = true;
          minimum = `Min ${spec.minimum}`;
          maximum = `Max ${spec.maximum}`;
          percent = Math.max(
            0,
            Math.min(100, (100 * (characteristic.value - spec.minimum)) / (spec.maximum - spec.minimum)),
          );
        }
      } else if (characteristic.spec.kind === "enum") {
        const options = characteristic.spec.options;
        const index = options.findIndex((option) => option.value === characteristic.value);
        if (options.length > 1 && index >= 0) {
          bounded = true;
          minimum = options[0]!.label;
          maximum = options[options.length - 1]!.label;
          percent = (100 * index) / (options.length - 1);
        }
      }
    }
  }

  if (pending) valueLine = valueLine ? `${valueLine} · Pending` : "Pending";
  return { header, valueLine, resolved, minimum, maximum, percent, bounded };
}

export function renderKey(
  settings: KeySettings,
  snapshot: Snapshot | undefined,
  connection: ClientStatus,
  pending: boolean,
): string {
  const parts = renderParts(
    settings,
    keySettingsConform(settings),
    keyBinding(settings),
    snapshot,
    connection,
    pending,
    keyEligible,
    settings.command,
    true,
  );
  const command = settings.command;
  const resolved = parts.resolved;
  const characteristic = resolved?.characteristic;
  let valueLine = parts.valueLine;
  if (
    command?.op === "set" &&
    typeof command.value === "number" &&
    characteristic?.spec.kind === "number" &&
    resolved &&
    isCommandApplicable(characteristic, command)
  ) {
    const target = `${command.value}${characteristic.spec.unit ?? ""}`;
    const unavailable = serviceState(connection) ?? unavailableText(resolved.device, characteristic);
    const pendingSuffix = pending ? " · Pending" : "";
    valueLine = unavailable
      ? `${(parts.valueLine ?? unavailable).replace(/ · Pending$/, "")} · to ${target}${pendingSuffix}`
      : `to ${target}${pendingSuffix}`;
  }
  return [...parts.header, valueLine].filter(Boolean).join("\n");
}

export function renderDial(
  settings: DialSettings,
  snapshot: Snapshot | undefined,
  connection: ClientStatus,
  pending: boolean,
): DisplayFeedback {
  const conform = dialSettingsConform(settings);
  const parts = renderParts(settings, conform, rotateBinding(settings), snapshot, connection, pending, rotateEligible);
  let title = parts.header.join(" · ");
  const press = conform ? resolveBinding(snapshot, pressBinding(settings)) : undefined;
  if (press?.characteristic.spec.kind === "boolean") {
    const stale = serviceState(connection) ?? unavailableText(press.device, press.characteristic);
    title += ` · ${press.characteristic.label}: ${readingText(press.characteristic)}${stale ? " (stale)" : ""}`;
  }
  const feedback = {
    title,
    current: parts.valueLine ?? "",
    minimum: { value: parts.minimum, enabled: parts.bounded },
    maximum: { value: parts.maximum, enabled: parts.bounded },
    bar: { value: parts.percent, enabled: parts.bounded },
  } satisfies FeedbackPayload & DisplayFeedback;
  return feedback;
}

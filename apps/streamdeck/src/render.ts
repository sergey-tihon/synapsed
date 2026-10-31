import {
  defaultCommand,
  isCommandApplicable,
  type Characteristic,
  type ClientStatus,
  type Device,
  type Snapshot,
} from "@synapsed/client";
import type { FeedbackPayload } from "@elgato/streamdeck";
import { settingsConform, type ActionSettings, type Binding } from "./settings.js";

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
  if (characteristic.spec.kind === "action") return "Invoke";
  if (characteristic.value === null) return "Not observed";
  switch (characteristic.spec.kind) {
    case "boolean":
      return characteristic.value === true ? "On" : "Off";
    case "enum":
      return characteristic.spec.options.find((option) => option.value === characteristic.value)?.label ?? "Unknown";
    case "number":
      return `${characteristic.value}${characteristic.spec.unit ? ` ${characteristic.spec.unit}` : ""}`;
    case "string":
      return characteristic.value === "" ? "(empty)" : String(characteristic.value);
  }
}

function unavailableText(device: Device, characteristic: Characteristic): string | undefined {
  const error = device.error ?? characteristic.error;
  if (error?.code === "driver_conflict" || error?.code === "ambiguous_device") return "Driver conflict";
  if (device.status !== "online") return "Device unavailable";
  if (characteristic.status !== "ok") return characteristic.status === "error" ? "Characteristic error" : "Unavailable";
  return undefined;
}

export type Display = { title: string; feedback: FeedbackPayload };

export function renderDisplay(
  settings: ActionSettings,
  snapshot: Snapshot | undefined,
  connection: ClientStatus,
  pending: boolean,
): Display {
  let label = "Synapsed";
  let current = "Configure";
  let state: string | undefined;
  let minimum = "";
  let maximum = "";
  let percent = 0;
  let bounded = false;
  const resolved = resolveBinding(snapshot, settings.binding);
  if (!settingsConform(settings)) {
    current = "Unsupported";
  } else if (!settings.binding) {
    current = "Configure";
  } else {
    const service =
      connection === "incompatible"
        ? "Incompatible"
        : connection === "connecting"
          ? "Connecting"
          : connection === "disconnected"
            ? "Service Offline"
            : undefined;
    if (!resolved) {
      current = service ?? "Missing";
    } else {
      const { device, characteristic } = resolved;
      label = characteristic.label;
      current = readingText(characteristic);
      state = service ?? unavailableText(device, characteristic);
      if (state && characteristic.spec.kind !== "action" && characteristic.value !== null)
        current = `${current} (stale)`;
      const command = settings.keyCommand ?? defaultCommand(characteristic);
      if (command && !isCommandApplicable(characteristic, command)) state = "Unsupported";
      if (settings.pressBinding || settings.pressCommand) {
        const secondary = resolveBinding(snapshot, settings.pressBinding ?? settings.binding);
        if (!secondary) state ??= "Missing press binding";
        else {
          const pressCommand = settings.pressCommand ?? defaultCommand(secondary.characteristic);
          if (pressCommand && !isCommandApplicable(secondary.characteristic, pressCommand))
            state ??= "Unsupported press";
          if (secondary.characteristic.spec.kind === "boolean") {
            const stale =
              connection !== "connected" ||
              secondary.device.status !== "online" ||
              secondary.characteristic.status !== "ok";
            label += ` · ${secondary.characteristic.label}: ${readingText(secondary.characteristic)}${stale ? " (stale)" : ""}`;
          }
        }
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
      if (pending) state = state ? `${state} · Pending` : "Pending";
    }
  }
  const heading = state ? `${label} · ${state}` : label;
  return {
    title: `${heading}\n${current}`,
    feedback: {
      title: heading,
      current,
      minimum: { value: minimum, enabled: bounded },
      maximum: { value: maximum, enabled: bounded },
      bar: { value: percent, enabled: bounded },
    },
  };
}

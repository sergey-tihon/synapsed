import type { Characteristic, Device, Snapshot } from "@synapsed/client";

export function characteristic(
  kind: "number" | "boolean" | "enum" | "string" | "action" = "number",
  value: Characteristic["value"] = 50,
  status: Characteristic["status"] = "ok",
  write = true,
): Characteristic {
  const spec =
    kind === "number"
      ? { kind, minimum: 0, maximum: 100, step: 1, integer: true, unit: "%" }
      : kind === "enum"
        ? {
            kind,
            options: [
              { value: "low", label: "Low" },
              { value: "high", label: "High" },
            ],
          }
        : kind === "string"
          ? { kind, maxLength: 80 }
          : { kind };
  return {
    id: "brightness",
    label: "Brightness",
    spec,
    access: { read: true, write, notify: true },
    value,
    status,
    error: null,
  } as unknown as Characteristic;
}

export function device(chars: Characteristic[] = [characteristic()], status: Device["status"] = "online"): Device {
  return {
    id: "monitor",
    label: "Monitor",
    kind: "monitor",
    driver: "benq-rd280u",
    status,
    error: null,
    characteristics: chars,
  };
}

export function snapshot(devices: Device[] = [device()]): Snapshot {
  return {
    apiVersion: 1,
    revision: "1",
    node: { instanceId: "instance", nodeId: "node", name: "Test" },
    drivers: [],
    devices,
  };
}

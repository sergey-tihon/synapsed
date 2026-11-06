import type { ClientStatus, DeviceStatus } from "@synapsed/client";
import type { InspectorCharacteristic } from "./catalog.js";

export type InspectorRequest = { event: "getCatalog" };

export type InspectorDevice = {
  id: string;
  label: string;
  kind: string;
  driver: string;
  status: DeviceStatus;
  groups: { id: string; label: string }[];
  characteristics: InspectorCharacteristic[];
};

export type InspectorCatalog = {
  event: "catalog";
  connection: ClientStatus;
  configurationError: string | null;
  devices: InspectorDevice[];
};

export type InspectorMessage = InspectorCatalog;

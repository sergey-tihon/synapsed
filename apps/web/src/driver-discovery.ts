import type { DeviceStatus, DriverState } from "@synapsed/client";

export interface DriverDiscoverySummary {
  message: string;
  state: "online" | "warn" | "error";
}

export function driverDiscoverySummary(
  driverState: DriverState,
  devices: readonly { status: DeviceStatus }[],
  isCurrent: boolean,
): DriverDiscoverySummary {
  if (!isCurrent) {
    return { message: "Snapshot stale · discovery status unknown", state: "warn" };
  }
  if (driverState === "disabled") {
    return { message: "Driver disabled · discovery stopped", state: "warn" };
  }
  if (driverState !== "online") {
    const state = driverState.replaceAll("_", " ");
    const tone = driverState === "error" || driverState === "permission_denied" ? "error" : "warn";
    return { message: `Driver ${state} · discovery status unavailable`, state: tone };
  }
  if (devices.length === 0) {
    return { message: "Loaded · no matching device discovered", state: "warn" };
  }

  const online = devices.filter((device) => device.status === "online").length;
  if (online === devices.length) {
    return {
      message: `Loaded · ${online} device${online === 1 ? "" : "s"} discovered and online`,
      state: "online",
    };
  }
  if (online === 0) {
    return {
      message: `Loaded · ${devices.length} published device${devices.length === 1 ? "" : "s"}, currently unavailable`,
      state: "warn",
    };
  }
  return {
    message: `Loaded · ${online} online · ${devices.length - online} unavailable`,
    state: "warn",
  };
}

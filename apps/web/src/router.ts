import type { LogLevel } from "@synapsed/client";
export type KindFilter = "usb" | "display" | "bluetooth";
export type AttachmentFilter = "all" | "host" | "dock" | "monitor";
export type DriverFilter = "any" | "specialized" | "generic" | "none";
export type LogLevelFilter = "all" | LogLevel;

export interface HardwareFilters {
  kinds: KindFilter[];
  attachment: AttachmentFilter;
  driver: DriverFilter;
  connected: boolean;
  q: string;
}

export type Route =
  | { page: "overview" }
  | { page: "hardware"; filters: HardwareFilters }
  | { page: "devices" }
  | { page: "device"; deviceId: string; characteristicId: string | null }
  | { page: "logs"; level: LogLevelFilter }
  | { page: "not-found" };

export const DEFAULT_HARDWARE_FILTERS: HardwareFilters = {
  kinds: [],
  attachment: "all",
  driver: "any",
  connected: false,
  q: "",
};

const kinds = new Set<KindFilter>(["usb", "display", "bluetooth"]);
const attachments = new Set<AttachmentFilter>(["all", "host", "dock", "monitor"]);
const drivers = new Set<DriverFilter>(["any", "specialized", "generic", "none"]);
const logLevels = new Set<LogLevelFilter>(["all", "debug", "info", "warn", "error"]);

function decodedPath(segment: string): string | null {
  try {
    return decodeURIComponent(segment);
  } catch {
    return null;
  }
}

function searchParams(search: string): URLSearchParams {
  const result = new URLSearchParams();
  for (const pair of search.split("&")) {
    if (!pair) continue;
    const separator = pair.indexOf("=");
    const rawKey = separator < 0 ? pair : pair.slice(0, separator);
    const rawValue = separator < 0 ? "" : pair.slice(separator + 1);
    try {
      result.append(decodeURIComponent(rawKey.replaceAll("+", " ")), decodeURIComponent(rawValue.replaceAll("+", " ")));
    } catch {
      // Ignore malformed parameters without discarding valid neighboring filters.
    }
  }
  return result;
}

function hardwareFilters(search: string): HardwareFilters {
  const query = searchParams(search);
  const selectedKinds = [
    ...new Set(
      (query.get("kind") ?? "").split(",").filter((kind): kind is KindFilter => kinds.has(kind as KindFilter)),
    ),
  ];
  const attachment = query.get("attach");
  const driver = query.get("driver");
  return {
    kinds: selectedKinds,
    attachment: attachments.has(attachment as AttachmentFilter) ? (attachment as AttachmentFilter) : "all",
    driver: drivers.has(driver as DriverFilter) ? (driver as DriverFilter) : "any",
    connected: query.get("connected") === "1",
    q: query.get("q") ?? "",
  };
}

export function parseRoute(hash: string): Route {
  const fragment = hash.startsWith("#") ? hash.slice(1) : hash;
  if (fragment === "" || fragment === "/") return { page: "overview" };
  const queryAt = fragment.indexOf("?");
  const path = queryAt < 0 ? fragment : fragment.slice(0, queryAt);
  const search = queryAt < 0 ? "" : fragment.slice(queryAt + 1);
  if (path === "/hardware") return { page: "hardware", filters: hardwareFilters(search) };
  if (path === "/devices") return { page: "devices" };
  if (path === "/logs") {
    const level = searchParams(search).get("level");
    return { page: "logs", level: logLevels.has(level as LogLevelFilter) ? (level as LogLevelFilter) : "all" };
  }
  const segments = path.split("/").filter(Boolean);
  if (segments.length === 2 && segments[0] === "devices") {
    const deviceId = decodedPath(segments[1] ?? "");
    if (!deviceId) return { page: "not-found" };
    const characteristic = searchParams(search).get("characteristic");
    return { page: "device", deviceId, characteristicId: characteristic };
  }
  return { page: "not-found" };
}

export function href(route: Route): string {
  switch (route.page) {
    case "overview":
      return "#/";
    case "devices":
      return "#/devices";
    case "device":
      return `#/devices/${encodeURIComponent(route.deviceId)}${route.characteristicId === null ? "" : `?characteristic=${encodeURIComponent(route.characteristicId)}`}`;
    case "logs":
      return route.level === "all" ? "#/logs" : `#/logs?level=${encodeURIComponent(route.level)}`;
    case "hardware": {
      const query = new URLSearchParams();
      if (route.filters.kinds.length) query.set("kind", [...new Set(route.filters.kinds)].join(","));
      if (route.filters.attachment !== "all") query.set("attach", route.filters.attachment);
      if (route.filters.driver !== "any") query.set("driver", route.filters.driver);
      if (route.filters.connected) query.set("connected", "1");
      if (route.filters.q) query.set("q", route.filters.q);
      const search = query.toString();
      return `#/hardware${search ? `?${search}` : ""}`;
    }
    case "not-found":
      return "#/not-found";
  }
}

export function characteristicElementId(deviceId: string, characteristicId: string): string {
  return `characteristic-${encodeURIComponent(deviceId)}-${encodeURIComponent(characteristicId)}`;
}

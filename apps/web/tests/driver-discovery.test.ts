import { describe, expect, it } from "vitest";
import { driverDiscoverySummary } from "../src/driver-discovery";

describe("driver discovery summary", () => {
  it("distinguishes an online loaded driver with no discovered device", () => {
    expect(driverDiscoverySummary("online", [], true)).toEqual({
      message: "Loaded · no matching device discovered",
      state: "warn",
    });
  });

  it("reports discovered and online devices separately from driver load state", () => {
    expect(driverDiscoverySummary("online", [{ status: "online" }], true)).toEqual({
      message: "Loaded · 1 device discovered and online",
      state: "online",
    });
  });

  it("does not claim a published but unavailable device is currently discovered", () => {
    expect(driverDiscoverySummary("online", [{ status: "unavailable" }], true)).toEqual({
      message: "Loaded · 1 published device, currently unavailable",
      state: "warn",
    });
  });

  it("marks discovery information stale when the client is disconnected", () => {
    expect(driverDiscoverySummary("online", [], false)).toEqual({
      message: "Snapshot stale · discovery status unknown",
      state: "warn",
    });
  });

  it("uses neutral wording for retryable driver failures", () => {
    expect(driverDiscoverySummary("permission_denied", [], true)).toEqual({
      message: "Driver permission denied · discovery status unavailable",
      state: "error",
    });
    expect(driverDiscoverySummary("disconnected", [], true)).toEqual({
      message: "Driver disconnected · discovery status unavailable",
      state: "warn",
    });
  });

  it("reports disabled drivers as stopped", () => {
    expect(driverDiscoverySummary("disabled", [], true)).toEqual({
      message: "Driver disabled · discovery stopped",
      state: "warn",
    });
  });
});

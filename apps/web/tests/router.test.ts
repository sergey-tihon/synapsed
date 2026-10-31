import { describe, expect, it } from "vitest";
import { href, parseRoute, type Route } from "../src/router";

describe("hash routes", () => {
  it.each<Route>([
    { page: "overview" },
    {
      page: "hardware",
      filters: {
        kinds: ["usb", "display"],
        attachment: "dock",
        driver: "specialized",
        connected: true,
        q: "dock/a?b&c#d",
      },
    },
    { page: "devices" },
    { page: "device", deviceId: "dock/a?b&c#d", characteristicId: "mode/level?x#y&z" },
    { page: "device", deviceId: "standalone", characteristicId: null },
    { page: "logs", level: "warn" },
    { page: "logs", level: "all" },
    { page: "not-found" },
  ])("round-trips $page routes", (route) => {
    expect(parseRoute(href(route))).toEqual(route);
  });

  it("decodes escaped slashes only after separating the device path", () => {
    expect(parseRoute("#/devices/a%2Fb?characteristic=c%3Fd%23e")).toEqual({
      page: "device",
      deviceId: "a/b",
      characteristicId: "c?d#e",
    });
  });

  it("drops malformed filter values and unknown page paths", () => {
    expect(parseRoute("#/hardware?kind=usb,display,unknown&attach=invalid&driver=bogus&connected=yes&q=%ZZ")).toEqual({
      page: "hardware",
      filters: { kinds: ["usb", "display"], attachment: "all", driver: "any", connected: false, q: "" },
    });
    expect(parseRoute("#/devices/%E0%A4%A")).toEqual({ page: "not-found" });
    expect(parseRoute("#/missing")).toEqual({ page: "not-found" });
    expect(parseRoute("#/logs?level=verbose")).toEqual({ page: "logs", level: "all" });
  });
});

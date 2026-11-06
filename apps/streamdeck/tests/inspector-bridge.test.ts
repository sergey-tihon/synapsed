/** @vitest-environment jsdom */
import { afterEach, expect, it, vi } from "vitest";

class FakeWebSocket extends EventTarget {
  static OPEN = 1;
  static latest: FakeWebSocket | undefined;
  readyState = 0;
  messages: string[] = [];

  constructor() {
    super();
    FakeWebSocket.latest = this;
  }

  send(message: string): void {
    this.messages.push(message);
  }

  close(): void {
    this.readyState = 3;
  }

  open(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.dispatchEvent(new Event("open"));
  }
}

afterEach(() => vi.unstubAllGlobals());

it("uses the registration UUID and ignores messages from stale sockets", async () => {
  vi.stubGlobal("WebSocket", FakeWebSocket);
  const inspector = await import("../inspector/src/streamdeck.js");
  inspector.connectElgatoStreamDeckSocket(
    "1234",
    "plugin-registration-uuid",
    "registerPropertyInspector",
    "{}",
    JSON.stringify({
      action: "com.sergey-tihon.synapsed.key",
      context: "action-instance-context",
      payload: { settings: {} },
    }),
  );
  const firstSocket = FakeWebSocket.latest;
  if (!firstSocket) throw new Error("Inspector websocket was not created.");
  firstSocket.open();
  inspector.setSettings({ deviceId: "monitor", characteristicId: "brightness" });

  expect(inspector.bridge.kind()).toBe("key");
  expect(firstSocket.messages.map((message) => JSON.parse(message))).toEqual([
    { event: "registerPropertyInspector", uuid: "plugin-registration-uuid" },
    {
      event: "sendToPlugin",
      context: "plugin-registration-uuid",
      action: "com.sergey-tihon.synapsed.key",
      payload: { event: "getCatalog" },
    },
    {
      event: "setSettings",
      context: "plugin-registration-uuid",
      action: "com.sergey-tihon.synapsed.key",
      payload: { deviceId: "monitor", characteristicId: "brightness" },
    },
  ]);

  inspector.connectElgatoStreamDeckSocket(
    "1234",
    "second-registration-uuid",
    "registerPropertyInspector",
    "{}",
    JSON.stringify({
      action: "com.sergey-tihon.synapsed.dial",
      payload: { settings: { title: "Second connection" } },
    }),
  );
  const secondSocket = FakeWebSocket.latest;
  if (!secondSocket || secondSocket === firstSocket) throw new Error("Second inspector websocket was not created.");
  firstSocket.dispatchEvent(
    new MessageEvent("message", {
      data: JSON.stringify({ event: "didReceiveSettings", payload: { settings: { title: "Stale settings" } } }),
    }),
  );
  expect(inspector.bridge.kind()).toBe("dial");
  expect(inspector.bridge.settings()).toEqual({ title: "Second connection" });
});

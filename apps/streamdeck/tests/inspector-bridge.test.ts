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

it("scopes settings and plugin requests to the action context", async () => {
  vi.stubGlobal("WebSocket", FakeWebSocket);
  const bridge = await import("../inspector/src/streamdeck.js");
  bridge.connectElgatoStreamDeckSocket(
    "1234",
    "plugin-registration-uuid",
    "registerPropertyInspector",
    "{}",
    JSON.stringify({
      action: "com.sergey-tihon.synapsed.characteristic",
      context: "action-instance-context",
      payload: { settings: {} },
    }),
  );
  const socket = FakeWebSocket.latest;
  if (!socket) throw new Error("Inspector websocket was not created.");
  socket.open();
  bridge.sendToPlugin({
    event: "catalog",
    selection: { deviceId: "", characteristicId: "", pressDeviceId: "", pressCharacteristicId: "" },
  });
  bridge.setSettings({ binding: { deviceId: "monitor", characteristicId: "brightness" } });

  expect(socket.messages.map((message) => JSON.parse(message))).toEqual([
    { event: "registerPropertyInspector", uuid: "plugin-registration-uuid" },
    {
      event: "sendToPlugin",
      context: "action-instance-context",
      action: "com.sergey-tihon.synapsed.characteristic",
      payload: {
        event: "catalog",
        selection: { deviceId: "", characteristicId: "", pressDeviceId: "", pressCharacteristicId: "" },
      },
    },
    {
      event: "setSettings",
      context: "action-instance-context",
      action: "com.sergey-tihon.synapsed.characteristic",
      payload: { binding: { deviceId: "monitor", characteristicId: "brightness" } },
    },
  ]);
});

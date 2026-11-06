import { createSignal } from "solid-js";
import { actionKind, type ActionKind, type ActionSettings } from "../../src/settings.js";
import type { InspectorCatalog, InspectorMessage, InspectorRequest } from "../../src/protocol.js";

export type StreamDeckActionInfo = {
  action?: string;
  context?: string;
  payload?: { settings?: ActionSettings };
};

type BridgeMessage =
  | { event: "sendToPropertyInspector"; context?: string; action?: string; payload: InspectorMessage }
  | { event: "didReceiveSettings"; context?: string; action?: string; payload: { settings?: ActionSettings } };

const [connected, setConnected] = createSignal(false);
const [settings, setCurrentSettings] = createSignal<ActionSettings>({});
const [catalog, setCatalog] = createSignal<InspectorCatalog | null>(null);
const [kind, setKind] = createSignal<ActionKind | undefined>();
let socket: WebSocket | undefined;
let inspectorUuid = "";
let action = "";

function send(event: string, payload: unknown): void {
  if (!inspectorUuid || socket?.readyState !== WebSocket.OPEN) return;
  socket.send(JSON.stringify({ event, context: inspectorUuid, action, payload }));
}

export function sendToPlugin(payload: InspectorRequest): void {
  send("sendToPlugin", payload);
}

/** The one settings writer used by the inspector UI. */
export function setSettings(next: ActionSettings): void {
  setCurrentSettings(next);
  send("setSettings", next);
}

function receive(data: string): void {
  let message: BridgeMessage;
  try {
    message = JSON.parse(data) as BridgeMessage;
  } catch {
    return;
  }
  if (message.event === "sendToPropertyInspector" && message.payload.event === "catalog") {
    setCatalog(message.payload);
  } else if (message.event === "didReceiveSettings") {
    const next = message.payload.settings;
    if (next && typeof next === "object" && !Array.isArray(next)) setCurrentSettings(next);
  }
}

function parseActionInfo(source: string): StreamDeckActionInfo {
  try {
    return JSON.parse(source) as StreamDeckActionInfo;
  } catch {
    return {};
  }
}

export function connectElgatoStreamDeckSocket(
  port: string,
  uuid: string,
  registerEvent: string,
  _info: string,
  actionInfo: string,
): void {
  const previous = socket;
  socket = undefined;
  previous?.close();
  const parsed = parseActionInfo(actionInfo);
  inspectorUuid = uuid;
  action = parsed.action ?? "";
  setKind(actionKind(action));
  const initialSettings = parsed.payload?.settings;
  setCurrentSettings(initialSettings && typeof initialSettings === "object" ? initialSettings : {});
  setCatalog(null);
  setConnected(false);
  const connection = new WebSocket(`ws://127.0.0.1:${port}`);
  socket = connection;
  connection.addEventListener("open", () => {
    if (socket !== connection) return;
    connection.send(JSON.stringify({ event: registerEvent, uuid }));
    setConnected(true);
    sendToPlugin({ event: "getCatalog" });
  });
  connection.addEventListener("message", (event) => {
    if (socket === connection && typeof event.data === "string") receive(event.data);
  });
  connection.addEventListener("close", () => {
    if (socket !== connection) return;
    socket = undefined;
    setConnected(false);
  });
  connection.addEventListener("error", () => {
    if (socket === connection) setConnected(false);
  });
}

declare global {
  interface Window {
    connectElgatoStreamDeckSocket: typeof connectElgatoStreamDeckSocket;
  }
}

window.connectElgatoStreamDeckSocket = connectElgatoStreamDeckSocket;

export const bridge = {
  connected,
  settings,
  catalog,
  kind,
};

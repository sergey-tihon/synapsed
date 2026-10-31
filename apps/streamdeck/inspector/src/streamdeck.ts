import { createSignal } from "solid-js";
import type { ActionSettings } from "../../src/settings.js";
import type {
  InspectorCatalog,
  InspectorCommandValidation,
  InspectorMessage,
  InspectorRequest,
  Selection,
} from "../../src/protocol.js";

export type StreamDeckActionInfo = {
  action?: string;
  context?: string;
  payload?: { settings?: ActionSettings };
};

type BridgeMessage =
  | { event: "sendToPropertyInspector"; payload: InspectorMessage }
  | { event: "didReceiveSettings"; payload: { settings?: ActionSettings } };

const [connected, setConnected] = createSignal(false);
const [settings, setCurrentSettings] = createSignal<ActionSettings>({});
const [catalog, setCatalog] = createSignal<InspectorCatalog | null>(null);
const [validation, setValidation] = createSignal<InspectorCommandValidation | null>(null);
let socket: WebSocket | undefined;
let context = "";
let action = "";

function send(event: string, payload: unknown): void {
  if (!context || socket?.readyState !== WebSocket.OPEN) return;
  socket.send(JSON.stringify({ event, context, action, payload }));
}

export function sendToPlugin(payload: InspectorRequest): void {
  send("sendToPlugin", payload);
}

/** The one settings writer used by the inspector UI. */
export function setSettings(next: ActionSettings): void {
  setCurrentSettings(next);
  send("setSettings", next);
}

export function requestCatalog(selection: Selection): void {
  sendToPlugin({ event: "catalog", selection });
}

function receive(data: string): void {
  let message: BridgeMessage;
  try {
    message = JSON.parse(data) as BridgeMessage;
  } catch {
    return;
  }
  if (message.event === "sendToPropertyInspector") {
    if (message.payload.event === "catalog") setCatalog(message.payload);
    else setValidation(message.payload);
  } else if (message.event === "didReceiveSettings") {
    setCurrentSettings(message.payload.settings ?? {});
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
  socket?.close();
  const parsed = parseActionInfo(actionInfo);
  context = parsed.context ?? "";
  action = parsed.action ?? "com.sergey-tihon.synapsed.characteristic";
  setCurrentSettings(parsed.payload?.settings ?? {});
  socket = new WebSocket(`ws://127.0.0.1:${port}`);
  socket.addEventListener("open", () => {
    setConnected(true);
    socket?.send(JSON.stringify({ event: registerEvent, uuid }));
  });
  socket.addEventListener("message", (event) => {
    if (typeof event.data === "string") receive(event.data);
  });
  socket.addEventListener("close", () => setConnected(false));
  socket.addEventListener("error", () => setConnected(false));
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
  validation,
};

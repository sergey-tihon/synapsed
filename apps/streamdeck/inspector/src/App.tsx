import { Show } from "solid-js";
import KeyInspector from "./components/KeyInspector.js";
import DialInspector from "./components/DialInspector.js";
import { bridge } from "./streamdeck.js";
import brandMark from "../../../../assets/brand/mark-tile-small.svg";
import "./inspector.css";

export default function App() {
  const connection = () => bridge.catalog()?.connection ?? (bridge.connected() ? "connecting" : "disconnected");
  const connectionLabel = () =>
    bridge.catalog()?.connection ?? (bridge.connected() ? "Connecting" : "Stream Deck disconnected");

  return (
    <main class="inspector">
      <header class="topbar">
        <div class="brand">
          <img class="brand-mark" src={brandMark} alt="" width="40" height="40" />
          <div>
            <strong>Synapsed</strong>
            <small>Local characteristic controls</small>
          </div>
        </div>
        <p class={`connection ${connection()}`} role="status">
          <span class="connection-dot" />
          {connectionLabel()}
        </p>
      </header>
      <Show when={bridge.catalog()?.configurationError}>
        {(error) => (
          <p class="banner error" role="alert">
            {error()}
          </p>
        )}
      </Show>
      <Show when={!bridge.connected()}>
        <p class="banner">Waiting for Stream Deck connection…</p>
      </Show>
      {bridge.kind() === "key" ? (
        <KeyInspector />
      ) : bridge.kind() === "dial" ? (
        <DialInspector />
      ) : (
        <p class="banner error">Unsupported action.</p>
      )}
    </main>
  );
}

import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import {
  characteristicSections,
  formatCharacteristicValue,
  type Characteristic,
  type Snapshot,
} from "@synapsed/client";
import { renderDisplay } from "../../src/render.js";
import { selectionFromSettings, type InspectorCatalog, type Selection } from "../../src/protocol.js";
import type { InspectorCharacteristic } from "../../src/catalog.js";
import type { Binding } from "../../src/settings.js";
import CommandEditor, { type ValidationState } from "./components/CommandEditor.js";
import { bridge, requestCatalog, sendToPlugin, setSettings } from "./streamdeck.js";
import { mergeValidatedCommand } from "../../src/model.js";
import brandMark from "../../../../assets/brand/mark-tile-small.svg";
import "./inspector.css";

let requestId = 0;
const pendingValidation = new Map<number, "key" | "press">();
const latestValidation: Record<"key" | "press", number> = { key: 0, press: 0 };

function binding(deviceId: string, characteristicId: string): Binding | null {
  return deviceId && characteristicId ? { deviceId, characteristicId } : null;
}

function deviceGroup(catalog: InspectorCatalog | null, status: "online" | "unavailable") {
  return catalog?.devices.filter((device) => device.status === status) ?? [];
}

function kindIcon(kind: string): string {
  switch (kind) {
    case "number":
      return "↕";
    case "boolean":
      return "◉";
    case "enum":
      return "≡";
    case "action":
      return "ϟ";
    case "string":
      return "Aa";
    default:
      return "·";
  }
}
function sameSelection(left: Selection, right: Selection): boolean {
  return (
    left.deviceId === right.deviceId &&
    left.characteristicId === right.characteristicId &&
    left.pressDeviceId === right.pressDeviceId &&
    left.pressCharacteristicId === right.pressCharacteristicId
  );
}
export default function App() {
  const [lastValidation, setLastValidation] = createSignal<ValidationState>(null);
  const [selected, setSelected] = createSignal<Selection>(selectionFromSettings(bridge.settings()));
  const [selectionEdited, setSelectionEdited] = createSignal(false);
  const settings = bridge.settings;
  const catalog = bridge.catalog;
  const selectedPrimary = createMemo(
    () =>
      catalog()
        ?.devices.find((device) => device.id === selected().deviceId)
        ?.characteristics.find((characteristic) => characteristic.id === selected().characteristicId) ?? null,
  );
  const selectedPress = createMemo(
    () =>
      catalog()
        ?.devices.find((device) => device.id === selected().pressDeviceId)
        ?.characteristics.find((characteristic) => characteristic.id === selected().pressCharacteristicId) ?? null,
  );
  const primaryBinding = () => binding(selected().deviceId, selected().characteristicId);
  const pressBinding = () => binding(selected().pressDeviceId, selected().pressCharacteristicId);

  function updatePrimary(deviceId: string, characteristicId: string): void {
    const previous = selected();
    const nextSelection = { ...previous, deviceId, characteristicId };
    setSelected(nextSelection);
    setSelectionEdited(true);
    const next = { ...settings() };
    if (deviceId && characteristicId) next.binding = { deviceId, characteristicId };
    else delete next.binding;
    if (previous.deviceId !== deviceId || previous.characteristicId !== characteristicId) delete next.keyCommand;
    setSettings(next);
  }

  function selectPrimaryDevice(deviceId: string): void {
    const nextSelection = { ...selected(), deviceId, characteristicId: "" };
    setSelected(nextSelection);
    setSelectionEdited(true);
  }

  function updatePress(deviceId: string, characteristicId: string): void {
    const previous = selected();
    const nextSelection = { ...previous, pressDeviceId: deviceId, pressCharacteristicId: characteristicId };
    setSelected(nextSelection);
    setSelectionEdited(true);
    const next = { ...settings() };
    if (deviceId && characteristicId) next.pressBinding = { deviceId, characteristicId };
    else delete next.pressBinding;
    if (previous.pressDeviceId !== deviceId || previous.pressCharacteristicId !== characteristicId)
      delete next.pressCommand;
    setSettings(next);
  }

  function selectPressDevice(deviceId: string): void {
    const nextSelection = { ...selected(), pressDeviceId: deviceId, pressCharacteristicId: "" };
    setSelected(nextSelection);
    setSelectionEdited(true);
    if (!deviceId) {
      const next = { ...settings() };
      delete next.pressBinding;
      delete next.pressCommand;
      setSettings(next);
    }
  }

  function validate(
    slot: "key" | "press",
    bound: Binding | null,
    op: "default" | "set" | "step" | "toggle" | "invoke",
    text?: string,
  ): void {
    if (!bridge.connected()) {
      setLastValidation({ slot, command: null, error: "Stream Deck is disconnected." });
      return;
    }
    const id = ++requestId;
    pendingValidation.set(id, slot);
    latestValidation[slot] = id;
    sendToPlugin({ event: "validateCommand", requestId: id, slot, binding: bound, op, text });
  }

  createEffect(
    () => {
      if (!bridge.connected()) return null;
      return {
        saved: selectionFromSettings(settings()),
        edited: selectionEdited(),
        selected: selected(),
      };
    },
    (state) => {
      if (!state) {
        pendingValidation.clear();
        return;
      }
      if (!state.edited && !sameSelection(state.selected, state.saved)) setSelected(state.saved);
      requestCatalog(state.edited ? state.selected : state.saved);
    },
  );

  createEffect(
    () => bridge.validation(),
    (result) => {
      if (!result) return;
      const slot = pendingValidation.get(result.requestId);
      if (!slot) return;
      pendingValidation.delete(result.requestId);
      if (latestValidation[slot] !== result.requestId) return;
      setLastValidation({ slot, command: result.command, error: result.error });
      if (result.error) return;
      setSettings(mergeValidatedCommand(settings(), slot, result.command));
    },
  );

  const preview = createMemo(() => {
    const current = catalog();
    if (!current) return null;
    const snapshot = {
      apiVersion: 1,
      revision: "",
      node: { instanceId: "", name: "Stream Deck preview", nodeId: "" },
      drivers: [],
      devices: current.devices,
    } as unknown as Snapshot;
    return renderDisplay(settings(), snapshot, current.connection, false);
  });
  const currentValue = (characteristic: InspectorCharacteristic | null) =>
    characteristic ? formatCharacteristicValue(characteristic as Characteristic) : "No characteristic selected";

  return (
    <main class="inspector">
      <header class="topbar">
        <div class="brand">
          <img class="brand-mark" src={brandMark} alt="" width="40" height="40" />
          <div>
            <strong>Synapsed</strong>
            <small>Characteristic controls</small>
          </div>
        </div>
        <p
          class={`connection ${catalog()?.connection ?? (bridge.connected() ? "connecting" : "disconnected")}`}
          role="status"
        >
          <span class="connection-dot" />
          {catalog()?.connection ?? (bridge.connected() ? "Connecting" : "Stream Deck disconnected")}
        </p>
      </header>
      <Show when={catalog()?.configurationError}>
        {(error) => (
          <p class="banner error" role="alert">
            {error()}
          </p>
        )}
      </Show>
      <Show when={!bridge.connected()}>
        <p class="banner">Waiting for Stream Deck connection…</p>
      </Show>
      <div class="columns">
        <div class="main-column">
          <section class="panel">
            <div class="section-heading">
              <div>
                <p class="eyebrow">PRIMARY BINDING</p>
                <h1>Characteristic</h1>
              </div>
            </div>
            <label class="field">
              Device
              <select
                aria-label="Primary device"
                value={selected().deviceId}
                onChange={(event) => selectPrimaryDevice(event.currentTarget.value)}
              >
                <option value="">Configure — select device</option>
                <Show when={deviceGroup(catalog(), "online").length > 0}>
                  <optgroup label="Online devices">
                    <For each={deviceGroup(catalog(), "online")}>
                      {(device) => (
                        <option value={device.id}>
                          {device.label} · {device.kind}
                        </option>
                      )}
                    </For>
                  </optgroup>
                </Show>
                <Show when={deviceGroup(catalog(), "unavailable").length > 0}>
                  <optgroup label="Unavailable devices">
                    <For each={deviceGroup(catalog(), "unavailable")}>
                      {(device) => (
                        <option value={device.id}>
                          {device.label} · {device.kind} · unavailable
                        </option>
                      )}
                    </For>
                  </optgroup>
                </Show>
                <Show
                  when={selected().deviceId && !catalog()?.devices.some((device) => device.id === selected().deviceId)}
                >
                  <option value={selected().deviceId}>Missing device: {selected().deviceId}</option>
                </Show>
              </select>
            </label>
            <label class="field">
              Characteristic
              <select
                aria-label="Primary characteristic"
                value={selected().characteristicId}
                disabled={!catalog()?.devices.some((device) => device.id === selected().deviceId)}
                onChange={(event) => updatePrimary(selected().deviceId, event.currentTarget.value)}
              >
                <option value="">Select characteristic</option>
                <For
                  each={characteristicSections(
                    catalog()?.devices.find((device) => device.id === selected().deviceId)?.groups ?? [],
                    catalog()?.devices.find((device) => device.id === selected().deviceId)?.characteristics ?? [],
                  )}
                >
                  {(section) =>
                    section.group ? (
                      <optgroup label={section.group.label}>
                        <For each={section.characteristics}>
                          {(characteristic) => (
                            <option value={characteristic.id}>
                              {characteristic.label}
                              {!characteristic.access.write ? " · read only" : ""}
                            </option>
                          )}
                        </For>
                      </optgroup>
                    ) : (
                      <For each={section.characteristics}>
                        {(characteristic) => (
                          <option value={characteristic.id}>
                            {characteristic.label}
                            {!characteristic.access.write ? " · read only" : ""}
                          </option>
                        )}
                      </For>
                    )
                  }
                </For>
                <Show when={selected().characteristicId && !selectedPrimary()}>
                  <option value={selected().characteristicId}>
                    Missing characteristic: {selected().characteristicId}
                  </option>
                </Show>
              </select>
            </label>
            <div class="characteristic-list" aria-label="Characteristics for selected device">
              <For
                each={characteristicSections(
                  catalog()?.devices.find((device) => device.id === selected().deviceId)?.groups ?? [],
                  catalog()?.devices.find((device) => device.id === selected().deviceId)?.characteristics ?? [],
                )}
              >
                {(section) => (
                  <>
                    <Show when={section.group}>
                      <p class="characteristic-group-label">{section.group?.label}</p>
                    </Show>
                    <For each={section.characteristics}>
                      {(characteristic) => (
                        <button
                          type="button"
                          class={`characteristic-option ${selected().characteristicId === characteristic.id ? "selected" : ""}`}
                          aria-pressed={selected().characteristicId === characteristic.id ? "true" : "false"}
                          onClick={() => updatePrimary(selected().deviceId, characteristic.id)}
                        >
                          <span class="kind-icon" aria-hidden="true">
                            {kindIcon(characteristic.spec.kind)}
                          </span>
                          <span class="characteristic-info">
                            <strong>{characteristic.label}</strong>
                            <small>
                              {currentValue(characteristic)} · {characteristic.access.write ? "Writable" : "Read only"}
                            </small>
                          </span>
                          <span class="characteristic-value">{currentValue(characteristic)}</span>
                          <Show
                            when={
                              characteristic.status !== "ok" ||
                              catalog()?.devices.find((device) => device.id === selected().deviceId)?.status !==
                                "online"
                            }
                          >
                            <span class="unavailable">Unavailable</span>
                          </Show>
                        </button>
                      )}
                    </For>
                  </>
                )}
              </For>
            </div>
            [apps/streamdeck/inspector/src/App.tsx#F5C5]
          </section>
          <CommandEditor
            label="Key / dial behavior"
            slot="key"
            characteristic={selectedPrimary()}
            binding={primaryBinding()}
            command={settings().keyCommand}
            validation={lastValidation()}
            onValidate={validate}
          />
          <section class="panel">
            <p class="eyebrow">PRESS BEHAVIOR</p>
            <h2>Key / dial press</h2>
            <label class="field">
              Press device
              <select
                aria-label="Press device"
                value={selected().pressDeviceId}
                onChange={(event) => selectPressDevice(event.currentTarget.value)}
              >
                <option value="">Use primary behavior</option>
                <Show when={deviceGroup(catalog(), "online").length > 0}>
                  <optgroup label="Online devices">
                    <For each={deviceGroup(catalog(), "online")}>
                      {(device) => (
                        <option value={device.id}>
                          {device.label} · {device.kind}
                        </option>
                      )}
                    </For>
                  </optgroup>
                </Show>
                <Show when={deviceGroup(catalog(), "unavailable").length > 0}>
                  <optgroup label="Unavailable devices">
                    <For each={deviceGroup(catalog(), "unavailable")}>
                      {(device) => <option value={device.id}>{device.label} · unavailable</option>}
                    </For>
                  </optgroup>
                </Show>
                <Show
                  when={
                    selected().pressDeviceId &&
                    !catalog()?.devices.some((device) => device.id === selected().pressDeviceId)
                  }
                >
                  <option value={selected().pressDeviceId}>Missing device: {selected().pressDeviceId}</option>
                </Show>
              </select>
            </label>
            <Show when={selected().pressDeviceId}>
              <label class="field">
                Press characteristic
                <select
                  aria-label="Press characteristic"
                  value={selected().pressCharacteristicId}
                  onChange={(event) => updatePress(selected().pressDeviceId, event.currentTarget.value)}
                >
                  <option value="">Select characteristic</option>
                  <For
                    each={characteristicSections(
                      catalog()?.devices.find((device) => device.id === selected().pressDeviceId)?.groups ?? [],
                      catalog()?.devices.find((device) => device.id === selected().pressDeviceId)?.characteristics ??
                        [],
                    )}
                  >
                    {(section) =>
                      section.group ? (
                        <optgroup label={section.group.label}>
                          <For each={section.characteristics}>
                            {(characteristic) => (
                              <option value={characteristic.id}>
                                {characteristic.label}
                                {!characteristic.access.write ? " · read only" : ""}
                              </option>
                            )}
                          </For>
                        </optgroup>
                      ) : (
                        <For each={section.characteristics}>
                          {(characteristic) => (
                            <option value={characteristic.id}>
                              {characteristic.label}
                              {!characteristic.access.write ? " · read only" : ""}
                            </option>
                          )}
                        </For>
                      )
                    }
                  </For>
                  <Show when={selected().pressCharacteristicId && !selectedPress()}>
                    <option value={selected().pressCharacteristicId}>
                      Missing characteristic: {selected().pressCharacteristicId}
                    </option>
                  </Show>
                </select>
              </label>
            </Show>
            <Show when={!selected().pressDeviceId}>
              <p class="muted">The key press uses the primary command.</p>
            </Show>
            <Show when={selectedPress()}>
              {(characteristic) => (
                <div class="observed">
                  <div>
                    <strong>{characteristic().label}</strong>
                    <span>{currentValue(characteristic())}</span>
                  </div>
                  <Show when={selectedPress()?.status !== "ok"}>
                    <span class="unavailable">Unavailable</span>
                  </Show>
                </div>
              )}
            </Show>
            <Show when={selected().pressDeviceId}>
              <CommandEditor
                label="Secondary press behavior"
                slot="press"
                characteristic={selectedPress()}
                binding={pressBinding()}
                command={settings().pressCommand}
                validation={lastValidation()}
                onValidate={validate}
              />
            </Show>
          </section>
          <p class="footnote">
            Rotation steps a writable number or enum. Commands run only while the service and characteristic are
            available.
          </p>
        </div>
        <aside class="preview panel">
          <p class="eyebrow">LIVE PREVIEW</p>
          <h2>Stream Deck key</h2>
          <div class="key-tile">
            <span class="key-icon">A</span>
            <strong>{preview()?.feedback.title ?? "Configure"}</strong>
            <span>{preview()?.feedback.current ?? "Choose a characteristic"}</span>
            <div class="meter">
              <i style={{ width: `${preview()?.feedback.bar.enabled ? (preview()?.feedback.bar.value ?? 0) : 0}%` }} />
            </div>
            <div class="meter-labels">
              <span>{preview()?.feedback.minimum.value}</span>
              <span>{preview()?.feedback.maximum.value}</span>
            </div>
          </div>
          <p class="preview-note">Preview uses the same display renderer as the plugin.</p>
        </aside>
      </div>
    </main>
  );
}

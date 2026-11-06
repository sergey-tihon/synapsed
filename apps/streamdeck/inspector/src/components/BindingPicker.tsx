import { characteristicSections, formatCharacteristicValue, type Characteristic } from "@synapsed/client";
import { createEffect, For, Show, type Accessor } from "solid-js";
import type { InspectorCharacteristic } from "../../../src/catalog.js";
import type { InspectorDevice } from "../../../src/protocol.js";

type Props = {
  label: string;
  devices: Accessor<InspectorDevice[]>;
  eligible: (characteristic: InspectorCharacteristic) => boolean;
  deviceId: Accessor<string | undefined>;
  characteristicId: Accessor<string | undefined>;
  showDevice: boolean;
  onDevice(deviceId: string | undefined): void;
  onCharacteristic(characteristicId: string | undefined): void;
};

export default function BindingPicker(props: Props) {
  const eligibleDevices = () =>
    props.devices().filter((device) => device.characteristics.some((characteristic) => props.eligible(characteristic)));
  const selectedDevice = () => props.devices().find((device) => device.id === props.deviceId());
  const eligibleCharacteristics = () =>
    selectedDevice()?.characteristics.filter((characteristic) => props.eligible(characteristic)) ?? [];
  const selectedCharacteristic = () =>
    eligibleCharacteristics().find((characteristic) => characteristic.id === props.characteristicId());
  const deviceSelection = () => {
    const id = props.deviceId() ?? "";
    return eligibleDevices().find((device) => device.id === id)?.id ?? id;
  };
  const characteristicSelection = () => {
    const id = props.characteristicId() ?? "";
    return eligibleCharacteristics().find((characteristic) => characteristic.id === id)?.id ?? id;
  };
  const sections = () => characteristicSections(selectedDevice()?.groups ?? [], eligibleCharacteristics());
  let deviceSelect: HTMLSelectElement | undefined;
  let characteristicSelect: HTMLSelectElement | undefined;
  createEffect(
    () => deviceSelection(),
    (value) => {
      if (deviceSelect) deviceSelect.value = value;
    },
  );
  createEffect(
    () => characteristicSelection(),
    (value) => {
      if (characteristicSelect) characteristicSelect.value = value;
    },
  );

  return (
    <div class="binding-picker">
      <Show when={props.showDevice}>
        <label class="field">
          Device
          <select
            ref={(element) => (deviceSelect = element)}
            aria-label={`${props.label} device`}
            value={deviceSelection()}
            onChange={(event) => props.onDevice(event.currentTarget.value || undefined)}
          >
            <option value="">Select device</option>
            <For each={eligibleDevices()}>
              {(device) => (
                <option value={device.id}>
                  {device.label}
                  {device.status !== "online" ? " · unavailable" : ""}
                </option>
              )}
            </For>
            <Show when={props.deviceId() && !eligibleDevices().some((device) => device.id === props.deviceId())}>
              <option value={props.deviceId()}>Missing device: {props.deviceId()}</option>
            </Show>
          </select>
        </label>
      </Show>
      <label class="field">
        {props.label}
        <select
          ref={(element) => (characteristicSelect = element)}
          aria-label={props.label}
          value={characteristicSelection()}
          disabled={!props.deviceId() || !selectedDevice()}
          onChange={(event) => props.onCharacteristic(event.currentTarget.value || undefined)}
        >
          <option value="">{props.showDevice ? "Select characteristic" : "Nothing"}</option>
          <For each={sections()}>
            {(section) =>
              section.group ? (
                <optgroup label={section.group.label}>
                  <For each={section.characteristics}>
                    {(characteristic) => <option value={characteristic.id}>{characteristic.label}</option>}
                  </For>
                </optgroup>
              ) : (
                <For each={section.characteristics}>
                  {(characteristic) => <option value={characteristic.id}>{characteristic.label}</option>}
                </For>
              )
            }
          </For>
          <Show
            when={
              props.characteristicId() &&
              !eligibleCharacteristics().some((characteristic) => characteristic.id === props.characteristicId())
            }
          >
            <option value={props.characteristicId()}>Missing characteristic: {props.characteristicId()}</option>
          </Show>
        </select>
      </label>
      <Show when={selectedCharacteristic()}>
        {(characteristic) => (
          <p class="observed">Current: {formatCharacteristicValue(characteristic() as unknown as Characteristic)}</p>
        )}
      </Show>
    </div>
  );
}

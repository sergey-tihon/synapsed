import { Show } from "solid-js";
import { initialCommand } from "../../../src/commands.js";
import { keyEligible } from "../../../src/eligibility.js";
import type { InspectorCharacteristic } from "../../../src/catalog.js";
import type { KeySettings } from "../../../src/settings.js";
import BindingPicker from "./BindingPicker.js";
import CommandEditor from "./CommandEditor.js";
import { bridge } from "../streamdeck.js";
import { update } from "../settings.js";

export default function KeyInspector() {
  const settings = () => bridge.settings() as KeySettings;
  const devices = () => bridge.catalog()?.devices ?? [];
  const selectedDevice = () => devices().find((device) => device.id === settings().deviceId);
  const selectedCharacteristic = () =>
    selectedDevice()?.characteristics.find((characteristic) => characteristic.id === settings().characteristicId);
  const titlePlaceholder = () => {
    const device = selectedDevice();
    const characteristic = selectedCharacteristic();
    return device && characteristic ? `${device.label} · ${characteristic.label}` : "Device · characteristic";
  };

  function selectCharacteristic(characteristicId: string | undefined): void {
    const characteristic = selectedDevice()?.characteristics.find((item) => item.id === characteristicId);
    update({
      characteristicId,
      command: characteristic && keyEligible(characteristic) ? initialCommand(characteristic) : undefined,
    });
  }

  return (
    <>
      <section class="panel">
        <h2>Title</h2>
        <label class="field">
          Custom title
          <input
            aria-label="Custom title"
            type="text"
            maxlength="64"
            placeholder={titlePlaceholder()}
            value={settings().title ?? ""}
            onInput={(event) =>
              update({ title: event.currentTarget.value === "" ? undefined : event.currentTarget.value })
            }
          />
        </label>
        <p class="muted">Leave empty to show the device and characteristic.</p>
      </section>

      <section class="panel">
        <h2>Characteristic</h2>
        <BindingPicker
          label="Characteristic"
          devices={devices}
          eligible={keyEligible}
          deviceId={() => settings().deviceId}
          characteristicId={() => settings().characteristicId}
          showDevice={true}
          onDevice={(deviceId) => update({ deviceId, characteristicId: undefined, command: undefined })}
          onCharacteristic={selectCharacteristic}
        />
      </section>

      <section class="panel">
        <h2>When pressed</h2>
        <Show when={selectedCharacteristic()} fallback={<p class="muted">Choose a characteristic first.</p>}>
          {(characteristic) => (
            <CommandEditor
              characteristic={() => characteristic() as InspectorCharacteristic}
              command={() => settings().command}
              onChange={(command) => update({ command })}
            />
          )}
        </Show>
      </section>
    </>
  );
}

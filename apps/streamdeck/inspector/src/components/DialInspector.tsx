import { Show } from "solid-js";
import { initialCommand } from "../../../src/commands.js";
import { pressEligible, rotateEligible } from "../../../src/eligibility.js";
import type { DialSettings } from "../../../src/settings.js";
import BindingPicker from "./BindingPicker.js";
import CommandEditor from "./CommandEditor.js";
import { bridge } from "../streamdeck.js";
import { update } from "../settings.js";

export default function DialInspector() {
  const settings = () => bridge.settings() as DialSettings;
  const devices = () => bridge.catalog()?.devices ?? [];
  const selectedDevice = () => devices().find((device) => device.id === settings().deviceId);
  const selectedCharacteristic = () =>
    selectedDevice()?.characteristics.find((characteristic) => characteristic.id === settings().characteristicId);
  const selectedPress = () => {
    const characteristic = selectedDevice()?.characteristics.find(
      (item) => item.id === settings().pressCharacteristicId,
    );
    return characteristic && pressEligible(characteristic) ? characteristic : undefined;
  };
  const titlePlaceholder = () => {
    const device = selectedDevice();
    const characteristic = selectedCharacteristic();
    return device && characteristic ? `${device.label} · ${characteristic.label}` : "Device · characteristic";
  };

  function selectPressCharacteristic(pressCharacteristicId: string | undefined): void {
    const characteristic = selectedDevice()?.characteristics.find((item) => item.id === pressCharacteristicId);
    update({
      pressCharacteristicId,
      pressCommand: characteristic && pressEligible(characteristic) ? initialCommand(characteristic) : undefined,
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
        <h2>Rotate</h2>
        <p class="muted">Each tick steps the value by one step.</p>
        <BindingPicker
          label="Characteristic"
          devices={devices}
          eligible={rotateEligible}
          deviceId={() => settings().deviceId}
          characteristicId={() => settings().characteristicId}
          showDevice={true}
          onDevice={(deviceId) =>
            update({
              deviceId,
              characteristicId: undefined,
              pressCharacteristicId: undefined,
              pressCommand: undefined,
            })
          }
          onCharacteristic={(characteristicId) => update({ characteristicId })}
        />
      </section>

      <Show when={settings().deviceId}>
        <section class="panel">
          <h2>Press</h2>
          <BindingPicker
            label="Press characteristic"
            devices={() => {
              const device = selectedDevice();
              return device ? [device] : [];
            }}
            eligible={pressEligible}
            deviceId={() => settings().deviceId}
            characteristicId={() => settings().pressCharacteristicId}
            showDevice={false}
            onDevice={() => {}}
            onCharacteristic={selectPressCharacteristic}
          />
          <Show when={selectedDevice() && !selectedDevice()?.characteristics.some(pressEligible)}>
            <p class="muted">This device has no on/off or action controls.</p>
          </Show>
          <Show when={selectedPress()}>
            {(characteristic) => (
              <CommandEditor
                characteristic={() => characteristic()}
                command={() => settings().pressCommand}
                onChange={(command) => update({ pressCommand: command })}
              />
            )}
          </Show>
        </section>
      </Show>
    </>
  );
}

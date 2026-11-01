import { createEffect, createSignal, For, Show } from "solid-js";
import type { CharacteristicCommand } from "@synapsed/client";
import type { InspectorCharacteristic } from "../../../src/catalog.js";
import type { Binding } from "../../../src/settings.js";

export type ValidationState = {
  slot: "key" | "press";
  error: string | null;
  command: CharacteristicCommand | null;
} | null;

type Props = {
  label: string;
  slot: "key" | "press";
  characteristic: InspectorCharacteristic | null;
  binding: Binding | null;
  command: CharacteristicCommand | undefined;
  validation: ValidationState;
  onValidate: (
    slot: Props["slot"],
    binding: Binding | null,
    op: "default" | "set" | "step" | "toggle" | "invoke",
    text?: string,
  ) => void;
};

function commandText(command: CharacteristicCommand | undefined): string {
  if (!command) return "";
  if (command.op === "set") return String(command.value);
  return "";
}
function currentNumber(characteristic: InspectorCharacteristic): number | "" {
  const value = characteristic.value;
  return typeof value === "number" ? value : "";
}

export default function CommandEditor(props: Props) {
  const [draft, setDraft] = createSignal(commandText(props.command));
  createEffect(
    () => props.command,
    (command) => {
      setDraft(commandText(command));
    },
  );
  const submit = (op: "set" | "step" | "toggle" | "invoke", text?: string) =>
    props.onValidate(props.slot, props.binding, op, text);
  const numberSpec = () => (props.characteristic?.spec.kind === "number" ? props.characteristic.spec : null);
  const numberBounded = () => {
    const spec = numberSpec();
    return spec !== null && spec.minimum !== null && spec.maximum !== null && spec.maximum > spec.minimum;
  };
  const stringSpec = () => (props.characteristic?.spec.kind === "string" ? props.characteristic.spec : null);
  const validation = () => (props.validation?.slot === props.slot ? props.validation : null);
  const stepDescription = () => {
    const spec = numberSpec();
    return spec ? `${spec.step ?? "Any"} step${spec.unit ? ` · ${spec.unit}` : ""}` : "";
  };

  return (
    <section class="command-editor">
      <div class="section-heading">
        <h3>{props.label}</h3>
        <button class="quiet" onClick={() => props.onValidate(props.slot, props.binding, "default")}>
          Use default behavior
        </button>
      </div>
      <Show
        when={props.characteristic}
        fallback={<p class="muted">Choose an available characteristic to edit its command.</p>}
      >
        {(characteristic) => (
          <>
            <p class="spec-note">
              {characteristic().label} · {characteristic().access.write ? "Writable" : "Read only"}
            </p>
            <Show when={characteristic().spec.kind === "number"}>
              <div class="control-row">
                <Show when={numberBounded()}>
                  <input
                    aria-label={`${props.label} value slider`}
                    type="range"
                    min={numberSpec()?.minimum ?? 0}
                    max={numberSpec()?.maximum ?? 100}
                    step={numberSpec()?.step ?? 1}
                    value={draft() !== "" ? draft() : String(characteristic().value ?? numberSpec()?.minimum ?? 0)}
                    disabled={!characteristic().access.write}
                    onInput={(event) => {
                      setDraft(event.currentTarget.value);
                      submit("set", event.currentTarget.value);
                    }}
                  />
                </Show>
                <input
                  aria-label={`${props.label} numeric value`}
                  type="number"
                  min={numberSpec()?.minimum ?? undefined}
                  max={numberSpec()?.maximum ?? undefined}
                  step={numberSpec()?.step ?? 1}
                  value={draft() !== "" ? draft() : currentNumber(characteristic())}
                  disabled={!characteristic().access.write}
                  onInput={(event) => {
                    setDraft(event.currentTarget.value);
                    submit("set", event.currentTarget.value);
                  }}
                />
              </div>
              <div class="control-row step-control">
                <label>
                  Step count
                  <input
                    aria-label={`${props.label} step count`}
                    type="number"
                    min="-100"
                    max="100"
                    step="1"
                    value={props.command?.op === "step" ? props.command.steps : 1}
                    disabled={!characteristic().operations.includes("step")}
                    onInput={(event) => submit("step", event.currentTarget.value)}
                  />
                </label>
                <span class="spec-note">{stepDescription()}</span>
              </div>
            </Show>
            <Show when={characteristic().spec.kind === "boolean"}>
              <div class="control-row">
                <button disabled={!characteristic().access.write} onClick={() => submit("set", "true")}>
                  Set On
                </button>
                <button disabled={!characteristic().access.write} onClick={() => submit("set", "false")}>
                  Set Off
                </button>
                <button disabled={!characteristic().operations.includes("toggle")} onClick={() => submit("toggle")}>
                  Toggle
                </button>
              </div>
            </Show>
            <Show when={characteristic().spec.kind === "enum"}>
              <label class="field">
                Enum value
                <select
                  aria-label={`${props.label} enum value`}
                  value={
                    props.command?.op === "set" ? String(props.command.value) : String(characteristic().value ?? "")
                  }
                  disabled={!characteristic().access.write}
                  onChange={(event) => submit("set", event.currentTarget.value)}
                >
                  <For each={props.characteristic?.spec.kind === "enum" ? props.characteristic.spec.options : []}>
                    {(option) => <option value={option.value}>{option.label}</option>}
                  </For>
                </select>
              </label>
              <Show when={characteristic().operations.includes("step")}>
                <button onClick={() => submit("step", "1")}>Step enum</button>
              </Show>
            </Show>
            <Show when={characteristic().spec.kind === "string"}>
              <label class="field">
                Text value
                <input
                  aria-label={`${props.label} string value`}
                  type="text"
                  maxlength={stringSpec()?.maxLength}
                  value={props.command?.op === "set" ? String(props.command.value) : draft()}
                  disabled={!characteristic().access.write}
                  onInput={(event) => {
                    setDraft(event.currentTarget.value);
                    submit("set", event.currentTarget.value);
                  }}
                />
              </label>
            </Show>
            <Show when={characteristic().spec.kind === "action"}>
              <button disabled={!characteristic().operations.includes("invoke")} onClick={() => submit("invoke")}>
                Invoke
              </button>
            </Show>
            <Show when={validation()}>
              {(current) => (
                <p class={current().error ? "validation error" : "validation"} role="status">
                  {current().error ?? "Command validated"}
                </p>
              )}
            </Show>
          </>
        )}
      </Show>
    </section>
  );
}

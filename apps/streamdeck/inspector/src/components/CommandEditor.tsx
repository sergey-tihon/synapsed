import { CharacteristicValueError, type CharacteristicCommand } from "@synapsed/client";
import { createEffect, createSignal, For, Show, type Accessor } from "solid-js";
import type { InspectorCharacteristic } from "../../../src/catalog.js";
import {
  commandForMode,
  commandModes,
  modeLabel,
  modeOf,
  parseSetValue,
  type CommandMode,
} from "../../../src/commands.js";

type Props = {
  characteristic: Accessor<InspectorCharacteristic>;
  command: Accessor<CharacteristicCommand | undefined>;
  onChange(command: CharacteristicCommand | undefined): void;
};

function setText(command: CharacteristicCommand | undefined): string {
  return command?.op === "set" ? String(command.value) : "";
}
function stepText(command: CharacteristicCommand | undefined): string {
  return command?.op === "step" ? String(Math.abs(command.steps)) : "1";
}

export default function CommandEditor(props: Props) {
  const [valueDraft, setValueDraft] = createSignal("");
  const [stepDraft, setStepDraft] = createSignal("1");
  const [error, setError] = createSignal<string>();
  const modes = () => commandModes(props.characteristic());
  const mode = () => modeOf(props.command()) ?? (modes().length === 1 ? modes()[0] : undefined);
  const numberSpec = () => {
    const spec = props.characteristic().spec;
    return spec.kind === "number" ? spec : undefined;
  };
  const enumSpec = () => {
    const spec = props.characteristic().spec;
    return spec.kind === "enum" ? spec : undefined;
  };
  const stringSpec = () => {
    const spec = props.characteristic().spec;
    return spec.kind === "string" ? spec : undefined;
  };

  createEffect(
    () => props.command(),
    (command) => {
      setValueDraft(setText(command));
      setStepDraft(stepText(command));
    },
  );

  function updateSet(text: string): void {
    setValueDraft(text);
    try {
      props.onChange(parseSetValue(props.characteristic(), text));
      setError(undefined);
    } catch (cause) {
      if (!(cause instanceof CharacteristicValueError)) throw cause;
      setError(cause.message);
    }
  }

  function updateSteps(text: string): void {
    setStepDraft(text);
    const steps = Number(text);
    if (!text || !Number.isInteger(steps) || steps < 1 || steps > 100) {
      setError("Enter a whole number from 1 to 100.");
      return;
    }
    setError(undefined);
    props.onChange({ op: "step", steps: mode() === "decrease" ? -steps : steps });
  }

  return (
    <div class="command-editor">
      <Show when={modes().length > 1}>
        <label class="field">
          Command
          <select
            aria-label="Command mode"
            value={mode() ?? ""}
            onChange={(event) => {
              const selected = event.currentTarget.value as CommandMode;
              props.onChange(commandForMode(props.characteristic(), selected, props.command()));
              setError(undefined);
            }}
          >
            <For each={modes()}>
              {(item) => <option value={item}>{modeLabel(props.characteristic(), item)}</option>}
            </For>
          </select>
        </label>
      </Show>
      <Show when={props.characteristic().spec.kind === "action"}>
        <p class="muted">Runs {props.characteristic().label}</p>
      </Show>
      <Show when={mode() === "increase" || mode() === "decrease"}>
        <label class="field">
          Steps
          <input
            aria-label="Steps"
            type="number"
            min="1"
            max="100"
            step="1"
            value={stepDraft()}
            onInput={(event) => updateSteps(event.currentTarget.value)}
          />
        </label>
        <Show when={numberSpec()?.step != null}>
          <p class="spec-note">
            1 step = {numberSpec()?.step} {numberSpec()?.unit ?? ""}
          </p>
        </Show>
      </Show>
      <Show when={mode() === "set" && enumSpec()}>
        <label class="field">
          Value
          <select
            aria-label="Set value"
            value={setText(props.command())}
            onChange={(event) => updateSet(event.currentTarget.value)}
          >
            <For each={enumSpec()?.options ?? []}>
              {(option) => <option value={option.value}>{option.label}</option>}
            </For>
          </select>
        </label>
      </Show>
      <Show when={mode() === "set" && numberSpec()}>
        <label class="field">
          Value
          <input
            aria-label="Set value"
            type="number"
            min={numberSpec()?.minimum ?? undefined}
            max={numberSpec()?.maximum ?? undefined}
            step={numberSpec()?.step ?? "any"}
            value={valueDraft()}
            onInput={(event) => updateSet(event.currentTarget.value)}
          />
        </label>
      </Show>
      <Show when={mode() === "set" && stringSpec()}>
        <label class="field">
          Value
          <input
            aria-label="Set value"
            type="text"
            maxlength={stringSpec()?.maxLength}
            value={valueDraft()}
            onInput={(event) => updateSet(event.currentTarget.value)}
          />
        </label>
      </Show>
      <Show when={error()}>
        {(message) => (
          <p class="error-text" role="alert">
            {message()}
          </p>
        )}
      </Show>
      <Show when={modes().length === 0}>
        <p class="muted">This characteristic is not writable.</p>
      </Show>
    </div>
  );
}

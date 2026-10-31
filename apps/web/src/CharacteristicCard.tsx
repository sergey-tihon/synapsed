import { createMemo, createSignal, For, Show } from "solid-js";
import {
  describeSpec,
  formatCharacteristicValue,
  isCommandApplicable,
  parseCharacteristicValue,
  statusSuffix,
  supportsStep,
  type Characteristic,
  type CharacteristicCommand,
} from "@synapsed/client";
import { characteristicElementId } from "./router";

interface Props {
  deviceId: string;
  characteristic: Characteristic;
  available: boolean;
  stale: boolean;
  pending: boolean;
  result: string | undefined;
  focusTarget: boolean;
  execute(command: CharacteristicCommand): Promise<boolean>;
}

export default function CharacteristicCard(props: Props) {
  // null means untouched: display the latest observation, not a fabricated draft.
  const [draft, setDraft] = createSignal<string | null>(null);
  const [inputError, setInputError] = createSignal("");
  const characteristicId = () => characteristicElementId(props.deviceId, props.characteristic.id);
  const value = createMemo(
    () => draft() ?? (props.characteristic.value === null ? "" : String(props.characteristic.value)),
  );
  const disabled = () => !props.available || props.pending;
  const spec = () => props.characteristic.spec;
  const numberSpec = createMemo(() => {
    const current = spec();
    return current.kind === "number" ? current : null;
  });
  const enumSpec = createMemo(() => {
    const current = spec();
    return current.kind === "enum" ? current : null;
  });
  const descriptionId = () => `${characteristicId()}-description`;
  const errorId = () => `${characteristicId()}-error`;
  const describedBy = () => `${descriptionId()} ${errorId()}`;
  const rangeValue = createMemo(() => {
    const currentSpec = numberSpec();
    const raw = draft() ?? props.characteristic.value;
    if (!currentSpec || raw === null || raw === "") return null;
    try {
      const parsed = parseCharacteristicValue(currentSpec, String(raw));
      if (typeof parsed !== "number") return null;
      if (currentSpec.minimum !== null && parsed < currentSpec.minimum) return null;
      if (currentSpec.maximum !== null && parsed > currentSpec.maximum) return null;
      return parsed;
    } catch {
      return null;
    }
  });
  const rangeAvailable = createMemo(() => {
    const currentSpec = numberSpec();
    return (
      !!currentSpec &&
      currentSpec.minimum !== null &&
      currentSpec.maximum !== null &&
      currentSpec.minimum < currentSpec.maximum &&
      rangeValue() !== null
    );
  });
  const rangeStep = () => {
    const currentSpec = numberSpec();
    return currentSpec ? (currentSpec.step ?? (currentSpec.integer ? 1 : "any")) : undefined;
  };
  const selectedEnumIndex = () => {
    if (draft() === null && props.characteristic.value === null) return -1;
    return enumSpec()?.options.findIndex((option) => option.value === value()) ?? -1;
  };

  function edit(next: string) {
    setDraft(next);
    setInputError("");
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (disabled()) return;
    try {
      if (draft() === null && props.characteristic.value === null)
        throw new Error("Enter a value explicitly before applying it.");
      const command: CharacteristicCommand = {
        op: "set",
        value: parseCharacteristicValue(spec(), value()),
      };
      if (!isCommandApplicable(props.characteristic, command))
        throw new Error("This value cannot be set with the current metadata.");
      setInputError("");
      if (await props.execute(command)) setDraft(null);
    } catch (error) {
      setInputError(error instanceof Error ? error.message : "Invalid value.");
    }
  }

  return (
    <article
      class="characteristic-card"
      id={characteristicId()}
      aria-busy={props.pending ? "true" : "false"}
      tabindex={props.focusTarget ? -1 : undefined}
    >
      <div class="characteristic-heading">
        <div>
          <h3>{props.characteristic.label}</h3>
          <p class="identifier">{props.characteristic.id}</p>
        </div>
        <span class="badge">{props.characteristic.access.write ? "Writable" : "Read-only"}</span>
      </div>
      <p class="observed-value">{formatCharacteristicValue(props.characteristic)}</p>
      <p class="observation-state">
        {props.stale
          ? "Stale · last known value"
          : props.characteristic.status === "ok"
            ? "Confirmed observation"
            : "Stale · last observation is not current"}
        {statusSuffix(props.characteristic) && !props.characteristic.error
          ? ` · ${statusSuffix(props.characteristic)}`
          : ""}
      </p>
      {props.characteristic.error ? (
        <p class="error">
          {props.characteristic.error.code}: {props.characteristic.error.message}
        </p>
      ) : null}
      <p id={descriptionId()} class="characteristic-description">
        {describeSpec(spec()) || (spec().kind === "action" ? "Action · no stored value" : "No additional constraints")}
      </p>
      <Show when={props.characteristic.access.write && spec().kind !== "action"}>
        <form class="characteristic-form" onSubmit={submit} novalidate>
          <Show when={spec().kind !== "boolean"}>
            <label for={`${characteristicId()}-value`}>Requested value</label>
          </Show>
          <Show when={spec().kind === "boolean"}>
            <label class="checkbox-label">
              <input
                id={`${characteristicId()}-value`}
                type="checkbox"
                checked={value() === "true"}
                disabled={disabled()}
                aria-describedby={describedBy()}
                onChange={(event) => edit(String(event.currentTarget.checked))}
              />
              Enabled
            </label>
            <Show when={props.characteristic.value === null && draft() === null}>
              <button type="button" disabled={disabled()} onClick={() => edit("false")}>
                Choose false
              </button>
            </Show>
          </Show>
          <Show when={enumSpec()}>
            <select
              id={`${characteristicId()}-value`}
              value={String(selectedEnumIndex())}
              disabled={disabled()}
              aria-describedby={describedBy()}
              onChange={(event) => {
                const selected = enumSpec()?.options[Number(event.currentTarget.value)];
                if (selected) edit(selected.value);
              }}
            >
              <option value="-1" disabled>
                Select a declared value
              </option>
              <For each={enumSpec()?.options ?? []}>
                {(option, index) => <option value={String(index())}>{option.label || '"" (empty label)'}</option>}
              </For>
            </select>
          </Show>
          <Show when={spec().kind === "number" || spec().kind === "string"}>
            <input
              id={`${characteristicId()}-value`}
              type="text"
              value={value()}
              disabled={disabled()}
              inputmode={numberSpec() ? "decimal" : "text"}
              aria-describedby={describedBy()}
              aria-invalid={inputError() ? "true" : "false"}
              onInput={(event) => edit(event.currentTarget.value)}
            />
            <Show when={rangeAvailable()}>
              <label class="range-label" for={`${characteristicId()}-range`}>
                Adjust requested value
              </label>
              <input
                id={`${characteristicId()}-range`}
                type="range"
                min={numberSpec()?.minimum ?? undefined}
                max={numberSpec()?.maximum ?? undefined}
                step={rangeStep()}
                value={rangeValue() ?? ""}
                disabled={disabled()}
                aria-describedby={describedBy()}
                onInput={(event) => edit(event.currentTarget.value)}
              />
            </Show>
            <Show when={spec().kind === "string" && props.characteristic.value === null && draft() === null}>
              <button type="button" disabled={disabled()} onClick={() => edit("")}>
                Choose empty string
              </button>
            </Show>
          </Show>
          <button type="submit" disabled={disabled()}>
            Apply
          </button>
          <p id={errorId()} class="error">
            {inputError()}
          </p>
        </form>
      </Show>
      <Show when={props.characteristic.access.write}>
        <div class="characteristic-commands">
          <Show when={supportsStep(props.characteristic)}>
            <button type="button" disabled={disabled()} onClick={() => void props.execute({ op: "step", steps: -1 })}>
              Decrease
            </button>
            <button type="button" disabled={disabled()} onClick={() => void props.execute({ op: "step", steps: 1 })}>
              Increase
            </button>
          </Show>
          <Show when={isCommandApplicable(props.characteristic, { op: "toggle" })}>
            <button type="button" disabled={disabled()} onClick={() => void props.execute({ op: "toggle" })}>
              Toggle
            </button>
          </Show>
          <Show when={isCommandApplicable(props.characteristic, { op: "invoke" })}>
            <button type="button" disabled={disabled()} onClick={() => void props.execute({ op: "invoke" })}>
              Invoke
            </button>
          </Show>
        </div>
      </Show>
      <Show when={props.pending}>
        <p class="command-feedback">Pending · waiting for observed confirmation…</p>
      </Show>
      <Show when={props.result}>
        <p class="command-feedback">{props.result}</p>
      </Show>
    </article>
  );
}

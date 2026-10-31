import { createMemo, createSignal, For, Show } from "solid-js";
import {
  isCommandApplicable,
  parseCharacteristicValue,
  supportsStep,
  type Characteristic,
  type CharacteristicCommand,
} from "@synapsed/client";
import { characteristicAnchor } from "./anchors";

export function observedValue(characteristic: Characteristic): string {
  if (characteristic.spec.kind === "action") return "No stored value";
  if (characteristic.value === null) return "Not yet observed";
  if (characteristic.spec.kind === "enum") {
    return (
      characteristic.spec.options.find((option) => option.value === characteristic.value)?.label ??
      String(characteristic.value)
    );
  }
  if (characteristic.value === "") return '"" (empty string)';
  return `${String(characteristic.value)}${characteristic.spec.kind === "number" && characteristic.spec.unit ? ` ${characteristic.spec.unit}` : ""}`;
}

function metadata(characteristic: Characteristic): string {
  const spec = characteristic.spec;
  switch (spec.kind) {
    case "number":
      return [
        spec.integer ? "Integer" : "Number (fractional values supported)",
        spec.minimum === null ? "no minimum" : `minimum ${spec.minimum}`,
        spec.maximum === null ? "no maximum" : `maximum ${spec.maximum}`,
        spec.step === null ? "no relative step" : `step ${spec.step}`,
        spec.unit ? `unit ${spec.unit}` : "",
      ]
        .filter(Boolean)
        .join(" · ");
    case "boolean":
      return "Boolean";
    case "enum":
      return "Declared choice";
    case "string":
      return `String · up to ${spec.maxLength} Unicode characters · empty string allowed`;
    case "action":
      return "Action · no arguments";
  }
}

interface Props {
  deviceId: string;
  characteristic: Characteristic;
  available: boolean;
  stale: boolean;
  pending: boolean;
  result: string | undefined;
  execute(command: CharacteristicCommand): Promise<boolean>;
}

export default function CharacteristicCard(props: Props) {
  // null means untouched: display the latest observation, not a fabricated draft.
  const [draft, setDraft] = createSignal<string | null>(null);
  const [inputError, setInputError] = createSignal("");
  const text = createMemo(
    () => draft() ?? (props.characteristic.value === null ? "" : String(props.characteristic.value)),
  );
  const disabled = () => !props.available || props.pending;
  const inputId = () => `${characteristicAnchor(props.deviceId, props.characteristic.id).slice(1)}-value`;
  function edit(value: string) {
    setDraft(value);
    setInputError("");
  }
  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (disabled()) return;
    try {
      if (draft() === null && props.characteristic.value === null) {
        throw new Error("Enter a value explicitly before applying it.");
      }
      const command: CharacteristicCommand = {
        op: "set",
        value: parseCharacteristicValue(props.characteristic.spec, text()),
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
      class="characteristic"
      id={characteristicAnchor(props.deviceId, props.characteristic.id).slice(1)}
      aria-busy={props.pending ? "true" : "false"}
    >
      <div class="row">
        <h4>
          <a href={characteristicAnchor(props.deviceId, props.characteristic.id)}>{props.characteristic.label}</a>
        </h4>
        <span class="badge">{props.characteristic.access.write ? "Writable" : "Read-only"}</span>
      </div>
      <p class="identifier">{props.characteristic.id}</p>
      <p class="value">{observedValue(props.characteristic)}</p>
      <p class="muted">
        {props.stale
          ? "Stale · last known value"
          : props.characteristic.status !== "ok"
            ? "Stale · last observation is not current"
            : props.characteristic.spec.kind === "action"
              ? "Ready for invocation"
              : props.characteristic.value === null
                ? "No observation yet"
                : "Confirmed observation"}{" "}
        · {props.characteristic.status}
      </p>
      <Show when={props.characteristic.error}>
        <p class="error">
          {props.characteristic.error?.code}: {props.characteristic.error?.message}
        </p>
      </Show>
      <p class="metadata">
        {metadata(props.characteristic)} · {props.characteristic.access.read ? "read" : "no read"}
        {props.characteristic.access.notify ? " · notify" : ""}
      </p>
      <Show when={props.characteristic.access.write}>
        <Show when={props.characteristic.spec.kind !== "action"}>
          <form onSubmit={submit} novalidate>
            <label for={inputId()}>Requested value</label>
            <Show when={props.characteristic.spec.kind === "boolean"}>
              <label class="checkbox">
                <input
                  id={inputId()}
                  type="checkbox"
                  checked={text() === "true"}
                  disabled={disabled()}
                  onChange={(event) => edit(String(event.currentTarget.checked))}
                />{" "}
                Enabled
              </label>
              <Show when={props.characteristic.value === null && draft() === null}>
                <button type="button" disabled={disabled()} onClick={() => edit("false")}>
                  Choose false
                </button>
              </Show>
            </Show>
            <Show when={props.characteristic.spec.kind === "enum"}>
              <select
                id={inputId()}
                value={String(
                  props.characteristic.spec.kind === "enum" && (draft() !== null || props.characteristic.value !== null)
                    ? props.characteristic.spec.options.findIndex((option) => option.value === text())
                    : -1,
                )}
                disabled={disabled()}
                onChange={(event) => {
                  const spec = props.characteristic.spec;
                  if (spec.kind === "enum") {
                    const option = spec.options[Number(event.currentTarget.value)];
                    if (option) edit(option.value);
                  }
                }}
              >
                <option value="-1" disabled>
                  Select a declared value
                </option>
                <For each={props.characteristic.spec.kind === "enum" ? props.characteristic.spec.options : []}>
                  {(option, index) => <option value={String(index())}>{option.label || '"" (empty label)'}</option>}
                </For>
              </select>
            </Show>
            <Show when={props.characteristic.spec.kind === "number" || props.characteristic.spec.kind === "string"}>
              <input
                id={inputId()}
                type="text"
                value={text()}
                disabled={disabled()}
                inputmode={props.characteristic.spec.kind === "number" ? "decimal" : "text"}
                onInput={(event) => edit(event.currentTarget.value)}
                aria-invalid={inputError() ? "true" : "false"}
              />
              <Show
                when={
                  props.characteristic.spec.kind === "string" && props.characteristic.value === null && draft() === null
                }
              >
                <button type="button" disabled={disabled()} onClick={() => edit("")}>
                  Choose empty string
                </button>
              </Show>
            </Show>
            <button type="submit" disabled={disabled()}>
              Apply
            </button>
          </form>
        </Show>
        <div class="commands">
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
        <p role="status">Pending · waiting for observed confirmation…</p>
      </Show>
      <Show when={inputError()}>
        <p class="error" role="alert">
          {inputError()}
        </p>
      </Show>
      <Show when={props.result}>
        <p role="status">{props.result}</p>
      </Show>
    </article>
  );
}

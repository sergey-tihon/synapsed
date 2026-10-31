import { useState } from "react";
import { Action, ActionPanel, Form, openExtensionPreferences } from "@raycast/api";
import {
  isCommandApplicable,
  parseCharacteristicValue,
  supportsStep,
  type Characteristic,
  type CharacteristicCommand,
} from "@synapsed/client";
import { bindingKey, displayValue, Session, useSession } from "./session";

export function CommandActions({
  session,
  deviceId,
  characteristic,
  enabled,
}: {
  session: Session;
  deviceId: string;
  characteristic: Characteristic;
  enabled: boolean;
}) {
  const commands: { title: string; command: CharacteristicCommand }[] = [];
  if (supportsStep(characteristic))
    commands.push(
      { title: "Decrease", command: { op: "step", steps: -1 } },
      { title: "Increase", command: { op: "step", steps: 1 } },
    );
  if (isCommandApplicable(characteristic, { op: "toggle" }))
    commands.push({ title: "Toggle", command: { op: "toggle" } });
  if (isCommandApplicable(characteristic, { op: "invoke" }))
    commands.push({ title: "Invoke", command: { op: "invoke" } });
  return (
    <>
      {enabled &&
        commands.map(({ title, command }) => (
          <Action key={title} title={title} onAction={() => session.execute(deviceId, characteristic.id, command)} />
        ))}
    </>
  );
}
export default function CharacteristicForm({
  session,
  deviceId,
  characteristicId,
}: {
  session: Session;
  deviceId: string;
  characteristicId: string;
}) {
  const state = useSession(session);
  const device = state.snapshot?.devices.find((item) => item.id === deviceId);
  const characteristic = device?.characteristics.find((item) => item.id === characteristicId);
  const [text, setText] = useState<string | undefined>();
  const [boolean, setBoolean] = useState<boolean | undefined>();
  const [error, setError] = useState<string>();
  const pending = state.pending.has(bindingKey(deviceId, characteristicId));
  const enabled = state.connection === "connected" && device?.status === "online" && !pending;
  if (!characteristic)
    return (
      <Form>
        <Form.Description title="Missing" text="This characteristic is no longer declared by the daemon." />
      </Form>
    );
  const spec = characteristic.spec;
  const writable = characteristic.access.write && spec.kind !== "action";
  const initialText = characteristic.value === null ? "" : String(characteristic.value);
  const submittedText =
    spec.kind === "boolean" ? String(boolean ?? characteristic.value === true) : (text ?? initialText);
  const validate = (value: string) => {
    try {
      parseCharacteristicValue(spec, value);
      setError(undefined);
      return true;
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "Invalid value");
      return false;
    }
  };
  async function submit() {
    const value = submittedText;
    if (!validate(value)) return;
    await session.execute(deviceId, characteristicId, {
      op: "set",
      value: parseCharacteristicValue(spec, value),
    });
  }
  return (
    <Form
      isLoading={pending}
      navigationTitle={characteristic.label}
      actions={
        <ActionPanel>
          {writable && enabled && <Action.SubmitForm title="Apply Value" onSubmit={submit} />}
          <CommandActions session={session} deviceId={deviceId} characteristic={characteristic} enabled={enabled} />
          <Action title="Open Extension Preferences" onAction={openExtensionPreferences} />
        </ActionPanel>
      }
    >
      <Form.Description
        title="Confirmed observation"
        text={`${displayValue(characteristic)} · ${characteristic.status}${state.connection !== "connected" ? " · Stale" : ""}`}
      />
      <Form.Description title="Service" text={`${state.connection}${state.error ? ` · ${state.error}` : ""}`} />
      {(device?.error || characteristic.error) && (
        <Form.Description
          title="Device / characteristic error"
          text={[device?.error?.message, characteristic.error?.message].filter(Boolean).join(" · ")}
        />
      )}
      <Form.Description
        title="Metadata"
        text={JSON.stringify({
          id: characteristic.id,
          spec,
          access: characteristic.access,
        })}
      />
      {!writable && (
        <Form.Description
          title="Access"
          text={spec.kind === "action" ? "No-argument action" : "Read only; no mutation available"}
        />
      )}
      {writable && enabled && spec.kind === "boolean" && (
        <Form.Checkbox
          id="value"
          title="Value"
          label={characteristic.label}
          value={boolean ?? characteristic.value === true}
          onChange={(value) => {
            setBoolean(value);
            setError(undefined);
          }}
        />
      )}
      {writable && enabled && spec.kind === "enum" && (
        <Form.Dropdown
          id="value"
          title="Value"
          value={text ?? initialText}
          error={error}
          onChange={(value) => {
            setText(value);
            validate(value);
          }}
        >
          {spec.options.map((option) => (
            <Form.Dropdown.Item key={option.value} value={option.value} title={option.label} />
          ))}
        </Form.Dropdown>
      )}
      {writable && enabled && (spec.kind === "number" || spec.kind === "string") && (
        <Form.TextField
          id="value"
          title="Value"
          value={text ?? initialText}
          error={error}
          onChange={(value) => {
            setText(value);
            validate(value);
          }}
          info={
            spec.kind === "string"
              ? `Maximum ${spec.maxLength} Unicode characters; empty strings are allowed.`
              : `${spec.integer ? "Integer" : "Finite number"}; bounds ${spec.minimum ?? "none"} to ${spec.maximum ?? "none"}${spec.unit ? ` ${spec.unit}` : ""}`
          }
        />
      )}
      {!enabled && (
        <Form.Description
          title="Writes disabled"
          text={pending ? "Waiting for observed confirmation" : "Service offline/incompatible or device unavailable"}
        />
      )}
    </Form>
  );
}

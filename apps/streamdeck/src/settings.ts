import { isCharacteristicCommand, type CharacteristicCommand } from "@synapsed/client";

export const ID_PATTERN = /^[a-z][a-z0-9_.-]{0,63}$/;

export type Binding = { deviceId: string; characteristicId: string };
export type ActionSettings = {
  binding?: Binding;
  keyCommand?: CharacteristicCommand;
  pressBinding?: Binding;
  pressCommand?: CharacteristicCommand;
};

export function isBinding(value: unknown): value is Binding {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const binding = value as Record<string, unknown>;
  return (
    Object.keys(binding).length === 2 &&
    typeof binding.deviceId === "string" &&
    ID_PATTERN.test(binding.deviceId) &&
    typeof binding.characteristicId === "string" &&
    ID_PATTERN.test(binding.characteristicId)
  );
}

export function settingsConform(settings: ActionSettings): boolean {
  return (
    (settings.binding === undefined || isBinding(settings.binding)) &&
    (settings.pressBinding === undefined || isBinding(settings.pressBinding)) &&
    (settings.keyCommand === undefined || isCharacteristicCommand(settings.keyCommand)) &&
    (settings.pressCommand === undefined || isCharacteristicCommand(settings.pressCommand))
  );
}

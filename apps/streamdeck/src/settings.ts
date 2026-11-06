import { isCharacteristicCommand, type CharacteristicCommand } from "@synapsed/client";

export const ID_PATTERN = /^[a-z][a-z0-9_.-]{0,63}$/;
export const MAX_TITLE_LENGTH = 64;

export type Binding = { deviceId: string; characteristicId: string };
export type ActionKind = "key" | "dial";
export const KEY_ACTION_UUID = "com.sergey-tihon.synapsed.key";
export const DIAL_ACTION_UUID = "com.sergey-tihon.synapsed.dial";
export type KeySettings = {
  deviceId?: string;
  characteristicId?: string;
  command?: CharacteristicCommand;
  title?: string;
};
export type DialSettings = {
  deviceId?: string;
  characteristicId?: string;
  pressCharacteristicId?: string;
  pressCommand?: CharacteristicCommand;
  title?: string;
};
export type ActionSettings = KeySettings | DialSettings;

export function actionKind(uuid: string | undefined): ActionKind | undefined {
  if (uuid === KEY_ACTION_UUID) return "key";
  if (uuid === DIAL_ACTION_UUID) return "dial";
  return undefined;
}

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

function optionalId(value: string | undefined): boolean {
  return value === undefined || (typeof value === "string" && ID_PATTERN.test(value));
}

function optionalCommand(value: CharacteristicCommand | undefined): boolean {
  return value === undefined || isCharacteristicCommand(value);
}

function optionalTitle(value: string | undefined): boolean {
  return value === undefined || (typeof value === "string" && value.length <= MAX_TITLE_LENGTH);
}

export function keySettingsConform(settings: KeySettings): boolean {
  return (
    optionalId(settings.deviceId) &&
    optionalId(settings.characteristicId) &&
    optionalCommand(settings.command) &&
    optionalTitle(settings.title)
  );
}

export function dialSettingsConform(settings: DialSettings): boolean {
  return (
    optionalId(settings.deviceId) &&
    optionalId(settings.characteristicId) &&
    optionalId(settings.pressCharacteristicId) &&
    optionalCommand(settings.pressCommand) &&
    optionalTitle(settings.title)
  );
}

export function keyBinding(settings: KeySettings): Binding | undefined {
  if (settings.deviceId === undefined || settings.characteristicId === undefined) return undefined;
  return { deviceId: settings.deviceId, characteristicId: settings.characteristicId };
}

export function rotateBinding(settings: DialSettings): Binding | undefined {
  if (settings.deviceId === undefined || settings.characteristicId === undefined) return undefined;
  return { deviceId: settings.deviceId, characteristicId: settings.characteristicId };
}

export function pressBinding(settings: DialSettings): Binding | undefined {
  if (settings.deviceId === undefined || settings.pressCharacteristicId === undefined) return undefined;
  return { deviceId: settings.deviceId, characteristicId: settings.pressCharacteristicId };
}

export function customTitle(settings: { title?: string }): string | undefined {
  if (typeof settings.title !== "string") return undefined;
  const title = settings.title.trim();
  return title ? title : undefined;
}

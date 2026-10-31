import type { CharacteristicCommand } from "@synapsed/client";
import type { ActionSettings } from "./settings.js";

export function clampDialTicks(ticks: number): number {
  return Math.max(-100, Math.min(100, Math.trunc(ticks)));
}

export function mergeValidatedCommand(
  settings: ActionSettings,
  slot: "key" | "press",
  command: CharacteristicCommand | null,
): ActionSettings {
  const next = { ...settings };
  const property = slot === "key" ? "keyCommand" : "pressCommand";
  if (command) next[property] = command;
  else delete next[property];
  return next;
}

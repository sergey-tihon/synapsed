import type { ActionSettings, DialSettings, KeySettings } from "../../src/settings.js";
import { bridge, setSettings } from "./streamdeck.js";

export type SettingsPatch = Partial<KeySettings & DialSettings>;

function clean(settings: Record<string, unknown>): ActionSettings {
  for (const [key, value] of Object.entries(settings)) {
    if (value === undefined) delete settings[key];
  }
  return settings as ActionSettings;
}

export function update(patch: SettingsPatch): void {
  setSettings(clean({ ...bridge.settings(), ...patch }));
}

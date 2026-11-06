import { describe, expect, it } from "vitest";
import {
  DIAL_ACTION_UUID,
  KEY_ACTION_UUID,
  actionKind,
  customTitle,
  dialSettingsConform,
  isBinding,
  keyBinding,
  keySettingsConform,
  pressBinding,
  rotateBinding,
} from "../src/settings.js";

const binding = { deviceId: "monitor", characteristicId: "brightness" };

describe("action settings", () => {
  it("validates IDs, commands and title length while ignoring unknown settings", () => {
    expect(keySettingsConform({ deviceId: "monitor", command: { op: "set", value: 42 } })).toBe(true);
    expect(keySettingsConform({ deviceId: "Bad ID" })).toBe(false);
    expect(keySettingsConform({ title: "x".repeat(65) })).toBe(false);
    expect(keySettingsConform({ title: 7 } as never)).toBe(false);
    expect(dialSettingsConform({ title: "x".repeat(64), extra: true } as never)).toBe(true);
    expect(keySettingsConform({ command: { op: "unknown" } as never })).toBe(false);
  });

  it("creates bindings only when their required device and characteristic IDs exist", () => {
    expect(isBinding(binding)).toBe(true);
    expect(isBinding({ ...binding, extra: true })).toBe(false);
    expect(isBinding({ deviceId: "../monitor", characteristicId: "brightness" })).toBe(false);
    expect(isBinding(null)).toBe(false);
    expect(keyBinding({ deviceId: "monitor" })).toBeUndefined();
    expect(keyBinding(binding)).toEqual(binding);
    expect(rotateBinding({ deviceId: "monitor" })).toBeUndefined();
    expect(pressBinding({ pressCharacteristicId: "power" })).toBeUndefined();
    expect(pressBinding({ deviceId: "monitor", pressCharacteristicId: "power" })).toEqual({
      deviceId: "monitor",
      characteristicId: "power",
    });
  });

  it("maps only the new action UUIDs and trims optional titles", () => {
    expect(actionKind(KEY_ACTION_UUID)).toBe("key");
    expect(actionKind(DIAL_ACTION_UUID)).toBe("dial");
    expect(actionKind("com.sergey-tihon.synapsed.characteristic")).toBeUndefined();
    expect(customTitle({ title: "  Desk up  " })).toBe("Desk up");
    expect(customTitle({ title: "   " })).toBeUndefined();
  });
});

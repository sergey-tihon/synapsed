import type { JsonValue } from "@elgato/utils";
import type { SendToPluginEvent } from "@elgato/streamdeck";
import {
  action,
  SingletonAction,
  type DialDownEvent,
  type DialRotateEvent,
  type DidReceiveSettingsEvent,
  type KeyDownEvent,
  type PropertyInspectorDidAppearEvent,
  type PropertyInspectorDidDisappearEvent,
  type WillAppearEvent,
  type WillDisappearEvent,
} from "@elgato/streamdeck";
import { DIAL_ACTION_UUID, KEY_ACTION_UUID, type DialSettings, type KeySettings } from "./settings.js";
import type { Contexts } from "./contexts.js";
import { clampDialTicks } from "./model.js";

@action({ UUID: KEY_ACTION_UUID })
export class KeyAction extends SingletonAction<KeySettings> {
  constructor(private readonly contexts: Contexts) {
    super();
  }

  override onWillAppear(ev: WillAppearEvent<KeySettings>): void {
    this.contexts.appear(ev.action, "key", ev.payload.settings);
  }

  override onWillDisappear(ev: WillDisappearEvent<KeySettings>): void {
    this.contexts.disappear(ev.action.id);
  }

  override onDidReceiveSettings(ev: DidReceiveSettingsEvent<KeySettings>): void {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
  }

  override onPropertyInspectorDidAppear(ev: PropertyInspectorDidAppearEvent<KeySettings>): Promise<void> {
    return this.contexts.inspectorAppeared(ev.action);
  }

  override onPropertyInspectorDidDisappear(ev: PropertyInspectorDidDisappearEvent<KeySettings>): void {
    this.contexts.inspectorDisappeared(ev.action.id);
  }

  override onSendToPlugin(ev: SendToPluginEvent<JsonValue, KeySettings>): void {
    this.contexts.inspectorRequest(ev.action.id, ev.payload);
  }

  override onKeyDown(ev: KeyDownEvent<KeySettings>): Promise<void> {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
    return this.contexts.trigger(ev.action.id, "key");
  }
}

@action({ UUID: DIAL_ACTION_UUID })
export class DialAction extends SingletonAction<DialSettings> {
  constructor(private readonly contexts: Contexts) {
    super();
  }

  override onWillAppear(ev: WillAppearEvent<DialSettings>): void {
    this.contexts.appear(ev.action, "dial", ev.payload.settings);
  }

  override onWillDisappear(ev: WillDisappearEvent<DialSettings>): void {
    this.contexts.disappear(ev.action.id);
  }

  override onDidReceiveSettings(ev: DidReceiveSettingsEvent<DialSettings>): void {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
  }

  override onPropertyInspectorDidAppear(ev: PropertyInspectorDidAppearEvent<DialSettings>): Promise<void> {
    return this.contexts.inspectorAppeared(ev.action);
  }

  override onPropertyInspectorDidDisappear(ev: PropertyInspectorDidDisappearEvent<DialSettings>): void {
    this.contexts.inspectorDisappeared(ev.action.id);
  }

  override onSendToPlugin(ev: SendToPluginEvent<JsonValue, DialSettings>): void {
    this.contexts.inspectorRequest(ev.action.id, ev.payload);
  }

  override onDialDown(ev: DialDownEvent<DialSettings>): Promise<void> {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
    return this.contexts.trigger(ev.action.id, "press");
  }

  override onDialRotate(ev: DialRotateEvent<DialSettings>): Promise<void> {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
    const steps = clampDialTicks(ev.payload.ticks);
    if (!Number.isFinite(steps) || steps === 0) return Promise.resolve();
    return this.contexts.trigger(ev.action.id, "rotate", steps);
  }
}

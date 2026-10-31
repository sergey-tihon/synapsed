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
import type { ActionSettings } from "./settings.js";
import type { Contexts } from "./contexts.js";
import { clampDialTicks } from "./model.js";

@action({ UUID: "com.sergey-tihon.synapsed.characteristic" })
export class CharacteristicAction extends SingletonAction<ActionSettings> {
  constructor(private readonly contexts: Contexts) {
    super();
  }

  override onWillAppear(ev: WillAppearEvent<ActionSettings>): void {
    this.contexts.appear(ev.action, ev.payload.settings);
  }

  override onWillDisappear(ev: WillDisappearEvent<ActionSettings>): void {
    this.contexts.disappear(ev.action.id);
  }

  override onDidReceiveSettings(ev: DidReceiveSettingsEvent<ActionSettings>): void {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
  }

  override onPropertyInspectorDidAppear(ev: PropertyInspectorDidAppearEvent<ActionSettings>): Promise<void> {
    return this.contexts.inspectorAppeared(ev.action);
  }

  override onPropertyInspectorDidDisappear(ev: PropertyInspectorDidDisappearEvent<ActionSettings>): void {
    this.contexts.inspectorDisappeared(ev.action.id);
  }

  override onSendToPlugin(ev: SendToPluginEvent<JsonValue, ActionSettings>): void {
    this.contexts.inspectorRequest(ev.action.id, ev.payload);
  }

  override onKeyDown(ev: KeyDownEvent<ActionSettings>): Promise<void> {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
    return this.contexts.trigger(ev.action.id, "key");
  }

  override onDialDown(ev: DialDownEvent<ActionSettings>): Promise<void> {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
    return this.contexts.trigger(ev.action.id, "press");
  }

  override onDialRotate(ev: DialRotateEvent<ActionSettings>): Promise<void> {
    this.contexts.settingsChanged(ev.action.id, ev.payload.settings);
    const steps = clampDialTicks(ev.payload.ticks);
    if (!Number.isFinite(steps) || steps === 0) return Promise.resolve();
    return this.contexts.trigger(ev.action.id, "rotate", steps);
  }
}

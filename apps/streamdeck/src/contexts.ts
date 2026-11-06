import streamDeck, { type Action } from "@elgato/streamdeck";
import {
  isCommandApplicable,
  type CharacteristicCommand,
  type ClientStatus,
  type SynapsedClient,
  type Snapshot,
} from "@synapsed/client";
import { buildCatalog } from "./catalog.js";
import type { InspectorRequest } from "./protocol.js";
import { keyEligible, pressEligible, rotateEligible } from "./eligibility.js";
import { renderDial, renderKey, resolveBinding } from "./render.js";
import {
  dialSettingsConform,
  keyBinding,
  keySettingsConform,
  pressBinding,
  rotateBinding,
  type ActionKind,
  type ActionSettings,
  type Binding,
  type DialSettings,
  type KeySettings,
} from "./settings.js";

function keyImage(
  binding: Binding | undefined,
  conform: boolean,
  snapshot: Snapshot | undefined,
  status: ClientStatus,
): string {
  if (!conform || !binding) return "imgs/keys/configure.svg";
  const resolved = resolveBinding(snapshot, binding);
  if (
    status !== "connected" ||
    !resolved ||
    resolved.device.status !== "online" ||
    resolved.characteristic.status !== "ok"
  )
    return "imgs/keys/offline.svg";
  const { characteristic } = resolved;
  if (characteristic.spec.kind === "boolean")
    return characteristic.value === true ? "imgs/keys/boolean-on.svg" : "imgs/keys/boolean-off.svg";
  return `imgs/keys/${characteristic.spec.kind}.svg`;
}

type VisibleContext = {
  action: Action<ActionSettings>;
  kind: ActionKind;
  settings: ActionSettings;
  active: boolean;
  version: number;
  rendering: boolean;
  controllers: Set<AbortController>;
};

type Inspector = {
  action: Action<ActionSettings>;
  active: boolean;
  appearances: number;
  sending: boolean;
  queued: boolean;
};

function bindingKey(binding: Binding): string {
  return `${binding.deviceId}/${binding.characteristicId}`;
}

/** One demand-owned subscription; all visible keys, dials and the open PI share accepted state. */
export class Contexts {
  private readonly visible = new Map<string, VisibleContext>();
  private readonly inspectors = new Map<string, Inspector>();
  private readonly pending = new Map<string, number>();
  private snapshot: Snapshot | undefined;
  private status: ClientStatus;
  private unsubscribe: (() => void) | undefined;
  private generation = 0;
  private closed = false;

  constructor(
    private readonly client: SynapsedClient | undefined,
    private readonly configurationError?: string,
  ) {
    this.status = client ? "disconnected" : "incompatible";
  }

  appear(action: Action<ActionSettings>, kind: ActionKind, settings: ActionSettings): void {
    this.retireVisible(action.id);
    const context: VisibleContext = {
      action,
      kind,
      settings,
      active: true,
      version: 0,
      rendering: false,
      controllers: new Set(),
    };
    this.visible.set(action.id, context);
    this.reconcileDemand();
    this.queueRender(context);
  }

  disappear(id: string): void {
    this.retireVisible(id);
    // SDK removes the action before routing PI disappearance, so that event may never arrive.
    this.retireInspector(id);
    this.reconcileDemand();
  }

  private retireVisible(id: string): void {
    const context = this.visible.get(id);
    if (context) {
      context.active = false;
      for (const controller of context.controllers) controller.abort();
      context.controllers.clear();
      this.visible.delete(id);
    }
  }

  settingsChanged(id: string, settings: ActionSettings): void {
    const context = this.visible.get(id);
    if (context) {
      context.settings = settings;
      this.queueRender(context);
    }
  }

  async inspectorAppeared(action: Action<ActionSettings>): Promise<void> {
    let inspector = this.inspectors.get(action.id);
    if (
      inspector &&
      inspector.action.manifestId === action.manifestId &&
      inspector.action.device.id === action.device.id
    ) {
      inspector.appearances += 1;
    } else {
      for (const id of this.inspectors.keys()) this.retireInspector(id);
      inspector = {
        action,
        active: true,
        appearances: 1,
        sending: false,
        queued: false,
      };
      this.inspectors.set(action.id, inspector);
    }
    this.reconcileDemand();
    this.queueCatalog(inspector);
  }

  inspectorDisappeared(id: string): void {
    const inspector = this.inspectors.get(id);
    if (inspector && --inspector.appearances <= 0) this.retireInspector(id);
    this.reconcileDemand();
  }

  private retireInspector(id: string): void {
    const inspector = this.inspectors.get(id);
    if (inspector) {
      inspector.active = false;
      this.inspectors.delete(id);
    }
  }

  inspectorRequest(id: string, payload: unknown): void {
    const inspector = this.inspectors.get(id);
    if (!inspector || !inspector.active || !payload || typeof payload !== "object" || Array.isArray(payload)) return;
    const request = payload as Partial<InspectorRequest>;
    if (request.event === "getCatalog") this.queueCatalog(inspector);
  }

  async trigger(id: string, slot: "key" | "rotate" | "press", steps?: number): Promise<void> {
    const context = this.visible.get(id);
    if (!context?.active || !this.client || this.status !== "connected") return;
    let binding: Binding | undefined;
    let command: CharacteristicCommand | undefined;
    if (slot === "key") {
      if (context.kind !== "key") return;
      const settings = context.settings as KeySettings;
      if (!keySettingsConform(settings)) return;
      binding = keyBinding(settings);
      command = settings.command;
    } else {
      if (context.kind !== "dial") return;
      const settings = context.settings as DialSettings;
      if (!dialSettingsConform(settings)) return;
      binding = slot === "rotate" ? rotateBinding(settings) : pressBinding(settings);
      if (slot === "press") command = settings.pressCommand;
    }
    const resolved = resolveBinding(this.snapshot, binding);
    if (!binding || !resolved) return;
    const { device, characteristic } = resolved;
    if (device.status !== "online" || characteristic.status !== "ok") return;
    if (slot === "rotate") {
      if (!rotateEligible(characteristic) || steps === undefined || steps === 0) return;
      command = { op: "step", steps };
    } else if (slot === "press" && !pressEligible(characteristic)) {
      return;
    }
    if (!command || !isCommandApplicable(characteristic, command)) return;
    const key = bindingKey(binding);
    this.pending.set(key, (this.pending.get(key) ?? 0) + 1);
    const controller = new AbortController();
    context.controllers.add(controller);
    this.renderAll();
    try {
      // Accepted HTTP observations pass through the shared client's ordering gate/subscription.
      // Never render the initiating response: a later WS observation may already be current.
      streamDeck.logger.info(`Sending ${command.op} to ${binding.deviceId}/${binding.characteristicId}`);
      await this.client.execute(binding.deviceId, binding.characteristicId, command, { signal: controller.signal });
    } catch (error) {
      if (context.active && !controller.signal.aborted) {
        streamDeck.logger.error(
          `Characteristic command failed: ${error instanceof Error ? error.message : "Request failed"}`,
        );
        try {
          if (context.action.isKey() || context.action.isDial()) await context.action.showAlert();
        } catch {
          /* Context may have disappeared. */
        }
      }
    } finally {
      context.controllers.delete(controller);
      const remaining = (this.pending.get(key) ?? 1) - 1;
      if (remaining > 0) this.pending.set(key, remaining);
      else this.pending.delete(key);
      this.renderAll();
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.generation += 1;
    for (const context of this.visible.values()) {
      context.active = false;
      for (const controller of context.controllers) controller.abort();
    }
    for (const inspector of this.inspectors.values()) inspector.active = false;
    this.visible.clear();
    this.inspectors.clear();
    this.unsubscribe?.();
    this.unsubscribe = undefined;
    this.client?.close();
  }

  private reconcileDemand(): void {
    if (this.closed) return;
    const demand = this.visible.size > 0 || this.inspectors.size > 0;
    if (!demand && this.unsubscribe) {
      this.generation += 1;
      this.unsubscribe();
      this.unsubscribe = undefined;
      this.status = "disconnected";
    } else if (demand && !this.unsubscribe && this.client) {
      const generation = ++this.generation;
      this.status = "connecting";
      this.unsubscribe = this.client.subscribe({
        onSnapshot: (snapshot) => {
          if (generation !== this.generation || this.closed) return;
          this.snapshot = snapshot;
          this.refreshAll();
        },
        onStatus: (status) => {
          if (generation !== this.generation || this.closed) return;
          this.status = status;
          this.refreshAll();
        },
        onError: () => {
          /* Connection state is rendered, never alert repeatedly during reconnect/polls. */
        },
        onLogs: () => {},
        onLog: () => {},
      });
    }
  }

  private refreshAll(): void {
    this.renderAll();
    for (const inspector of this.inspectors.values()) this.queueCatalog(inspector);
  }

  private renderAll(): void {
    for (const context of this.visible.values()) this.queueRender(context);
  }

  private queueRender(context: VisibleContext): void {
    context.version += 1;
    if (!context.active || context.rendering) return;
    context.rendering = true;
    void this.renderLoop(context);
  }

  private async renderLoop(context: VisibleContext): Promise<void> {
    try {
      let rendered = -1;
      while (context.active && rendered !== context.version) {
        rendered = context.version;
        const bindings =
          context.kind === "key"
            ? [keyBinding(context.settings as KeySettings)]
            : [rotateBinding(context.settings as DialSettings), pressBinding(context.settings as DialSettings)];
        const pending = bindings.some((binding) => binding && this.pending.has(bindingKey(binding)));
        if (context.kind === "key" && context.action.isKey()) {
          const settings = context.settings as KeySettings;
          await context.action.setImage(
            keyImage(keyBinding(settings), keySettingsConform(settings), this.snapshot, this.status),
          );
          await context.action.setTitle(renderKey(settings, this.snapshot, this.status, pending));
        } else if (context.kind === "dial" && context.action.isDial()) {
          const settings = context.settings as DialSettings;
          await context.action.setImage(
            keyImage(rotateBinding(settings), dialSettingsConform(settings), this.snapshot, this.status),
          );
          await context.action.setFeedback(renderDial(settings, this.snapshot, this.status, pending));
        }
      }
    } catch {
      // Render failures are not command failures and must not flash alerts on polling.
    } finally {
      context.rendering = false;
    }
  }

  private queueCatalog(inspector: Inspector): void {
    inspector.queued = true;
    if (!inspector.active || inspector.sending) return;
    inspector.sending = true;
    void this.catalogLoop(inspector);
  }

  private async catalogLoop(inspector: Inspector): Promise<void> {
    try {
      while (inspector.active && inspector.queued) {
        inspector.queued = false;
        const catalog = buildCatalog(this.snapshot, this.status, this.configurationError);
        if (!inspector.active || streamDeck.ui.action?.id !== inspector.action.id) break;
        await streamDeck.ui.sendToPropertyInspector({ event: "catalog", ...catalog });
      }
    } catch {
      // A closed/replaced inspector is not a user command failure.
    } finally {
      inspector.sending = false;
    }
  }
}

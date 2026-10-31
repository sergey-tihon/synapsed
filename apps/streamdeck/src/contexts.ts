import streamDeck, { type Action } from "@elgato/streamdeck";
import {
  defaultCommand,
  isCommandApplicable,
  supportsStep,
  type CharacteristicCommand,
  type ClientStatus,
  type SynapsedClient,
  type Snapshot,
} from "@synapsed/client";
import { buildCatalog } from "./catalog.js";
import { isSelection, selectionFromSettings, type InspectorRequest } from "./protocol.js";
import { renderDisplay, resolveBinding } from "./render.js";
import { settingsConform, type ActionSettings, type Binding } from "./settings.js";
import { validateInspectorCommand } from "./inspector-command.js";

function keyImage(settings: ActionSettings, snapshot: Snapshot | undefined, status: ClientStatus): string {
  if (!settingsConform(settings) || !settings.binding) return "imgs/keys/configure.svg";
  const resolved = resolveBinding(snapshot, settings.binding);
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
  selection: ReturnType<typeof selectionFromSettings>;
  requested: boolean;
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

  appear(action: Action<ActionSettings>, settings: ActionSettings): void {
    this.retireVisible(action.id);
    const context: VisibleContext = {
      action,
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
    const inspector = this.inspectors.get(id);
    if (inspector) {
      inspector.selection = selectionFromSettings(settings);
      this.queueCatalog(inspector);
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
      const settings = this.visible.get(action.id)?.settings ?? {};
      inspector = {
        action,
        active: true,
        appearances: 1,
        sending: false,
        queued: false,
        requested: false,
        selection: selectionFromSettings(settings),
      };
      this.inspectors.set(action.id, inspector);
    }
    this.reconcileDemand();
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
    if (request.event === "catalog") {
      if (isSelection(request.selection)) {
        inspector.requested = true;
        inspector.selection = request.selection;
        this.queueCatalog(inspector);
      }
      return;
    }
    if (request.event === "validateCommand")
      void this.validateInspectorCommand(inspector, request as Record<string, unknown>);
  }

  private async validateInspectorCommand(inspector: Inspector, request: Record<string, unknown>): Promise<void> {
    if (request.slot !== "key" && request.slot !== "press") return;
    if (typeof request.requestId !== "number" || !Number.isSafeInteger(request.requestId)) return;
    const requestId = request.requestId;
    const slot = request.slot;
    const { command, error } = validateInspectorCommand(this.snapshot, request.binding, request.op, request.text);
    if (!inspector.active || streamDeck.ui.action?.id !== inspector.action.id) return;
    try {
      await streamDeck.ui.sendToPropertyInspector({ event: "commandValidation", slot, requestId, command, error });
    } catch {
      /* The inspector may have closed. */
    }
  }

  async trigger(id: string, kind: "key" | "press" | "rotate", steps?: number): Promise<void> {
    const context = this.visible.get(id);
    if (!context?.active || !this.client || this.status !== "connected" || !settingsConform(context.settings)) return;
    const settings = context.settings;
    const useSecondary =
      kind === "press" && (settings.pressBinding !== undefined || settings.pressCommand !== undefined);
    const binding = useSecondary ? (settings.pressBinding ?? settings.binding) : settings.binding;
    const resolved = resolveBinding(this.snapshot, binding);
    if (!binding || !resolved) return;
    const { device, characteristic } = resolved;
    if (device.status !== "online" || characteristic.status !== "ok") return;
    let command: CharacteristicCommand | null;
    if (kind === "rotate") {
      if (settings.keyCommand && !isCommandApplicable(characteristic, settings.keyCommand)) return;
      if (!supportsStep(characteristic) || steps === undefined || steps === 0) return;
      command = { op: "step", steps };
    } else {
      command = useSecondary
        ? (settings.pressCommand ?? defaultCommand(characteristic))
        : (settings.keyCommand ?? defaultCommand(characteristic));
    }
    if (!command || !isCommandApplicable(characteristic, command) || (command.op === "step" && command.steps === 0))
      return;
    const key = bindingKey(binding);
    this.pending.set(key, (this.pending.get(key) ?? 0) + 1);
    const controller = new AbortController();
    context.controllers.add(controller);
    this.renderAll();
    try {
      // Accepted HTTP observations pass through the shared client's ordering gate/subscription.
      // Never render the initiating response: a later WS observation may already be current.
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
        const bindings = [context.settings.binding, context.settings.pressBinding];
        const pending = bindings.some((binding) => binding && this.pending.has(bindingKey(binding)));
        const display = renderDisplay(context.settings, this.snapshot, this.status, pending);
        if (context.action.isKey() || context.action.isDial()) {
          await context.action.setImage(keyImage(context.settings, this.snapshot, this.status));
        }
        if (context.action.isDial()) await context.action.setFeedback(display.feedback);
        else if (context.action.isKey()) await context.action.setTitle(display.title);
      }
    } catch {
      // Render failures are not command failures and must not flash alerts on polling.
    } finally {
      context.rendering = false;
    }
  }

  private queueCatalog(inspector: Inspector): void {
    inspector.queued = true;
    if (!inspector.active || !inspector.requested || inspector.sending) return;
    inspector.sending = true;
    void this.catalogLoop(inspector);
  }

  private async catalogLoop(inspector: Inspector): Promise<void> {
    try {
      while (inspector.active && inspector.queued) {
        inspector.queued = false;
        const catalog = buildCatalog(this.snapshot, this.status, inspector.selection, this.configurationError);
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

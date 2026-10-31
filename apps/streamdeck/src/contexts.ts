import streamDeck, { type Action } from "@elgato/streamdeck";
import {
  defaultCommand,
  isCommandApplicable,
  parseCharacteristicValue,
  supportsStep,
  type Characteristic,
  type CharacteristicCommand,
  type ClientStatus,
  type SynapsedClient,
  type Snapshot,
} from "@synapsed/client";
import { renderDisplay, resolveBinding } from "./render.js";
import { isBinding, settingsConform, type ActionSettings, type Binding } from "./settings.js";

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
  settings: ActionSettings;
  active: boolean;
  appearances: number;
  version: number;
  sending: boolean;
  queued: boolean;
  selection?: { deviceId: string; characteristicId: string; pressDeviceId: string; pressCharacteristicId: string };
};

function bindingKey(binding: Binding): string {
  return `${binding.deviceId}/${binding.characteristicId}`;
}

/** Strip additive unknown fields before sending public metadata through the SDK's JSON API. */
function inspectorCharacteristic(characteristic: Characteristic | undefined) {
  if (!characteristic) return null;
  const spec = characteristic.spec;
  const metadata =
    spec.kind === "number"
      ? {
          kind: spec.kind,
          minimum: spec.minimum,
          maximum: spec.maximum,
          step: spec.step,
          integer: spec.integer,
          unit: spec.unit,
        }
      : spec.kind === "enum"
        ? { kind: spec.kind, options: spec.options.map(({ value, label }) => ({ value, label })) }
        : spec.kind === "string"
          ? { kind: spec.kind, maxLength: spec.maxLength }
          : { kind: spec.kind };
  const operations: string[] = [];
  if (characteristic.access.write && spec.kind !== "action") operations.push("set");
  if (supportsStep(characteristic)) operations.push("step");
  if (characteristic.access.write && spec.kind === "boolean") operations.push("toggle");
  if (characteristic.access.write && spec.kind === "action") operations.push("invoke");
  return {
    id: characteristic.id,
    label: characteristic.label,
    spec: metadata,
    access: {
      read: characteristic.access.read,
      write: characteristic.access.write,
      notify: characteristic.access.notify,
    },
    value: characteristic.value,
    status: characteristic.status,
    operations,
    defaultCommand: defaultCommand(characteristic),
    error: characteristic.error ? { code: characteristic.error.code, message: characteristic.error.message } : null,
  };
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
      inspector.settings = settings;
      inspector.version += 1;
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
      // Match SDK3's current-PI appearance stack, including overlapping same-context events.
      inspector.appearances += 1;
    } else {
      for (const id of this.inspectors.keys()) this.retireInspector(id);
      inspector = {
        action,
        settings: this.visible.get(action.id)?.settings ?? {},
        active: true,
        appearances: 1,
        version: 0,
        sending: false,
        queued: false,
      };
      this.inspectors.set(action.id, inspector);
    }
    inspector.version += 1;
    this.reconcileDemand();
    this.queueCatalog(inspector);
    const version = inspector.version;
    try {
      const settings = await action.getSettings();
      if (inspector.active && inspector.version === version) {
        inspector.settings = settings;
        this.queueCatalog(inspector);
      }
    } catch {
      // No hardware read or initialization settings write; the PI can request its own settings.
    }
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
    const request = payload as Record<string, unknown>;
    if (request.event === "validateCommand") {
      void this.validateInspectorCommand(inspector, request);
      return;
    }
    if (
      !["getDevices", "getCharacteristics", "getPressDevices", "getPressCharacteristics", "getCatalog"].includes(
        String(request.event),
      )
    )
      return;
    const selection = request.selection;
    if (selection && typeof selection === "object" && !Array.isArray(selection)) {
      const fields = selection as Record<string, unknown>;
      if (
        ["deviceId", "characteristicId", "pressDeviceId", "pressCharacteristicId"].every(
          (field) =>
            typeof fields[field] === "string" &&
            (fields[field] === "" || /^[a-z][a-z0-9_.-]{0,63}$/.test(fields[field] as string)),
        )
      ) {
        inspector.selection = fields as Inspector["selection"];
      }
    }
    this.queueCatalog(inspector);
  }

  private async validateInspectorCommand(inspector: Inspector, request: Record<string, unknown>): Promise<void> {
    if (request.slot !== "key" && request.slot !== "press") return;
    if (typeof request.requestId !== "number" || !Number.isSafeInteger(request.requestId)) return;
    const requestId = request.requestId;
    const slot = request.slot;
    let command: CharacteristicCommand | null = null;
    let error: string | null = null;
    try {
      const binding = request.binding;
      if (!isBinding(binding)) throw new Error("Select a device and characteristic first.");
      const resolved = resolveBinding(this.snapshot, binding);
      if (!resolved) throw new Error("The selected characteristic is missing.");
      const characteristic = resolved.characteristic;
      switch (request.op) {
        case "default":
          break;
        case "set":
          if (typeof request.text !== "string" || request.text.length > 4096)
            throw new Error("Enter a bounded characteristic value.");
          command = { op: "set", value: parseCharacteristicValue(characteristic.spec, request.text) };
          break;
        case "step":
          if (typeof request.text !== "string" || !/^[+-]?\d+$/.test(request.text))
            throw new Error("Enter a whole step count from -100 to 100.");
          command = { op: "step", steps: Number(request.text) };
          break;
        case "toggle":
          command = { op: "toggle" };
          break;
        case "invoke":
          command = { op: "invoke" };
          break;
        default:
          throw new Error("Select a supported operation.");
      }
      if (command && !isCommandApplicable(characteristic, command))
        throw new Error("The command is not supported by current metadata.");
    } catch (failure) {
      error = failure instanceof Error ? failure.message : "Invalid characteristic command.";
    }
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
        if (!context.active) break;
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
    if (!inspector.active || inspector.sending) return;
    inspector.sending = true;
    void this.catalogLoop(inspector);
  }

  private async catalogLoop(inspector: Inspector): Promise<void> {
    try {
      while (inspector.active && inspector.queued) {
        inspector.queued = false;
        const settings = inspector.settings;
        const selection = inspector.selection ?? {
          deviceId: settings.binding?.deviceId ?? "",
          characteristicId: settings.binding?.characteristicId ?? "",
          pressDeviceId: settings.pressBinding?.deviceId ?? "",
          pressCharacteristicId: settings.pressBinding?.characteristicId ?? "",
        };
        const devices = this.snapshot?.devices ?? [];
        const deviceItems = devices.map((device) => ({
          value: device.id,
          label: `${device.label} (${device.kind})${device.status !== "online" ? " — unavailable" : ""}`,
        }));
        const withMissing = (
          items: { value: string; label: string; disabled?: boolean }[],
          selected: string,
          placeholder: string,
        ) => {
          const result = [{ value: "", label: placeholder }, ...items];
          if (selected && !result.some((item) => item.value === selected))
            result.push({ value: selected, label: `Missing: ${selected}`, disabled: true });
          return result;
        };
        const primaryDevice = devices.find((device) => device.id === selection.deviceId);
        const pressDevice = devices.find((device) => device.id === selection.pressDeviceId);
        const itemsFor = (device: typeof primaryDevice) =>
          device?.characteristics.map((characteristic) => ({
            value: characteristic.id,
            label: `${characteristic.label}${!characteristic.access.write ? " (read-only)" : ""}${characteristic.status !== "ok" ? ` — ${characteristic.status}` : ""}`,
          })) ?? [];
        const primary = primaryDevice?.characteristics.find((item) => item.id === selection.characteristicId);
        const press = pressDevice?.characteristics.find((item) => item.id === selection.pressCharacteristicId);
        const payloads = [
          { event: "getDevices", items: withMissing(deviceItems, selection.deviceId, "Configure — select device") },
          {
            event: "getCharacteristics",
            items: withMissing(itemsFor(primaryDevice), selection.characteristicId, "Select characteristic"),
          },
          {
            event: "getPressDevices",
            items: withMissing(deviceItems, selection.pressDeviceId, "Use primary key behavior"),
          },
          {
            event: "getPressCharacteristics",
            items: withMissing(itemsFor(pressDevice), selection.pressCharacteristicId, "Select press characteristic"),
          },
          {
            event: "catalog",
            connection: this.status,
            configurationError: this.configurationError ?? null,
            selection,
            primary: inspectorCharacteristic(primary),
            press: inspectorCharacteristic(press),
          },
        ];
        for (const payload of payloads) {
          // SDK3 routes to its currently visible PI; check exact action context before every send.
          if (!inspector.active || streamDeck.ui.action?.id !== inspector.action.id) break;
          await streamDeck.ui.sendToPropertyInspector(payload);
        }
      }
    } catch {
      // A closed/replaced inspector is not a user command failure.
    } finally {
      inspector.sending = false;
    }
  }
}

import { useSyncExternalStore } from "react";
import { showToast, Toast } from "@raycast/api";
import {
  createClient,
  isCommandApplicable,
  type CharacteristicCommand,
  type ClientStatus,
  type Snapshot,
  type StatusResponse,
  type SynapsedClient,
} from "@synapsed/client";

import { bindingKey, dispatchPrecondition } from "./model";
export interface ViewState {
  snapshot?: Snapshot;
  status?: StatusResponse;
  connection: ClientStatus;
  error?: string;
  pending: ReadonlySet<string>;
}
export { bindingKey, displayValue } from "./model";
export class Session {
  private client?: SynapsedClient;
  private abort?: AbortController;
  private unsubscribe?: () => void;
  private listeners = new Set<() => void>();
  private live = false;
  private generation = 0;
  private readGeneration = 0;
  private busy = new Set<string>();
  private state: ViewState = { connection: "connecting", pending: new Set() };
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  getState = () => this.state;
  private update(patch: Partial<ViewState>) {
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((listener) => listener());
  }
  start(endpoint: string) {
    this.live = true;
    const generation = ++this.generation;
    this.abort = new AbortController();
    try {
      this.client = createClient({ baseUrl: endpoint });
      this.unsubscribe = this.client.subscribe({
        onSnapshot: (snapshot) => {
          if (this.live && generation === this.generation) this.update({ snapshot, error: undefined });
        },
        onStatus: (connection) => {
          if (!this.live || generation !== this.generation) return;
          this.update({ connection });
          const readGeneration = ++this.readGeneration;
          if (connection !== "connected") return;
          const client = this.client;
          const signal = this.abort?.signal;
          if (!client || !signal) return;
          void client
            .getStatus({ signal })
            .then((status) => {
              if (
                this.live &&
                generation === this.generation &&
                readGeneration === this.readGeneration &&
                this.state.snapshot?.node.instanceId === status.node.instanceId
              )
                this.update({ status });
            })
            .catch((error: Error) => {
              if (this.live && generation === this.generation && readGeneration === this.readGeneration)
                this.update({ error: error.message });
            });
          void client.getSnapshot({ signal }).catch((error: Error) => {
            if (this.live && generation === this.generation && readGeneration === this.readGeneration)
              this.update({ error: error.message });
          });
        },
        onError: (error) => {
          if (this.live && generation === this.generation) this.update({ error: error.message });
        },
      });
    } catch (error) {
      this.update({
        connection: "incompatible",
        error: error instanceof Error ? error.message : "Invalid endpoint",
      });
    }
  }
  close() {
    this.live = false;
    this.generation += 1;
    this.abort?.abort();
    this.unsubscribe?.();
    this.client?.close();
    this.busy.clear();
  }
  async execute(deviceId: string, characteristicId: string, command: CharacteristicCommand): Promise<boolean> {
    const snapshot = this.state.snapshot;
    const device = snapshot?.devices.find((item) => item.id === deviceId);
    const characteristic = device?.characteristics.find((item) => item.id === characteristicId);
    const client = this.client;
    const key = bindingKey(deviceId, characteristicId);
    if (
      !dispatchPrecondition({
        live: this.live,
        hasClient: Boolean(client),
        hasSnapshot: Boolean(snapshot),
        connected: this.state.connection === "connected",
        deviceOnline: device?.status === "online",
        characteristicAvailable: Boolean(characteristic && characteristic.status === "ok"),
        commandApplicable: Boolean(characteristic && isCommandApplicable(characteristic, command)),
        notBusy: !this.busy.has(key),
      })
    ) {
      await showToast({
        style: Toast.Style.Failure,
        title: "Command unavailable",
        message: "Reconnect and check the current characteristic metadata.",
      });
      return false;
    }
    if (!snapshot || !device || !characteristic || !client) return false;
    const generation = this.generation;
    const instance = snapshot.node.instanceId;
    const owner = device.driver;
    const metadata = JSON.stringify([characteristic.spec, characteristic.access]);
    const signal = this.abort?.signal;
    if (!signal) return false;
    this.busy.add(key);
    this.update({ pending: new Set(this.busy) });
    let toast: Toast | undefined;
    try {
      toast = await showToast({
        style: Toast.Style.Animated,
        title: `Updating ${characteristic.label}`,
      });
      if (!this.live || generation !== this.generation) return false;
      const currentDevice = this.state.snapshot?.devices.find((item) => item.id === deviceId);
      const current = currentDevice?.characteristics.find((item) => item.id === characteristicId);
      if (
        this.client !== client ||
        this.state.connection !== "connected" ||
        this.state.snapshot?.node.instanceId !== instance ||
        currentDevice?.driver !== owner ||
        currentDevice.status !== "online" ||
        !current ||
        current.status !== "ok" ||
        JSON.stringify([current.spec, current.access]) !== metadata ||
        !isCommandApplicable(current, command)
      )
        throw new Error(
          "The device or characteristic changed before dispatch. Review the current state and try again.",
        );
      const observed = await client.execute(deviceId, characteristicId, command, { signal });
      if (!this.live || generation !== this.generation) return false;
      const confirmed = observed.devices
        .find((item) => item.id === deviceId)
        ?.characteristics.find((item) => item.id === characteristicId);
      toast.style = Toast.Style.Success;
      toast.title = "Observed result";
      toast.message = confirmed
        ? `${displayValue(confirmed)} · ${confirmed.status}`
        : "Invocation completed; characteristic no longer present";
      return true;
    } catch (error) {
      if (this.live && generation === this.generation && toast) {
        toast.style = Toast.Style.Failure;
        toast.title = "Command failed";
        toast.message = error instanceof Error ? error.message : "Request failed";
      }
      return false;
    } finally {
      if (generation === this.generation) {
        this.busy.delete(key);
        if (this.live) this.update({ pending: new Set(this.busy) });
      }
      if ((!this.live || generation !== this.generation) && toast) await toast.hide();
    }
  }
}
export function useSession(session: Session) {
  return useSyncExternalStore(session.subscribe, session.getState);
}

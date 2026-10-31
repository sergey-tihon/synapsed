import { createEffect, createMemo, For, Show } from "solid-js";
import CharacteristicCard from "../CharacteristicCard";
import { characteristicElementId, DEFAULT_HARDWARE_FILTERS, href } from "../router";
import { characteristicKey, useSession } from "../session";

interface Props {
  deviceId: string;
  characteristicId: string | null;
}

export default function Device(props: Props) {
  const session = useSession();
  const device = createMemo(() => session.snapshot()?.devices.find((candidate) => candidate.id === props.deviceId));
  const driver = createMemo(() => session.snapshot()?.drivers.find((candidate) => candidate.id === device()?.driver));
  let focusKey = "";

  createEffect(
    () => {
      const characteristicId = props.characteristicId;
      const currentDevice = device();
      return {
        deviceId: props.deviceId,
        characteristicId,
        available:
          characteristicId !== null &&
          currentDevice?.characteristics.some((item) => item.id === characteristicId) === true,
      };
    },
    ({ deviceId, characteristicId, available }) => {
      if (characteristicId === null || !available) {
        focusKey = "";
        return;
      }
      const key = `${deviceId}:${characteristicId}`;
      if (focusKey === key || typeof document === "undefined") return;
      queueMicrotask(() => {
        const element = document.getElementById(characteristicElementId(deviceId, characteristicId));
        if (!element) return;
        element.scrollIntoView({ block: "center" });
        element.focus({ preventScroll: true });
        focusKey = key;
      });
    },
  );

  const returnToHardware = () =>
    href({ page: "hardware", filters: { ...DEFAULT_HARDWARE_FILTERS, q: props.deviceId } });

  return (
    <div class="page-stack">
      <header class="page-title">
        <p class="eyebrow">PUBLISHED DEVICE</p>
        <Show when={device()} fallback={<h1>Device controls</h1>}>
          <h1>{device()?.label}</h1>
        </Show>
        <p>{props.deviceId}</p>
      </header>
      <Show when={session.serviceError()}>
        <p class="error">{session.serviceError()}</p>
      </Show>
      <Show when={!session.snapshot()}>
        <section class="panel">
          <p>Waiting for the current device catalog…</p>
        </section>
      </Show>
      <Show when={session.snapshot() && !device()}>
        <section class="notice-panel">
          <h2>Device not found</h2>
          <p>The requested device is not present in the current snapshot. No binding was changed.</p>
          <a href="#/devices">Return to devices →</a>
        </section>
      </Show>
      <Show when={device()}>
        <section class="panel device-overview">
          <div class="section-heading">
            <div>
              <p class="eyebrow">{device()?.kind}</p>
              <h2>{device()?.label}</h2>
            </div>
            <span class="badge" data-state={device()?.status}>
              {device()?.status}
              {session.connection() === "connected" ? "" : " · stale"}
            </span>
          </div>
          <dl class="device-meta">
            <dt>Device ID</dt>
            <dd>{device()?.id}</dd>
            <dt>Driver</dt>
            <dd>
              {device()?.driver}
              {driver() ? ` · ${driver()?.version}` : " · not installed"}
            </dd>
            <dt>Characteristics</dt>
            <dd>{device()?.characteristics.length}</dd>
          </dl>
          {device()?.error ? (
            <p class="error">
              {device()?.error?.code}: {device()?.error?.message}
            </p>
          ) : null}
          {driver()?.error ? <p class="error">Driver: {driver()?.error?.message}</p> : null}
          <a class="button-link" href={returnToHardware()}>
            View hardware node →
          </a>
        </section>
        <Show
          when={
            device() &&
            props.characteristicId !== null &&
            !device()?.characteristics.some((item) => item.id === props.characteristicId)
          }
        >
          <p class="notice" role="alert">
            The linked characteristic is not present in this device snapshot. Nothing has been rebound.
          </p>
        </Show>
        <section class="controls-section" aria-labelledby="controls-heading">
          <div class="section-heading">
            <div>
              <p class="eyebrow">OBSERVED STATE</p>
              <h2 id="controls-heading">Characteristics</h2>
            </div>
            <span class="badge">{session.connection() === "connected" ? "Current" : "Stale"}</span>
          </div>
          <div class="characteristic-grid">
            <For each={device()?.characteristics ?? []}>
              {(characteristic) => {
                const key = characteristicKey(props.deviceId, characteristic.id);
                const focusTarget = () => props.characteristicId === characteristic.id;
                return (
                  <CharacteristicCard
                    deviceId={props.deviceId}
                    characteristic={characteristic}
                    available={
                      session.connection() === "connected" &&
                      device()?.status === "online" &&
                      characteristic.status === "ok"
                    }
                    stale={
                      session.connection() !== "connected" ||
                      device()?.status !== "online" ||
                      characteristic.status !== "ok"
                    }
                    pending={session.pending().has(key)}
                    result={session.results().get(key)?.message}
                    focusTarget={focusTarget()}
                    execute={(command) => session.execute(props.deviceId, characteristic.id, command)}
                  />
                );
              }}
            </For>
          </div>
          <Show when={device()?.characteristics.length === 0}>
            <p class="muted">This device declares no characteristics.</p>
          </Show>
        </section>
      </Show>
    </div>
  );
}

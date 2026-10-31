import { For, Show } from "solid-js";
import { href } from "../router";
import { useSession } from "../session";

export default function Devices() {
  const session = useSession();
  const snapshot = () => session.snapshot();
  return (
    <div class="page-stack">
      <header class="page-title">
        <p class="eyebrow">DRIVER-PUBLISHED</p>
        <h1>Devices</h1>
        <p>Only installed drivers can publish device characteristics and controls.</p>
      </header>
      <Show when={session.serviceError()}>
        <p class="error">{session.serviceError()}</p>
      </Show>
      <Show
        when={snapshot()}
        fallback={
          <section class="panel">
            <p>Waiting for the device catalog…</p>
          </section>
        }
      >
        <Show
          when={snapshot()?.devices.length}
          fallback={
            <section class="panel">
              <p>No devices are currently published. No controls are manufactured.</p>
            </section>
          }
        >
          <div class="device-grid">
            <For each={snapshot()?.devices ?? []}>
              {(device) => {
                const driver = () => snapshot()?.drivers.find((item) => item.id === device.driver);
                return (
                  <article class="device-card">
                    <div class="section-heading">
                      <div>
                        <p class="eyebrow">{device.kind}</p>
                        <h2>{device.label}</h2>
                      </div>
                      <span class="badge" data-state={device.status}>
                        {device.status}
                        {session.connection() === "connected" ? "" : " · stale"}
                      </span>
                    </div>
                    <p class="muted">{device.id}</p>
                    <dl class="device-meta">
                      <dt>Driver</dt>
                      <dd>
                        {device.driver}
                        {driver() ? ` · ${driver()?.version}` : " · not installed"}
                      </dd>
                      <dt>Characteristics</dt>
                      <dd>{device.characteristics.length}</dd>
                    </dl>
                    {device.error ? (
                      <p class="error">
                        {device.error.code}: {device.error.message}
                      </p>
                    ) : null}
                    <Show when={driver()?.error}>
                      <p class="error">Driver: {driver()?.error?.message}</p>
                    </Show>
                    <a class="button-link" href={href({ page: "device", deviceId: device.id, characteristicId: null })}>
                      Open controls →
                    </a>
                  </article>
                );
              }}
            </For>
          </div>
        </Show>
      </Show>
    </div>
  );
}

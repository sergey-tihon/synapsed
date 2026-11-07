import { For, Show } from "solid-js";
import { href } from "../router";
import DriverDiscoveryStatus from "../components/DriverDiscoveryStatus";
import { useSession } from "../session";

export default function Devices() {
  const session = useSession();
  const snapshot = () => session.snapshot();
  const isCurrent = () => session.connection() === "connected";
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
        <section class="panel" aria-labelledby="driver-discovery-heading">
          <div class="section-heading">
            <div>
              <p class="eyebrow">LOADED IS NOT DISCOVERED</p>
              <h2 id="driver-discovery-heading">Driver discovery</h2>
            </div>
          </div>
          <p class="muted">
            Online means the driver is loaded and operating. The line under each driver says whether it has published a
            matching device.
          </p>
          <Show when={snapshot()?.drivers.length} fallback={<p class="muted">No driver packages installed.</p>}>
            <ul class="driver-discovery-list">
              <For each={snapshot()?.drivers ?? []}>
                {(driver) => (
                  <li>
                    <div class="driver-discovery-row">
                      <div>
                        <strong>{driver.id}</strong>
                        <span class="muted">{driver.version}</span>
                      </div>
                      <span class="badge" data-state={driver.state}>
                        {driver.state}
                      </span>
                    </div>
                    <DriverDiscoveryStatus
                      driver={driver}
                      devices={snapshot()?.devices.filter((device) => device.driver === driver.id) ?? []}
                      isCurrent={isCurrent()}
                    />
                    {driver.error ? <p class="error">{driver.error.message}</p> : null}
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </section>
        <Show
          when={snapshot()?.devices.length}
          fallback={
            <section class="panel">
              <h2>No devices discovered</h2>
              <p>No controls are available until a driver matches and publishes a device.</p>
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
                        {isCurrent() ? "" : " · stale"}
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

import { createSignal, For, Show } from "solid-js";
import { formatCharacteristicValue, type CharacteristicChange } from "@synapsed/client";
import { href } from "../router";
import { matchesEntrySearch } from "../entry-search";
import { useSession } from "../session";

function localTime(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.valueOf()) ? timestamp : date.toLocaleString();
}

export default function Events() {
  const session = useSession();
  const [query, setQuery] = createSignal("");
  const changes = () => session.changes();
  const deviceFor = (change: CharacteristicChange) =>
    session.snapshot()?.devices.find((device) => device.id === change.deviceId);
  const characteristicFor = (change: CharacteristicChange) =>
    deviceFor(change)?.characteristics.find((characteristic) => characteristic.id === change.characteristicId);
  const searchableText = (change: CharacteristicChange) =>
    [
      change.timestamp,
      change.status,
      change.deviceId,
      change.characteristicId,
      deviceFor(change)?.label,
      characteristicFor(change)?.label,
      change.value === null ? "No value" : String(change.value),
      change.error?.code,
      change.error?.message,
    ].join(" ");
  const visibleChanges = () => {
    const term = query().trim();
    const entries = changes();
    return term ? entries.filter((change) => matchesEntrySearch(term, searchableText(change))) : entries;
  };

  return (
    <div class="page-stack activity-page">
      <header class="page-title">
        <p class="eyebrow">SERVICE EVENTS</p>
        <h1>Events</h1>
        <p>Live characteristic changes. At most 200 are retained.</p>
      </header>
      <section class="panel" aria-labelledby="changes-heading">
        <div class="section-heading">
          <h2 id="changes-heading">Characteristic changes</h2>
          <label class="entry-search">
            Search
            <input
              type="search"
              value={query()}
              onInput={(event) => setQuery(event.currentTarget.value)}
              placeholder="Device, characteristic, value"
              aria-label="Search characteristic changes"
            />
          </label>
        </div>
        <p class="sr-only" aria-live="polite">
          {visibleChanges().length} matching characteristic changes.
        </p>
        <Show
          when={visibleChanges().length > 0}
          fallback={
            <p>
              {query().trim() ? "No characteristic changes match this search." : "No characteristic changes received."}
            </p>
          }
        >
          <ol class="logs-list">
            <For each={visibleChanges()}>
              {(change) => {
                const deviceLabel = () => deviceFor(change)?.label ?? change.deviceId;
                const characteristicLabel = () => characteristicFor(change)?.label ?? change.characteristicId;
                return (
                  <li>
                    <div class="log-heading">
                      <time datetime={change.timestamp}>{localTime(change.timestamp)}</time>
                      <span class="badge" data-status={change.status}>
                        {change.status}
                      </span>
                      <a
                        href={href({
                          page: "device",
                          deviceId: change.deviceId,
                          characteristicId: change.characteristicId,
                        })}
                      >
                        {deviceLabel()} · {characteristicLabel()}
                      </a>
                    </div>
                    <p>
                      {(() => {
                        const characteristic = characteristicFor(change);
                        return characteristic
                          ? formatCharacteristicValue({
                              ...characteristic,
                              value: change.value,
                              status: change.status,
                              error: change.error,
                            })
                          : change.value === null
                            ? "No value"
                            : String(change.value);
                      })()}
                      {change.error ? ` — ${change.error.message}` : ""}
                    </p>
                  </li>
                );
              }}
            </For>
          </ol>
        </Show>
      </section>
    </div>
  );
}

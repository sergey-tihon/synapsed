import { createMemo, For, Show } from "solid-js";
import type { LogEntry } from "@synapsed/client";
import type { LogLevelFilter } from "../router";
import { useSession } from "../session";

interface Props {
  level: LogLevelFilter;
  onLevelChange(level: LogLevelFilter): void;
}

const levels: { value: LogLevelFilter; label: string }[] = [
  { value: "all", label: "All levels" },
  { value: "debug", label: "Debug" },
  { value: "info", label: "Info" },
  { value: "warn", label: "Warning" },
  { value: "error", label: "Error" },
];

function localTime(entry: LogEntry): string {
  const date = new Date(entry.timestamp);
  return Number.isNaN(date.valueOf()) ? entry.timestamp : date.toLocaleString();
}

export default function Logs(props: Props) {
  const session = useSession();
  const entries = createMemo(() =>
    session.logs().filter((entry) => props.level === "all" || entry.level === props.level),
  );
  return (
    <div class="page-stack">
      <header class="page-title">
        <p class="eyebrow">SERVICE EVENTS</p>
        <h1>Logs</h1>
        <p>Recent and live service messages. At most 200 entries are retained.</p>
      </header>
      <section class="panel" aria-labelledby="logs-filter-heading">
        <div class="section-heading">
          <h2 id="logs-filter-heading">Recent logs</h2>
          <label class="log-filter">
            Level
            <select
              value={props.level}
              onChange={(event) => props.onLevelChange(event.currentTarget.value as LogLevelFilter)}
            >
              <For each={levels}>{(level) => <option value={level.value}>{level.label}</option>}</For>
            </select>
          </label>
        </div>
        <p class="muted">
          {session.connection() === "connected" ? "Live service stream" : "Retained entries · stale while disconnected"}
        </p>
        <p class="sr-only" aria-live="polite">
          {entries().length} matching log entries.
        </p>
        <Show
          when={entries().length > 0}
          fallback={<p>{session.logs().length ? "No entries match this level." : "No log entries received."}</p>}
        >
          <ol class="logs-list">
            <For each={entries()}>
              {(entry) => (
                <li>
                  <div class="log-heading">
                    <time datetime={entry.timestamp}>{localTime(entry)}</time>
                    <span class="badge" data-level={entry.level}>
                      {entry.level}
                    </span>
                    <code>{entry.target}</code>
                  </div>
                  <p>{entry.message}</p>
                </li>
              )}
            </For>
          </ol>
        </Show>
      </section>
    </div>
  );
}

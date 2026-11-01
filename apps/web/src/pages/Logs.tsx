import { createMemo, createSignal, For, Show } from "solid-js";
import type { LogLevelFilter } from "../router";
import { useSession } from "../session";
import { matchesEntrySearch } from "../entry-search";

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

function localTime(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.valueOf()) ? timestamp : date.toLocaleString();
}

export default function Logs(props: Props) {
  const session = useSession();
  const [query, setQuery] = createSignal("");
  const entries = createMemo(() => {
    const term = query().trim();
    return session.logs().filter((entry) => {
      const matchesLevel = props.level === "all" || entry.level === props.level;
      const matchesSearch = matchesEntrySearch(
        term,
        `${entry.timestamp} ${entry.level} ${entry.target} ${entry.message}`,
      );
      return matchesLevel && matchesSearch;
    });
  });

  return (
    <div class="page-stack activity-page">
      <header class="page-title">
        <p class="eyebrow">SERVICE LOGS</p>
        <h1>Logs</h1>
        <p>Search service logs and filter by level. At most 200 entries are retained.</p>
      </header>
      <section class="panel" aria-labelledby="logs-heading">
        <div class="section-heading">
          <h2 id="logs-heading">Service logs</h2>
          <div class="entry-filters">
            <label class="log-filter">
              Level
              <select
                value={props.level}
                onChange={(event) => props.onLevelChange(event.currentTarget.value as LogLevelFilter)}
              >
                <For each={levels}>{(level) => <option value={level.value}>{level.label}</option>}</For>
              </select>
            </label>
            <label class="entry-search">
              Search
              <input
                type="search"
                value={query()}
                onInput={(event) => setQuery(event.currentTarget.value)}
                placeholder="Level, target, message"
                aria-label="Search service logs"
              />
            </label>
          </div>
        </div>
        <p class="muted">
          {session.connection() === "connected" ? "Live service stream" : "Retained entries · stale while disconnected"}
        </p>
        <p class="sr-only" aria-live="polite">
          {entries().length} matching log entries.
        </p>
        <Show
          when={entries().length > 0}
          fallback={<p>{session.logs().length ? "No log entries match these filters." : "No log entries received."}</p>}
        >
          <ol class="logs-list">
            <For each={entries()}>
              {(entry) => (
                <li>
                  <div class="log-heading">
                    <time datetime={entry.timestamp}>{localTime(entry.timestamp)}</time>
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

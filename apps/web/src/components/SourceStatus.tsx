import { For } from "solid-js";
import type { InventorySource } from "@synapsed/client";

interface Props {
  sources: InventorySource[];
}

function stateLabel(state: InventorySource["state"]): string {
  switch (state) {
    case "ok":
      return "Available";
    case "not_scanned":
      return "Not scanned";
    case "unavailable":
      return "Unavailable";
    case "permission_denied":
      return "Permission denied";
    case "error":
      return "Error";
    case "timeout":
      return "Timed out";
  }
}

export default function SourceStatus(props: Props) {
  return (
    <section class="source-status" aria-labelledby="source-status-heading">
      <h2 id="source-status-heading">Hardware sources</h2>
      {props.sources.length ? (
        <ul>
          <For each={props.sources}>
            {(source) => (
              <li data-state={source.state}>
                <strong>{source.kind.toUpperCase()}</strong>
                <span>{stateLabel(source.state)}</span>
                {source.message ? <span class="muted">{source.message}</span> : null}
              </li>
            )}
          </For>
        </ul>
      ) : (
        <p class="muted">No source status was reported.</p>
      )}
    </section>
  );
}

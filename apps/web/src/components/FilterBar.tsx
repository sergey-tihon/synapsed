import type { AttachmentFilter, DriverFilter, HardwareFilters, KindFilter } from "../router";

interface Props {
  filters: HardwareFilters;
  onChange(filters: HardwareFilters): void;
}

const kinds: { value: KindFilter; label: string }[] = [
  { value: "usb", label: "USB" },
  { value: "display", label: "Displays" },
  { value: "bluetooth", label: "Bluetooth" },
];
const attachments: { value: AttachmentFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "host", label: "This machine" },
  { value: "dock", label: "Dock" },
  { value: "monitor", label: "Monitor" },
];

export default function FilterBar(props: Props) {
  function toggleKind(kind: KindFilter) {
    const selected = new Set(props.filters.kinds);
    if (selected.has(kind)) selected.delete(kind);
    else selected.add(kind);
    props.onChange({ ...props.filters, kinds: [...selected] });
  }
  return (
    <div class="filter-bar" role="group" aria-label="Hardware filters">
      <label class="filter-search">
        <span>Search hardware</span>
        <input
          type="search"
          value={props.filters.q}
          placeholder="Name, kind, or vendor ID"
          onInput={(event) => props.onChange({ ...props.filters, q: event.currentTarget.value })}
        />
      </label>
      <fieldset class="filter-kind">
        <legend>Kind</legend>
        <div class="filter-chips">
          {kinds.map((kind) => (
            <button
              type="button"
              aria-pressed={props.filters.kinds.includes(kind.value) ? "true" : "false"}
              onClick={() => toggleKind(kind.value)}
            >
              {kind.label}
            </button>
          ))}
        </div>
      </fieldset>
      <fieldset class="filter-attachment">
        <legend>Attachment</legend>
        <div class="filter-chips">
          {attachments.map((attachment) => (
            <button
              type="button"
              aria-pressed={props.filters.attachment === attachment.value ? "true" : "false"}
              onClick={() => props.onChange({ ...props.filters, attachment: attachment.value })}
            >
              {attachment.label}
            </button>
          ))}
        </div>
      </fieldset>
      <label class="filter-driver">
        <span>Driver</span>
        <select
          value={props.filters.driver}
          onChange={(event) => props.onChange({ ...props.filters, driver: event.currentTarget.value as DriverFilter })}
        >
          <option value="any">Any driver</option>
          <option value="specialized">Specialized</option>
          <option value="generic">Generic</option>
          <option value="none">No driver</option>
        </select>
      </label>
      <label class="filter-toggle">
        <input
          type="checkbox"
          checked={props.filters.connected}
          onChange={(event) => props.onChange({ ...props.filters, connected: event.currentTarget.checked })}
        />
        <span>Connected</span>
      </label>
    </div>
  );
}

import type { Characteristic, CharacteristicSpec } from "./generated/index.js";

/** Returns the display label for an enum value, or the value itself if unknown. */
export function enumOptionLabel(spec: Extract<CharacteristicSpec, { kind: "enum" }>, value: string): string {
  return spec.options.find((option) => option.value === value)?.label ?? value;
}

/** Describes the constraints of a characteristic for display beside its control. */
export function describeSpec(spec: CharacteristicSpec): string {
  if (spec.kind === "number") {
    const bounds =
      spec.minimum !== null && spec.maximum !== null
        ? `${spec.minimum}–${spec.maximum}`
        : spec.minimum !== null
          ? `≥ ${spec.minimum}`
          : spec.maximum !== null
            ? `≤ ${spec.maximum}`
            : null;
    const range = bounds === null ? null : `${bounds}${spec.unit ? ` ${spec.unit}` : ""}`;
    return [range, spec.step === null ? null : `step ${spec.step}`].filter(Boolean).join(", ");
  }
  if (spec.kind === "string") return `Up to ${spec.maxLength} characters`;
  if (spec.kind === "enum") return spec.options.map((option) => option.label).join(" · ");
  return "";
}

/** Formats the observed value consistently across all clients. */
export function formatCharacteristicValue(ch: Characteristic): string {
  if (ch.spec.kind === "action") return "Action";
  if (ch.value === null) return "Not yet observed";
  if (ch.spec.kind === "boolean") return ch.value ? "On" : "Off";
  if (ch.spec.kind === "enum") return enumOptionLabel(ch.spec, String(ch.value));
  if (ch.spec.kind === "number") return `${ch.value}${ch.spec.unit ? ` ${ch.spec.unit}` : ""}`;
  return ch.value === "" ? "Empty string" : String(ch.value);
}

/** Describes non-healthy state without duplicating status-specific UI rules. */
export function statusSuffix(ch: Characteristic): string {
  if (ch.status === "ok") return "";
  return ch.error?.message ?? (ch.status === "unavailable" ? "Unavailable" : "Error");
}

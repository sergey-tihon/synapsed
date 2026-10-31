import { createMemo } from "solid-js";
import type { InventoryDriver } from "@synapsed/client";
import { driverBadge } from "../inventory";

interface Props {
  driver: InventoryDriver | null;
}

export default function DriverBadge(props: Props) {
  const badge = createMemo(() => driverBadge(props.driver));
  return (
    <span class={`driver-badge driver-badge--${badge().className}`}>
      <span class="driver-dot" aria-hidden="true" />
      {badge().label}
    </span>
  );
}

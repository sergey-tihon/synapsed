import type { Device, DriverStatus } from "@synapsed/client";
import { driverDiscoverySummary } from "../driver-discovery";

interface Props {
  driver: DriverStatus;
  devices: readonly Device[];
  isCurrent: boolean;
}

export default function DriverDiscoveryStatus(props: Props) {
  const summary = () => driverDiscoverySummary(props.driver.state, props.devices, props.isCurrent);
  return (
    <p class="driver-discovery" data-state={summary().state}>
      {summary().message}
    </p>
  );
}

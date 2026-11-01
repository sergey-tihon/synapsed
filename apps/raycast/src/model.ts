import { formatCharacteristicValue } from "@synapsed/client";
import type { Characteristic } from "@synapsed/client";

export const bindingKey = (deviceId: string, characteristicId: string) => JSON.stringify([deviceId, characteristicId]);
export const displayValue = (characteristic: Characteristic) => formatCharacteristicValue(characteristic);

export function dispatchPrecondition(input: {
  live: boolean;
  hasClient: boolean;
  hasSnapshot: boolean;
  connected: boolean;
  deviceOnline: boolean;
  characteristicAvailable: boolean;
  commandApplicable: boolean;
  notBusy: boolean;
}): boolean {
  return (
    input.live &&
    input.hasClient &&
    input.hasSnapshot &&
    input.connected &&
    input.deviceOnline &&
    input.characteristicAvailable &&
    input.commandApplicable &&
    input.notBusy
  );
}

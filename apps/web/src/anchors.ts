/** Shared fragment contract with the native tray; IDs are encoded independently. */
export function characteristicAnchor(deviceId: string, characteristicId: string): string {
  return `#device=${encodeURIComponent(deviceId)}&characteristic=${encodeURIComponent(characteristicId)}`;
}

export function scrollToCharacteristic(hash: string): boolean {
  if (!hash.startsWith("#device=")) return false;
  const element = document.getElementById(hash.slice(1));
  if (!element) return false;
  element.scrollIntoView({ block: "center" });
  return true;
}

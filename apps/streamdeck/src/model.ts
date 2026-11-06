export function clampDialTicks(ticks: number): number {
  return Math.max(-100, Math.min(100, Math.trunc(ticks)));
}

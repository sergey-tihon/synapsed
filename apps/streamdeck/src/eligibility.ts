type Eligible = { access: { write: boolean }; spec: { kind: string; step?: number | null } };

export const keyEligible = (characteristic: Eligible): boolean =>
  characteristic.access.write || characteristic.spec.kind !== "action";

export const rotateEligible = (characteristic: Eligible): boolean =>
  characteristic.access.write &&
  (characteristic.spec.kind === "enum" || (characteristic.spec.kind === "number" && characteristic.spec.step != null));

export const pressEligible = (characteristic: Eligible): boolean =>
  characteristic.access.write && (characteristic.spec.kind === "boolean" || characteristic.spec.kind === "action");

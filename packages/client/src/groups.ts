export interface CharacteristicSection<T> {
  group: { id: string; label: string } | null;
  characteristics: T[];
}

export function characteristicSections<T extends { group: string | null }>(
  groups: readonly { id: string; label: string }[],
  characteristics: readonly T[],
): CharacteristicSection<T>[] {
  if (groups.length === 0) {
    return [{ group: null, characteristics: [...characteristics] }];
  }

  const sections: CharacteristicSection<T>[] = [];
  const matched = new Set<T>();
  for (const group of groups) {
    const members = characteristics.filter((characteristic) => characteristic.group === group.id);
    if (members.length > 0) {
      sections.push({ group, characteristics: members });
      for (const member of members) matched.add(member);
    }
  }

  const unmatched = characteristics.filter((characteristic) => !matched.has(characteristic));
  if (unmatched.length > 0) {
    sections.push({ group: null, characteristics: unmatched });
  }
  return sections;
}

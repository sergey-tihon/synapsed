export function matchesEntrySearch(query: string, text: string): boolean {
  const term = query.trim().toLowerCase();
  return term === "" || text.toLowerCase().includes(term);
}

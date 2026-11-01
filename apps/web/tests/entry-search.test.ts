import { describe, expect, it } from "vitest";
import { matchesEntrySearch } from "../src/entry-search";

describe("entry search", () => {
  it("matches case-insensitively after trimming the query", () => {
    expect(matchesEntrySearch("  AMBIENT  ", "BenQ RD280U · Ambient Light 141")).toBe(true);
    expect(matchesEntrySearch("benq", "BenQ RD280U · Ambient Light 141")).toBe(true);
    expect(matchesEntrySearch("moonhalo", "BenQ RD280U · Ambient Light 141")).toBe(false);
  });

  it("matches every entry when the query is blank", () => {
    expect(matchesEntrySearch("  ", "any text")).toBe(true);
  });
});

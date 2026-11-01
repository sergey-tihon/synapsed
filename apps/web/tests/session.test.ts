import { describe, expect, it } from "vitest";
import type { CharacteristicChange, LogEntry } from "@synapsed/client";
import {
  appendChanges,
  expireResultsOnRevision,
  mergeLogs,
  resetForInstance,
  type SessionReducerState,
} from "../src/session";

function log(id: string, message = `entry ${id}`): LogEntry {
  return {
    id,
    level: "info",
    message,
    target: "test",
    timestamp: "2026-10-31T12:00:00Z",
  };
}

function change(id: string): CharacteristicChange {
  return {
    revision: "1",
    timestamp: "2026-10-31T12:00:00Z",
    deviceId: "monitor",
    characteristicId: `change_${id}`,
    value: 1,
    status: "ok",
    error: null,
  };
}

describe("session reducers", () => {
  it("deduplicates log sequences, sorts numerically and retains the newest 200", () => {
    const merged = mergeLogs([log("2", "original")], [log("10"), log("2", "duplicate"), log("1")]);
    expect(merged.map((entry) => entry.id)).toEqual(["1", "2", "10"]);
    expect(merged[1]?.message).toBe("original");

    const capped = mergeLogs(
      [],
      Array.from({ length: 205 }, (_, index) => log(String(index + 1))),
    );
    expect(capped).toHaveLength(200);
    expect(capped[0]?.id).toBe("6");
    expect(capped.at(-1)?.id).toBe("205");
  });

  it("appends characteristic changes oldest-first and retains the newest 200", () => {
    const changes = appendChanges(
      [],
      Array.from({ length: 205 }, (_, index) => change(String(index + 1))),
    );
    expect(changes).toHaveLength(200);
    expect(changes[0]?.characteristicId).toBe("change_6");
    expect(changes.at(-1)?.characteristicId).toBe("change_205");
  });

  it("expires only command results older than a newer snapshot revision", () => {
    const results = new Map([
      ["old", { message: "old command", revision: "8" }],
      ["current", { message: "current command", revision: "9" }],
    ]);
    const atSameRevision = expireResultsOnRevision(results, "9");
    expect([...atSameRevision.keys()]).toEqual(["current"]);
    expect(expireResultsOnRevision(atSameRevision, "9")).toBe(atSameRevision);
    expect(expireResultsOnRevision(atSameRevision, "10")).toEqual(new Map());
  });

  it("preserves state for one daemon instance and resets logs, changes and results on replacement", () => {
    const retainedChanges = [change("brightness")];
    const state: SessionReducerState = {
      instanceId: "instance-a",
      logs: [log("1")],
      changes: retainedChanges,
      results: new Map([["device", { message: "confirmed", revision: "4" }]]),
    };
    expect(resetForInstance(state, "instance-a")).toBe(state);
    expect(resetForInstance(state, "instance-a").changes).toBe(retainedChanges);
    expect(resetForInstance(state, "instance-b")).toEqual({
      instanceId: "instance-b",
      logs: [],
      changes: [],
      results: new Map(),
    });
  });
});

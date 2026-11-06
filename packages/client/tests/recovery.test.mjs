import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiRequestError } from "../dist/index.js";
import { observer, serverFixture, snapshot } from "./server.mjs";

test("driver restart accepts the recovered snapshot and validates unknown-driver errors", async (t) => {
  const fixture = await serverFixture(t);
  fixture.current = snapshot("1");
  fixture.current.drivers = [
    {
      id: "fixture",
      version: "0.1.0",
      state: "error",
      error: { code: "plugin_error", message: "Driver unavailable." },
    },
  ];
  const client = fixture.client();
  const observed = observer(client);
  await observed.until((state) => state.snapshots.some((value) => value.revision === "1"));

  fixture.routes.set("POST /api/v1/drivers/fixture/restart", async ({ reply }) => {
    fixture.current = snapshot("2");
    fixture.broadcast({ type: "snapshot", snapshot: fixture.current });
    reply(fixture.current);
  });
  const recovered = await client.restartDriver("fixture");
  assert.equal(recovered.revision, "2");
  assert.equal(recovered.drivers[0]?.state, "online");
  await observed.until((state) => state.snapshots.some((value) => value.revision === "2"));

  fixture.routes.set("POST /api/v1/drivers/ghost/restart", async ({ reply }) => {
    reply({ error: { code: "unknown_driver", message: "Driver is not installed." } }, 404);
  });
  await assert.rejects(client.restartDriver("ghost"), (error) => {
    assert.ok(error instanceof ApiRequestError);
    assert.equal(error.apiError.code, "unknown_driver");
    return true;
  });
});

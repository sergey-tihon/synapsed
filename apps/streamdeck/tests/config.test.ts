import { afterEach, describe, expect, it } from "vitest";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { loadEndpoint } from "../src/config.js";

const roots: string[] = [];
async function configHome(contents?: string | Uint8Array): Promise<string> {
  const root = await mkdtemp(path.join(tmpdir(), "synapsed-streamdeck-config-"));
  roots.push(root);
  const directory = path.join(root, "synapsed");
  await mkdir(directory);
  if (contents !== undefined) await writeFile(path.join(directory, "client.toml"), contents);
  return root;
}
afterEach(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

describe("loadEndpoint", () => {
  it("uses and normalizes the environment override", async () => {
    expect(await loadEndpoint({ SYNAPSED_ENDPOINT: "http://localhost:24817/" } as NodeJS.ProcessEnv)).toBe(
      "http://localhost:24817",
    );
  });

  it("uses the default endpoint when the config file is missing", async () => {
    const home = await configHome();
    expect(await loadEndpoint({ XDG_CONFIG_HOME: home } as NodeJS.ProcessEnv)).toBe("http://127.0.0.1:24816");
  });

  it("rejects files larger than 64 KiB", async () => {
    const home = await configHome(Buffer.alloc(64 * 1024 + 1, 0x20));
    await expect(loadEndpoint({ XDG_CONFIG_HOME: home } as NodeJS.ProcessEnv)).rejects.toThrow("exceeds 64 KiB");
  });

  it("rejects invalid TOML without exposing parser input", async () => {
    const home = await configHome('endpoint = "');
    await expect(loadEndpoint({ XDG_CONFIG_HOME: home } as NodeJS.ProcessEnv)).rejects.toThrow("not valid TOML");
  });

  it("rejects unknown client configuration keys", async () => {
    const home = await configHome('endpoint = "http://localhost:24817"\nsecret = "no"\n');
    await expect(loadEndpoint({ XDG_CONFIG_HOME: home } as NodeJS.ProcessEnv)).rejects.toThrow(
      "only a string endpoint",
    );
  });
});

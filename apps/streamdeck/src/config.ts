import { open } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { parse } from "smol-toml";
import { validateBaseUrl } from "@synapsed/client";

const DEFAULT_ENDPOINT = "http://127.0.0.1:24816";
const CONFIG_LIMIT = 64 * 1024;

/** Only the public client configuration is read, never daemon settings or credentials. */
export async function loadEndpoint(): Promise<string> {
  if (process.env.SYNAPSED_ENDPOINT !== undefined) {
    return validateBaseUrl(process.env.SYNAPSED_ENDPOINT).origin;
  }
  const configHome = process.env.XDG_CONFIG_HOME || path.join(homedir(), ".config");
  const filename = path.join(configHome, "synapsed", "client.toml");
  let file;
  try {
    file = await open(filename, "r");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return DEFAULT_ENDPOINT;
    throw new Error("Cannot read Synapsed client configuration.");
  }
  try {
    // The capped read also protects against growth between stat and read.
    const bytes = Buffer.alloc(CONFIG_LIMIT + 1);
    let length = 0;
    while (length < bytes.length) {
      const result = await file.read(bytes, length, bytes.length - length, null);
      if (result.bytesRead === 0) break;
      length += result.bytesRead;
    }
    if (length > CONFIG_LIMIT) throw new Error("Synapsed client configuration exceeds 64 KiB.");
    let config;
    try {
      config = parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, length)));
    } catch {
      // Parser errors may contain private input; never expose them.
      throw new Error("Synapsed client configuration is not valid TOML.");
    }
    const fields = Object.keys(config);
    if (
      fields.some((field) => field !== "endpoint") ||
      (config.endpoint !== undefined && typeof config.endpoint !== "string")
    ) {
      throw new Error("Synapsed client configuration accepts only a string endpoint field.");
    }
    return validateBaseUrl(config.endpoint === undefined ? DEFAULT_ENDPOINT : (config.endpoint as string)).origin;
  } finally {
    await file.close();
  }
}

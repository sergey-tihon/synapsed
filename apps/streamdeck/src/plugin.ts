import streamDeck from "@elgato/streamdeck";
import { createClient, type SynapsedClient } from "@synapsed/client";
import { CharacteristicAction } from "./actions.js";
import { loadEndpoint } from "./config.js";
import { Contexts } from "./contexts.js";

let client: SynapsedClient | undefined;
let configurationError: string | undefined;
try {
  client = createClient({ baseUrl: await loadEndpoint() });
} catch (error) {
  configurationError = error instanceof Error ? error.message : "Invalid Synapsed client configuration.";
  streamDeck.logger.error(configurationError);
}
const contexts = new Contexts(client, configurationError);
streamDeck.actions.registerAction(new CharacteristicAction(contexts));
for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.once(signal, () => {
    contexts.close();
    process.exit(0);
  });
}
process.once("exit", () => contexts.close());
await streamDeck.connect();

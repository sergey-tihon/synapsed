import { execFileSync } from "node:child_process";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const mode = process.argv[2];
if (!["generate", "check"].includes(mode) || process.argv.length !== 3) {
  console.error("Usage: node scripts/api.mjs generate|check");
  process.exit(2);
}

function generate(schema, generated) {
  execFileSync("cargo", ["run", "-p", "synapsed-api", "--features", "schema", "--bin", "export-schema", "--", schema], {
    cwd: root,
    stdio: "inherit",
  });
  execFileSync(process.execPath, ["packages/client/scripts/generate.mjs", schema, generated], {
    cwd: root,
    stdio: "inherit",
  });
}

async function files(directory, prefix = "") {
  const result = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const relative = path.join(prefix, entry.name);
    if (entry.isDirectory()) result.push(...(await files(path.join(directory, entry.name), relative)));
    else if (entry.isFile()) result.push(relative);
    else throw new Error(`Unexpected non-file generated entry: ${relative}`);
  }
  return result.sort();
}

async function compare(expected, actual, label) {
  const a = await files(expected);
  const b = await files(actual);
  if (JSON.stringify(a) !== JSON.stringify(b))
    throw new Error(`${label} generated file set differs; run npm run api:generate`);
  for (const file of a) {
    const [left, right] = await Promise.all([readFile(path.join(expected, file)), readFile(path.join(actual, file))]);
    if (!left.equals(right)) throw new Error(`${label}/${file} differs; run npm run api:generate`);
  }
}

let temporary;
try {
  const schema = path.join(root, "packages/client/schema");
  const generated = path.join(root, "packages/client/src/generated");
  if (mode === "generate") generate(schema, generated);
  else {
    temporary = await mkdtemp(path.join(tmpdir(), "synapsed-api-check-"));
    const nextSchema = path.join(temporary, "schema");
    const nextGenerated = path.join(temporary, "generated");
    generate(nextSchema, nextGenerated);
    await compare(schema, nextSchema, "schema");
    await compare(generated, nextGenerated, "generated");
    console.log("API schemas, types and validators match generated output");
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : "API generation failed");
  process.exitCode = 1;
} finally {
  if (temporary) await rm(temporary, { recursive: true, force: true });
}

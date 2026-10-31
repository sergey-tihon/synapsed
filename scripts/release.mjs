import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const tag = process.argv[2];
execFileSync(process.execPath, [join(root, "scripts/workspace.mjs"), "check-release", tag ?? ""], {
  cwd: root,
  stdio: "inherit",
});
if (!/^v\d+\.\d+\.\d+\.0$/.test(tag ?? "")) throw new Error("A matching four-part release tag is required.");
const version = tag.slice(1);
const directory = "release-assets";
const artifacts = [
  ...["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"].map(
    (target) => `Synapsed-${version}-${target}.tar.gz`,
  ),
  `Synapsed-${version}-macos-arm64.streamDeckPlugin`,
  `Synapsed-${version}-macos-arm64-tray.tar.gz`,
  `Synapsed-${version}-raycast.rayext`,
];
const expected = new Set(artifacts.flatMap((name) => [name, `${name}.sha256`]));
const actual = await readdir(directory);
if (actual.length !== expected.size || actual.some((name) => !expected.has(name)))
  throw new Error("Release assets must contain exactly all six native-target artifacts and their checksums.");
for (const name of artifacts) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(join(directory, name))) hash.update(chunk);
  const expectedLine = `${hash.digest("hex")}  ${name}\n`;
  if ((await readFile(join(directory, `${name}.sha256`), "utf8")) !== expectedLine)
    throw new Error(`Release checksum failed: ${name}`);
}
console.log(`Verified ${tag} and ${artifacts.length} artifacts/checksums.`);

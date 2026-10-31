import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, resolve, relative } from "node:path";
import { copyFile, lstat, mkdir, readFile, symlink, unlink } from "node:fs/promises";
import { spawn } from "node:child_process";
import { constants } from "node:fs";

const cwd = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(resolve(cwd, "package.json"));
const commands = {
  build: ["build", "-e", "dist", "-o", "dist"],
  bundle: ["bundle", "-o", "../../dist/Synapsed-0.1.0.0-raycast.rayext"],
  lint: ["lint"],
  develop: ["develop"],
};
const command = process.argv[2];
if (process.argv.length !== 3 || !Object.hasOwn(commands, command)) {
  throw new Error("Expected exactly one SDK command: build, bundle, lint, develop");
}
const typescriptPackage = require.resolve("typescript/package.json");
const typescript = JSON.parse(await readFile(typescriptPackage, "utf8"));
if (typescript.version !== "5.9.3") throw new Error("Raycast requires pinned TypeScript 5.9.3");
const binDirectory = resolve(cwd, "node_modules/.bin");
const tsc = resolve(binDirectory, "tsc");
async function exists(path) {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}
if (!(await exists(tsc))) {
  await mkdir(binDirectory, { recursive: true });
  await symlink(relative(binDirectory, resolve(dirname(typescriptPackage), "bin/tsc")), tsc);
}
const sdkPackage = require.resolve("@raycast/api/package.json");
const sdk = JSON.parse(await readFile(sdkPackage, "utf8"));
if (sdk.version !== "2.10.1") throw new Error("Raycast requires pinned SDK 2.10.1");
const ray = resolve(dirname(sdkPackage), sdk.bin.ray);
const lockAlias = resolve(cwd, "package-lock.json");
let createdAlias = false;
const notices = resolve(cwd, "assets/THIRD-PARTY-NOTICES.txt");
let createdNotices = false;
try {
  if (command === "lint") {
    if (await exists(lockAlias))
      throw new Error("Unexpected extension package-lock.json; preserve it and use the canonical root lockfile");
    await symlink("../../package-lock.json", lockAlias);
    createdAlias = true;
  }
  if (command === "bundle") {
    await mkdir(resolve(cwd, "../../dist"), { recursive: true });
    await copyFile(resolve(cwd, "../../THIRD-PARTY-NOTICES.txt"), notices, constants.COPYFILE_EXCL);
    createdNotices = true;
  }
  const child = spawn(process.execPath, [ray, ...commands[command]], {
    cwd,
    stdio: "inherit",
    shell: false,
    env: command === "lint" ? { ...process.env, CI: "true" } : process.env,
  });
  const forward = (signal) => child.kill(signal);
  const interrupt = () => forward("SIGINT");
  const terminate = () => forward("SIGTERM");
  process.on("SIGINT", interrupt);
  process.on("SIGTERM", terminate);
  try {
    process.exitCode = await new Promise((accept, reject) => {
      child.once("error", reject);
      child.once("exit", (code) => accept(code ?? 1));
    });
  } finally {
    process.off("SIGINT", interrupt);
    process.off("SIGTERM", terminate);
  }
} finally {
  if (createdAlias) await unlink(lockAlias);
  if (createdNotices) await unlink(notices);
}

import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { cp, copyFile, mkdir, mkdtemp, readFile, readdir, rm, chmod, writeFile } from "node:fs/promises";
import { createReadStream } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { basename, dirname, join, relative } from "node:path";
import { tmpdir } from "node:os";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const output = join(root, "dist");
const macos = process.platform === "darwin";
const env = macos ? { ...process.env, MACOSX_DEPLOYMENT_TARGET: "12.0", COPYFILE_DISABLE: "1" } : process.env;
const execute = promisify(execFile);
const plugin = "apps/streamdeck/com.sergey-tihon.synapsed.sdPlugin";
const action = process.argv[2];
if (!macos && process.platform !== "linux") throw new Error("Supported build hosts are macOS Apple Silicon and Linux.");

function run(command, args, cwd = root) {
  console.log(`> ${command} ${args.join(" ")}`);
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, env, stdio: "inherit", shell: false });
    child.once("error", reject);
    child.once("close", (code, signal) =>
      code === 0 ? resolve() : reject(new Error(`${command} failed (${signal ?? code}).`)),
    );
  });
}
const npm = (workspace, command) => run("npm", ["run", command, "--workspace", workspace]);
const ray = (command) => run(process.execPath, ["apps/raycast/scripts/run-ray.mjs", command]);
function requireMacos() {
  if (!macos) throw new Error("This client requires a macOS Apple Silicon build host.");
}
async function target() {
  const { stdout } = await execute("rustc", ["-vV"], { cwd: root, env });
  const host = stdout.match(/^host: (.+)$/m)?.[1];
  if (!host || (macos && host !== "aarch64-apple-darwin") || (!macos && host !== "x86_64-unknown-linux-gnu")) {
    throw new Error("Build on a supported native Rust host target.");
  }
  return host;
}
async function cargo(args) {
  const separator = args.indexOf("--");
  const cargoArgs = separator < 0 ? args : args.slice(0, separator);
  const rustcArgs = separator < 0 ? [] : args.slice(separator + 1);
  await run("cargo", [
    ...cargoArgs,
    "--target",
    await target(),
    "--locked",
    ...(separator < 0 ? [] : ["--", ...rustcArgs]),
  ]);
}
async function portable() {
  await npm("@synapsed/client", "build");
  await npm("@synapsed/web", "build");
  await cargo(["build"]);
}
async function build() {
  await portable();
  await npm("@synapsed/streamdeck", "build");
  if (macos) {
    await ray("build");
    await cargo(["build", "-p", "synapsed-macos-tray"]);
  }
}
async function versions(tag) {
  const manifest = JSON.parse(await readFile(join(root, plugin, "manifest.json"), "utf8"));
  const pkg = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
  const version = manifest.Version;
  if (!/^\d+\.\d+\.\d+\.0$/.test(version) || version !== `${pkg.version}.0` || (tag && tag !== `v${version}`)) {
    throw new Error("Tag, four-part manifest and three-part npm versions must match.");
  }
  const cargoSource = await readFile(join(root, "Cargo.toml"), "utf8");
  const workspacePackage = cargoSource.split(/^\[workspace\.package\]\s*$/m)[1]?.split(/^\[/m)[0];
  if (workspacePackage?.match(/^version\s*=\s*"([^"]+)"/m)?.[1] !== pkg.version)
    throw new Error("Rust workspace version must match npm.");
  const plist = await readFile(join(root, "apps/macos-tray/Info.plist"), "utf8");
  const plistVersion = plist.match(/<key>CFBundleShortVersionString<\/key>\s*<string>([^<]+)<\/string>/)?.[1];
  const buildVersion = plist.match(/<key>CFBundleVersion<\/key>\s*<string>([^<]+)<\/string>/)?.[1];
  if (plistVersion !== pkg.version || buildVersion !== pkg.version)
    throw new Error("macOS tray plist versions must match npm.");
  const omarchy = JSON.parse(await readFile(join(root, "apps/omarchy/manifest.json"), "utf8"));
  if (omarchy.version !== pkg.version) throw new Error("Omarchy plugin manifest version must match npm.");
  return version;
}
async function binaryDirectory(profile) {
  const { stdout } = await execute("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"], {
    cwd: root,
    env,
    maxBuffer: 4 * 1024 * 1024,
  });
  return join(JSON.parse(stdout).target_directory, await target(), profile);
}
async function checksum(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  await writeFile(`${path}.sha256`, `${hash.digest("hex")}  ${basename(path)}\n`);
  console.log(relative(root, path));
}
async function archive(staging, filename) {
  const path = join(output, filename);
  await run("tar", ["-czf", path, "-C", staging, "."]);
  await checksum(path);
}
async function daemonArchive(version) {
  await npm("@synapsed/client", "build");
  await npm("@synapsed/web", "build");
  await cargo(["build", "--release"]);
  const staging = await mkdtemp(join(tmpdir(), "synapsed-daemon-package-"));
  try {
    await mkdir(join(staging, "bin"));
    const binaries = await binaryDirectory("release");
    for (const name of ["synapsed", "synapsectl"]) {
      const destination = join(staging, "bin", name);
      await copyFile(join(binaries, name), destination);
      await chmod(destination, 0o755);
    }
    for (const name of ["generic-ddc", "benq-rd280u"])
      await cp(join(root, "drivers", name), join(staging, "drivers", name), { recursive: true });
    for (const name of ["deploy", "docs"]) await cp(join(root, name), join(staging, name), { recursive: true });
    if (!macos) {
      await cp(join(root, "apps/omarchy"), join(staging, "omarchy"), {
        recursive: true,
        filter: (source) => !source.includes(join(root, "apps", "omarchy", "tests")),
      });
    }
    for (const name of ["README.md", "CONTRIBUTE.md", "LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.txt"])
      await copyFile(join(root, name), join(staging, name));
    await archive(staging, `Synapsed-${version}-${await target()}.tar.gz`);
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
}
async function pluginArchive(version) {
  requireMacos();
  await npm("@synapsed/streamdeck", "build");
  const staging = await mkdtemp(join(tmpdir(), "synapsed-streamdeck-package-"));
  try {
    // The SDK rewrites its input manifest; never pass it the source bundle.
    const stagedPlugin = join(staging, basename(plugin));
    const sourceLogs = join(root, plugin, "logs");
    await cp(join(root, plugin), stagedPlugin, {
      recursive: true,
      filter: (source) => source !== sourceLogs,
    });
    await run("npm", [
      "exec",
      "--offline",
      "--",
      "streamdeck",
      "pack",
      stagedPlugin,
      "--output",
      staging,
      "--no-update-check",
      "--force",
    ]);
    const files = (await readdir(staging)).filter((name) => name.endsWith(".streamDeckPlugin"));
    if (files.length !== 1) throw new Error("Stream Deck pack did not create exactly one installable plugin.");
    const destination = join(output, `Synapsed-${version}-macos-arm64.streamDeckPlugin`);
    await copyFile(join(staging, files[0]), destination);
    await checksum(destination);
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
}
async function trayArchive(version) {
  requireMacos();
  await cargo(["build", "--release", "-p", "synapsed-macos-tray"]);
  const staging = await mkdtemp(join(tmpdir(), "synapsed-tray-package-"));
  try {
    const contents = join(staging, "Synapsed.app", "Contents");
    await mkdir(join(contents, "MacOS"), { recursive: true });
    const binary = join(contents, "MacOS", "synapsed-macos-tray");
    await copyFile(join(await binaryDirectory("release"), "synapsed-macos-tray"), binary);
    await chmod(binary, 0o755);
    await copyFile(join(root, "apps/macos-tray/Info.plist"), join(contents, "Info.plist"));
    await mkdir(join(contents, "Resources"), { recursive: true });
    await run("iconutil", [
      "-c",
      "icns",
      join(root, "apps/macos-tray/AppIcon.iconset"),
      "-o",
      join(contents, "Resources/AppIcon.icns"),
    ]);
    for (const name of ["LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.txt"])
      await copyFile(join(root, name), join(staging, name));
    await archive(staging, `Synapsed-${version}-macos-arm64-tray.tar.gz`);
    console.log("Tray archive is unsigned and not notarized.");
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
}
async function raycastArchive(version) {
  requireMacos();
  await ray("bundle");
  await checksum(join(output, `Synapsed-${version}-raycast.rayext`));
}

try {
  switch (action) {
    case "build":
      await build();
      break;
    case "build-server":
      await cargo(["build"]);
      break;
    case "build-tray":
      requireMacos();
      await cargo(["build", "-p", "synapsed-macos-tray"]);
      break;
    case "lint":
      // Clippy compiles the web-ui feature, which embeds the built dashboard.
      await npm("@synapsed/client", "build");
      await npm("@synapsed/web", "build");
      await cargo([
        "clippy",
        "--workspace",
        "--all-targets",
        ...(!macos ? ["--exclude", "synapsed-macos-tray"] : []),
        "--",
        "-D",
        "warnings",
      ]);
      await npm("@synapsed/streamdeck", "typecheck");
      await npm("@synapsed/web", "typecheck");
      break;
    case "test":
      await build();
      await run("npm", ["run", "api:check"]);
      await run("npm", ["run", "brand:check"]);
      await cargo(["test", "--workspace", "--all-features", ...(!macos ? ["--exclude", "synapsed-macos-tray"] : [])]);
      await npm("@synapsed/client", "test");
      await npm("@synapsed/web", "test");
      await npm("@synapsed/streamdeck", "test");
      await run(process.execPath, ["--test", "apps/omarchy/tests/model.test.mjs"]);
      break;
    case "validate":
      requireMacos();
      await npm("@synapsed/streamdeck", "validate");
      break;
    case "check-release":
      await versions(process.argv[3]);
      break;
    case "pack": {
      await mkdir(output, { recursive: true });
      const version = await versions();
      await daemonArchive(version);
      if (macos) {
        await pluginArchive(version);
        await trayArchive(version);
        await raycastArchive(version);
      }
      break;
    }
    case "pack-plugin":
      requireMacos();
      await mkdir(output, { recursive: true });
      await npm("@synapsed/client", "build");
      await pluginArchive(await versions());
      break;
    case "pack-tray":
      requireMacos();
      await mkdir(output, { recursive: true });
      await trayArchive(await versions());
      break;
    case "pack-raycast":
      requireMacos();
      await mkdir(output, { recursive: true });
      await npm("@synapsed/client", "build");
      await raycastArchive(await versions());
      break;
    default:
      throw new Error(`Unknown workspace command: ${action ?? "missing"}.`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}

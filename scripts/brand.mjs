import { readFile, writeFile, mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { inflateSync } from "node:zlib";
import { Resvg } from "@resvg/resvg-js";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputs = new Map();
const mono = await readFile(join(root, "assets/brand/mark-mono.svg"), "utf8");
// Detailed app tile for large surfaces; simplified tile/mark for small, tray and favicon sizes.
const tile = await readFile(join(root, "assets/brand/mark-tile.svg"), "utf8");
const tileSmall = await readFile(join(root, "assets/brand/mark-tile-small.svg"), "utf8");
const monoWhite = mono.replaceAll("#000", "#FFFFFF");
const appTile = (size) => svgRender(size < 128 ? tileSmall : tile, size);

function svgRender(source, size, background = undefined) {
  const options = { fitTo: { mode: "width", value: size }, ...(background ? { background } : {}) };
  return new Resvg(source, options).render().asPng();
}
function add(path, content) {
  outputs.set(path, Buffer.isBuffer(content) ? content : Buffer.from(content));
}
function iconsetName(size, scale) {
  return `icon_${size}x${size}${scale === 2 ? "@2x" : ""}.png`;
}
function ico(pngs) {
  const header = Buffer.alloc(6 + pngs.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(pngs.length, 4);
  let offset = header.length;
  pngs.forEach(([size, png], index) => {
    const entry = 6 + index * 16;
    header[entry] = size;
    header[entry + 1] = size;
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(png.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += png.length;
  });
  return Buffer.concat([header, ...pngs.map(([, png]) => png)]);
}
function pngRgba(png) {
  let offset = 8;
  const chunks = [];
  let width;
  let height;
  while (offset < png.length) {
    const length = png.readUInt32BE(offset);
    const type = png.toString("ascii", offset + 4, offset + 8);
    const data = png.subarray(offset + 8, offset + 8 + length);
    offset += length + 12;
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      if (data[8] !== 8 || data[9] !== 6) throw new Error("Expected 8-bit RGBA PNG");
    }
    if (type === "IDAT") chunks.push(data);
  }
  const stride = width * 4;
  const filtered = inflateSync(Buffer.concat(chunks));
  const pixels = Buffer.alloc(height * stride);
  let at = 0;
  for (let y = 0; y < height; y++) {
    const filter = filtered[at++];
    for (let x = 0; x < stride; x++) {
      const raw = filtered[at++];
      const left = x >= 4 ? pixels[y * stride + x - 4] : 0;
      const up = y ? pixels[(y - 1) * stride + x] : 0;
      const upperLeft = y && x >= 4 ? pixels[(y - 1) * stride + x - 4] : 0;
      let predictor = 0;
      if (filter === 1) predictor = left;
      else if (filter === 2) predictor = up;
      else if (filter === 3) predictor = Math.floor((left + up) / 2);
      else if (filter === 4) {
        const p = left + up - upperLeft;
        const pa = Math.abs(p - left);
        const pb = Math.abs(p - up);
        const pc = Math.abs(p - upperLeft);
        predictor = pa <= pb && pa <= pc ? left : pb <= pc ? up : upperLeft;
      } else if (filter !== 0) throw new Error(`Unsupported PNG filter ${filter}`);
      pixels[y * stride + x] = (raw + predictor) & 255;
    }
  }
  return pixels;
}
add("apps/web/public/favicon.svg", tileSmall);
add("apps/web/public/favicon.ico", ico([16, 32, 48].map((size) => [size, appTile(size)])));
add("apps/web/public/apple-touch-icon.png", svgRender(tile, 180, "#000000"));
add("apps/raycast/assets/icon.png", appTile(512));
const sd = "apps/streamdeck/com.sergey-tihon.synapsed.sdPlugin/";
add(`${sd}imgs/plugin/marketplace.png`, appTile(256));
add(`${sd}imgs/plugin/marketplace@2x.png`, appTile(512));
add(`${sd}imgs/plugin/category-icon.png`, svgRender(monoWhite, 28));
add(`${sd}imgs/plugin/category-icon@2x.png`, svgRender(monoWhite, 56));
add(`${sd}imgs/actions/characteristic/icon.png`, svgRender(monoWhite, 20));
add(`${sd}imgs/actions/characteristic/icon@2x.png`, svgRender(monoWhite, 40));
add(`${sd}imgs/actions/characteristic/key.png`, svgRender(tileSmall, 72));
add(`${sd}imgs/actions/characteristic/key@2x.png`, svgRender(tileSmall, 144));

// Mono mark paths are the single source for the Omarchy glyph and Stream Deck key mark.
const monoPaths = [...mono.matchAll(/<path stroke-width="(\d+)" d="([^"]+)"\/>/g)].map(([, width, d]) => ({
  width: Number(width),
  d,
}));
if (monoPaths.length !== 2) throw new Error("Expected two stroked paths in mark-mono.svg");
const qmlPaths = monoPaths
  .map(
    ({ width, d }) =>
      `    ShapePath {\n        strokeWidth: ${width}\n        capStyle: ShapePath.RoundCap\n        joinStyle: ShapePath.RoundJoin\n        strokeColor: foregroundColor\n        fillColor: "transparent"\n        PathSvg { path: "${d}" }\n    }\n`,
  )
  .join("");
add(
  "apps/omarchy/BrandGlyph.qml",
  `import QtQuick\nimport QtQuick.Shapes\n\nShape {\n    id: brandGlyph\n    property color foregroundColor: "white"\n    transform: Scale {\n        xScale: Math.min(brandGlyph.width, brandGlyph.height) / 1024\n        yScale: Math.min(brandGlyph.width, brandGlyph.height) / 1024\n    }\n\n${qmlPaths}}\n`,
);

const keyMarkPaths = (stroke) =>
  monoPaths.map(({ width, d }) => `<path d="${d}" stroke="${stroke}" stroke-width="${width}"/>`).join("");
const keyMark = `<g transform="translate(22 36) scale(.0977)" fill="none" stroke-linecap="round" stroke-linejoin="round">${keyMarkPaths("url(#m)")}</g>`;
function keySvg(kind) {
  const glyphs = {
    configure: keyMark,
    number: `<path d="M52 72h40m-40 28h40m-40 28h40M72 62v20m0 28v20"/>`,
    "boolean-on": `<circle cx="72" cy="108" r="34"/><path d="M72 58v45"/><circle cx="118" cy="116" r="14" fill="#6CE3E7" stroke="none"/>`,
    "boolean-off": `<circle cx="72" cy="108" r="34"/><path d="M72 58v45"/>`,
    enum: `<rect x="44" y="72" width="56" height="18" rx="4"/><rect x="44" y="101" width="56" height="18" rx="4"/><rect x="44" y="130" width="56" height="18" rx="4"/>`,
    action: `<path d="M82 52 49 108h26l-8 40 39-58H79z"/>`,
    string: `<path d="M44 76h56M72 76v68M52 144h40M107 80h34m-17 0v64m-15 0h30"/>`,
    offline: `${keyMark}<path d="M42 42l104 104" stroke="#6CE3E7"/>`,
  };
  return `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="0"><stop stop-color="#8E86DC"/><stop offset="1" stop-color="#6CE3E7"/></linearGradient><linearGradient id="m" x1="0" y1="232" x2="0" y2="792" gradientUnits="userSpaceOnUse"><stop stop-color="#6CE3E7"/><stop offset=".5" stop-color="#7AB8EA"/><stop offset="1" stop-color="#8E86DC"/></linearGradient></defs><rect width="144" height="144" rx="20" fill="#11151C"/><path d="M0 0h144v6H0z" fill="url(#g)"/><g fill="none" stroke="#E6E9F5" stroke-width="7" stroke-linecap="round" stroke-linejoin="round">${glyphs[kind]}</g></svg>`;
}
for (const kind of ["configure", "number", "boolean-on", "boolean-off", "enum", "action", "string", "offline"])
  add(`${sd}imgs/keys/${kind}.svg`, keySvg(kind));

const rgba = pngRgba(svgRender(mono, 36));
const black = Buffer.alloc(36 * 36 * 4);
for (let i = 0; i < rgba.length; i += 4) {
  black[i + 3] = rgba[i + 3];
}
add("apps/macos-tray/assets/tray-template.rgba", black);
for (const size of [16, 32, 128, 256, 512])
  for (const scale of [1, 2]) add(`apps/macos-tray/AppIcon.iconset/${iconsetName(size, scale)}`, appTile(size * scale));

async function materialize(dir) {
  for (const [path, content] of outputs) {
    const dest = join(dir, path);
    await mkdir(dirname(dest), { recursive: true });
    await writeFile(dest, content);
  }
}
const mode = process.argv[2];
if (mode === "generate") {
  for (const [path, content] of outputs) {
    const dest = join(root, path);
    await mkdir(dirname(dest), { recursive: true });
    await writeFile(dest, content);
  }
  await rm(join(root, `${sd}imgs/actions/benq`), { recursive: true, force: true });
} else if (mode === "check") {
  try {
    await readFile(join(root, `${sd}imgs/actions/benq/icon.png`));
    throw new Error("Legacy BenQ-specific Stream Deck icons remain");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const temp = await mkdtemp(join(tmpdir(), "synapsed-brand-"));
  try {
    await materialize(temp);
    for (const path of outputs.keys()) {
      const expected = await readFile(join(temp, path));
      let actual;
      try {
        actual = await readFile(join(root, path));
      } catch {
        throw new Error(`Missing generated brand asset: ${path}`);
      }
      if (!actual.equals(expected)) throw new Error(`Generated brand asset is stale: ${path}`);
    }
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
} else throw new Error("Usage: node scripts/brand.mjs generate|check");

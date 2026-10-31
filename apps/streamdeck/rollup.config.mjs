import commonjs from "@rollup/plugin-commonjs";
import nodeResolve from "@rollup/plugin-node-resolve";
import terser from "@rollup/plugin-terser";
import typescript from "@rollup/plugin-typescript";
import path from "node:path";
import url from "node:url";
import { readFileSync } from "node:fs";

const isWatching = !!process.env.ROLLUP_WATCH;
const sdPlugin = "com.sergey-tihon.synapsed.sdPlugin";
const notices = new URL("../../THIRD-PARTY-NOTICES.txt", import.meta.url);

export default {
  input: "src/plugin.ts",
  output: {
    format: "es",
    file: `${sdPlugin}/bin/plugin.js`,
    sourcemap: isWatching,
    sourcemapPathTransform: (source, map) => url.pathToFileURL(path.resolve(path.dirname(map), source)).href,
  },
  plugins: [
    {
      name: "watch-manifest",
      buildStart() {
        this.addWatchFile(`${sdPlugin}/manifest.json`);
        this.addWatchFile(url.fileURLToPath(notices));
      },
    },
    typescript({ mapRoot: isWatching ? "./" : undefined }),
    nodeResolve({ browser: false, exportConditions: ["node"], preferBuiltins: true }),
    commonjs(),
    !isWatching && terser(),
    {
      name: "emit-module-package-file",
      generateBundle() {
        this.emitFile({ fileName: "package.json", source: '{ "type": "module" }', type: "asset" });
        this.emitFile({ fileName: "THIRD-PARTY-NOTICES.txt", source: readFileSync(notices, "utf8"), type: "asset" });
      },
    },
  ],
};

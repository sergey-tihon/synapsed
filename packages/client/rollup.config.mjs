import commonjs from "@rollup/plugin-commonjs";
import nodeResolve from "@rollup/plugin-node-resolve";
import typescript from "@rollup/plugin-typescript";

export default {
  input: "src/index.ts",
  output: {
    dir: "dist",
    format: "es",
    sourcemap: true,
  },
  plugins: [
    typescript({ tsconfig: "./tsconfig.json" }),
    nodeResolve({ browser: true, preferBuiltins: false }),
    // Ajv standalone ESM still uses require() for a few runtime helpers.
    // Bundle those helpers, not Ajv's runtime schema compiler.
    commonjs({ transformMixedEsModules: true }),
  ],
};

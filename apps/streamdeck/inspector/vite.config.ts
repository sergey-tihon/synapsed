import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vite";

export default defineConfig({
  root: "inspector",
  base: "./",
  plugins: [solid()],
  build: {
    outDir: "../com.sergey-tihon.synapsed.sdPlugin/bin/inspector",
    emptyOutDir: true,
    cssCodeSplit: false,
    modulePreload: false,
    rollupOptions: {
      output: {
        format: "iife",
        inlineDynamicImports: true,
        entryFileNames: "inspector.js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
});
